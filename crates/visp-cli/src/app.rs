#![allow(dead_code)]
#![allow(clippy::bool_assert_comparison)]

use ratatui::{
    style::Style,
    text::{Line, Span},
    widgets::ListState,
};
use ratatui_textarea::WrapMode;
use unicode_width::UnicodeWidthChar;

use crate::theme;
use visp_proto::visp::{ServerMessage, server_message};

/// 检测内容是否为 diff 格式（git diff, diff -u 等输出）
fn detect_diff(content: &str) -> Option<()> {
    let mut lines = content.lines();
    // 检查是否以 diff --git 开头（git diff）
    if lines.clone().any(|l| l.starts_with("diff --git")) {
        return Some(());
    }
    // 或检查是否有 --- / +++ 配对（context/unified diff）
    let has_three_dashes = lines.clone().any(|l| l.starts_with("--- "));
    let has_plus_plus = lines.any(|l| l.starts_with("+++ "));
    if has_three_dashes && has_plus_plus {
        return Some(());
    }
    None
}

/// 滚动状态（替代 tui-scrollview）
#[derive(Clone, Copy, Debug, Default)]
pub struct ScrollState {
    pub x: u16,
    pub y: u16,
}

impl ScrollState {
    pub fn scroll_up(&mut self) {
        self.y = self.y.saturating_sub(1);
    }
    pub fn scroll_down(&mut self) {
        self.y = self.y.saturating_add(1);
    }
}

/// 用 syntect 高亮代码块，返回 ratatui 行
fn highlight_code_block(lang: &str, code: &str) -> Vec<Line<'static>> {
    use syntect::easy::HighlightLines;
    use syntect::highlighting::ThemeSet;
    use syntect::parsing::SyntaxSet;
    use syntect::util::LinesWithEndings;

    let ss = SyntaxSet::load_defaults_newlines();
    let ts = ThemeSet::load_defaults();
    let theme = &ts.themes["base16-ocean.dark"];

    let syntax = if lang.is_empty() {
        // 先检测 diff 格式（git diff / diff -u 等输出）
        detect_diff(code)
            .and_then(|_| ss.find_syntax_by_name("diff"))
            .or_else(|| {
                // 再尝试从 shebang（#!/usr/bin/env python 等）检测语言
                ss.find_syntax_by_first_line(code)
            })
            .unwrap_or_else(|| ss.find_syntax_plain_text())
    } else {
        ss.find_syntax_by_token(lang)
            .or_else(|| ss.find_syntax_by_name(lang))
            .unwrap_or_else(|| ss.find_syntax_plain_text())
    };

    let mut h = HighlightLines::new(syntax, theme);
    let mut result = Vec::new();

    for line in LinesWithEndings::from(code) {
        if let Ok(ranges) = h.highlight_line(line, &ss) {
            let spans: Vec<Span<'static>> = ranges
                .iter()
                .map(|(syn_style, text)| {
                    let fg = ratatui::style::Color::Rgb(
                        syn_style.foreground.r,
                        syn_style.foreground.g,
                        syn_style.foreground.b,
                    );
                    Span::styled(text.to_string(), Style::default().fg(fg))
                })
                .collect();
            // LinesWithEndings preserves trailing \n; remove it
            let line_text: String = spans.iter().map(|s| s.content.as_ref()).collect();
            if line_text.ends_with('\n') {
                // 去掉最后的换行，重新构建不带 \n 的 spans
                let trimmed: Vec<Span<'static>> = ranges
                    .iter()
                    .map(|(syn_style, text)| {
                        let t = text.trim_end_matches('\n');
                        let fg = ratatui::style::Color::Rgb(
                            syn_style.foreground.r,
                            syn_style.foreground.g,
                            syn_style.foreground.b,
                        );
                        Span::styled(t.to_string(), Style::default().fg(fg))
                    })
                    .collect();
                result.push(Line::from(trimmed));
            } else {
                result.push(Line::from(spans));
            }
        }
    }
    result
}

/// 在 markdown 中查找代码块并用唯一标记替换，返回 (处理后的文本, 高亮行列表)
fn process_code_blocks(md: &str) -> (String, Vec<Vec<Line<'static>>>) {
    use regex::Regex;
    let re = Regex::new(r"(?ms)```(\w*)\n(.*?)```").unwrap();
    let mut highlighted: Vec<Vec<Line<'static>>> = Vec::new();
    let mut result = String::new();
    let mut last_end = 0;

    for cap in re.captures_iter(md) {
        let full_match = cap.get(0).unwrap();
        let lang = cap.get(1).map_or("", |m| m.as_str());
        let code = cap.get(2).map_or("", |m| m.as_str());

        // 追加匹配前的文本
        result.push_str(&md[last_end..full_match.start()]);

        // 高亮代码
        let lines = highlight_code_block(lang, code);
        let idx = highlighted.len();
        highlighted.push(lines);

        // 插入标记（使用控制字符确保不被 markdown 解析器干扰）
        result.push_str(&format!("\x00CODEBLOCK_{}\x00", idx));
        last_end = full_match.end();
    }
    // 追加剩余文本
    result.push_str(&md[last_end..]);
    (result, highlighted)
}

/// 将一个带样式的 Line 按屏幕宽度拆分为多行，保留样式，每行末尾补空格到指定宽度
fn wrap_styled_line(line: &Line<'_>, width: usize) -> Vec<Line<'static>> {
    use std::collections::VecDeque;

    struct StyledChar {
        ch: char,
        style: Style,
        cw: usize,
    }

    // 展平 spans 为字符序列
    let mut chars: VecDeque<StyledChar> = VecDeque::new();
    for span in &line.spans {
        for ch in span.content.chars() {
            let cw = ch.width().unwrap_or(0);
            chars.push_back(StyledChar {
                ch,
                style: span.style,
                cw,
            });
        }
    }

    if chars.is_empty() {
        return vec![Line::from(" ")];
    }

    let mut result: Vec<Line<'static>> = Vec::new();
    let mut line_spans: Vec<(String, Style)> = Vec::new();
    let mut col: usize = 0;

    while let Some(sc) = chars.pop_front() {
        if sc.ch == '\n' {
            // 显式换行
            flush_line(&mut line_spans, width, &mut result, col);
            col = 0;
            continue;
        }
        if col + sc.cw > width && col > 0 {
            flush_line(&mut line_spans, width, &mut result, col);
            col = 0;
        }
        // 与最后一个 span 样式相同则合并，否则新建
        if let Some((text, last_style)) = line_spans.last_mut()
            && *last_style == sc.style
        {
            text.push(sc.ch);
        } else {
            line_spans.push((sc.ch.to_string(), sc.style));
        }
        col += sc.cw;
    }

    // 最后一行
    if !line_spans.is_empty() {
        let mut spans: Vec<Span<'static>> = line_spans
            .into_iter()
            .map(|(t, s)| Span::styled(t, s))
            .collect();
        // 补空格到宽度
        if col < width {
            spans.push(Span::styled(" ".repeat(width - col), Style::default()));
        }
        result.push(Line::from(spans));
    }

    result
}

fn flush_line(
    line_spans: &mut Vec<(String, Style)>,
    width: usize,
    result: &mut Vec<Line<'static>>,
    col: usize,
) {
    let mut spans: Vec<Span<'static>> = line_spans
        .drain(..)
        .map(|(t, s)| Span::styled(t, s))
        .collect();
    if col < width {
        spans.push(Span::styled(" ".repeat(width - col), Style::default()));
    }
    result.push(Line::from(spans));
}

#[derive(Debug, Clone, PartialEq)]
pub enum LineType {
    User,
    Assistant,
    Thinking,
    ToolCall { name: String },
    ToolResult { name: String },
    ToolError { name: String },
    Error,
    Status,
    Usage,
}

/// 工具名称 → emoji 图标映射
pub fn tool_icon(name: &str) -> &'static str {
    match name {
        "read_file" | "read_files" => "📖",
        "write_file" => "📝",
        "edit_file" => "✏️",
        "grep" => "🔍",
        "glob" => "📂",
        "bash" | "cmd" | "powershell" => "💻",
        "fetch_web" | "fetch" => "🌐",
        n if n.starts_with("codegraph_") => "🔎",
        _ => "🔧",
    }
}

/// 根据工具名获取结果最大行数
/// - None = 不截断
/// - Some(0) = 不显示内容
/// - Some(N) = 最多 N 行
pub fn max_lines_for_tool(name: &str) -> Option<usize> {
    match name {
        "read_file" | "read_files" => Some(0),
        "edit_file" | "write_file" => None,
        "bash" | "cmd" | "powershell" => Some(30),
        "grep" => Some(20),
        "glob" => Some(15),
        "fetch_web" | "fetch" => Some(20),
        n if n.starts_with("codegraph_") => Some(20),
        _ => Some(20),
    }
}

/// read_file 类工具的摘要行
pub fn result_summary(name: &str, content: &str) -> String {
    match name {
        "read_file" | "read_files" => {
            let lines = content.lines().count();
            let bytes = content.len();
            format!("Read {} bytes ({} lines)", bytes, lines)
        }
        _ => String::new(),
    }
}

#[derive(Debug, Clone)]
pub struct ChatLine {
    pub id: u64,
    pub version: u64,
    pub line_type: LineType,
    pub content: String,
    pub call_id: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentStatus {
    Running,
    Done,
    Error,
    ViewOnly,
}

#[derive(Debug, Clone)]
pub struct TabEntry {
    pub session_id: String,
    pub agent_name: String,
    pub status: AgentStatus,
    pub frames: Vec<ServerMessage>,
    pub messages: Vec<ChatLine>,
    pub rendered_up_to: usize,
    pub streaming_text: String,
    pub generating: bool,
    pub pending_usage: Option<(u32, u32, u32, u32, u32)>,
    pub next_message_id: u64,
    pub scroll: usize,
}

impl TabEntry {
    pub fn new(session_id: impl Into<String>, agent_name: impl Into<String>) -> Self {
        Self {
            session_id: session_id.into(),
            agent_name: agent_name.into(),
            status: AgentStatus::Running,
            frames: Vec::new(),
            messages: Vec::new(),
            rendered_up_to: 0,
            streaming_text: String::new(),
            generating: false,
            pending_usage: None,
            next_message_id: 0,
            scroll: 0,
        }
    }

    pub fn new_view_only(session_id: impl Into<String>, agent_name: impl Into<String>) -> Self {
        let mut entry = Self::new(session_id, agent_name);
        entry.status = AgentStatus::ViewOnly;
        entry
    }

    pub fn push_chat_line(
        &mut self,
        line_type: LineType,
        content: String,
        call_id: Option<String>,
    ) {
        let id = self.next_message_id;
        self.next_message_id += 1;
        self.messages.push(ChatLine {
            id,
            version: 0,
            line_type,
            content,
            call_id,
        });
    }

    pub fn flush_streaming(&mut self) {
        if !self.streaming_text.is_empty() {
            let text = std::mem::take(&mut self.streaming_text);
            self.push_chat_line(LineType::Assistant, text, None);
        }
    }

    /// 消费 pending_usage，将 token 统计追加到最后一条 Assistant 消息或 streaming_text。
    /// 用于回放时在 UserMessage 和 Done 帧处理中追加 token footer。
    pub fn consume_pending_usage(&mut self) {
        if let Some((it, ot, tc, ccit, crit)) = self.pending_usage.take() {
            let time = chrono::Local::now().format("%H:%M:%S");
            let suffix = if ccit > 0 || crit > 0 {
                format!(
                    "\n\n[{} | Tokens: {} in / {} out | Cache: {} create / {} read | Tools: {}]",
                    time, it, ot, ccit, crit, tc
                )
            } else {
                format!(
                    "\n\n[{} | Tokens: {} in / {} out | Tools: {}]",
                    time, it, ot, tc
                )
            };
            if !self.streaming_text.is_empty() {
                // 有流式文本：追加到 streaming_text，flush 时一并成为本条 Assistant
                self.streaming_text.push_str(&suffix);
            } else if let Some(last) = self
                .messages
                .iter_mut()
                .rev()
                .find(|m| matches!(m.line_type, LineType::Assistant))
            {
                // 已 flush（如 ToolCall 后）：追加到最后一条 Assistant 消息
                last.content.push_str(&suffix);
                last.version += 1;
            } else {
                // 没有任何 Assistant 消息：用 streaming_text 新建一条
                self.streaming_text.push_str(&suffix);
            }
        }
    }

    pub fn update_thinking(&mut self, content: String) {
        if let Some(last) = self.messages.last_mut()
            && matches!(last.line_type, LineType::Thinking)
        {
            last.content = content;
            last.version += 1;
        } else {
            self.push_chat_line(LineType::Thinking, content, None);
        }
    }

    /// 查找与指定 call_id 匹配的 ToolCall 消息，在其后追加结果内容。
    /// 若未找到匹配的 ToolCall，则创建一个新的 ToolCall 消息。
    pub fn insert_tool_result(&mut self, call_id: &str, content: String) {
        if let Some(msg) = self.messages.iter_mut().find(|m| {
            matches!(m.line_type, LineType::ToolCall { .. })
                && m.call_id.as_deref() == Some(call_id)
        }) {
            msg.content.push('\n');
            msg.content.push_str(&content);
            msg.version += 1;
        } else {
            self.push_chat_line(
                LineType::ToolCall {
                    name: String::new(),
                },
                content,
                Some(call_id.to_string()),
            );
        }
    }

    /// 按 id 查找消息并更新内容，版本号 +1。
    pub fn update_message(&mut self, id: u64, content: String) {
        if let Some(msg) = self.messages.iter_mut().find(|m| m.id == id) {
            msg.content = content;
            msg.version += 1;
        }
    }

    /// 处理 self.frames 中尚未渲染的消息（从 rendered_up_to 开始）。
    /// 将 frames 中的 ServerMessage 转化为 ChatLine 追加到 self.messages。
    pub fn render_pending(&mut self) {
        use visp_proto::visp::server_message;

        while self.rendered_up_to < self.frames.len() {
            let msg = self.frames[self.rendered_up_to].clone();
            match msg.payload {
                Some(server_message::Payload::TextDelta(delta)) => {
                    self.streaming_text.push_str(&delta.delta);
                }
                Some(server_message::Payload::ToolCall(tc)) => {
                    self.flush_streaming();
                    self.push_chat_line(
                        LineType::ToolCall { name: tc.tool_name },
                        tc.arguments,
                        Some(tc.call_id),
                    );
                }
                Some(server_message::Payload::ToolResult(tr)) => {
                    self.flush_streaming();
                    let tool_name = if !tr.tool_name.is_empty() {
                        tr.tool_name
                    } else {
                        self.messages
                            .iter()
                            .find(|m| {
                                matches!(&m.line_type, LineType::ToolCall { .. })
                                    && m.call_id.as_deref() == Some(&tr.call_id)
                            })
                            .and_then(|m| {
                                if let LineType::ToolCall { name } = &m.line_type {
                                    Some(name.clone())
                                } else {
                                    None
                                }
                            })
                            .unwrap_or_default()
                    };
                    self.push_chat_line(
                        if tr.is_error {
                            LineType::ToolError { name: tool_name }
                        } else {
                            LineType::ToolResult { name: tool_name }
                        },
                        tr.content,
                        Some(tr.call_id),
                    );
                }
                Some(server_message::Payload::ThinkingBlock(tb)) => {
                    let text = format!("[Thinking] {}", tb.thinking);
                    self.update_thinking(text);
                }
                Some(server_message::Payload::StatusUpdate(su)) => {
                    // NOTE: user_inputs handling deferred to later step
                    self.push_chat_line(LineType::Status, su.message, None);
                }
                Some(server_message::Payload::UserMessage(um)) => {
                    // 回放时重放用户消息：先消费 pending_usage 为上一条 assistant 追加 token 统计
                    self.consume_pending_usage();
                    self.flush_streaming();
                    self.push_chat_line(LineType::User, um.content, None);
                }
                Some(server_message::Payload::Error(err)) => {
                    self.flush_streaming();
                    self.push_chat_line(
                        LineType::Error,
                        format!("{}: {}", err.code, err.message),
                        None,
                    );
                    self.generating = false;
                    self.status = AgentStatus::Error;
                }
                Some(server_message::Payload::Done(_)) => {
                    // 将 token 统计 + 时间戳追加到对话区
                    self.consume_pending_usage();
                    self.flush_streaming();
                    self.generating = false;
                    if self.status == AgentStatus::Running {
                        self.status = AgentStatus::Done;
                    }
                }
                _ => {}
            }
            self.rendered_up_to += 1;
        }
    }
}

#[derive(Debug, Clone)]
pub struct TabBar {
    pub tabs: Vec<TabEntry>,
    pub active: usize,
    pub page_start: usize,
    pub last_term_width: u16,
    /// 上次渲染时 tabs widget content_area 的 x 坐标（屏幕绝对列）
    pub last_tab_area_x: u16,
    /// 上次渲染时 tabs widget content_area 的 y 坐标（tab 内容行，分隔线在 y+1）
    pub last_tab_area_y: u16,
}

impl TabBar {
    pub fn new(main_session_id: String) -> Self {
        Self {
            tabs: vec![TabEntry::new(main_session_id, "default")],
            active: 0,
            page_start: 0,
            last_term_width: 0,
            last_tab_area_x: 0,
            last_tab_area_y: 0,
        }
    }

    /// 在 index=1 处插入新的子 agent tab。
    /// 若 active >= 1，自动将 active 加 1 以保持指向同一 tab。
    /// 返回新 tab 的索引。
    pub fn insert_sub_agent(
        &mut self,
        session_id: impl Into<String>,
        agent_name: impl Into<String>,
        view_only: bool,
    ) -> usize {
        let entry = if view_only {
            TabEntry::new_view_only(session_id, agent_name)
        } else {
            TabEntry::new(session_id, agent_name)
        };
        self.tabs.insert(1, entry);
        if self.active >= 1 {
            self.active += 1;
        }
        1
    }

    pub fn find_index_by_session(&self, session_id: &str) -> Option<usize> {
        self.tabs.iter().position(|t| t.session_id == session_id)
    }

    pub fn find_or_insert(&mut self, session_id: &str, agent_name: &str) -> usize {
        // Default tab matches — return 0
        if session_id == self.tabs[0].session_id {
            return 0;
        }
        // Already exists
        if let Some(idx) = self.find_index_by_session(session_id) {
            return idx;
        }
        // Create new (default Running)
        self.insert_sub_agent(session_id, agent_name, false)
    }

    /// 计算每页的 sub 索引范围（不含 default）。
    /// 每页固定 4 个 sub tab。
    pub fn layout_pages(&self, _term_width: u16) -> Vec<std::ops::Range<usize>> {
        let sub_count = self.tabs.len().saturating_sub(1);
        if sub_count == 0 {
            #[allow(clippy::single_range_in_vec_init)]
            return vec![1..1]; // 空范围
        }
        const PER_PAGE: usize = 4;
        let total_pages = sub_count.div_ceil(PER_PAGE);
        (0..total_pages)
            .map(|p| {
                let start = 1 + p * PER_PAGE;
                let end = (start + PER_PAGE).min(self.tabs.len());
                start..end
            })
            .collect()
    }

    /// 当前页号（从 0 开始）
    pub fn current_page(&self) -> usize {
        self.page_start
    }

    /// 当前页的 sub 范围
    pub fn current_page_subs(&self, term_width: u16) -> std::ops::Range<usize> {
        let pages = self.layout_pages(term_width);
        if pages.is_empty() {
            return 0..0;
        }
        let p = self.page_start.min(pages.len() - 1);
        pages[p].clone()
    }

    /// active tab 在当前页可见 titles 中的索引。
    /// 仅当 active 在当前页可见时返回 Some。
    pub fn select_idx_for_current_page(&self, term_width: u16) -> Option<usize> {
        if self.active == 0 {
            return Some(0);
        }
        let range = self.current_page_subs(term_width);
        if range.contains(&self.active) {
            // visible titles = [default, sub_range[0], sub_range[1], ...]
            Some(1 + (self.active - range.start))
        } else {
            None
        }
    }

    /// 翻到下一页（边界停止）。
    pub fn next_page(&mut self, term_width: u16) -> bool {
        let pages = self.layout_pages(term_width);
        if self.page_start + 1 < pages.len() {
            self.page_start += 1;
            true
        } else {
            false
        }
    }

    /// 翻到上一页（边界停止）。
    pub fn prev_page(&mut self) -> bool {
        if self.page_start > 0 {
            self.page_start -= 1;
            true
        } else {
            false
        }
    }

    /// 确保 active tab 在当前页可见。
    /// 如果 active 不在可见范围，翻到包含它的那一页。
    pub fn ensure_active_visible(&mut self, term_width: u16) {
        if self.active == 0 {
            return; // default 永远可见
        }
        let pages = self.layout_pages(term_width);
        for (i, range) in pages.iter().enumerate() {
            if range.contains(&self.active) {
                self.page_start = i;
                return;
            }
        }
    }

    /// Activate the tab at the given index and render its pending frames.
    pub fn activate(&mut self, index: usize) {
        if index >= self.tabs.len() {
            return;
        }
        self.active = index;
        self.tabs[index].render_pending();
    }

    /// Activate the next tab (circular). No-op if empty.
    pub fn activate_next(&mut self) {
        if self.tabs.is_empty() {
            return;
        }
        let next = (self.active + 1) % self.tabs.len();
        self.activate(next);
    }

    /// Activate the previous tab (circular). No-op if empty.
    pub fn activate_prev(&mut self) {
        if self.tabs.is_empty() {
            return;
        }
        let prev = if self.active == 0 {
            self.tabs.len() - 1
        } else {
            self.active - 1
        };
        self.activate(prev);
    }

    /// Close the active sub-agent tab.
    /// Only allowed when active > 0 and the tab's status is Done or Error.
    /// Returns true if the tab was closed; false otherwise.
    pub fn close_active(&mut self) -> bool {
        if self.active == 0 {
            return false;
        }
        if self.tabs[self.active].status == AgentStatus::Running {
            return false;
        }
        self.tabs.remove(self.active);
        if self.active > 0 {
            self.active -= 1;
        }
        self.tabs[self.active].render_pending();
        self.ensure_active_visible(self.last_term_width);
        true
    }
}

#[derive(Debug, Clone)]
pub struct ConfirmState {
    pub query_id: String,
    pub message: String,
    pub options: Vec<String>,
    pub allow_other: bool,
    pub selected_index: usize,
    pub other_active: bool,
}

pub struct MessageCache {
    pub msg_id: u64,
    pub msg_version: u64,
    pub width: u16,
    pub lines: Vec<Line<'static>>,
    pub line_count: u16,
}

impl MessageCache {
    pub fn from_message(msg: &ChatLine, width: u16) -> Self {
        // 背景色由 ui.rs 的 BlockStyle::bg_fill 统一处理
        let base_style = Style::default().fg(theme::fg_for(msg.line_type.clone()));
        let lines: Vec<Line<'static>> = match msg.line_type {
            LineType::Assistant => {
                // 第一步：用 syntect 高亮代码块，替换为标记
                let (processed, highlighted_blocks) = process_code_blocks(&msg.content);
                // 第二步：ratatui-markdown 渲染（代码块位置是标记）
                use ratatui_markdown::markdown::MarkdownRenderer;
                let renderer = MarkdownRenderer::new(width as usize);
                let blocks = renderer.parse(&processed);
                let md_lines =
                    renderer.render(&blocks, &ratatui_markdown::theme::ThemeConfig::default());
                // 第三步：将标记替换为 syntect 高亮行
                md_lines
                    .into_iter()
                    .flat_map(|l| {
                        let text: String = l.spans.iter().map(|s| s.content.as_ref()).collect();
                        if let Some(rest) = text.strip_prefix('\x00')
                            && let Some(id_tag) = rest.strip_prefix("CODEBLOCK_")
                            && let Some(end) = id_tag.find('\x00')
                            && let Ok(idx) = id_tag[..end].parse::<usize>()
                            && idx < highlighted_blocks.len()
                        {
                            return highlighted_blocks[idx].clone();
                        }
                        // 非代码行：白色
                        vec![Line::styled(text, Style::default().fg(theme::ASSISTANT_FG))]
                    })
                    .collect()
            }
            LineType::ToolCall { ref name } => {
                let icon = tool_icon(name);
                let mut lines = Vec::new();
                let wrapped = wrap_text(&msg.content, width);
                let display_lines = if wrapped.len() > 5 {
                    let mut truncated: Vec<String> = wrapped.into_iter().take(5).collect();
                    truncated.push(format!("... [truncated, {}B]", msg.content.len()));
                    truncated
                } else {
                    wrapped
                };

                for (i, dl) in display_lines.iter().enumerate() {
                    let content = if dl.is_empty() {
                        " ".repeat(width as usize)
                    } else {
                        pad_to_width(dl, width as usize)
                    };
                    // 首行加图标，后续行（结果部分）灰色
                    let line_style = if i == 0 {
                        Style::default().fg(theme::TOOL_CALL_FG)
                    } else {
                        Style::default().fg(theme::TOOL_RESULT_FG)
                    };
                    let display = if i == 0 {
                        format!("{} {} {}", icon, name, content)
                    } else {
                        content
                    };
                    lines.push(Line::styled(display, line_style));
                }
                let line_count = lines.len() as u16;
                return Self {
                    msg_id: msg.id,
                    msg_version: msg.version,
                    width,
                    lines,
                    line_count,
                };
            }
            LineType::ToolResult { ref name } => {
                let max_lines = max_lines_for_tool(name);
                let icon = tool_icon(name);
                let mut lines = Vec::new();
                if max_lines == Some(0) {
                    // 不显示内容，只显示摘要
                    let summary = result_summary(name, &msg.content);
                    let status_line = format!("  ✓ {} {} {}", icon, name, summary);
                    lines.push(Line::styled(
                        pad_to_width(&status_line, width as usize),
                        Style::default().fg(theme::TOOL_RESULT_FG),
                    ));
                } else {
                    // bash/shell 输出：首行就是内容，不走"灰色状态头"模式
                    let is_shell = matches!(name.as_str(), "bash" | "cmd" | "powershell");

                    let content_body = if is_shell {
                        // shell：全部内容传语法高亮，不加灰色头
                        &msg.content
                    } else if let Some((first, rest)) = msg.content.split_once('\n') {
                        // 其他工具：第一行作为灰色状态头
                        let status = format!("  ✓ {} {} {}", icon, name, first);
                        lines.push(Line::styled(
                            pad_to_width(&status, width as usize),
                            Style::default().fg(theme::TOOL_RESULT_FG),
                        ));
                        rest
                    } else {
                        let summary = msg.content.clone();
                        let status = format!("  ✓ {} {}", icon, summary);
                        lines.push(Line::styled(
                            pad_to_width(&status, width as usize),
                            Style::default().fg(theme::TOOL_RESULT_FG),
                        ));
                        return Self {
                            msg_id: msg.id,
                            msg_version: msg.version,
                            width,
                            lines,
                            line_count: 1,
                        };
                    };

                    let mut highlighted = highlight_code_block("", content_body);
                    let total_len = highlighted.len();
                    if let Some(max) = max_lines
                        && total_len > max
                    {
                        highlighted.truncate(max);
                        let remaining = total_len.saturating_sub(max);
                        highlighted.push(Line::styled(
                            format!("... [truncated, {} more lines]", remaining),
                            Style::default().fg(theme::TOOL_RESULT_FG),
                        ));
                    }
                    // 填充每行到完整宽度
                    for line in &mut highlighted {
                        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
                        let w = text.len();
                        if (w as u16) < width {
                            line.spans
                                .push(Span::raw(" ".repeat((width - w as u16) as usize)));
                        }
                    }
                    lines.extend(highlighted);
                }
                let line_count = lines.len() as u16;
                return Self {
                    msg_id: msg.id,
                    msg_version: msg.version,
                    width,
                    lines,
                    line_count,
                };
            }
            LineType::ToolError { ref name } => {
                let icon = tool_icon(name);
                let mut lines = Vec::new();
                let error_line = format!("❌ {} {} {}", icon, name, msg.content);
                lines.push(Line::styled(
                    pad_to_width(&error_line, width as usize),
                    Style::default().fg(theme::ERROR_FG),
                ));
                let line_count = lines.len() as u16;
                return Self {
                    msg_id: msg.id,
                    msg_version: msg.version,
                    width,
                    lines,
                    line_count,
                };
            }
            _ => {
                let mut lines = Vec::new();
                let wrapped = wrap_text(&msg.content, width);
                for dl in wrapped.iter() {
                    let content = if dl.is_empty() {
                        " ".repeat(width as usize)
                    } else {
                        pad_to_width(dl, width as usize)
                    };
                    lines.push(Line::styled(content, base_style));
                }
                let line_count = lines.len() as u16;
                return Self {
                    msg_id: msg.id,
                    msg_version: msg.version,
                    width,
                    lines,
                    line_count,
                };
            }
        };

        let line_count = lines.len() as u16;
        Self {
            msg_id: msg.id,
            msg_version: msg.version,
            width,
            lines,
            line_count,
        }
    }

    pub fn matches(&self, msg: &ChatLine, width: u16) -> bool {
        self.msg_id == msg.id && self.msg_version == msg.version && self.width == width
    }
}

pub(crate) fn wrap_text(text: &str, screen_width: u16) -> Vec<String> {
    let mut result = Vec::new();
    let sw = screen_width as usize;
    if sw == 0 {
        return result;
    }
    for paragraph in text.split('\n') {
        if paragraph.is_empty() {
            result.push(String::new());
            continue;
        }
        let chars: Vec<char> = paragraph.chars().collect();
        let mut start = 0;
        while start < chars.len() {
            // 寻找可折行位置：优先按单词边界（空白字符）断行
            let mut w: usize = 0;
            let mut end = start;
            let mut break_at = None; // 最后一个空白符位置

            for (i, &c) in chars[start..].iter().enumerate() {
                let cw = c.width().unwrap_or(0);

                // 记录空白符位置作为可选断点
                if c.is_whitespace() && w > 0 {
                    break_at = Some((start + i, w));
                }

                if w + cw > sw {
                    if let Some((bp, _)) = break_at {
                        // 有空白符可断：在空白符处折行
                        end = bp;
                        break;
                    }
                    // 整个单词超过一行宽度，允许字符断行
                    if w > 0 {
                        end = start + i;
                        break;
                    }
                }

                w += cw;
                end = start + i + 1;

                // 不主动在 w == sw 时断行，留给下一轮 overflow 逻辑
                // 处理，确保在单词边界折行，而非截断单词
            }

            result.push(chars[start..end].iter().collect());
            start = end;

            // 如果断点在空白符，跳过后续空白避免空行
            while start < chars.len() && chars[start].is_whitespace() {
                start += 1;
            }
        }
    }
    result
}

pub(crate) fn pad_to_width(s: &str, width: usize) -> String {
    let len: usize = s.chars().map(|c| c.width().unwrap_or(0)).sum();
    if len < width {
        format!("{}{}", s, " ".repeat(width - len))
    } else {
        s.to_string()
    }
}

/// Session 选择器状态（ratatui List widget）
pub struct SessionSelectState {
    pub labels: Vec<String>,
    pub state: ListState,
    pub session_ids: Vec<String>,
}

/// 模型选择器状态
pub struct ModelSelectState {
    /// 选择器中显示的标签（如 "GPT-4o (OpenAI)"）
    pub display_labels: Vec<String>,
    /// 发送给 daemon 的模型名称（如 "GPT-4o"）
    pub model_keys: Vec<String>,
    pub state: ListState,
}

/// 从 ServerMessage 中提取 (session_id, agent_name)。
/// 没有 agent_name 字段的 payload 返回空字符串。
fn extract_session_and_agent(msg: &ServerMessage) -> (String, String) {
    match &msg.payload {
        Some(server_message::Payload::TextDelta(d)) => (d.session_id.clone(), d.agent_name.clone()),
        Some(server_message::Payload::ToolCall(d)) => (d.session_id.clone(), d.agent_name.clone()),
        Some(server_message::Payload::ToolResult(d)) => {
            (d.session_id.clone(), d.agent_name.clone())
        }
        Some(server_message::Payload::StatusUpdate(d)) => {
            (d.session_id.clone(), d.agent_name.clone())
        }
        Some(server_message::Payload::Error(d)) => (d.session_id.clone(), d.agent_name.clone()),
        Some(server_message::Payload::Done(d)) => (d.session_id.clone(), String::new()),
        Some(server_message::Payload::UserQuery(d)) => (d.session_id.clone(), String::new()),
        Some(server_message::Payload::ThinkingBlock(d)) => (d.session_id.clone(), String::new()),
        Some(server_message::Payload::UsageInfo(d)) => (d.session_id.clone(), String::new()),
        Some(server_message::Payload::UserMessage(d)) => (d.session_id.clone(), String::new()),
        None => (String::new(), String::new()),
    }
}

pub struct AppState {
    pub tab_bar: TabBar,
    pub main_session_id: String,
    /// 当前请求的 token 用量 (input, output, cache_create, cache_read)
    pub current_request_usage: (u32, u32, u32, u32),
    pub message_caches: Vec<MessageCache>,
    pub scroll_following: bool,
    pub scroll_state: ScrollState,
    pub cache_width: u16,

    // 输入
    pub textarea: ratatui_textarea::TextArea<'static>,
    pub input_history: Vec<String>,
    pub history_index: Option<usize>,

    // 状态
    /// generating 期间的 spinner 帧序号，由主循环 tick 递增
    pub spinner_frame: usize,
    pub stale_done_expected: bool,
    /// 当前正在处理的请求 ID（用于 Done 后发 Ack）
    pub current_request_id: Option<String>,
    pub needs_render: bool,
    pub last_scroll_time: Option<std::time::Instant>,
    pub last_stream_render: Option<std::time::Instant>,
    pub confirm: Option<ConfirmState>,
    pub model: String,
    pub model_key: String,
    pub session_id: String,
    pub should_quit: bool,
    /// 当前 session 累计 token 数（input + output），用于状态栏显示
    pub total_input_tokens: u32,
    pub total_output_tokens: u32,
    pub total_cache_creation_input_tokens: u32,
    pub total_cache_read_input_tokens: u32,
    /// 鼠标文本选择状态（内容坐标）
    pub text_selection: crate::selection::TextSelection,
    /// 上一次复制是否成功（用于状态提示）
    pub last_copy_msg: Option<String>,
    /// 标记：下一帧渲染时从 buffer 提取选中文字
    pub pending_copy: bool,
    /// 渲染时提取的待复制文本（draw 完成后由主循环执行 OSC 52）
    pub pending_copy_text: Option<String>,
    /// last_copy_msg 的设置时间，用于自动清除提示
    pub last_copy_time: Option<std::time::Instant>,
    /// 上一次渲染的 chat area 矩形（屏幕坐标，用于鼠标命中测试）
    pub chat_area_rect: (u16, u16, u16, u16),
    /// 用户输入了 /new 命令，主循环需要创建新 session
    pub pending_new_session: bool,
    /// 用户输入了 /list 命令，主循环需要列出 session
    pub pending_list_sessions: bool,
    /// 用户输入了 /sessions <id>，主循环需要切换到目标 session
    pub pending_switch_session: Option<String>,
    /// 是否显示帮助弹窗
    pub show_help: bool,
    /// session 选择器弹出面板（/list 或 /sessions 无参触发）
    pub session_select: Option<SessionSelectState>,
    /// 可用的模型名称列表（显示标签）
    pub available_models: Vec<String>,
    /// 可用的模型 lookup key 列表
    pub model_keys: Vec<String>,
    /// 用户输入了 /model（无参），主循环需要获取模型列表并显示选择器
    pub pending_model_select: bool,
    /// 模型选择器弹出面板（/model 无参触发）
    pub model_select: Option<ModelSelectState>,
    /// 项目路径，用于文件操作
    pub project_path: String,
    /// Tab 补全状态：记录原始前缀和匹配列表，用于循环切换
    pub tab_completion: Option<TabCompletionState>,
}

/// Tab 补全循环状态
#[derive(Debug, Clone)]
pub struct TabCompletionState {
    /// 用户输入的原始前缀（如 "/i"）
    pub prefix: String,
    /// 所有匹配的命令列表
    pub matches: Vec<String>,
    /// 当前选中的索引
    pub index: usize,
}

impl AppState {
    pub fn new(session_id: String, model: String, model_key: String, project_path: String) -> Self {
        let mut textarea = Self::new_textarea();
        textarea.set_placeholder_text("Type your message...");
        let main_session_id = session_id.clone();
        Self {
            tab_bar: TabBar::new(main_session_id.clone()),
            main_session_id,
            current_request_usage: (0, 0, 0, 0),
            message_caches: Vec::new(),
            scroll_following: true,
            scroll_state: ScrollState::default(),
            cache_width: 0,
            textarea,
            input_history: Vec::new(),
            history_index: None,
            spinner_frame: 0,
            stale_done_expected: false,
            current_request_id: None,
            needs_render: true,
            last_scroll_time: None,
            last_stream_render: None,
            confirm: None,
            model,
            model_key,
            session_id,
            should_quit: false,
            total_input_tokens: 0,
            total_output_tokens: 0,
            total_cache_creation_input_tokens: 0,
            total_cache_read_input_tokens: 0,
            text_selection: crate::selection::TextSelection::default(),
            last_copy_msg: None,
            pending_copy: false,
            pending_copy_text: None,
            last_copy_time: None,
            chat_area_rect: (0, 0, 0, 0),
            pending_new_session: false,
            pending_list_sessions: false,
            pending_switch_session: None,
            show_help: false,
            session_select: None,
            available_models: Vec::new(),
            model_keys: Vec::new(),
            pending_model_select: false,
            model_select: None,
            project_path,
            tab_completion: None,
        }
    }

    /// 创建配置了 word wrap 的新 TextArea
    pub fn new_textarea() -> ratatui_textarea::TextArea<'static> {
        let mut ta = ratatui_textarea::TextArea::default();
        ta.set_wrap_mode(WrapMode::WordOrGlyph);
        ta.set_placeholder_text("Type your message...");
        ta
    }

    // ── Tab 访问方法 ────────────────────────────────────────
    pub fn active_tab(&self) -> &TabEntry {
        &self.tab_bar.tabs[self.tab_bar.active]
    }

    pub fn active_tab_mut(&mut self) -> &mut TabEntry {
        let idx = self.tab_bar.active;
        &mut self.tab_bar.tabs[idx]
    }

    pub fn active_messages(&self) -> &[ChatLine] {
        &self.active_tab().messages
    }

    // ── 消息操作 ────────────────────────────────────────────
    //
    // 设计约定（Step 5）：
    // - 类型 A（本地 UI 反馈/用户输入）：永远写 default tab（tabs[0]）
    // - 类型 B（agent 事件）：按 session_id 路由到对应 tab

    /// 类型 A：永远写 default tab（tabs[0]）
    pub fn add_message(&mut self, line_type: LineType, content: String) {
        self.tab_bar.tabs[0].push_chat_line(line_type, content, None);
    }

    /// 类型 B：按 session_id 路由
    pub fn add_message_to_session(
        &mut self,
        session_id: &str,
        line_type: LineType,
        content: String,
    ) {
        self.tab_mut_by_session(session_id)
            .push_chat_line(line_type, content, None);
    }

    /// 类型 B：按 session_id 路由的 tool_line
    pub fn add_tool_line_to_session(
        &mut self,
        session_id: &str,
        line_type: LineType,
        content: String,
        call_id: &str,
    ) {
        self.tab_mut_by_session(session_id).push_chat_line(
            line_type,
            content,
            Some(call_id.to_string()),
        );
    }

    /// 类型 B：按 session_id 路由的 update_thinking
    pub fn update_thinking_to_session(&mut self, session_id: &str, content: String) {
        self.tab_mut_by_session(session_id).update_thinking(content);
    }

    /// 类型 B：按 session_id 路由的 append_streaming
    pub fn append_streaming_to_session(&mut self, session_id: &str, delta: &str) {
        self.tab_mut_by_session(session_id)
            .streaming_text
            .push_str(delta);
    }

    /// 类型 B：按 session_id 路由的 flush_streaming
    pub fn flush_streaming_to_session(&mut self, session_id: &str) {
        self.tab_mut_by_session(session_id).flush_streaming();
    }

    /// 内部：按 session_id 找 tab，未知 ID 回退到 default（tabs[0]）
    fn tab_mut_by_session(&mut self, session_id: &str) -> &mut TabEntry {
        let idx = self.tab_bar.find_index_by_session(session_id).unwrap_or(0);
        &mut self.tab_bar.tabs[idx]
    }

    // ── 兼容 shim（旧签名，路由到 main_session_id）─────────────

    /// 兼容 shim：路由到 main_session_id 对应 tab
    pub fn add_tool_line(&mut self, line_type: LineType, content: String, call_id: &str) {
        let main_sid = self.main_session_id.clone();
        self.add_tool_line_to_session(&main_sid, line_type, content, call_id);
    }

    /// 兼容 shim：路由到 main_session_id 对应 tab
    pub fn append_streaming(&mut self, delta: &str) {
        let main_sid = self.main_session_id.clone();
        self.append_streaming_to_session(&main_sid, delta);
    }

    /// 兼容 shim：路由到 main_session_id 对应 tab
    pub fn flush_streaming(&mut self) {
        let main_sid = self.main_session_id.clone();
        self.flush_streaming_to_session(&main_sid);
    }

    /// 兼容 shim：路由到 main_session_id 对应 tab
    pub fn update_thinking(&mut self, content: String) {
        let main_sid = self.main_session_id.clone();
        self.update_thinking_to_session(&main_sid, content);
    }

    // ── 保留（非 session 路由，仍操作 active tab）──────────────

    pub fn insert_tool_result(&mut self, call_id: &str, content: String) {
        self.active_tab_mut().insert_tool_result(call_id, content);
    }

    pub fn update_message(&mut self, id: u64, content: String) {
        self.active_tab_mut().update_message(id, content);
    }

    // ── 其他原有方法 ─────────────────────────────────────────

    pub fn try_begin_scroll(&mut self) -> bool {
        const COOLDOWN_MS: u128 = 30;
        let now = std::time::Instant::now();
        if let Some(last) = self.last_scroll_time
            && now.duration_since(last).as_millis() < COOLDOWN_MS
        {
            return false;
        }
        self.last_scroll_time = Some(now);
        true
    }

    /// Points 三点跑马灯当前帧（generating 期间用作 placeholder 动画）
    pub fn spinner_glyph(&self) -> &'static str {
        const FRAMES: [&str; 4] = [
            "\u{2219}\u{2219}\u{2219}",
            "\u{25cf}\u{2219}\u{2219}",
            "\u{2219}\u{25cf}\u{2219}",
            "\u{2219}\u{2219}\u{25cf}",
        ];
        FRAMES[self.spinner_frame % FRAMES.len()]
    }

    pub fn try_begin_stream_render(&mut self) -> bool {
        const COOLDOWN_MS: u128 = 30;
        let now = std::time::Instant::now();
        if let Some(last) = self.last_stream_render
            && now.duration_since(last).as_millis() < COOLDOWN_MS
        {
            return false;
        }
        self.last_stream_render = Some(now);
        true
    }

    // ════════════════════════════════════════════════════════════
    // Shim API — 代理到 active_tab() / active_tab_mut()
    // ════════════════════════════════════════════════════════════

    // --- 读取（getter）---
    pub fn messages(&self) -> &[ChatLine] {
        &self.active_tab().messages
    }
    pub fn streaming_text(&self) -> &str {
        &self.active_tab().streaming_text
    }
    pub fn streaming_is_empty(&self) -> bool {
        self.active_tab().streaming_text.is_empty()
    }
    pub fn streaming_lines_count(&self) -> usize {
        self.active_tab().streaming_text.lines().count()
    }
    pub fn generating(&self) -> bool {
        self.active_tab().generating
    }
    pub fn has_pending_usage(&self) -> bool {
        self.active_tab().pending_usage.is_some()
    }

    // --- 写入（操作型）---
    pub fn set_generating(&mut self, v: bool) {
        self.active_tab_mut().generating = v;
    }
    pub fn clear_streaming(&mut self) {
        self.active_tab_mut().streaming_text.clear();
    }
    pub fn append_streaming_text(&mut self, s: &str) {
        self.active_tab_mut().streaming_text.push_str(s);
    }
    pub fn truncate_streaming(&mut self, n: usize) {
        self.active_tab_mut().streaming_text.truncate(n);
    }
    pub fn streaming_rfind(&self, needle: &str) -> Option<usize> {
        self.active_tab().streaming_text.rfind(needle)
    }

    pub fn set_pending_usage(&mut self, v: Option<(u32, u32, u32, u32, u32)>) {
        self.active_tab_mut().pending_usage = v;
    }
    pub fn take_pending_usage(&mut self) -> Option<(u32, u32, u32, u32, u32)> {
        self.active_tab_mut().pending_usage.take()
    }
    pub fn clear_pending_usage(&mut self) {
        self.active_tab_mut().pending_usage = None;
    }

    // --- 用于 ui.rs 的迭代访问（消息存在性查询）---
    pub fn has_message_with_id(&self, id: u64) -> bool {
        self.active_tab().messages.iter().any(|m| m.id == id)
    }

    pub fn clear_messages(&mut self) {
        self.active_tab_mut().messages.clear();
        self.message_caches.clear();
    }

    /// 根据 session_id 将 ServerMessage 路由到正确的 tab，
    /// 如果是 active tab 则立即调用 render_pending()。
    pub fn route_frame(&mut self, frame: ServerMessage) {
        let (session_id, agent_name) = extract_session_and_agent(&frame);

        let idx = if session_id.is_empty() || session_id == self.main_session_id {
            0
        } else if let Some(i) = self.tab_bar.find_index_by_session(&session_id) {
            // 升级 fallback "agent" 名字为真实 agent_name
            // 首帧可能是 UsageInfo/ThinkingBlock 等无 agent_name 字段的 payload，
            // 导致 tab 创建时名字退化为 "agent"；后续带 agent_name 的帧来时升级
            if !agent_name.is_empty() && self.tab_bar.tabs[i].agent_name == "agent" {
                self.tab_bar.tabs[i].agent_name = agent_name.clone();
            }
            i
        } else {
            let title = if agent_name.is_empty() {
                "agent".to_string()
            } else {
                agent_name.clone()
            };
            let view_only = matches!(&frame.payload,
                Some(server_message::Payload::StatusUpdate(su)) if su.view_only);
            let idx = self
                .tab_bar
                .insert_sub_agent(session_id.clone(), title, view_only);
            // For ViewOnly tabs, add task prompt from input_history[0]
            if view_only && !self.input_history.is_empty() {
                let prompt = format!("[task prompt] {}", self.input_history[0]);
                self.tab_bar.tabs[idx].push_chat_line(LineType::Status, prompt, None);
            }
            idx
        };

        let active = self.tab_bar.active;

        // 立即根据 payload 更新 tab.status（不依赖 render_pending），
        // 否则非 active 的子 tab Done/Error 后图标不会刷新，需要等切过去
        match &frame.payload {
            Some(server_message::Payload::Done(_)) => {
                if self.tab_bar.tabs[idx].status == AgentStatus::Running {
                    self.tab_bar.tabs[idx].status = AgentStatus::Done;
                }
            }
            Some(server_message::Payload::Error(err)) => {
                self.tab_bar.tabs[idx].status = AgentStatus::Error;
                if err.code == "SessionNotActive" {
                    self.tab_bar.tabs[idx].push_chat_line(
                        LineType::Status,
                        "该会话已结束，无法继续输入".into(),
                        None,
                    );
                }
            }
            _ => {}
        }

        self.tab_bar.tabs[idx].frames.push(frame);

        if idx == active {
            self.tab_bar.tabs[idx].render_pending();
        }
    }

    /// Token 三层路由 — L1: tab.pending_usage, L2: current_request_usage
    ///
    /// 按 session_id 路由 UsageInfo 到对应 tab 的 pending_usage（L1），
    /// 同时累加到 current_request_usage（L2）。
    /// 不直接修改 total_*_tokens（L3）——L3 由 Done 时 apply_done_token_settlement 处理。
    pub fn apply_usage_info(
        &mut self,
        session_id: &str,
        input: u32,
        output: u32,
        tool_calls: u32,
        cache_create: u32,
        cache_read: u32,
    ) {
        // 按 session_id 路由：空 ID 或主 session → default tab
        let idx = if session_id.is_empty() || session_id == self.main_session_id {
            0
        } else if let Some(i) = self.tab_bar.find_index_by_session(session_id) {
            i
        } else {
            self.tab_bar
                .insert_sub_agent(session_id.to_string(), "agent", false)
        };
        // L1: 写入 tab.pending_usage
        self.tab_bar.tabs[idx].pending_usage =
            Some((input, output, tool_calls, cache_create, cache_read));
        // L2: 累加到 current_request_usage（仅 input/output/cache_create/cache_read）
        self.current_request_usage.0 += input;
        self.current_request_usage.1 += output;
        self.current_request_usage.2 += cache_create;
        self.current_request_usage.3 += cache_read;
        // L3: 立即累加到 total tokens，状态栏即时显示
        self.total_input_tokens += input;
        self.total_output_tokens += output;
        self.total_cache_creation_input_tokens += cache_create;
        self.total_cache_read_input_tokens += cache_read;
    }

    /// Token 三层路由 — Done 结算
    ///
    /// - default tab（idx == 0）：清零 L2（L3 已在 apply_usage_info 中即时累加）
    /// - sub tab（idx != 0）：pending_usage 由 render_pending 消费，此处不做处理
    /// - 状态守卫：仅 Running 状态的 tab 执行结算；Error/Done 跳過
    pub fn apply_done_token_settlement(&mut self, session_id: &str) {
        let idx = if session_id.is_empty() || session_id == self.main_session_id {
            0
        } else if let Some(i) = self.tab_bar.find_index_by_session(session_id) {
            i
        } else {
            return; // Unknown session, skip
        };

        // 状态守卫：仅 Running 状态的 tab 执行 token 结算
        if self.tab_bar.tabs[idx].status != AgentStatus::Running {
            return;
        }

        if idx == 0 {
            // 清零 L2（L3 已在 apply_usage_info 中即时累加）
            self.current_request_usage = (0, 0, 0, 0);
        }
        // ── sub tab Done ──
        // pending_usage 由 TabEntry::render_pending 在 Done 帧处理时消费，
        // 此处不做任何处理
    }

    /// 重置为新 session 的状态（保留 textarea 内容）
    pub fn reset_for_new_session(&mut self, session_id: String, model: String, model_key: String) {
        // 重置 active tab 的内容状态（msg/streaming/generating/usage）
        let tab = self.active_tab_mut();
        tab.messages.clear();
        tab.frames.clear();
        tab.streaming_text.clear();
        tab.generating = false;
        tab.pending_usage = None;
        tab.next_message_id = 0;
        tab.scroll = 0;
        tab.rendered_up_to = 0;
        tab.status = AgentStatus::Running;
        // 更新 tab 的 session_id
        tab.session_id = session_id.clone();

        self.message_caches.clear();
        self.stale_done_expected = false;
        self.current_request_id = None;
        self.confirm = None;
        self.total_input_tokens = 0;
        self.total_output_tokens = 0;
        self.total_cache_creation_input_tokens = 0;
        self.total_cache_read_input_tokens = 0;
        self.current_request_usage = (0, 0, 0, 0);
        self.pending_new_session = false;
        self.pending_list_sessions = false;
        self.pending_switch_session = None;
        self.pending_model_select = false;
        self.session_select = None;
        self.model_select = None;
        self.session_id = session_id;
        self.model = model;
        self.model_key = model_key;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_spinner_glyph_cycles_points_frames() {
        let mut app = AppState::new("sid".into(), "m".into(), "".into(), String::new());
        // Points 四帧循环：∙∙∙ / ●∙∙ / ∙●∙ / ∙∙●
        let expected = [
            "\u{2219}\u{2219}\u{2219}",
            "\u{25cf}\u{2219}\u{2219}",
            "\u{2219}\u{25cf}\u{2219}",
            "\u{2219}\u{2219}\u{25cf}",
        ];
        for (i, want) in expected.iter().enumerate() {
            app.spinner_frame = i;
            assert_eq!(app.spinner_glyph(), *want);
        }
        // 回绕：第 5 帧回到第 1 帧
        app.spinner_frame = 4;
        assert_eq!(app.spinner_glyph(), expected[0]);
    }

    // ════════════════════════════════════════════════════════════
    // wrap_text 测试
    // ════════════════════════════════════════════════════════════

    #[test]
    fn test_wrap_text_exact_fit() {
        // 刚好填满一行，无截断问题
        let result = wrap_text("12345", 5);
        assert_eq!(result, vec!["12345"]);
    }

    #[test]
    fn test_wrap_text_word_boundary() {
        // 单词边界折行：hello word, width=7
        // 修复前：["hello w", "ord"]（单词被截断）
        // 修复后：["hello", "word"]
        let result = wrap_text("hello word", 7);
        assert_eq!(result, vec!["hello", "word"]);
    }

    #[test]
    fn test_wrap_text_word_boundary_exact() {
        // 前一个单词刚好填满，后续还有单词
        let result = wrap_text("hello word", 5);
        assert_eq!(result, vec!["hello", "word"]);
    }

    #[test]
    fn test_wrap_text_long_word_breaks_char() {
        // 单词超过一行宽度，允许字符级断行
        let result = wrap_text("Helloworld", 5);
        assert_eq!(result, vec!["Hello", "world"]);
    }

    #[test]
    fn test_wrap_text_multi_word_boundary() {
        // 多个单词，每次都在单词边界折行
        let result = wrap_text("This is a test hello", 8);
        assert_eq!(result, vec!["This is", "a test", "hello"]);
    }

    #[test]
    fn test_wrap_text_chinese_english_mixed() {
        // 中文 + 英文混合
        let result = wrap_text("这是一个test", 8);
        assert_eq!(result, vec!["这是一个", "test"]);
    }

    #[test]
    fn test_wrap_text_newline_paragraphs() {
        // 显式换行符
        let result = wrap_text("hello\nworld", 10);
        assert_eq!(result, vec!["hello", "world"]);
    }

    #[test]
    fn test_wrap_text_empty() {
        // 空字符串返回一个空行（split('\n') 行为）
        let result = wrap_text("", 10);
        assert_eq!(result, vec![""]);
    }

    #[test]
    fn test_wrap_text_empty_line() {
        let result = wrap_text("\n", 10);
        assert_eq!(result, vec!["", ""]);
    }

    #[test]
    fn test_wrap_text_zero_width() {
        let result = wrap_text("hello", 0);
        assert!(result.is_empty());
    }

    #[test]
    fn test_wrap_text_word_not_truncated_at_exact_fill() {
        // 核心场景：单词紧贴右边界时不截断
        // "abcde fghij" width=5
        // "abcde" 占满5，后面有空格，应在空格处折行
        let result = wrap_text("abcde fghij", 5);
        assert_eq!(result, vec!["abcde", "fghij"]);
    }

    #[test]
    fn test_stale_done_expected_default() {
        let app = AppState::new("s".into(), "m".into(), "".into(), String::new());
        assert!(!app.stale_done_expected);
    }

    #[test]
    fn test_app_state_new() {
        let app = AppState::new(
            "test-session".into(),
            "deepseek-v4-flash".into(),
            "".into(),
            String::new(),
        );
        assert_eq!(app.session_id, "test-session");
        assert_eq!(app.model, "deepseek-v4-flash");
        assert!(app.messages().is_empty());
        assert!(app.streaming_is_empty());
        assert!(!app.generating());
        assert!(app.confirm.is_none());
        assert!(!app.should_quit);
        assert!(app.scroll_following);
        assert_eq!(app.scroll_state.x, 0);
        assert_eq!(app.scroll_state.y, 0);
    }

    #[test]
    fn test_add_message() {
        let mut app = AppState::new("s".into(), "m".into(), "".into(), String::new());
        app.add_message(LineType::User, "hello".into());
        assert_eq!(app.messages().len(), 1);
        assert_eq!(app.messages()[0].content, "hello");
        assert_eq!(app.messages()[0].line_type, LineType::User);
    }

    #[test]
    fn test_add_message_id_increments() {
        let mut app = AppState::new("s".into(), "m".into(), "".into(), String::new());
        app.add_message(LineType::User, "a".into());
        app.add_message(LineType::Assistant, "b".into());
        assert_eq!(app.messages()[0].id, 0);
        assert_eq!(app.messages()[1].id, 1);
    }

    #[test]
    fn test_add_message_version_initial() {
        let mut app = AppState::new("s".into(), "m".into(), "".into(), String::new());
        app.add_message(LineType::User, "hello".into());
        assert_eq!(app.messages()[0].version, 0);
    }

    #[test]
    fn test_streaming_text() {
        let mut app = AppState::new("s".into(), "m".into(), "".into(), String::new());
        app.append_streaming("Hello ");
        app.append_streaming("world");
        assert_eq!(app.streaming_text(), "Hello world");
        app.flush_streaming();
        assert!(app.streaming_is_empty());
        assert_eq!(app.messages().len(), 1);
        assert_eq!(app.messages()[0].line_type, LineType::Assistant);
        assert_eq!(app.messages()[0].content, "Hello world");
    }

    #[test]
    fn test_update_message_increments_version() {
        let mut app = AppState::new("s".into(), "m".into(), "".into(), String::new());
        app.add_message(LineType::Assistant, "original".into());
        let id = app.messages()[0].id;
        app.update_message(id, "updated".into());
        assert_eq!(app.messages()[0].version, 1);
        assert_eq!(app.messages()[0].content, "updated");
    }

    #[test]
    fn test_update_message_id_not_found_does_nothing() {
        let mut app = AppState::new("s".into(), "m".into(), "".into(), String::new());
        app.add_message(LineType::Assistant, "original".into());
        let original_version = app.messages()[0].version;
        app.update_message(999, "nope".into());
        assert_eq!(app.messages()[0].version, original_version);
        assert_eq!(app.messages()[0].content, "original");
    }

    #[test]
    fn test_clear_messages() {
        let mut app = AppState::new("s".into(), "m".into(), "".into(), String::new());
        app.add_message(LineType::User, "hello".into());
        app.add_message(LineType::Assistant, "world".into());
        assert_eq!(app.messages().len(), 2);
        app.clear_messages();
        assert!(app.messages().is_empty());
    }

    #[test]
    fn test_message_cache_creation() {
        let msg = ChatLine {
            id: 0,
            version: 0,
            line_type: LineType::User,
            content: "hello world".into(),
            call_id: None,
        };
        let cache = MessageCache::from_message(&msg, 80);
        assert_eq!(cache.msg_id, 0);
        assert_eq!(cache.msg_version, 0);
        assert_eq!(cache.width, 80);
        assert!(cache.line_count > 0);
        assert!(!cache.lines.is_empty());
    }

    #[test]
    fn test_message_cache_matches() {
        let msg = ChatLine {
            id: 0,
            version: 0,
            line_type: LineType::User,
            content: "hello".into(),
            call_id: None,
        };
        let cache = MessageCache::from_message(&msg, 80);
        assert!(cache.matches(&msg, 80));
        // 不同 version 不匹配
        let mut msg2 = msg.clone();
        msg2.version = 1;
        assert!(!cache.matches(&msg2, 80));
        // 不同 width 不匹配
        assert!(!cache.matches(&msg, 40));
        // 不同 id 不匹配
        let msg3 = ChatLine {
            id: 1,
            version: 0,
            line_type: LineType::User,
            content: "hello".into(),
            call_id: None,
        };
        assert!(!cache.matches(&msg3, 80));
    }

    #[test]
    fn test_cache_user_message_has_top_bottom_padding() {
        let msg = ChatLine {
            id: 0,
            version: 0,
            line_type: LineType::User,
            content: "hello".into(),
            call_id: None,
        };
        let cache = MessageCache::from_message(&msg, 80);
        // User 消息背景由 bg_fill 处理，行级不带 padding
        assert!(cache.line_count >= 1);
    }

    #[test]
    fn test_cache_tool_call_truncation() {
        let msg = ChatLine {
            id: 0,
            version: 0,
            line_type: LineType::ToolCall {
                name: "bash".into(),
            },
            content: "line1\nline2\nline3\nline4\nline5\nline6\nline7".into(),
            call_id: None,
        };
        let cache = MessageCache::from_message(&msg, 80);
        // 首行是图标+第一行内容，后续 4 行 + 1 行 [...] = 共 6 行
        assert_eq!(cache.line_count, 6);
    }

    #[test]
    fn test_clear_messages_also_clears_caches() {
        let mut app = AppState::new("s".into(), "m".into(), "".into(), String::new());
        app.add_message(LineType::User, "hello".into());
        // 手动添加一个 cache 模拟渲染后的状态
        app.message_caches
            .push(MessageCache::from_message(&app.messages()[0], 80));
        assert_eq!(app.message_caches.len(), 1);
        app.clear_messages();
        assert!(app.messages().is_empty());
        assert!(app.message_caches.is_empty());
    }

    #[test]
    fn test_add_tool_line_stores_call_id() {
        let mut app = AppState::new("s".into(), "m".into(), "".into(), String::new());
        app.add_tool_line(
            LineType::ToolCall {
                name: "test".into(),
            },
            "cmd".into(),
            "tc_1",
        );
        assert_eq!(app.messages()[0].call_id.as_deref(), Some("tc_1"));
    }

    #[test]
    fn test_insert_tool_result_appends_to_matching_call() {
        let mut app = AppState::new("s".into(), "m".into(), "".into(), String::new());
        app.add_tool_line(
            LineType::ToolCall {
                name: "test".into(),
            },
            "cmd1".into(),
            "id1",
        );
        app.add_tool_line(
            LineType::ToolCall {
                name: "test".into(),
            },
            "cmd2".into(),
            "id2",
        );
        app.insert_tool_result("id1", "result1".into());
        // result 追加到匹配的 cmd1 后面
        assert_eq!(app.messages()[0].content, "cmd1\nresult1");
        assert_eq!(app.messages()[1].content, "cmd2");
        assert!(matches!(
            app.messages()[1].line_type,
            LineType::ToolCall { .. }
        ));
    }

    #[test]
    fn test_insert_tool_result_without_matching_call_appends() {
        let mut app = AppState::new("s".into(), "m".into(), "".into(), String::new());
        app.add_tool_line(
            LineType::ToolCall {
                name: "test".into(),
            },
            "cmd".into(),
            "id1",
        );
        app.insert_tool_result("nonexistent", "result".into());
        // 没有匹配的 call_id，作为新的 ToolCall 追加到末尾
        assert_eq!(app.messages().len(), 2);
        assert_eq!(app.messages()[1].content, "result");
    }

    #[test]
    fn test_multiple_tool_calls_grouped() {
        let mut app = AppState::new("s".into(), "m".into(), "".into(), String::new());
        app.add_tool_line(
            LineType::ToolCall {
                name: "test".into(),
            },
            "cmd1".into(),
            "a",
        );
        app.add_tool_line(
            LineType::ToolCall {
                name: "test".into(),
            },
            "cmd2".into(),
            "b",
        );
        app.insert_tool_result("b", "result2".into());
        app.insert_tool_result("a", "result1".into());
        // result 追加到各自的 call 后面
        assert_eq!(app.messages()[0].content, "cmd1\nresult1");
        assert_eq!(app.messages()[1].content, "cmd2\nresult2");
    }

    #[test]
    fn test_confirm_state_new() {
        let cs = ConfirmState {
            query_id: "q1".into(),
            message: "test?".into(),
            options: vec!["Yes".into(), "No".into()],
            allow_other: false,
            selected_index: 0,
            other_active: false,
        };
        assert_eq!(cs.selected_index, 0);
        assert!(!cs.other_active);
    }

    #[test]
    fn test_confirm_state_tool_approval() {
        let cs = ConfirmState {
            query_id: "q1".into(),
            message: "Allow tool?".into(),
            options: vec![],
            allow_other: false,
            selected_index: 0,
            other_active: false,
        };
        assert!(cs.options.is_empty());
    }

    #[test]
    fn test_confirm_state_other_mode() {
        let mut cs = ConfirmState {
            query_id: "q1".into(),
            message: "Choose?".into(),
            options: vec!["Yes".into(), "No".into()],
            allow_other: true,
            selected_index: 0,
            other_active: false,
        };
        assert!(!cs.other_active);
        cs.other_active = true;
        assert!(cs.other_active);
        cs.other_active = false;
        // selected_index 指向 "Other"（index == options.len()）
        cs.selected_index = cs.options.len();
        assert_eq!(cs.selected_index, 2);
    }

    // ════════════════════════════════════════════════════════════
    // AgentStatus / TabEntry / TabBar 测试
    // ════════════════════════════════════════════════════════════

    #[test]
    fn test_agent_status_default_is_running() {
        let tab = TabEntry::new("sid", "agent");
        assert_eq!(tab.status, AgentStatus::Running);
    }

    #[test]
    fn test_tab_entry_new_with_session_and_name() {
        let tab = TabEntry::new("sid-1", "agent-A");
        assert_eq!(tab.session_id, "sid-1");
        assert_eq!(tab.agent_name, "agent-A");
    }

    #[test]
    fn test_tab_entry_initial_empty() {
        let tab = TabEntry::new("sid", "agent");
        assert!(tab.frames.is_empty());
        assert!(tab.messages.is_empty());
        assert!(tab.streaming_text.is_empty());
        assert_eq!(tab.rendered_up_to, 0);
    }

    #[test]
    fn test_tab_entry_default_per_tab_state() {
        let tab = TabEntry::new("sid", "agent");
        assert!(!tab.generating);
        assert!(tab.pending_usage.is_none());
        assert_eq!(tab.scroll, 0);
    }

    // ── AgentStatus::ViewOnly ──────────────────────────────────

    #[test]
    fn agent_status_view_only_variant_exists() {
        let status = AgentStatus::ViewOnly;
        match status {
            AgentStatus::ViewOnly => {} // 命中正确分支
            _ => panic!("Expected ViewOnly variant"),
        }
    }

    #[test]
    fn agent_status_all_variants_display() {
        // 所有变体都能 match 不 panic
        let variants = [
            AgentStatus::Running,
            AgentStatus::Done,
            AgentStatus::Error,
            AgentStatus::ViewOnly,
        ];
        for v in &variants {
            match v {
                AgentStatus::Running => {}
                AgentStatus::Done => {}
                AgentStatus::Error => {}
                AgentStatus::ViewOnly => {}
            }
        }
    }

    // ── TabEntry::new_view_only ────────────────────────────────

    #[test]
    fn tab_entry_new_view_only_has_view_only_status() {
        let tab = TabEntry::new_view_only("sid", "name");
        assert_eq!(tab.status, AgentStatus::ViewOnly);
    }

    #[test]
    fn tab_entry_new_keeps_running_status() {
        let tab = TabEntry::new("sid", "name");
        assert_eq!(tab.status, AgentStatus::Running);
    }

    #[test]
    fn tab_entry_new_view_only_other_fields_default() {
        let new_tab = TabEntry::new("sid", "name");
        let vo_tab = TabEntry::new_view_only("sid", "name");

        // 与 new() 一致的默认值
        assert!(vo_tab.frames.is_empty());
        assert!(vo_tab.messages.is_empty());
        assert_eq!(vo_tab.scroll, 0);
        assert_eq!(vo_tab.rendered_up_to, new_tab.rendered_up_to);
        assert_eq!(vo_tab.streaming_text, new_tab.streaming_text);
        assert!(!vo_tab.generating);
        assert_eq!(vo_tab.pending_usage, new_tab.pending_usage);
        assert_eq!(vo_tab.next_message_id, new_tab.next_message_id);
        assert_eq!(vo_tab.session_id, new_tab.session_id);
        assert_eq!(vo_tab.agent_name, new_tab.agent_name);
    }

    #[test]
    fn test_tabbar_new_creates_default_tab() {
        let bar = TabBar::new("main-sid".into());
        assert_eq!(bar.tabs.len(), 1);
        assert_eq!(bar.tabs[0].agent_name, "default");
        assert_eq!(bar.tabs[0].session_id, "main-sid");
        assert_eq!(bar.active, 0);
        assert_eq!(bar.page_start, 0);
    }

    #[test]
    fn test_tabbar_new_has_last_term_width_zero() {
        let bar = TabBar::new("main-sid".into());
        assert_eq!(bar.last_term_width, 0);
    }

    #[test]
    fn test_tabbar_insert_sub_agent_at_index_1() {
        let mut bar = TabBar::new("main".into());
        bar.insert_sub_agent("sub1", "agentA", false);
        assert_eq!(bar.tabs.len(), 2);
        assert_eq!(bar.tabs[1].session_id, "sub1");
    }

    #[test]
    fn test_tabbar_insert_two_sub_agents_newer_first() {
        let mut bar = TabBar::new("main".into());
        bar.insert_sub_agent("sub1", "A", false);
        bar.insert_sub_agent("sub2", "B", false);
        assert_eq!(bar.tabs.len(), 3);
        assert_eq!(bar.tabs[1].session_id, "sub2");
        assert_eq!(bar.tabs[2].session_id, "sub1");
    }

    #[test]
    fn test_tabbar_insert_does_not_change_active() {
        let mut bar = TabBar::new("main".into());
        assert_eq!(bar.active, 0);
        bar.insert_sub_agent("sub1", "A", false);
        assert_eq!(bar.active, 0);
    }

    #[test]
    fn test_tabbar_insert_when_active_geq_1_shifts_active_plus_1() {
        let mut bar = TabBar::new("main".into());
        bar.insert_sub_agent("sub1", "A", false);
        bar.active = 1;
        bar.insert_sub_agent("sub2", "B", false);
        assert_eq!(bar.active, 2);
        // Still pointing to sub1 (now at index 2)
        assert_eq!(bar.tabs[bar.active].session_id, "sub1");
    }

    #[test]
    fn test_tabbar_find_index_by_session() {
        let mut bar = TabBar::new("main".into());
        bar.insert_sub_agent("sub1", "A", false);
        bar.insert_sub_agent("sub2", "B", false);
        assert_eq!(bar.find_index_by_session("sub1"), Some(2));
        assert_eq!(bar.find_index_by_session("sub2"), Some(1));
        assert_eq!(bar.find_index_by_session("nonexistent"), None);
    }

    #[test]
    fn test_tabbar_find_or_insert_creates_when_missing() {
        let mut bar = TabBar::new("main".into());
        let idx = bar.find_or_insert("new-sid", "agentX");
        assert_eq!(idx, 1);
        assert_eq!(bar.tabs.len(), 2);
    }

    #[test]
    fn test_tabbar_find_or_insert_returns_existing() {
        let mut bar = TabBar::new("main".into());
        bar.insert_sub_agent("sub1", "A", false);
        let len_before = bar.tabs.len();
        let idx = bar.find_or_insert("sub1", "A");
        assert_eq!(idx, 1);
        assert_eq!(bar.tabs.len(), len_before);
    }

    // ════════════════════════════════════════════════════════════
    // TabEntry::render_pending 测试
    // ════════════════════════════════════════════════════════════

    fn td(delta: &str) -> visp_proto::visp::ServerMessage {
        visp_proto::visp::ServerMessage {
            payload: Some(visp_proto::visp::server_message::Payload::TextDelta(
                visp_proto::visp::TextDelta {
                    delta: delta.into(),
                    session_id: String::new(),
                    agent_name: String::new(),
                },
            )),
        }
    }

    fn tool_call(name: &str, call_id: &str, args: &str) -> visp_proto::visp::ServerMessage {
        visp_proto::visp::ServerMessage {
            payload: Some(visp_proto::visp::server_message::Payload::ToolCall(
                visp_proto::visp::ToolCall {
                    tool_name: name.into(),
                    call_id: call_id.into(),
                    arguments: args.into(),
                    session_id: String::new(),
                    agent_name: String::new(),
                },
            )),
        }
    }

    fn tool_result(
        call_id: &str,
        tool_name: &str,
        content: &str,
        is_error: bool,
    ) -> visp_proto::visp::ServerMessage {
        visp_proto::visp::ServerMessage {
            payload: Some(visp_proto::visp::server_message::Payload::ToolResult(
                visp_proto::visp::ToolResult {
                    call_id: call_id.into(),
                    tool_name: tool_name.into(),
                    content: content.into(),
                    is_error,
                    session_id: String::new(),
                    agent_name: String::new(),
                },
            )),
        }
    }

    fn done_msg() -> visp_proto::visp::ServerMessage {
        visp_proto::visp::ServerMessage {
            payload: Some(visp_proto::visp::server_message::Payload::Done(
                visp_proto::visp::Done {
                    session_id: String::new(),
                },
            )),
        }
    }

    fn error_msg(code: &str, message: &str) -> visp_proto::visp::ServerMessage {
        visp_proto::visp::ServerMessage {
            payload: Some(visp_proto::visp::server_message::Payload::Error(
                visp_proto::visp::Error {
                    code: code.into(),
                    message: message.into(),
                    session_id: String::new(),
                    agent_name: String::new(),
                },
            )),
        }
    }

    #[test]
    fn test_render_pending_empty_frames_noop() {
        let mut tab = TabEntry::new("sid", "agent");
        tab.render_pending();
        assert_eq!(tab.rendered_up_to, 0);
        assert!(tab.messages.is_empty());
    }

    #[test]
    fn test_render_pending_text_delta_appends_streaming() {
        let mut tab = TabEntry::new("sid", "agent");
        tab.frames.push(td("hello"));
        tab.frames.push(td(" world"));
        tab.render_pending();
        assert_eq!(tab.streaming_text, "hello world");
        assert!(tab.messages.is_empty());
        assert_eq!(tab.rendered_up_to, 2);
    }

    #[test]
    fn test_render_pending_tool_call_flushes_streaming() {
        let mut tab = TabEntry::new("sid", "agent");
        tab.frames.push(td("hi"));
        tab.frames.push(tool_call("bash", "c1", r#"{}"#));
        tab.render_pending();
        // streaming flushed
        assert!(tab.streaming_text.is_empty());
        // 2 messages: Assistant("hi") + ToolCall
        assert_eq!(tab.messages.len(), 2);
        assert_eq!(tab.messages[0].line_type, LineType::Assistant);
        assert_eq!(tab.messages[0].content, "hi");
        assert_eq!(
            tab.messages[1].line_type,
            LineType::ToolCall {
                name: "bash".into()
            }
        );
        assert_eq!(tab.messages[1].call_id.as_deref(), Some("c1"));
    }

    #[test]
    fn test_render_pending_tool_result_finds_tool_name_within_tab() {
        let mut tab = TabEntry::new("sid", "agent");
        tab.frames.push(tool_call("bash", "c1", r#"{}"#));
        tab.frames.push(tool_result("c1", "", "ok", false));
        tab.render_pending();
        assert_eq!(tab.messages.len(), 2);
        assert_eq!(
            tab.messages[0].line_type,
            LineType::ToolCall {
                name: "bash".into()
            }
        );
        assert_eq!(
            tab.messages[1].line_type,
            LineType::ToolResult {
                name: "bash".into()
            }
        );
    }

    #[test]
    fn test_render_pending_idempotent() {
        let mut tab = TabEntry::new("sid", "agent");
        tab.frames.push(td("hello"));
        tab.render_pending();
        assert_eq!(tab.streaming_text, "hello");
        assert_eq!(tab.rendered_up_to, 1);
        // second call: no new frames, should be noop
        tab.render_pending();
        assert_eq!(tab.streaming_text, "hello");
        assert_eq!(tab.rendered_up_to, 1);
    }

    #[test]
    fn test_render_pending_increments_rendered_up_to() {
        let mut tab = TabEntry::new("sid", "agent");
        tab.frames.push(td("a"));
        tab.frames.push(td("b"));
        tab.frames.push(td("c"));
        tab.render_pending();
        assert_eq!(tab.rendered_up_to, 3);
    }

    #[test]
    fn test_render_pending_done_running_to_done() {
        let mut tab = TabEntry::new("sid", "agent");
        tab.generating = true;
        tab.frames.push(done_msg());
        tab.render_pending();
        assert_eq!(tab.status, AgentStatus::Done);
        assert!(!tab.generating);
    }

    #[test]
    fn test_render_pending_done_does_not_overwrite_error() {
        let mut tab = TabEntry::new("sid", "agent");
        tab.status = AgentStatus::Error;
        tab.generating = true;
        tab.frames.push(done_msg());
        tab.render_pending();
        assert_eq!(tab.status, AgentStatus::Error);
        assert!(!tab.generating);
    }

    #[test]
    fn test_render_pending_done_does_not_overwrite_done() {
        let mut tab = TabEntry::new("sid", "agent");
        tab.status = AgentStatus::Done;
        tab.frames.push(done_msg());
        tab.render_pending();
        assert_eq!(tab.status, AgentStatus::Done);
    }

    #[test]
    fn test_render_pending_error_event_updates_status_to_error() {
        let mut tab = TabEntry::new("sid", "agent");
        tab.generating = true;
        tab.frames.push(error_msg("X", "boom"));
        tab.render_pending();
        assert_eq!(tab.status, AgentStatus::Error);
        assert!(!tab.generating);
        assert_eq!(tab.messages.len(), 1);
        assert_eq!(tab.messages[0].line_type, LineType::Error);
    }

    #[test]
    fn test_render_pending_error_then_done_status_remains_error() {
        let mut tab = TabEntry::new("sid", "agent");
        tab.frames.push(error_msg("X", "boom"));
        tab.frames.push(done_msg());
        tab.render_pending();
        assert_eq!(tab.status, AgentStatus::Error);
        assert!(!tab.generating);
    }

    #[test]
    fn test_render_pending_done_clears_generating() {
        let mut tab = TabEntry::new("sid", "agent");
        tab.generating = true;
        tab.frames.push(done_msg());
        tab.render_pending();
        assert!(!tab.generating);
        assert_eq!(tab.status, AgentStatus::Done);
    }

    // ════════════════════════════════════════════════════════════
    // Step 5: Message API 重构 — 类型 A（default tab）vs 类型 B（session 路由）
    // ════════════════════════════════════════════════════════════

    #[test]
    fn test_add_message_writes_to_default_tab() {
        let mut app = AppState::new("sid".into(), "m".into(), "".into(), String::new());
        app.tab_bar.insert_sub_agent("sub-1", "agentA", false);
        app.add_message(LineType::User, "hi".into());
        assert_eq!(app.tab_bar.tabs[0].messages.len(), 1);
        assert_eq!(app.tab_bar.tabs[1].messages.len(), 0);
        assert_eq!(app.tab_bar.tabs[0].messages[0].content, "hi");
    }

    #[test]
    fn test_add_message_writes_to_default_when_active_is_sub() {
        let mut app = AppState::new("sid".into(), "m".into(), "".into(), String::new());
        app.tab_bar.insert_sub_agent("sub-1", "agentA", false);
        // Switch active to sub tab
        app.tab_bar.active = 1;
        app.add_message(LineType::User, "hello".into());
        // Default tab gets the message
        assert_eq!(app.tab_bar.tabs[0].messages.len(), 1);
        assert_eq!(app.tab_bar.tabs[0].messages[0].content, "hello");
        // Sub tab remains empty
        assert_eq!(app.tab_bar.tabs[1].messages.len(), 0);
    }

    #[test]
    fn test_add_message_to_session_routes_to_correct_tab() {
        let mut app = AppState::new("sid".into(), "m".into(), "".into(), String::new());
        app.tab_bar.insert_sub_agent("sub-1", "agentA", false);
        app.tab_bar.insert_sub_agent("sub-2", "agentB", false);
        app.add_message_to_session("sub-1", LineType::Assistant, "from agent".into());
        // sub-2 at index 1 (newest first), sub-1 at index 2
        assert_eq!(app.tab_bar.tabs[0].messages.len(), 0);
        assert_eq!(app.tab_bar.tabs[1].messages.len(), 0);
        assert_eq!(app.tab_bar.tabs[2].messages.len(), 1);
        assert_eq!(app.tab_bar.tabs[2].messages[0].content, "from agent");
    }

    #[test]
    fn test_add_message_to_session_unknown_falls_back_to_default() {
        let mut app = AppState::new("sid".into(), "m".into(), "".into(), String::new());
        app.tab_bar.insert_sub_agent("sub-1", "agentA", false);
        app.add_message_to_session("unknown-sid", LineType::Status, "fallback".into());
        // Default tab gets the fallback
        assert_eq!(app.tab_bar.tabs[0].messages.len(), 1);
        assert_eq!(app.tab_bar.tabs[0].messages[0].content, "fallback");
        // Sub tab unchanged
        assert_eq!(app.tab_bar.tabs[1].messages.len(), 0);
    }

    #[test]
    fn test_add_tool_line_to_session_routes_by_session_id() {
        let mut app = AppState::new("sid".into(), "m".into(), "".into(), String::new());
        app.tab_bar.insert_sub_agent("sub-1", "agentA", false);
        app.add_tool_line_to_session(
            "sub-1",
            LineType::ToolCall {
                name: "bash".into(),
            },
            "echo hi".into(),
            "tc_1",
        );
        assert_eq!(app.tab_bar.tabs[0].messages.len(), 0);
        assert_eq!(app.tab_bar.tabs[1].messages.len(), 1);
        assert_eq!(
            app.tab_bar.tabs[1].messages[0].call_id.as_deref(),
            Some("tc_1")
        );
    }

    #[test]
    fn test_update_thinking_to_session_routes_by_session_id() {
        let mut app = AppState::new("sid".into(), "m".into(), "".into(), String::new());
        app.tab_bar.insert_sub_agent("sub-1", "agentA", false);
        app.update_thinking_to_session("sub-1", "thinking...".into());
        assert_eq!(app.tab_bar.tabs[0].messages.len(), 0);
        assert_eq!(app.tab_bar.tabs[1].messages.len(), 1);
        assert_eq!(
            app.tab_bar.tabs[1].messages[0].line_type,
            LineType::Thinking
        );
        assert_eq!(app.tab_bar.tabs[1].messages[0].content, "thinking...");
        // Update existing thinking
        app.update_thinking_to_session("sub-1", "updated thinking".into());
        assert_eq!(app.tab_bar.tabs[1].messages.len(), 1);
        assert_eq!(app.tab_bar.tabs[1].messages[0].content, "updated thinking");
    }

    #[test]
    fn test_append_streaming_to_session_routes_by_session_id() {
        let mut app = AppState::new("sid".into(), "m".into(), "".into(), String::new());
        app.tab_bar.insert_sub_agent("sub-1", "agentA", false);
        app.append_streaming_to_session("sub-1", "Hello ");
        app.append_streaming_to_session("sub-1", "world");
        assert_eq!(app.tab_bar.tabs[0].streaming_text, "");
        assert_eq!(app.tab_bar.tabs[1].streaming_text, "Hello world");
    }

    #[test]
    fn test_flush_streaming_to_session_routes_by_session_id() {
        let mut app = AppState::new("sid".into(), "m".into(), "".into(), String::new());
        app.tab_bar.insert_sub_agent("sub-1", "agentA", false);
        app.append_streaming_to_session("sub-1", "Hello world");
        app.flush_streaming_to_session("sub-1");
        // After flush, streaming_text is cleared and a message is added
        assert_eq!(app.tab_bar.tabs[0].streaming_text, "");
        assert_eq!(app.tab_bar.tabs[0].messages.len(), 0);
        assert_eq!(app.tab_bar.tabs[1].streaming_text, "");
        assert_eq!(app.tab_bar.tabs[1].messages.len(), 1);
        assert_eq!(
            app.tab_bar.tabs[1].messages[0].line_type,
            LineType::Assistant
        );
        assert_eq!(app.tab_bar.tabs[1].messages[0].content, "Hello world");
    }

    // ════════════════════════════════════════════════════════════
    // Step 6: route_frame tests
    // ════════════════════════════════════════════════════════════

    fn make_text_delta_frame(sid: &str, agent_name: &str, delta: &str) -> ServerMessage {
        ServerMessage {
            payload: Some(visp_proto::visp::server_message::Payload::TextDelta(
                visp_proto::visp::TextDelta {
                    delta: delta.into(),
                    session_id: sid.into(),
                    agent_name: agent_name.into(),
                },
            )),
        }
    }

    fn make_tool_call_frame(
        sid: &str,
        agent_name: &str,
        call_id: &str,
        tool: &str,
        args: &str,
    ) -> ServerMessage {
        ServerMessage {
            payload: Some(visp_proto::visp::server_message::Payload::ToolCall(
                visp_proto::visp::ToolCall {
                    call_id: call_id.into(),
                    tool_name: tool.into(),
                    arguments: args.into(),
                    session_id: sid.into(),
                    agent_name: agent_name.into(),
                },
            )),
        }
    }

    fn make_done_frame(sid: &str) -> ServerMessage {
        ServerMessage {
            payload: Some(visp_proto::visp::server_message::Payload::Done(
                visp_proto::visp::Done {
                    session_id: sid.into(),
                },
            )),
        }
    }

    fn make_status_update_frame_with_view_only(
        sid: &str,
        agent_name: &str,
        msg: &str,
        view_only: bool,
    ) -> ServerMessage {
        ServerMessage {
            payload: Some(server_message::Payload::StatusUpdate(
                visp_proto::visp::StatusUpdate {
                    message: msg.into(),
                    session_id: sid.into(),
                    agent_name: agent_name.into(),
                    user_inputs: vec![],
                    view_only,
                },
            )),
        }
    }

    fn make_status_update_frame(sid: &str, agent_name: &str, msg: &str) -> ServerMessage {
        ServerMessage {
            payload: Some(visp_proto::visp::server_message::Payload::StatusUpdate(
                visp_proto::visp::StatusUpdate {
                    message: msg.into(),
                    session_id: sid.into(),
                    agent_name: agent_name.into(),
                    user_inputs: vec![],
                    view_only: false,
                },
            )),
        }
    }

    #[test]
    fn test_route_frame_text_delta_main_session() {
        let mut app = AppState::new("main-sid".into(), "m".into(), "".into(), String::new());
        let frame = make_text_delta_frame("main-sid", "", "hello");
        app.route_frame(frame);
        assert_eq!(app.tab_bar.tabs[0].frames.len(), 1);
        assert_eq!(app.tab_bar.tabs[0].streaming_text, "hello");
    }

    #[test]
    fn test_route_frame_text_delta_sub_session_creates_tab() {
        let mut app = AppState::new("main-sid".into(), "m".into(), "".into(), String::new());
        let frame = make_text_delta_frame("sub-1", "explorer", "hello");
        app.route_frame(frame);
        assert_eq!(app.tab_bar.tabs.len(), 2);
        assert_eq!(app.tab_bar.tabs[1].session_id, "sub-1");
        assert_eq!(app.tab_bar.tabs[1].agent_name, "explorer");
        assert_eq!(app.tab_bar.tabs[1].frames.len(), 1);
        assert!(app.tab_bar.tabs[1].messages.is_empty());
    }

    #[test]
    fn test_route_frame_tool_call_routes() {
        let mut app = AppState::new("main-sid".into(), "m".into(), "".into(), String::new());
        app.tab_bar.insert_sub_agent("sub-1", "agentA", false);
        let frame = make_tool_call_frame("sub-1", "agentA", "c1", "bash", "{}");
        app.route_frame(frame);
        assert_eq!(app.tab_bar.tabs[0].frames.len(), 0);
        assert_eq!(app.tab_bar.tabs[1].frames.len(), 1);
    }

    #[test]
    fn test_route_frame_done_to_correct_tab() {
        let mut app = AppState::new("main-sid".into(), "m".into(), "".into(), String::new());
        app.tab_bar.insert_sub_agent("sub-1", "agentA", false);
        let frame = make_done_frame("sub-1");
        app.route_frame(frame);
        assert_eq!(app.tab_bar.tabs[0].frames.len(), 0);
        assert_eq!(app.tab_bar.tabs[1].frames.len(), 1);
    }

    #[test]
    fn test_route_frame_unknown_session_uses_agent_name_as_title() {
        let mut app = AppState::new("main-sid".into(), "m".into(), "".into(), String::new());
        let frame = make_text_delta_frame("new-sid", "X", "hi");
        app.route_frame(frame);
        assert_eq!(app.tab_bar.tabs.len(), 2);
        assert_eq!(app.tab_bar.tabs[1].session_id, "new-sid");
        assert_eq!(app.tab_bar.tabs[1].agent_name, "X");

        let frame2 = make_text_delta_frame("other-sid", "", "there");
        app.route_frame(frame2);
        assert_eq!(app.tab_bar.tabs.len(), 3);
        assert_eq!(app.tab_bar.tabs[1].session_id, "other-sid");
        assert_eq!(app.tab_bar.tabs[1].agent_name, "agent");
    }

    #[test]
    fn test_route_frame_upgrades_fallback_agent_name() {
        // 首帧 agent_name 为空 → tab 创建为 fallback "agent"
        let mut app = AppState::new("main-sid".into(), "m".into(), "".into(), String::new());
        let f1 = make_text_delta_frame("sub-sid", "", "first");
        app.route_frame(f1);
        assert_eq!(app.tab_bar.tabs[1].agent_name, "agent");

        // 后续帧带真实 agent_name → 升级 fallback "agent" 为真名
        let f2 = make_text_delta_frame("sub-sid", "explorer", "second");
        app.route_frame(f2);
        assert_eq!(app.tab_bar.tabs[1].agent_name, "explorer");
        assert_eq!(app.tab_bar.tabs[1].frames.len(), 2);
    }

    #[test]
    fn test_route_frame_active_tab_renders_immediately() {
        let mut app = AppState::new("main-sid".into(), "m".into(), "".into(), String::new());
        app.tab_bar.insert_sub_agent("sub-1", "agentA", false);
        app.tab_bar.active = 1;
        let frame = make_tool_call_frame("sub-1", "agentA", "c1", "bash", "{}");
        app.route_frame(frame);
        assert_eq!(app.tab_bar.tabs[1].messages.len(), 1);
        assert_eq!(
            app.tab_bar.tabs[1].messages[0].line_type,
            LineType::ToolCall {
                name: "bash".into()
            }
        );
    }

    #[test]
    fn test_route_frame_inactive_tab_accumulates_only() {
        let mut app = AppState::new("main-sid".into(), "m".into(), "".into(), String::new());
        app.tab_bar.insert_sub_agent("sub-1", "agentA", false);
        app.tab_bar.active = 1;
        let frame = make_text_delta_frame("main-sid", "", "hello");
        app.route_frame(frame);
        assert_eq!(app.tab_bar.tabs[0].frames.len(), 1);
        assert!(app.tab_bar.tabs[0].streaming_text.is_empty());
    }

    #[test]
    fn test_route_frame_done_updates_inactive_sub_tab_status_immediately() {
        // 子 tab 收到 Done，即使它不是 active，status 也应立刻变 Done（图标实时刷新）
        let mut app = AppState::new("main-sid".into(), "m".into(), "".into(), String::new());
        app.tab_bar.insert_sub_agent("sub-1", "agentA", false);
        // active 仍是 0 (default)
        assert_eq!(app.tab_bar.tabs[1].status, AgentStatus::Running);
        app.route_frame(make_done_frame("sub-1"));
        assert_eq!(app.tab_bar.tabs[1].status, AgentStatus::Done);
    }

    #[test]
    fn test_route_frame_error_updates_inactive_sub_tab_status_immediately() {
        let mut app = AppState::new("main-sid".into(), "m".into(), "".into(), String::new());
        app.tab_bar.insert_sub_agent("sub-1", "agentA", false);
        assert_eq!(app.tab_bar.tabs[1].status, AgentStatus::Running);
        app.route_frame(make_error_frame("sub-1", "agentA", "X", "boom"));
        assert_eq!(app.tab_bar.tabs[1].status, AgentStatus::Error);
    }

    #[test]
    fn test_route_frame_status_update_routes_by_session_id() {
        let mut app = AppState::new("main-sid".into(), "m".into(), "".into(), String::new());
        app.tab_bar.insert_sub_agent("sub-1", "agentA", false);
        let frame = make_status_update_frame("sub-1", "agentA", "working...");
        app.route_frame(frame);
        assert_eq!(app.tab_bar.tabs[0].frames.len(), 0);
        assert_eq!(app.tab_bar.tabs[1].frames.len(), 1);
    }

    #[test]
    fn test_route_frame_empty_session_id_falls_back_to_default() {
        let mut app = AppState::new("main-sid".into(), "m".into(), "".into(), String::new());
        let frame = make_text_delta_frame("", "", "hello");
        app.route_frame(frame);
        assert_eq!(app.tab_bar.tabs[0].frames.len(), 1);
        assert_eq!(app.tab_bar.tabs[0].streaming_text, "hello");
    }

    // ════════════════════════════════════════════════════════════
    // Step 6a: route_frame view_only tab creation
    // ════════════════════════════════════════════════════════════

    #[test]
    fn route_frame_status_update_view_only_creates_view_only_tab() {
        let mut app = AppState::new("main-sid".into(), "m".into(), "".into(), String::new());
        let frame = make_status_update_frame_with_view_only("sub-1", "agentA", "hi", true);
        app.route_frame(frame);
        assert_eq!(app.tab_bar.tabs.len(), 2);
        assert_eq!(app.tab_bar.tabs[1].session_id, "sub-1");
        assert_eq!(app.tab_bar.tabs[1].agent_name, "agentA");
        assert_eq!(app.tab_bar.tabs[1].status, AgentStatus::ViewOnly);
    }

    #[test]
    fn route_frame_status_update_view_false_creates_running_tab() {
        let mut app = AppState::new("main-sid".into(), "m".into(), "".into(), String::new());
        let frame = make_status_update_frame_with_view_only("sub-1", "agentA", "hi", false);
        app.route_frame(frame);
        assert_eq!(app.tab_bar.tabs.len(), 2);
        assert_eq!(app.tab_bar.tabs[1].status, AgentStatus::Running);
    }

    #[test]
    fn route_frame_existing_view_only_tab_not_recreated() {
        let mut app = AppState::new("main-sid".into(), "m".into(), "".into(), String::new());
        // Create sub-1 tab first
        app.tab_bar.insert_sub_agent("sub-1", "agentA", true);
        let len_before = app.tab_bar.tabs.len();
        // Send another StatusUpdate for same session
        let frame = make_status_update_frame_with_view_only("sub-1", "agentA", "updated", true);
        app.route_frame(frame);
        // Tab count unchanged (no duplicate)
        assert_eq!(app.tab_bar.tabs.len(), len_before);
        // Frame was still added to existing tab
        assert_eq!(app.tab_bar.tabs[1].frames.len(), 1);
    }

    #[test]
    fn route_frame_user_inputs_populated_for_view_only_tab() {
        let mut app = AppState::new("main-sid".into(), "m".into(), "".into(), String::new());
        // Simulate Phase 1: input_history already populated (as handle_grpc_message would)
        app.input_history.push("my original task prompt".into());
        app.input_history.push("follow-up question".into());
        // Send StatusUpdate with view_only=true
        let frame = make_status_update_frame_with_view_only("sub-1", "agentA", "restored", true);
        app.route_frame(frame);
        // ViewOnly tab should have task prompt message
        let tab = &app.tab_bar.tabs[1];
        assert_eq!(tab.status, AgentStatus::ViewOnly);
        assert!(!tab.messages.is_empty());
        assert!(tab.messages[0].content.contains("[task prompt]"));
        assert!(tab.messages[0].content.contains("my original task prompt"));
    }

    // ════════════════════════════════════════════════════════════
    // Step 6c: SessionNotActive Error frame rendering
    // ════════════════════════════════════════════════════════════

    #[test]
    fn route_frame_error_session_not_active_renders_hint() {
        let mut app = AppState::new("main-sid".into(), "m".into(), "".into(), String::new());
        let frame = make_error_frame("sub-1", "agentA", "SessionNotActive", "session expired");
        app.route_frame(frame);
        // Tab was created automatically by route_frame for unknown session
        assert_eq!(app.tab_bar.tabs.len(), 2);
        let tab = &app.tab_bar.tabs[1];
        assert_eq!(tab.status, AgentStatus::Error);
        // Should have friendly hint message
        assert!(!tab.messages.is_empty());
        assert!(tab.messages[0].content.contains("该会话已结束"));
    }

    #[test]
    fn route_frame_error_other_codes_unchanged() {
        let mut app = AppState::new("main-sid".into(), "m".into(), "".into(), String::new());
        app.tab_bar.insert_sub_agent("sub-1", "agentA", false);
        let frame = make_error_frame("sub-1", "agentA", "SomeOtherError", "details");
        app.route_frame(frame);
        let tab = &app.tab_bar.tabs[1];
        assert_eq!(tab.status, AgentStatus::Error);
        // No friendly hint for non-SessionNotActive
        for msg in &tab.messages {
            assert!(!msg.content.contains("该会话已结束"));
        }
    }

    #[test]
    fn route_frame_error_routes_by_session_id() {
        let mut app = AppState::new("main-sid".into(), "m".into(), "".into(), String::new());
        app.tab_bar.insert_sub_agent("sub-1", "agentA", false);
        app.tab_bar.insert_sub_agent("sub-2", "agentB", false);
        // Error for sub-1 (index 2, newest inserts at 1)
        let frame = make_error_frame("sub-1", "agentA", "SessionNotActive", "expired");
        app.route_frame(frame);
        // Only sub-1 tab gets the error
        assert_eq!(app.tab_bar.tabs[1].session_id, "sub-2");
        assert_eq!(app.tab_bar.tabs[2].session_id, "sub-1");
        assert_eq!(app.tab_bar.tabs[1].status, AgentStatus::Running);
        assert_eq!(app.tab_bar.tabs[2].status, AgentStatus::Error);
    }

    // ════════════════════════════════════════════════════════════
    // Step 6b: ViewOnly tab UI behavior tests
    // ════════════════════════════════════════════════════════════

    #[test]
    fn view_only_tab_input_submission_disabled() {
        let mut app = AppState::new("sid".into(), "m".into(), "".into(), String::new());
        app.tab_bar.insert_sub_agent("sub-1", "agentA", true);
        app.tab_bar.active = 1;
        // Active tab is a ViewOnly sub-tab
        assert_eq!(app.active_tab().status, AgentStatus::ViewOnly);
        assert_ne!(app.tab_bar.active, 0);
        // The condition in handle_key_event that blocks Enter
        // (active != 0) is true, so input is blocked
    }

    #[test]
    fn view_only_tab_shows_task_prompt_marker() {
        let mut app = AppState::new("sid".into(), "m".into(), "".into(), String::new());
        app.input_history.push("task prompt text".into());
        let frame = make_status_update_frame_with_view_only("sub-1", "agentA", "hi", true);
        app.route_frame(frame);
        // ViewOnly tab should have a task prompt message
        let tab = &app.tab_bar.tabs[1];
        assert!(
            tab.messages
                .iter()
                .any(|m| m.content.contains("[task prompt]"))
        );
        assert!(
            tab.messages
                .iter()
                .any(|m| m.content.contains("task prompt text"))
        );
    }

    #[test]
    fn view_only_tab_arrow_keys_browse_input_history() {
        let mut app = AppState::new("sid".into(), "m".into(), "".into(), String::new());
        app.input_history.push("first".into());
        app.input_history.push("second".into());
        app.input_history.push("third".into());

        // Simulate ↑ pressed while no history_index (go to last)
        let idx = app
            .history_index
            .map_or(app.input_history.len().saturating_sub(1), |i| {
                i.saturating_sub(1)
            });
        assert_eq!(idx, 2);
        app.history_index = Some(idx);
        app.textarea = AppState::new_textarea();
        app.textarea.insert_str(&app.input_history[idx]);
        assert_eq!(app.textarea.lines()[0], "third");

        // ↑ again
        let idx = app.history_index.unwrap().saturating_sub(1);
        assert_eq!(idx, 1);
        app.history_index = Some(idx);
        app.textarea = AppState::new_textarea();
        app.textarea.insert_str(&app.input_history[idx]);
        assert_eq!(app.textarea.lines()[0], "second");

        // ↓ (go forward)
        let ni = app.history_index.unwrap() + 1;
        assert_eq!(ni, 2);
        app.history_index = Some(ni);
        app.textarea = AppState::new_textarea();
        app.textarea.insert_str(&app.input_history[ni]);
        assert_eq!(app.textarea.lines()[0], "third");

        // ↓ again (past end → clear)
        let ni = app.history_index.unwrap() + 1;
        assert_eq!(ni, 3);
        app.history_index = None;
        app.textarea = AppState::new_textarea();
        assert!(app.textarea.lines()[0].is_empty());
    }

    // ════════════════════════════════════════════════════════════
    // Step 7: Tab navigation (Alt+←/→)
    // ════════════════════════════════════════════════════════════

    #[test]
    fn test_tabbar_activate_next_advances() {
        let mut bar = TabBar::new("main".into());
        bar.insert_sub_agent("sub1", "A", false);
        bar.insert_sub_agent("sub2", "B", false);
        assert_eq!(bar.active, 0);
        bar.activate_next();
        assert_eq!(bar.active, 1);
    }

    #[test]
    fn test_tabbar_activate_next_at_last_wraps_to_zero() {
        let mut bar = TabBar::new("main".into());
        bar.insert_sub_agent("sub1", "A", false);
        bar.insert_sub_agent("sub2", "B", false);
        bar.active = 2;
        bar.activate_next();
        assert_eq!(bar.active, 0);
    }

    #[test]
    fn test_tabbar_activate_prev_at_zero_wraps_to_last() {
        let mut bar = TabBar::new("main".into());
        bar.insert_sub_agent("sub1", "A", false);
        bar.insert_sub_agent("sub2", "B", false);
        assert_eq!(bar.active, 0);
        bar.activate_prev();
        assert_eq!(bar.active, 2);
    }

    #[test]
    fn test_tabbar_activate_prev_decrements() {
        let mut bar = TabBar::new("main".into());
        bar.insert_sub_agent("sub1", "A", false);
        bar.insert_sub_agent("sub2", "B", false);
        bar.active = 2;
        bar.activate_prev();
        assert_eq!(bar.active, 1);
    }

    #[test]
    fn test_tabbar_activate_calls_render_pending_on_target() {
        let mut bar = TabBar::new("main".into());
        bar.insert_sub_agent("sub1", "A", false);
        bar.insert_sub_agent("sub2", "B", false);
        // Push a ToolCall frame to tabs[1] — TextDelta only fills streaming_text,
        // so we use ToolCall to verify render_pending was indeed called (messages != empty)
        bar.tabs[1].frames.push(tool_call("bash", "c1", r#"{}"#));
        bar.activate(1);
        // After activate, render_pending was called, so messages should have content
        assert!(!bar.tabs[1].messages.is_empty());
        assert_eq!(
            bar.tabs[1].messages[0].line_type,
            LineType::ToolCall {
                name: "bash".into()
            }
        );
    }

    // ════════════════════════════════════════════════════════════
    // Step 8: Token three-layer routing (L1 / L2 / L3)
    // ════════════════════════════════════════════════════════════

    fn make_usage_info_frame(
        sid: &str,
        input: u32,
        output: u32,
        tool_calls: u32,
        cache_create: u32,
        cache_read: u32,
    ) -> ServerMessage {
        ServerMessage {
            payload: Some(server_message::Payload::UsageInfo(
                visp_proto::visp::UsageInfo {
                    input_tokens: input,
                    output_tokens: output,
                    tool_calls,
                    session_id: sid.into(),
                    cache_creation_input_tokens: cache_create,
                    cache_read_input_tokens: cache_read,
                },
            )),
        }
    }

    #[test]
    fn test_usage_routed_to_tab_pending_usage() {
        let mut app = AppState::new("main-sid".into(), "m".into(), "".into(), String::new());
        app.tab_bar.insert_sub_agent("sub-1", "agentA", false);
        app.apply_usage_info("sub-1", 100, 20, 3, 10, 5);
        // L1: sub tab pending_usage is set
        assert_eq!(app.tab_bar.tabs[1].pending_usage, Some((100, 20, 3, 10, 5)));
        // Default tab unchanged
        assert!(app.tab_bar.tabs[0].pending_usage.is_none());
    }

    #[test]
    fn test_usage_accumulates_to_current_request_usage() {
        let mut app = AppState::new("main-sid".into(), "m".into(), "".into(), String::new());
        app.apply_usage_info("main-sid", 50, 10, 0, 5, 2);
        app.apply_usage_info("main-sid", 30, 8, 0, 3, 1);
        // L2 = cumulative sum
        assert_eq!(app.current_request_usage, (80, 18, 8, 3));
    }

    #[test]
    fn test_usage_now_directly_updates_total_tokens() {
        let mut app = AppState::new("main-sid".into(), "m".into(), "".into(), String::new());
        app.apply_usage_info("main-sid", 50, 10, 0, 5, 2);
        // L3 updated directly from apply_usage_info for status bar
        assert_eq!(app.total_input_tokens, 50);
        assert_eq!(app.total_output_tokens, 10);
        assert_eq!(app.total_cache_creation_input_tokens, 5);
        assert_eq!(app.total_cache_read_input_tokens, 2);
    }

    #[test]
    fn test_done_default_displays_l2_and_clears() {
        let mut app = AppState::new("main-sid".into(), "m".into(), "".into(), String::new());
        app.current_request_usage = (100, 50, 20, 10);
        app.apply_done_token_settlement("main-sid");
        // L2 is cleared
        assert_eq!(app.current_request_usage, (0, 0, 0, 0));
        // No Usage message added (token footer is appended in render_pending)
        assert!(app.tab_bar.tabs[0].messages.is_empty());
    }

    #[test]
    fn test_done_default_clears_l2_only() {
        let mut app = AppState::new("main-sid".into(), "m".into(), "".into(), String::new());
        // L2 was accumulated from UsageInfo (which also updated L3)
        app.current_request_usage = (100, 50, 20, 10);
        // L3 was already set by apply_usage_info
        app.total_input_tokens = 100;
        app.total_output_tokens = 50;
        app.total_cache_creation_input_tokens = 20;
        app.total_cache_read_input_tokens = 10;
        // Done clears L2, does NOT touch L3 (already done by apply_usage_info)
        app.apply_done_token_settlement("main-sid");
        assert_eq!(app.current_request_usage, (0, 0, 0, 0));
        assert_eq!(app.total_input_tokens, 100);
        assert_eq!(app.total_output_tokens, 50);
        assert_eq!(app.total_cache_creation_input_tokens, 20);
        assert_eq!(app.total_cache_read_input_tokens, 10);
    }

    #[test]
    fn test_done_sub_does_not_consume_pending_usage() {
        let mut app = AppState::new("main-sid".into(), "m".into(), "".into(), String::new());
        app.tab_bar.insert_sub_agent("sub-1", "agentA", false);
        app.tab_bar.tabs[1].pending_usage = Some((200, 30, 5, 15, 8));
        app.current_request_usage = (999, 999, 999, 999); // arbitrary L2
        app.apply_done_token_settlement("sub-1");
        // Sub tab: pending_usage is NOT consumed here (render_pending handles it)
        assert_eq!(app.tab_bar.tabs[1].pending_usage, Some((200, 30, 5, 15, 8)));
        // No Usage message added
        assert!(app.tab_bar.tabs[1].messages.is_empty());
        // L2 unchanged
        assert_eq!(app.current_request_usage, (999, 999, 999, 999));
        // L3 unchanged
        assert_eq!(app.total_input_tokens, 0);
    }

    #[test]
    fn test_done_sub_does_not_clear_l2() {
        let mut app = AppState::new("main-sid".into(), "m".into(), "".into(), String::new());
        app.tab_bar.insert_sub_agent("sub-1", "agentA", false);
        app.tab_bar.tabs[1].pending_usage = Some((10, 5, 1, 2, 3));
        app.current_request_usage = (50, 25, 10, 5);
        app.apply_done_token_settlement("sub-1");
        // L2 preserved
        assert_eq!(app.current_request_usage, (50, 25, 10, 5));
    }

    #[test]
    fn test_user_input_clears_current_request_usage() {
        let mut app = AppState::new("main-sid".into(), "m".into(), "".into(), String::new());
        app.apply_usage_info("main-sid", 100, 50, 0, 20, 10);
        assert_eq!(app.current_request_usage, (100, 50, 20, 10));
        // Simulate user input clearing L2
        app.current_request_usage = (0, 0, 0, 0);
        assert_eq!(app.current_request_usage, (0, 0, 0, 0));
    }

    #[test]
    fn test_done_status_guard_blocks_token_settlement() {
        let mut app = AppState::new("main-sid".into(), "m".into(), "".into(), String::new());
        app.tab_bar.tabs[0].status = AgentStatus::Error;
        app.current_request_usage = (100, 50, 20, 10);
        app.apply_done_token_settlement("main-sid");
        // No token line added
        assert!(app.tab_bar.tabs[0].messages.is_empty());
        // L3 unchanged
        assert_eq!(app.total_input_tokens, 0);
        // L2 unchanged
        assert_eq!(app.current_request_usage, (100, 50, 20, 10));
    }

    // ── TabBar pagination tests ─────────────────────────────────────

    fn make_tab_bar_with_subs(n: usize) -> TabBar {
        let mut tb = TabBar::new("main".into());
        for i in 0..n {
            tb.insert_sub_agent(format!("sub-{}", i), format!("agent{}", i), false);
        }
        tb
    }

    #[test]
    fn test_layout_pages_default_always_first() {
        let tb = make_tab_bar_with_subs(10);
        let pages = tb.layout_pages(80);
        for range in &pages {
            assert!(
                !range.contains(&0),
                "Page {:?} contains default tab 0",
                range
            );
        }
    }

    #[test]
    fn test_layout_pages_single_page_when_fits() {
        let tb = make_tab_bar_with_subs(2);
        let pages = tb.layout_pages(80);
        assert_eq!(pages.len(), 1);
        assert_eq!(pages[0], 1..3); // subs at indices 1,2
    }

    #[test]
    fn test_layout_pages_multi_page_when_overflow() {
        let tb = make_tab_bar_with_subs(10);
        let pages = tb.layout_pages(80);
        assert_eq!(pages.len(), 3); // 4+4+2
        assert_eq!(pages[0], 1..5);
        assert_eq!(pages[1], 5..9);
        assert_eq!(pages[2], 9..11);
    }

    #[test]
    fn test_alt_shift_right_advances_page() {
        let mut tb = make_tab_bar_with_subs(10);
        assert_eq!(tb.page_start, 0);
        let result = tb.next_page(80);
        assert!(result);
        assert_eq!(tb.page_start, 1);
    }

    #[test]
    fn test_alt_shift_left_at_zero_stops() {
        let mut tb = make_tab_bar_with_subs(10);
        tb.page_start = 0;
        let result = tb.prev_page();
        assert!(!result);
        assert_eq!(tb.page_start, 0);
    }

    #[test]
    fn test_alt_shift_right_at_last_stops() {
        let mut tb = make_tab_bar_with_subs(10);
        tb.page_start = 2; // last page (0-indexed, 3 pages total)
        let result = tb.next_page(80);
        assert!(!result);
        assert_eq!(tb.page_start, 2);
    }

    #[test]
    fn test_select_idx_in_visible_when_active_in_page() {
        let mut tb = make_tab_bar_with_subs(8);
        tb.page_start = 0; // subs 1-4 visible
        tb.active = 2; // "sub-6" at index 2
        let idx = tb.select_idx_for_current_page(80);
        // visible = [default, sub[1], sub[2]] → idx = 1 + (2-1) = 2
        assert_eq!(idx, Some(2));
    }

    #[test]
    fn test_active_tab_change_auto_scrolls_to_visible_page() {
        let mut tb = make_tab_bar_with_subs(8);
        tb.active = 6; // on page 1 (subs 5-8)
        tb.page_start = 0; // wrong page
        tb.ensure_active_visible(80);
        assert_eq!(tb.page_start, 1); // page containing index 6
    }

    // ════════════════════════════════════════════════════════════
    // Step 11: Ctrl+W close sub-agent tab
    // ════════════════════════════════════════════════════════════

    fn make_tab_bar_with_done_subs(n: usize) -> TabBar {
        let mut tb = TabBar::new("main".into());
        for i in 0..n {
            tb.insert_sub_agent(format!("sub-{}", i), format!("agent{}", i), false);
        }
        for tab in tb.tabs.iter_mut().skip(1) {
            tab.status = AgentStatus::Done;
        }
        tb
    }

    #[test]
    fn test_ctrl_w_on_default_is_noop() {
        let mut tb = TabBar::new("main".into());
        tb.insert_sub_agent("sub-1", "agentA", false);
        // active is 0 (default)
        assert!(!tb.close_active());
        assert_eq!(tb.tabs.len(), 2);
        assert_eq!(tb.active, 0);
    }

    #[test]
    fn test_ctrl_w_on_running_sub_is_noop() {
        let mut tb = TabBar::new("main".into());
        tb.insert_sub_agent("sub-1", "agentA", false);
        tb.active = 1;
        // status defaults to Running
        assert_eq!(tb.tabs[1].status, AgentStatus::Running);
        assert!(!tb.close_active());
        assert_eq!(tb.tabs.len(), 2);
    }

    #[test]
    fn test_ctrl_w_on_done_sub_removes_tab() {
        let mut tb = make_tab_bar_with_done_subs(1);
        tb.active = 1;
        assert!(tb.close_active());
        assert_eq!(tb.tabs.len(), 1); // only default remains
        assert_eq!(tb.active, 0);
    }

    #[test]
    fn test_ctrl_w_on_error_sub_removes_tab() {
        let mut tb = TabBar::new("main".into());
        tb.insert_sub_agent("sub-1", "agentA", false);
        tb.tabs[1].status = AgentStatus::Error;
        tb.active = 1;
        assert!(tb.close_active());
        assert_eq!(tb.tabs.len(), 1);
    }

    #[test]
    fn test_ctrl_w_activates_previous_tab() {
        let mut tb = make_tab_bar_with_done_subs(3);
        // tabs: [default, sub-2(Done), sub-1(Done), sub-0(Done)]
        tb.active = 2; // sub-1 (index 2)
        assert!(tb.close_active());
        // After remove: [default, sub-2, sub-0]; active decrements to 1 → sub-2
        assert_eq!(tb.active, 1);
        assert_eq!(tb.tabs[tb.active].session_id, "sub-2");
    }

    #[test]
    fn test_ctrl_w_at_last_sub_falls_back_to_default() {
        let mut tb = make_tab_bar_with_done_subs(1);
        tb.active = 1;
        assert!(tb.close_active());
        assert_eq!(tb.tabs.len(), 1);
        assert_eq!(tb.active, 0);
    }

    #[test]
    fn test_ctrl_w_renders_pending_for_new_active() {
        let mut tb = make_tab_bar_with_done_subs(2);
        // tabs: [default, sub-1(Done), sub-0(Done)]
        tb.active = 2; // sub-0
        // Push a frame to sub-1 (index 1) — will become active after close
        tb.tabs[1].frames.push(tool_call("bash", "c1", r#"{}"#));
        assert!(tb.close_active());
        // After close: active=1 → sub-1, render_pending was called
        assert!(!tb.tabs[1].messages.is_empty());
        assert_eq!(
            tb.tabs[1].messages[0].line_type,
            LineType::ToolCall {
                name: "bash".into()
            }
        );
    }

    #[test]
    fn test_ctrl_w_adjusts_tab_page() {
        // 5 subs → 2 pages (PER_PAGE=4): page 0 = indices 1-4, page 1 = index 5
        let mut tb = make_tab_bar_with_done_subs(5);
        tb.active = 5; // last sub, on page 1
        tb.page_start = 1;
        tb.last_term_width = 80;
        assert!(tb.close_active());
        // After close: active=4, which falls in page 0 (indices 1-4)
        assert_eq!(tb.page_start, 0);
        assert_eq!(tb.active, 4);
        assert_eq!(tb.tabs.len(), 5); // default + 4 subs
    }

    #[test]
    fn test_ctrl_w_closed_session_can_reopen_on_new_event() {
        let mut app = AppState::new("main-sid".into(), "m".into(), "".into(), String::new());
        // Route a frame to create a sub tab
        let frame = make_text_delta_frame("sub-1", "agentA", "hello");
        app.route_frame(frame);
        assert_eq!(app.tab_bar.tabs.len(), 2);

        // Set sub to Done and close it
        app.tab_bar.tabs[1].status = AgentStatus::Done;
        app.tab_bar.active = 1;
        assert!(app.tab_bar.close_active());
        assert_eq!(app.tab_bar.tabs.len(), 1);

        // Route another frame with same session_id — should re-create tab
        let frame2 = make_text_delta_frame("sub-1", "agentA", "world");
        app.route_frame(frame2);
        assert_eq!(app.tab_bar.tabs.len(), 2);
        assert_eq!(app.tab_bar.tabs[1].session_id, "sub-1");
    }

    // ════════════════════════════════════════════════════════════
    // Step 12: end-to-end integration tests
    // ════════════════════════════════════════════════════════════

    fn make_error_frame(sid: &str, agent_name: &str, code: &str, message: &str) -> ServerMessage {
        ServerMessage {
            payload: Some(server_message::Payload::Error(visp_proto::visp::Error {
                code: code.into(),
                message: message.into(),
                session_id: sid.into(),
                agent_name: agent_name.into(),
            })),
        }
    }

    #[test]
    fn test_e2e_spawn_subagent_creates_tab() {
        let mut app = AppState::new("main".into(), "m".into(), "".into(), String::new());
        let frame = make_text_delta_frame("sub1", "explorer", "hello");
        app.route_frame(frame);
        assert_eq!(app.tab_bar.tabs.len(), 2);
        assert_eq!(app.tab_bar.tabs[1].session_id, "sub1");
        assert_eq!(app.tab_bar.tabs[1].agent_name, "explorer");
        assert_eq!(app.tab_bar.active, 0);
    }

    #[test]
    fn test_e2e_subagent_done_changes_status_color() {
        let mut app = AppState::new("main".into(), "m".into(), "".into(), String::new());
        app.tab_bar.insert_sub_agent("sub1", "agentA", false);
        assert_eq!(app.tab_bar.tabs[1].status, AgentStatus::Running);
        // Tab must be active for route_frame to auto-render the Done frame
        app.tab_bar.active = 1;
        let frame = make_done_frame("sub1");
        app.route_frame(frame);
        assert_eq!(app.tab_bar.tabs[1].status, AgentStatus::Done);
    }

    #[test]
    fn test_e2e_subagent_error_status_guards_done() {
        let mut app = AppState::new("main".into(), "m".into(), "".into(), String::new());
        app.tab_bar.insert_sub_agent("sub1", "agentA", false);
        app.tab_bar.active = 1;
        // Error frame changes status to Error
        let err_frame = make_error_frame("sub1", "agentA", "ERR", "oops");
        app.route_frame(err_frame);
        assert_eq!(app.tab_bar.tabs[1].status, AgentStatus::Error);
        // Done frame should NOT override Error status (guard: only Running → Done)
        let done_frame = make_done_frame("sub1");
        app.route_frame(done_frame);
        assert_eq!(app.tab_bar.tabs[1].status, AgentStatus::Error);
    }

    #[test]
    fn test_e2e_subagent_inactive_does_not_pollute_default() {
        let mut app = AppState::new("main".into(), "m".into(), "".into(), String::new());
        // active defaults to 0; sub frames should go to sub tab, not default
        let f1 = make_text_delta_frame("sub1", "agentA", "hello");
        let f2 = make_tool_call_frame("sub1", "agentA", "c1", "bash", "{}");
        app.route_frame(f1);
        app.route_frame(f2);
        // Default tab untouched
        assert_eq!(app.tab_bar.tabs[0].frames.len(), 0);
        // Sub tab has accumulated frames
        assert_eq!(app.tab_bar.tabs[1].frames.len(), 2);
    }

    #[test]
    fn test_e2e_switch_to_sub_renders_accumulated() {
        let mut app = AppState::new("main".into(), "m".into(), "".into(), String::new());
        // Accumulate 4 TextDelta + 1 ToolCall (ToolCall creates a message)
        for i in 0..4 {
            let frame = make_text_delta_frame("sub1", "agentA", &format!("delta{}", i));
            app.route_frame(frame);
        }
        let tc = make_tool_call_frame("sub1", "agentA", "c1", "bash", "{}");
        app.route_frame(tc);
        // Before activation: lazy, no messages rendered
        assert_eq!(app.tab_bar.tabs[1].messages.len(), 0);
        assert_eq!(app.tab_bar.tabs[1].frames.len(), 5);
        // Switch to sub tab → auto renders all pending frames
        app.tab_bar.activate(1);
        // After rendering: messages populated, all frames processed
        assert!(!app.tab_bar.tabs[1].messages.is_empty());
        assert_eq!(app.tab_bar.tabs[1].rendered_up_to, 5);
    }

    #[test]
    fn test_e2e_token_l1_preserved_for_render_pending() {
        let mut app = AppState::new("main".into(), "m".into(), "".into(), String::new());
        app.tab_bar.insert_sub_agent("sub1", "agentA", false);
        // Route UsageInfo for sub (L1)
        app.apply_usage_info("sub1", 100, 200, 5, 10, 20);
        assert_eq!(
            app.tab_bar.tabs[1].pending_usage,
            Some((100, 200, 5, 10, 20))
        );
        // Route Done for sub → pending_usage preserved (render_pending handles it)
        app.apply_done_token_settlement("sub1");
        // L1 preserved for render_pending
        assert_eq!(
            app.tab_bar.tabs[1].pending_usage,
            Some((100, 200, 5, 10, 20))
        );
        // No Usage message added (render_pending appends to assistant text)
        assert!(app.tab_bar.tabs[1].messages.is_empty());
        // L2 still accumulated (sub Done does NOT clear L2)
        assert_eq!(app.current_request_usage, (100, 200, 10, 20));
        // L3 already updated by apply_usage_info (status bar shows immediately)
        assert_eq!(app.total_input_tokens, 100);
        assert_eq!(app.total_output_tokens, 200);
        assert_eq!(app.total_cache_creation_input_tokens, 10);
        assert_eq!(app.total_cache_read_input_tokens, 20);
    }

    #[test]
    fn test_e2e_token_l2_l3_only_on_default_done() {
        let mut app = AppState::new("main".into(), "m".into(), "".into(), String::new());
        // Two UsageInfo frames for main session
        app.apply_usage_info("main", 50, 80, 2, 5, 10);
        app.apply_usage_info("main", 30, 40, 1, 3, 5);
        // L2 accumulated (input, output, cache_create, cache_read)
        assert_eq!(app.current_request_usage, (80, 120, 8, 15));
        // Done for main → L2 cleared, L3 updated
        app.apply_done_token_settlement("main");
        // L2 cleared
        assert_eq!(app.current_request_usage, (0, 0, 0, 0));
        // L3 accumulated
        assert_eq!(app.total_input_tokens, 80);
        assert_eq!(app.total_output_tokens, 120);
        assert_eq!(app.total_cache_creation_input_tokens, 8);
        assert_eq!(app.total_cache_read_input_tokens, 15);
        // No Usage message added (token footer is appended in render_pending)
        assert!(app.tab_bar.tabs[0].messages.is_empty());
    }

    #[test]
    fn test_e2e_no_sub_prefix_in_messages() {
        let mut app = AppState::new("main".into(), "m".into(), "".into(), String::new());
        let frame = make_text_delta_frame("sub1", "agentA", "hello world");
        app.route_frame(frame);
        // Switch to sub tab to render
        app.tab_bar.activate(1);
        // No message should contain the "[sub:" prefix (removed in Step 3)
        for msg in &app.tab_bar.tabs[1].messages {
            assert!(
                !msg.content.contains("[sub:"),
                "Message content should not contain [sub: prefix, got: {}",
                msg.content
            );
        }
    }
}
