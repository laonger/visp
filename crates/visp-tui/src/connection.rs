#![allow(dead_code)]

//! TUI 连接状态机、idle 看门狗判定与重连恢复抽象（设计 §5.9、§7 决策 14）。
//!
//! 本模块只承载**可测的纯逻辑**与**连接抽象**：
//! - 纯函数：状态迁移 `transition`、退避计算 `backoff_delay`、idle 判定 `idle_expired`。
//! - 常量集中定义：idle 阈值、探测超时、退避端点（回滚时只改这里，设计 §9）。
//! - [`Connection`] trait：建立新连接 / 下行收帧 / 上行发送 / 健康探测 / 丢弃旧连接五职责。
//!   `client.rs` 的 `VispClient`/`ChatHandle` 经 [`LiveConnection`] 挂接；测试用脚本化假体。
//! - [`recover`]：重连恢复流程（新连接 → 健康探测 → join 回放 → 刷新模型）。
//!
//! event loop 的接线（`event.rs`）只消费本模块的纯函数与 `recover`，状态本身存于
//! `AppState.connection_state`（设计 §5.9）。

use std::future::Future;
use std::time::{Duration, Instant};

use visp_proto::visp::ServerMessage;

use crate::client::{ChatHandle, VispClient};

// ── 常量集中定义（回滚要求：紧急调整只改这里，设计 §9）──────────────

/// idle 看门狗阈值：空闲连接超过此时长触发一次独立健康探测。
pub const IDLE_TIMEOUT: Duration = Duration::from_secs(45);
/// 单次健康探测超时：超时按探测失败处理。
pub const PROBE_TIMEOUT: Duration = Duration::from_secs(5);
/// 指数退避起始延迟（首次重连尝试前的等待）。
pub const BACKOFF_INITIAL: Duration = Duration::from_secs(1);
/// 指数退避封顶延迟。
pub const BACKOFF_MAX: Duration = Duration::from_secs(30);

// ── 状态与事件 ────────────────────────────────────────────────────

/// 连接状态机状态。
///
/// `Reconnecting.attempt` 表示「已失败的连续重连尝试次数」，进入重连时为 0；
/// 每次尝试失败递增，成功后状态回到 [`ConnState::Connected`]（计数自然归零）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnState {
    /// 已连接（含「空闲但探测中」——探测不迁移状态）。
    Connected,
    /// 重连中，`attempt` 为已失败的尝试次数。
    Reconnecting { attempt: u32 },
}

impl ConnState {
    pub fn is_connected(&self) -> bool {
        matches!(self, ConnState::Connected)
    }

    pub fn is_reconnecting(&self) -> bool {
        matches!(self, ConnState::Reconnecting { .. })
    }
}

/// 驱动状态机的事件集合。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnEvent {
    /// 下行流干净关闭（`recv()` 返回 `None`）。
    StreamClosed,
    /// idle 阈值到期（应发起一次健康探测）。
    IdleTimeout,
    /// 健康探测成功。
    ProbeSuccess,
    /// 健康探测失败或超时。
    ProbeFailed,
    /// 重连恢复完成（新流建立 + join 回放 + 刷新模型）。
    Reconnected,
    /// 一次重连尝试失败（进入下一次退避）。
    ReconnectFailed,
    /// 用户退出（Ctrl+D）。
    UserExit,
}

/// 状态机产出的动作指令。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnAction {
    /// 无动作。
    None,
    /// 发起一次独立健康探测。
    Probe,
    /// 按退避执行一次重连恢复。
    Reconnect,
    /// 重置 idle 计时（探测成功或恢复完成）。
    ResetIdle,
    /// 无条件退出。
    Exit,
}

/// 连接状态机纯函数：`(当前状态, 事件) -> (新状态, 动作)`。
///
/// 迁移规则严格遵循设计 §5.9/§7 决策 14：**失败即重连、成功即重置，无任何累计宽限**。
/// 重连期间的陈旧事件（流关闭/idle/探测结果）一律忽略，不产生第三路径。
pub fn transition(state: ConnState, event: ConnEvent) -> (ConnState, ConnAction) {
    use ConnAction::{Exit, None, Probe, Reconnect, ResetIdle};
    use ConnEvent::*;
    match state {
        ConnState::Connected => match event {
            // 两个断线入口（干净关闭 / 探测失败）汇聚到同一迁移。
            StreamClosed | ProbeFailed => (ConnState::Reconnecting { attempt: 0 }, Reconnect),
            IdleTimeout => (state, Probe),
            ProbeSuccess | Reconnected => (state, ResetIdle),
            ReconnectFailed => (state, None),
            UserExit => (state, Exit),
        },
        ConnState::Reconnecting { attempt } => match event {
            UserExit => (state, Exit),
            ReconnectFailed => (
                ConnState::Reconnecting {
                    attempt: attempt + 1,
                },
                Reconnect,
            ),
            Reconnected => (ConnState::Connected, ResetIdle),
            // 陈旧信号不得触发任何新迁移（负向防回潮）。
            StreamClosed | IdleTimeout | ProbeSuccess | ProbeFailed => (state, None),
        },
    }
}

/// 指数退避纯函数：`尝试次数 -> 延迟`（1s 起步、倍增、30s 封顶）。
///
/// `attempt` 为已失败的连续重连次数：0 → 1s、1 → 2s、2 → 4s、…、封顶 30s。
pub fn backoff_delay(attempt: u32) -> Duration {
    let shift = attempt.min(31);
    let factor = 1u64.checked_shl(shift).unwrap_or(u64::MAX);
    let secs = BACKOFF_INITIAL.as_secs().saturating_mul(factor);
    Duration::from_secs(secs.min(BACKOFF_MAX.as_secs()))
}

/// idle 判定纯函数（时钟由调用方注入）。
pub fn idle_expired(last_frame_at: Instant, now: Instant) -> bool {
    now.saturating_duration_since(last_frame_at) >= IDLE_TIMEOUT
}

/// idle 计时器：记录最后收帧时刻，提供「收帧重置 / 到期判定」。
///
/// 看门狗只在空闲连接上活动；任何 Chat 流帧到达都会调用 [`IdleClock::on_frame`]。
#[derive(Debug, Clone, Copy)]
pub struct IdleClock {
    last_frame_at: Instant,
}

impl IdleClock {
    pub fn new(now: Instant) -> Self {
        Self { last_frame_at: now }
    }

    /// 收到任一 Chat 流帧：重置计时。
    pub fn on_frame(&mut self, now: Instant) {
        self.last_frame_at = now;
    }

    /// 探测成功 / 恢复完成后重置计时（进入下一周期）。
    pub fn reset(&mut self, now: Instant) {
        self.last_frame_at = now;
    }

    pub fn expired(&self, now: Instant) -> bool {
        idle_expired(self.last_frame_at, now)
    }

    pub fn last_frame_at(&self) -> Instant {
        self.last_frame_at
    }
}

/// 给健康探测套上 [`PROBE_TIMEOUT`] 超时；超时按失败处理。
pub async fn probe_with_timeout<F>(probe: F) -> Result<bool, String>
where
    F: Future<Output = Result<bool, String>>,
{
    match tokio::time::timeout(PROBE_TIMEOUT, probe).await {
        Ok(r) => r,
        Err(_) => Err("health probe timed out".to_string()),
    }
}

// ── 连接抽象 ──────────────────────────────────────────────────────

/// 连接抽象（设计 §5.9 职责五项）。
///
/// 生产实现 [`LiveConnection`] 挂接 `VispClient`/`ChatHandle`；测试用脚本化假体
/// （帧序列 + 故障注入：流关闭 / 无响应 / 延迟）。
pub trait Connection: Send {
    /// 建立新连接：新 client + 新 Chat 流（旧 Channel 不可复用）。
    fn establish(&mut self) -> impl Future<Output = Result<(), String>> + Send;
    /// 下行收帧：异步等待下一 [`ServerMessage`] 或流结束（`None`）。
    fn recv(&mut self) -> impl Future<Output = Option<ServerMessage>> + Send;
    /// 上行发送：join 回放当前主 session。
    fn send_join(&mut self);
    /// 上行发送：cancel。
    fn send_cancel(&mut self);
    /// 健康探测：独立 unary，不经 Chat 流，带 [`PROBE_TIMEOUT`] 超时。
    fn health_probe(&mut self) -> impl Future<Output = Result<bool, String>> + Send;
    /// 刷新模型列表（`get_session`）。
    fn refresh_models(
        &mut self,
    ) -> impl Future<Output = Result<(Vec<String>, Vec<String>), String>> + Send;
    /// 主动丢弃旧连接。
    fn drop_old(&mut self);
}

/// 重连恢复产物：刷新到的模型列表（`get_session` 非致命，失败时为空）。
#[derive(Debug, Clone, Default)]
pub struct Recovered {
    pub available_models: Vec<String>,
    pub model_keys: Vec<String>,
}

/// 重连成功后的新连接句柄（经 channel 从后台任务送回 event loop）。
pub struct ReconnectSuccess {
    pub client: VispClient,
    pub chat: ChatHandle,
    pub recovered: Recovered,
}

/// 重连恢复流程（复用 `/sessions <id>` 既有路径，设计 §5.9）：
///
/// 1. 重新 connect 构造新 client + 新 Chat 流；
/// 2. `health_check` 确认存活（失败/超时即放弃本次尝试）；
/// 3. 新流 `send_join` 回放当前主 session；
/// 4. `get_session` 刷新 `available_models`/`model_keys`（非致命）。
///
/// 本地 streaming 状态清理属 App 侧职责，由 event loop 调用 [`crate::event`] 侧完成。
pub async fn recover<C: Connection>(conn: &mut C) -> Result<Recovered, String> {
    conn.establish().await?;
    match conn.health_probe().await {
        Ok(true) => {}
        Ok(false) => {
            conn.drop_old();
            return Err("daemon not alive".to_string());
        }
        Err(e) => {
            conn.drop_old();
            return Err(e);
        }
    }
    conn.send_join();
    let (available_models, model_keys) = conn.refresh_models().await.unwrap_or_default();
    Ok(Recovered {
        available_models,
        model_keys,
    })
}

/// [`Connection`] 的生产实现：持有 `VispClient` 与新 Chat 流。
pub struct LiveConnection {
    addr: String,
    session_id: String,
    client: Option<VispClient>,
    chat: Option<ChatHandle>,
}

impl LiveConnection {
    pub fn new(addr: impl Into<String>, session_id: impl Into<String>) -> Self {
        Self {
            addr: addr.into(),
            session_id: session_id.into(),
            client: None,
            chat: None,
        }
    }

    /// 取出恢复后持有的新 client 与新 Chat 流。
    pub fn into_parts(self) -> (VispClient, ChatHandle) {
        (
            self.client.expect("LiveConnection client missing"),
            self.chat.expect("LiveConnection chat missing"),
        )
    }
}

impl Connection for LiveConnection {
    async fn establish(&mut self) -> Result<(), String> {
        let mut client = VispClient::connect(&self.addr).await?;
        let chat = client.chat(&self.session_id).await?;
        self.client = Some(client);
        self.chat = Some(chat);
        Ok(())
    }

    async fn recv(&mut self) -> Option<ServerMessage> {
        match self.chat.as_mut() {
            Some(c) => c.recv().await,
            None => None,
        }
    }

    fn send_join(&mut self) {
        if let Some(c) = self.chat.as_ref() {
            c.send_join();
        }
    }

    fn send_cancel(&mut self) {
        if let Some(c) = self.chat.as_ref() {
            c.send_cancel();
        }
    }

    async fn health_probe(&mut self) -> Result<bool, String> {
        let client = self
            .client
            .as_mut()
            .ok_or_else(|| "no client for health probe".to_string())?;
        probe_with_timeout(client.health_check()).await
    }

    async fn refresh_models(&mut self) -> Result<(Vec<String>, Vec<String>), String> {
        let client = self
            .client
            .as_mut()
            .ok_or_else(|| "no client for get_session".to_string())?;
        let session = client.get_session(&self.session_id).await?;
        Ok((session.available_models, session.model_keys))
    }

    fn drop_old(&mut self) {
        self.chat = None;
        self.client = None;
    }
}

#[cfg(test)]
#[path = "connection_tests.rs"]
mod tests;
