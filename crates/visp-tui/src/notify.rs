//! 终端通知模块。
//!
//! 本模块负责探测终端通知协议，并在事件点（回合完成 / 需要用户输入）向
//! 终端写入通知字节。
//!
//! # 通道选择（重要）
//!
//! 已识别终端按**标准终端通知协议**发出序列（OSC 9 / OSC 777 / OSC 99）；
//! **未识别终端（含 rmux / tmux 等复用器）回退 BEL（`0x07`）兜底**——
//! 保响铃/🔔 提示，无文本横幅。该策略已修订并记录于设计 §1
//! 「实现偏离说明（2026-09-18）」（原决策「未知终端盲发 OSC 9」作废），
//! 属事实上的复用器适配（不特殊识别某个复用器，通用「未识别 → BEL」）。
//!
//! 复用器拦截 OSC 时不做绕行，是否透传由复用器自身负责（如 tmux 的
//! allow-passthrough）；已识别终端仍按 OSC 9/777/99 发送。
//!
//! # 职责边界
//!
//! 本模块只负责协议探测 / 序列编码 / 开关 / 节流；**不感知 session 归属**。
//! 「仅主 session 通知」由挂接点（`event.rs` 的两处 Done / UserQuery 分支）判定。

use std::time::{Duration, Instant};

/// BEL 控制字符（`0x07`）。
///
/// 未识别终端（含 rmux / tmux 等复用器）的兜底编码，由
/// [`encode`]`(Protocol::Bel, ..)` 使用。
pub const BEL: u8 = 0x07;

/// 编码 BEL 通知字节。
///
/// BEL 不带任何文本负载；OSC 9/777/99 等带文本的协议由 [`encode`] 编码，
/// `on_event` 按探测到的协议分派。未识别终端（含复用器等）的兜底编码，
/// 由 `encode(Protocol::Bel, ..)` 使用。
pub fn encode_bell() -> Vec<u8> {
    vec![BEL]
}

/// OSC 99 单块 payload 上限（字节，编码前）。kitty 规范硬限制。
const OSC99_PAYLOAD_MAX_BYTES: usize = 2048;

/// OSC 99 `f` 元数据中的应用名（base64 编码后发送，供用户侧过滤）。
const APP_NAME: &str = "visp";

/// 按协议编码通知序列（决策 D1/D2/D3/D4）。
///
/// - D1：三种协议统一以 ST（`\x1b\\`）结尾，无尾随字节
/// - D2：控制字符清洗（[`sanitize`]）：`\n`/`\r`/`\t` → 空格，其余 C0/C1 删除
/// - D3：body 为空（空串或清洗后为空）→ 返回 `None`，不生成序列
/// - D4：OSC 99 每块 payload 经 [`truncate_utf8_safe`] 截断至 ≤ 2048 字节
///
/// OSC 99 带标题时发两段：`p=title`（`d=0` 未完，携带 `f=<base64("visp")>`）
/// → `p=body`（`d=1` 结束）；无标题时发单块最简形态 `\x1b]99;;body\x1b\\`。
pub fn encode(protocol: Protocol, title: &str, body: &str) -> Option<Vec<u8>> {
    let body = sanitize(body);
    if body.is_empty() {
        // 决策 D3：空 body（或清洗后为空）不发送。
        return None;
    }
    let title = sanitize(title);
    let mut out = Vec::new();
    match protocol {
        Protocol::Osc9 => {
            // OSC 9 无标题字段，仅正文（标题由终端取窗口标题）。
            out.extend_from_slice(b"\x1b]9;");
            out.extend_from_slice(body.as_bytes());
            out.extend_from_slice(b"\x1b\\");
        }
        Protocol::Osc777 => {
            out.extend_from_slice(b"\x1b]777;notify;");
            out.extend_from_slice(title.as_bytes());
            out.push(b';');
            out.extend_from_slice(body.as_bytes());
            out.extend_from_slice(b"\x1b\\");
        }
        Protocol::Kitty99 => {
            if title.is_empty() {
                // 无标题 → 单块最简形态 `\x1b]99;;body\x1b\\`。
                out.extend_from_slice(b"\x1b]99;;");
                out.extend_from_slice(
                    truncate_utf8_safe(&body, OSC99_PAYLOAD_MAX_BYTES).as_bytes(),
                );
                out.extend_from_slice(b"\x1b\\");
            } else {
                // 标题块（d=0 未完，携带 f=visp 供用户侧过滤）。
                out.extend_from_slice(b"\x1b]99;i=1:d=0:p=title:f=");
                out.extend_from_slice(base64_encode(APP_NAME.as_bytes()).as_bytes());
                out.push(b';');
                out.extend_from_slice(
                    truncate_utf8_safe(&title, OSC99_PAYLOAD_MAX_BYTES).as_bytes(),
                );
                out.extend_from_slice(b"\x1b\\");
                // 正文块（d=1 结束）。
                out.extend_from_slice(b"\x1b]99;i=1:d=1:p=body;");
                out.extend_from_slice(
                    truncate_utf8_safe(&body, OSC99_PAYLOAD_MAX_BYTES).as_bytes(),
                );
                out.extend_from_slice(b"\x1b\\");
            }
        }
        Protocol::Bel => {
            // 未识别终端兜底：仅 BEL，无文本负载（D3 空 body 闸门已在上方拦截）。
            return Some(encode_bell());
        }
    }
    Some(out)
}

/// 控制字符清洗（决策 D2）：
/// - `\n` / `\r` / `\t` → 空格（避免多行正文被拼成一行后不可读）
/// - 其余 C0（U+0000..U+001F，含 ESC/BEL）与 C1（U+0080..U+009F）→ 删除
///
/// 满足 kitty OSC 99 的 safe-UTF-8（无控制字符）要求。
fn sanitize(s: &str) -> String {
    s.chars()
        .filter_map(|c| match c {
            '\n' | '\r' | '\t' => Some(' '),
            c if (c as u32) < 0x20 || (0x80..=0x9F).contains(&(c as u32)) => None,
            c => Some(c),
        })
        .collect()
}

/// UTF-8 边界安全截断（决策 D4）：返回 `s` 的最长前缀，字节数 ≤ `max_bytes`
/// 且不在 UTF-8 字符中间切断。
fn truncate_utf8_safe(s: &str, max_bytes: usize) -> &str {
    if s.len() <= max_bytes {
        return s;
    }
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// 手写 base64 编码（workspace 无 base64 依赖，不新增；仅用于 OSC 99 的
/// `f` 元数据，输入为 ASCII 应用名）。
fn base64_encode(input: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let n = ((chunk[0] as u32) << 16)
            | ((*chunk.get(1).unwrap_or(&0) as u32) << 8)
            | *chunk.get(2).unwrap_or(&0) as u32;
        out.push(TABLE[(n >> 18) as usize & 0x3F] as char);
        out.push(TABLE[(n >> 12) as usize & 0x3F] as char);
        out.push(if chunk.len() > 1 {
            TABLE[(n >> 6) as usize & 0x3F] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            TABLE[n as usize & 0x3F] as char
        } else {
            '='
        });
    }
    out
}

/// 终端 OSC 通知协议。
///
/// 优先级（高 → 低）：`Kitty99` > `Osc777` > `Osc9` > `Bel`。
/// 来源：kitty 的 `kitten notify` 文档（Kitty99）、WezTerm 的
/// OSC 777 扩展、以及被 ghostty / iTerm2 等广泛支持的 OSC 9。
///
/// `Bel` 是未识别终端的兜底编码（含 rmux / tmux 等复用器场景，仅 BEL、
/// 无文本负载）。该兜底是**经用户拍板的刻意选择**（2026-09-18），属事实上
/// 的复用器适配（不特殊识别某个复用器）；已记录于设计 §1
/// 「实现偏离说明（2026-09-18）」（原决策「未知终端盲发 OSC 9」作废）。
///
/// `on_event` 按探测/配置选定的协议调用 [`encode`] 输出对应序列。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Protocol {
    /// kitty 终端协议（`OSC 99`），支持标题 + 正文。
    Kitty99,
    /// WezTerm 协议（`OSC 777`）。
    Osc777,
    /// 通用协议（`OSC 9`），ghostty / iTerm.app 等支持；其余终端静默忽略。
    Osc9,
    /// 未识别终端兜底（`BEL`，`0x07`）：仅响铃/🔔，无文本横幅。
    /// 含 rmux / tmux 等复用器场景；经用户拍板（2026-09-18），
    /// 见设计 §1「实现偏离说明」。
    Bel,
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
/// 4. 其余情况（含全空环境、rmux / tmux 等复用器）→ `Bel`（兜底：仅 BEL
///    响铃，无文本横幅；经用户拍板 2026-09-18，见设计 §1「实现偏离说明」）
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
    // 4. 未识别终端兜底（含复用器）：BEL 响铃，不盲发 OSC。
    Protocol::Bel
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

    /// 事件入口。闸门链（计划文档 step 3b）：
    /// `protocol.is_none()`（禁用短路）→ `enabled` → 对应 kind 开关 →
    /// 节流（按 kind 独立计时）→ 编码（按探测协议）→
    /// 仅在生成序列时更新 `last_sent`。
    ///
    /// **会话过滤（仅根 session）不属本模块职责**：由挂接点 `event.rs`
    /// 判定主 session 后决定是否调用（2026-09-18 分层修正）。
    ///
    /// 文案（设计 §2.1）：标题固定 `visp`；正文由调用方提供（`Done` 传
    /// `event.rs` 的固定文案 `DONE_BODY`，`UserQuery` 传 `uq.message`），
    /// 此处统一按字符截断至 200（决策 D4）。
    pub fn on_event(&mut self, kind: NotifyKind, body: &str, now: Instant) -> Option<Vec<u8>> {
        // 闸门 1：禁用短路。protocol 为 None（非 tty 或配置关闭）时整模块不工作。
        let protocol = self.protocol?;
        // enabled 防御性复查：`new()` 在 enabled=false 时已把 protocol 置 None，
        // 此检查不改变行为，仅保持字段可读（与计划文档 3b 的 enabled 闸门一致）。
        if !self.enabled {
            return None;
        }
        // 闸门 2：对应 kind 开关。
        let kind_enabled = match kind {
            NotifyKind::Done => self.on_complete,
            NotifyKind::UserQuery => self.on_user_input,
        };
        if !kind_enabled {
            return None;
        }
        // 闸门 3：节流（按 kind 独立计时；min_interval == 0 表示不节流）。
        if self.min_interval > Duration::ZERO
            && let Some(last) = self.last_sent[kind as usize]
            && now.saturating_duration_since(last) < self.min_interval
        {
            return None;
        }
        // 文案（设计 §2.1）：标题固定 `visp`；正文取自调用方参数
        // （Done = event.rs 的 DONE_BODY，UserQuery = uq.message），
        // 统一按字符截断至 200（决策 D4，engine 层截断）。
        let title = "visp";
        let body: String = body.chars().take(200).collect();
        // 编码（决策 D3：空 body / 清洗后为空 → None，不发送）。
        let bytes = encode(protocol, title, &body)?;
        // 节流记录「实际发送时刻」：仅在生成序列时更新。
        self.last_sent[kind as usize] = Some(now);
        Some(bytes)
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
