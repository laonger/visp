//! 内置 herdr 绑定（设计 D8；herdr 集成设计决策 3–4）。
//!
//! herdr 是首个**进程内**消费者：以 [`Handler`] 实现接入与脚本规则相同的 executor
//! 接口，并由 [`builtin_rule`] 以 `id = "builtin:herdr"` 的内置规则形式呈现；daemon
//! 侧注册留待接线步骤。
//!
//! ## 状态映射
//!
//! | hook 事件 | herdr state |
//! |---|---|
//! | `UserPromptSubmit` | `working` |
//! | `PermissionRequest` | `blocked`（`--message` 为脱敏摘要） |
//! | `Stop` / `AgentRunEnd` / `SubagentStop` / `SessionStart` | `idle` |
//! | `StopFailure` / `PostToolUseFailure` | `idle` |
//! | 其余事件 | 不上报（no-op） |
//!
//! ## 护栏与失败隔离
//!
//! - **护栏**：仅当 `HERDR_ENV=1` 且 `HERDR_BIN_PATH`/`HERDR_PANE_ID` 均非空时才启用；
//!   否则**静默 no-op**（不 spawn、不计数）。
//! - **失败隔离**：CLI 不存在/超时/非零退出 → 仅 `tracing::debug` 日志 + 计数，
//!   **绝不 panic 或上抛**。
//! - **非阻塞**：执行发生在 executor 的 per-rule worker 内（fire-and-forget 语义），
//!   不阻塞总线发射点；单次调用设短超时。
//!
//! 上报命令：`"$HERDR_BIN_PATH" pane report-agent "$HERDR_PANE_ID" --source custom:visp
//! --agent visp --state <s> [--message <m>]`。argv 构造见 [`report_args`]（纯函数，可测）。

use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use tokio::process::Command;

use crate::dispatcher::{DispatchInput, DispatchRule, Handler, QueuePolicy};
use crate::event::HookEventName;

/// 启用 herdr 上报的开关变量（值须为 [`HERDR_ENV_ON`]）。
pub const HERDR_ENV: &str = "HERDR_ENV";
/// 开关变量启用值。
pub const HERDR_ENV_ON: &str = "1";
/// herdr CLI 可执行文件路径。
pub const HERDR_BIN_PATH: &str = "HERDR_BIN_PATH";
/// 当前 pane 标识。
pub const HERDR_PANE_ID: &str = "HERDR_PANE_ID";
/// 上报 source 标签。
pub const HERDR_SOURCE: &str = "custom:visp";
/// 上报 agent 标签。
pub const HERDR_AGENT: &str = "visp";
/// 内置规则 id。
pub const BUILTIN_HERDR_RULE_ID: &str = "builtin:herdr";
/// 单次 CLI 调用默认超时（毫秒；「数量级秒」的短超时）。
pub const DEFAULT_HERDR_TIMEOUT_MS: u64 = 2_000;

/// herdr 语义状态（仅三态；`unknown` 由 herdr 侧兜底，本绑定不主动上报）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HerdrState {
    /// 生成中。
    Working,
    /// 等待用户处理（审批弹窗 / 提问）。
    Blocked,
    /// 等待新输入。
    Idle,
}

impl HerdrState {
    /// 上报字面量。
    pub const fn as_str(self) -> &'static str {
        match self {
            HerdrState::Working => "working",
            HerdrState::Blocked => "blocked",
            HerdrState::Idle => "idle",
        }
    }
}

/// 事件 → herdr 状态映射；未约定事件返回 `None`（不上报）。
pub fn state_for_event(event: HookEventName) -> Option<HerdrState> {
    match event {
        HookEventName::UserPromptSubmit => Some(HerdrState::Working),
        HookEventName::PermissionRequest => Some(HerdrState::Blocked),
        HookEventName::Stop
        | HookEventName::AgentRunEnd
        | HookEventName::SubagentStop
        | HookEventName::SessionStart
        | HookEventName::StopFailure
        | HookEventName::PostToolUseFailure => Some(HerdrState::Idle),
        _ => None,
    }
}

/// 事件摘要（脱敏）：仅 `PermissionRequest` 携带 `--message`，只暴露 `kind` 语义标签，
/// 不含任何用户原文。
pub fn message_for(event: &DispatchInput) -> Option<String> {
    if event.event != HookEventName::PermissionRequest {
        return None;
    }
    let label = match event.kind.as_deref() {
        Some("approval") => "待审批",
        Some("question") => "待回答",
        _ => "等待用户处理",
    };
    Some(format!("visp {label}"))
}

/// 构造 `pane report-agent` 的 argv（不含可执行文件本身）。
pub fn report_args(pane_id: &str, state: HerdrState, message: Option<&str>) -> Vec<String> {
    let mut args = vec![
        "pane".to_string(),
        "report-agent".to_string(),
        pane_id.to_string(),
        "--source".to_string(),
        HERDR_SOURCE.to_string(),
        "--agent".to_string(),
        HERDR_AGENT.to_string(),
        "--state".to_string(),
        state.as_str().to_string(),
    ];
    if let Some(message) = message {
        args.push("--message".to_string());
        args.push(message.to_string());
    }
    args
}

/// 内置 herdr 规则：同一 executor 接口下的第一条内置规则。
///
/// `on_full = coalesce_latest`（状态型），事件集即 [`state_for_event`] 可映射的事件；
/// 执行由 [`HerdrBinding`] 承担。
pub fn builtin_rule() -> DispatchRule {
    DispatchRule {
        id: BUILTIN_HERDR_RULE_ID.to_string(),
        order: None,
        event: vec![
            HookEventName::SessionStart,
            HookEventName::UserPromptSubmit,
            HookEventName::AgentRunEnd,
            HookEventName::SubagentStop,
            HookEventName::Stop,
            HookEventName::StopFailure,
            HookEventName::PermissionRequest,
            HookEventName::PostToolUseFailure,
        ],
        matcher: None,
        enabled: true,
        on_full: QueuePolicy::CoalesceLatest,
        parallel: false,
        cooldown_ms: 0,
    }
}

/// herdr 上报计数（设计 §15 的失败隔离观测面）。
#[derive(Debug, Default)]
pub struct HerdrStats {
    reported: AtomicU64,
    failed: AtomicU64,
    timed_out: AtomicU64,
    spawn_failed: AtomicU64,
}

impl HerdrStats {
    /// 以零计数构造。
    pub fn new() -> Self {
        Self::default()
    }

    /// CLI 正常退出（exit 0）次数。
    pub fn reported(&self) -> u64 {
        self.reported.load(Ordering::SeqCst)
    }

    /// 启动后失败次数（非零退出 / 被信号终止 / 等待出错）。
    pub fn failed(&self) -> u64 {
        self.failed.load(Ordering::SeqCst)
    }

    /// 超时被杀次数。
    pub fn timed_out(&self) -> u64 {
        self.timed_out.load(Ordering::SeqCst)
    }

    /// 无法启动次数（CLI 不存在 / 权限不足等）。
    pub fn spawn_failed(&self) -> u64 {
        self.spawn_failed.load(Ordering::SeqCst)
    }
}

/// 满足护栏时的上报配置。
#[derive(Debug, Clone)]
struct HerdrConfig {
    bin_path: String,
    pane_id: String,
}

/// 内置 herdr 绑定：把 hook 事件映射为 herdr 状态并经 `HERDR_BIN_PATH` CLI 上报。
///
/// 由环境探测构造（[`HerdrBinding::from_env`]）；护栏不满足时 `config = None`，
/// [`Handler::run`] 静默 no-op。
#[derive(Debug, Clone)]
pub struct HerdrBinding {
    config: Option<HerdrConfig>,
    timeout: Duration,
    stats: std::sync::Arc<HerdrStats>,
}

impl HerdrBinding {
    /// 以默认超时从环境快照构造（测试可注入以避免依赖真实进程环境）。
    pub fn from_env(env: &[(String, String)]) -> Self {
        Self::from_env_with_timeout(env, Duration::from_millis(DEFAULT_HERDR_TIMEOUT_MS))
    }

    /// 以指定单次超时从环境快照构造。
    pub fn from_env_with_timeout(env: &[(String, String)], timeout: Duration) -> Self {
        let get = |key: &str| {
            env.iter()
                .find(|(candidate, _)| candidate == key)
                .map(|(_, value)| value.clone())
        };
        let enabled = get(HERDR_ENV).as_deref() == Some(HERDR_ENV_ON);
        let non_empty = |value: Option<String>| value.filter(|v| !v.is_empty());
        let config = if enabled {
            match (
                non_empty(get(HERDR_BIN_PATH)),
                non_empty(get(HERDR_PANE_ID)),
            ) {
                (Some(bin_path), Some(pane_id)) => Some(HerdrConfig { bin_path, pane_id }),
                _ => None,
            }
        } else {
            None
        };
        Self {
            config,
            timeout,
            stats: std::sync::Arc::new(HerdrStats::new()),
        }
    }

    /// 采集当前进程环境构造。
    pub fn from_current_env() -> Self {
        let env: Vec<(String, String)> = std::env::vars().collect();
        Self::from_env(&env)
    }

    /// 护栏是否满足（`HERDR_ENV=1` 且 BIN/PANE 非空）。
    pub fn enabled(&self) -> bool {
        self.config.is_some()
    }

    /// 计数快照。
    pub fn stats(&self) -> &HerdrStats {
        &self.stats
    }

    /// 执行一次上报（失败仅日志 + 计数，绝不 panic/上抛）。
    async fn execute(&self, event: &DispatchInput) {
        let Some(config) = &self.config else {
            return;
        };
        let Some(state) = state_for_event(event.event) else {
            return;
        };

        let args = report_args(&config.pane_id, state, message_for(event).as_deref());

        let mut command = Command::new(&config.bin_path);
        command
            .args(&args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true);

        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(error) => {
                self.stats.spawn_failed.fetch_add(1, Ordering::SeqCst);
                tracing::debug!(bin = %config.bin_path, %error, "herdr CLI 启动失败，已忽略");
                return;
            }
        };

        match tokio::time::timeout(self.timeout, child.wait()).await {
            Ok(Ok(status)) if status.success() => {
                self.stats.reported.fetch_add(1, Ordering::SeqCst);
            }
            Ok(Ok(status)) => {
                self.stats.failed.fetch_add(1, Ordering::SeqCst);
                tracing::debug!(bin = %config.bin_path, code = ?status.code(), "herdr CLI 非零退出");
            }
            Ok(Err(error)) => {
                self.stats.failed.fetch_add(1, Ordering::SeqCst);
                tracing::debug!(bin = %config.bin_path, %error, "herdr CLI 等待失败");
            }
            Err(_) => {
                let _ = child.kill().await;
                let _ = child.wait().await;
                self.stats.timed_out.fetch_add(1, Ordering::SeqCst);
                tracing::debug!(bin = %config.bin_path, "herdr CLI 超时，已终止");
            }
        }
    }
}

#[async_trait]
impl Handler for HerdrBinding {
    async fn run(&self, _rule: &DispatchRule, event: &DispatchInput) {
        self.execute(event).await;
    }
}
