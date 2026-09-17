//! 终端通知模块。
//!
//! 本模块负责探测终端通知协议，并在事件点（回合完成 / 需要用户输入）向
//! 终端写入通知字节。
//!
//! # 通道选择（重要）
//!
//! 当前唯一实际生效的通道是 **BEL（`0x07`）**：用户终端栈为
//! Ghostty → rmux（tmux 兼容复用器）→ visp-cli TUI。rmux 会把 pane 内的
//! `0x07` 原样转发给外层终端（Ghostty 收到后响铃 + dock 弹跳），而
//! OSC 9 / OSC 777 / OSC 99 会被 rmux 静默丢弃（其 `osc_notification`
//! 为空实现）。因此编码器只做 BEL；OSC 序列等横幅工作需要时再实现，
//! 届时需把 rmux 环境路由回 BEL。

use std::time::{Duration, Instant};

/// BEL 控制字符（`0x07`）：当前唯一能穿透 rmux 的通知通道。
pub const BEL: u8 = 0x07;

/// 编码 BEL 通知字节。
///
/// BEL 不带任何文本负载；OSC 9/777/99 等带文本的协议在 rmux 下会被
/// 丢弃，等横幅工作需要时再实现 `encode(protocol, ...)`（函数名与本
/// 函数不冲突）。
pub fn encode_bell() -> Vec<u8> {
    vec![BEL]
}

/// 终端 OSC 通知协议。
///
/// 优先级（高 → 低）：`Kitty99` > `Osc777` > `Osc9`。
/// 来源：kitty 的 `kitten notify` 文档（Kitty99）、WezTerm 的
/// OSC 777 扩展、以及被 ghostty / iTerm2 等广泛支持的 OSC 9。
///
/// 注意：当前编码器只输出 BEL，协议仅作为 `selected_protocol()` 的
/// 可观测值（配置 override 会反映在这里）；OSC 编码在 Wave 3 实现。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Protocol {
    /// kitty 终端协议（`OSC 99`），支持标题 + 正文。
    Kitty99,
    /// WezTerm 协议（`OSC 777`）。
    Osc777,
    /// 通用协议（`OSC 9`），ghostty / iTerm.app 等支持；其余终端静默忽略。
    Osc9,
}

/// 通知事件类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotifyKind {
    /// 任务完成。
    Done,
    /// 需要用户输入。
    UserQuery,
}

/// 终端环境快照（探测输入）。
#[derive(Debug, Clone)]
pub struct Env {
    kitty_window_id: Option<String>,
    term: Option<String>,
    term_program: Option<String>,
}

impl Env {
    /// 从进程环境变量读取（生产路径）。
    pub fn from_process_env() -> Env {
        Env {
            kitty_window_id: std::env::var("KITTY_WINDOW_ID").ok(),
            term: std::env::var("TERM").ok(),
            term_program: std::env::var("TERM_PROGRAM").ok(),
        }
    }

    /// 从显式键值对构造（测试注入，避免污染进程环境变量）。
    #[allow(dead_code)] // 仅测试注入使用（notify_tests）
    pub fn from_pairs(
        kitty_window_id: Option<&str>,
        term: Option<&str>,
        term_program: Option<&str>,
    ) -> Env {
        Env {
            kitty_window_id: kitty_window_id.map(str::to_owned),
            term: term.map(str::to_owned),
            term_program: term_program.map(str::to_owned),
        }
    }
}

/// `TERM_PROGRAM` 值（小写）→ 协议 映射表。
///
/// 注意：iTerm.app 的 `TERM_PROGRAM` 字面量就是 `iTerm.app`（含点号）。
const TERM_PROGRAM_PROTOCOLS: &[(&str, Protocol)] = &[
    ("wezterm", Protocol::Osc777),
    ("ghostty", Protocol::Osc9),
    ("iterm.app", Protocol::Osc9),
];

/// 按优先级探测终端协议（高 → 低）：
/// 1. Kitty：`KITTY_WINDOW_ID` 存在或 `TERM` 含 `"kitty"` → `Kitty99`
/// 2. WezTerm：`TERM_PROGRAM` 不区分大小写等于 `"WezTerm"` → `Osc777`
/// 3. ghostty / iTerm.app：`TERM_PROGRAM` 或 `TERM` 含 `"ghostty"` → `Osc9`
/// 4. 其余情况（含全空环境）→ `Osc9`（盲发兜底：不支持的终端静默忽略 OSC 序列，无副作用）
fn probe(env: &Env) -> Protocol {
    // 1. Kitty 优先：KITTY_WINDOW_ID 是 kitty 终端特有的环境变量。
    if env.kitty_window_id.is_some() || env.term.as_deref().is_some_and(|t| t.contains("kitty")) {
        return Protocol::Kitty99;
    }
    // 2/3. TERM_PROGRAM 映射表（不区分大小写）。
    if let Some(tp) = env.term_program.as_deref() {
        let lower = tp.to_ascii_lowercase();
        if let Some((_, p)) = TERM_PROGRAM_PROTOCOLS.iter().find(|(k, _)| *k == lower) {
            return *p;
        }
    }
    // 3b. TERM 含 "ghostty"（SSH 透传场景：TERM_PROGRAM 为空，TERM=xterm-ghostty）。
    if env.term.as_deref().is_some_and(|t| t.contains("ghostty")) {
        return Protocol::Osc9;
    }
    // 4. 盲发兜底。
    Protocol::Osc9
}

/// 探测终端通知协议。
///
/// - `is_tty == false` → `None`（整模块禁用；即使有 override 也返回 `None`）
/// - `protocol_override = Some(p)` → `Some(p)`（配置优先于探测）
/// - 否则按 [`probe`] 的优先级返回。
pub fn detect(env: &Env, protocol_override: Option<Protocol>, is_tty: bool) -> Option<Protocol> {
    // 非 tty：整模块禁用，override 也不生效。
    if !is_tty {
        return None;
    }
    // 配置优先于探测。
    if let Some(p) = protocol_override {
        return Some(p);
    }
    Some(probe(env))
}

/// 通知引擎：事件闸门链 + 节流 + 编码。
pub struct NotifyEngine {
    /// `None` = 禁用（非 tty 或配置关闭时）。
    protocol: Option<Protocol>,
    enabled: bool,
    on_complete: bool,
    on_user_input: bool,
    min_interval: Duration,
    /// 每种通知类型的最近发送时刻（下标 = `NotifyKind as usize`）。
    last_sent: [Option<Instant>; 2],
}

impl NotifyEngine {
    /// 构造引擎。`is_tty == false` 或 `enabled == false` 时 `protocol = None`（禁用）。
    pub fn new(
        is_tty: bool,
        protocol_override: Option<Protocol>,
        enabled: bool,
        on_complete: bool,
        on_user_input: bool,
        min_interval: Duration,
    ) -> Self {
        // 配置关闭 → 整模块禁用；否则由 detect 决定（非 tty 时 detect 返回 None）。
        let protocol = if enabled {
            detect(&Env::from_process_env(), protocol_override, is_tty)
        } else {
            None
        };
        Self {
            protocol,
            enabled,
            on_complete,
            on_user_input,
            min_interval,
            last_sent: [None, None],
        }
    }

    /// 禁用态：`protocol = None` + 各开关 `false`。
    pub fn disabled() -> Self {
        Self {
            protocol: None,
            enabled: false,
            on_complete: false,
            on_user_input: false,
            min_interval: Duration::ZERO,
            last_sent: [None, None],
        }
    }

    /// 当前选中的协议（`None` = 禁用）。
    pub fn selected_protocol(&self) -> Option<Protocol> {
        self.protocol
    }

    /// 指定通知类型的最近发送时刻。
    #[allow(dead_code)] // 仅测试断言使用（event_tests 验证挂接点）
    pub fn last_sent_at(&self, kind: NotifyKind) -> Option<Instant> {
        self.last_sent[kind as usize]
    }

    /// 事件入口。闸门链（计划文档 step 3b，BEL 版）：
    /// `protocol.is_none()`（禁用短路）→ `!is_main`（会话过滤）→
    /// 对应 kind 开关 → 节流（按 kind 独立计时）→ 编码 BEL → 更新 `last_sent`。
    ///
    /// `body` 保留：BEL 不带文本负载，OSC 编码（Wave 3）会用到。
    pub fn on_event(
        &mut self,
        kind: NotifyKind,
        is_main: bool,
        body: &str,
        now: Instant,
    ) -> Option<Vec<u8>> {
        // 闸门 1：禁用短路。protocol 为 None（非 tty 或配置关闭）时整模块不工作。
        self.protocol?;
        // enabled 防御性复查：`new()` 在 enabled=false 时已把 protocol 置 None，
        // 此检查不改变行为，仅保持字段可读（与计划文档 3b 的 enabled 闸门一致）。
        if !self.enabled {
            return None;
        }
        // 闸门 2：会话过滤——子 session 不响铃。
        if !is_main {
            return None;
        }
        // 闸门 3：对应 kind 开关。
        let kind_enabled = match kind {
            NotifyKind::Done => self.on_complete,
            NotifyKind::UserQuery => self.on_user_input,
        };
        if !kind_enabled {
            return None;
        }
        // 闸门 4：节流（按 kind 独立计时；min_interval == 0 表示不节流）。
        if self.min_interval > Duration::ZERO
            && let Some(last) = self.last_sent[kind as usize]
            && now.saturating_duration_since(last) < self.min_interval
        {
            return None;
        }
        // 编码 BEL 并更新 last_sent。
        let _ = body; // BEL 不带文本；OSC 编码（Wave 3）会使用 body。
        self.last_sent[kind as usize] = Some(now);
        Some(encode_bell())
    }
}

/// 唯一 IO 点（决策 D5）：把通知字节写入 stdout 并 flush。
///
/// 失败时 `tracing::debug!` 静默降级——通知是可选功能，写失败不影响主流程。
pub fn write_to_stdout(bytes: &[u8]) {
    use std::io::Write;
    let mut out = std::io::stdout().lock();
    if let Err(e) = out.write_all(bytes).and_then(|_| out.flush()) {
        tracing::debug!("notify: failed to write to stdout: {e}");
    }
}

#[cfg(test)]
#[path = "notify_tests.rs"]
mod tests;