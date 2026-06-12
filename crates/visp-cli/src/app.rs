#![allow(dead_code)]
#![allow(clippy::bool_assert_comparison)]

use ratatui::{
    style::Style,
    text::{Line, Span},
};
use ratatui_textarea::WrapMode;
use unicode_width::UnicodeWidthChar;

use crate::theme;

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
                        format!("{} {}", icon, content)
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
                    let status_line = format!("  ✓ {} {}", icon, summary);
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
                        let status = format!("  ✓ {} {}", icon, first);
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
                let error_line = format!("❌ {} {}", icon, msg.content);
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

pub struct AppState {
    // 对话
    pub messages: Vec<ChatLine>,
    pub message_caches: Vec<MessageCache>,
    pub streaming_text: String,
    pub scroll_following: bool,
    pub scroll_state: tui_scrollview::ScrollViewState,
    pub cache_width: u16,

    // 输入
    pub textarea: ratatui_textarea::TextArea<'static>,
    pub input_history: Vec<String>,
    pub history_index: Option<usize>,

    // 状态
    pub generating: bool,
    pub stale_done_expected: bool,
    /// 当前正在处理的请求 ID（用于 Done 后发 Ack）
    pub current_request_id: Option<&'static str>,
    pub needs_render: bool,
    pub last_scroll_time: Option<std::time::Instant>,
    pub last_stream_render: Option<std::time::Instant>,
    pub next_message_id: u64,
    pub confirm: Option<ConfirmState>,
    pub model: String,
    pub session_id: String,
    pub should_quit: bool,
    pub pending_usage: Option<(u32, u32, u32, u32, u32)>,
    /// 当前 session 累计 token 数（input + output），用于状态栏显示
    pub total_input_tokens: u32,
    pub total_output_tokens: u32,
    pub total_cache_creation_input_tokens: u32,
    pub total_cache_read_input_tokens: u32,
    pub mouse_captured: bool,
    /// 用户输入了 /new 命令，主循环需要创建新 session
    pub pending_new_session: bool,
    /// 是否显示帮助弹窗
    pub show_help: bool,
}

impl AppState {
    pub fn new(session_id: String, model: String) -> Self {
        let mut textarea = Self::new_textarea();
        textarea.set_placeholder_text("Type your message...");
        Self {
            messages: Vec::new(),
            message_caches: Vec::new(),
            streaming_text: String::new(),
            scroll_following: true,
            scroll_state: tui_scrollview::ScrollViewState::default(),
            cache_width: 0,
            textarea,
            input_history: Vec::new(),
            history_index: None,
            generating: false,
            stale_done_expected: false,
            current_request_id: None,
            needs_render: true,
            last_scroll_time: None,
            last_stream_render: None,
            next_message_id: 0,
            confirm: None,
            model,
            session_id,
            should_quit: false,
            pending_usage: None,
            total_input_tokens: 0,
            total_output_tokens: 0,
            total_cache_creation_input_tokens: 0,
            total_cache_read_input_tokens: 0,
            mouse_captured: true,
            pending_new_session: false,
            show_help: false,
        }
    }

    /// 创建配置了 word wrap 的新 TextArea
    pub fn new_textarea() -> ratatui_textarea::TextArea<'static> {
        let mut ta = ratatui_textarea::TextArea::default();
        ta.set_wrap_mode(WrapMode::WordOrGlyph);
        ta.set_placeholder_text("Type your message...");
        ta
    }

    pub fn add_message(&mut self, line_type: LineType, content: String) {
        let id = self.next_message_id;
        self.next_message_id += 1;
        self.messages.push(ChatLine {
            id,
            version: 0,
            line_type,
            content,
            call_id: None,
        });
    }

    pub fn add_tool_line(&mut self, line_type: LineType, content: String, call_id: &str) {
        let id = self.next_message_id;
        self.next_message_id += 1;
        self.messages.push(ChatLine {
            id,
            version: 0,
            line_type,
            content,
            call_id: Some(call_id.to_string()),
        });
    }

    pub fn insert_tool_result(&mut self, call_id: &str, content: String) {
        if let Some(msg) = self.messages.iter_mut().find(|m| {
            matches!(m.line_type, LineType::ToolCall { .. })
                && m.call_id.as_deref() == Some(call_id)
        }) {
            msg.content.push('\n');
            msg.content.push_str(&content);
            msg.version += 1;
        } else {
            let id = self.next_message_id;
            self.next_message_id += 1;
            self.messages.push(ChatLine {
                id,
                version: 0,
                line_type: LineType::ToolCall {
                    name: String::new(),
                },
                content,
                call_id: Some(call_id.to_string()),
            });
        }
    }

    pub fn append_streaming(&mut self, delta: &str) {
        self.streaming_text.push_str(delta);
    }

    pub fn flush_streaming(&mut self) {
        if !self.streaming_text.is_empty() {
            let text = std::mem::take(&mut self.streaming_text);
            self.add_message(LineType::Assistant, text);
        }
    }

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

    pub fn update_message(&mut self, id: u64, content: String) {
        if let Some(msg) = self.messages.iter_mut().find(|m| m.id == id) {
            msg.content = content;
            msg.version += 1;
        }
    }

    /// 原地更新最后一条 thinking 消息，用于流式 reasoning 显示。
    /// 如果最后一条不是 Thinking 则新增一行。
    pub fn update_thinking(&mut self, content: String) {
        if let Some(last) = self.messages.last_mut()
            && matches!(last.line_type, LineType::Thinking)
        {
            last.content = content;
            last.version += 1;
        } else {
            self.add_message(LineType::Thinking, content);
        }
    }

    pub fn clear_messages(&mut self) {
        self.messages.clear();
        self.message_caches.clear();
    }

    /// 重置为新 session 的状态（保留 mouse 设置和 textarea 内容）
    pub fn reset_for_new_session(&mut self, session_id: String, model: String) {
        self.messages.clear();
        self.message_caches.clear();
        self.streaming_text.clear();
        self.generating = false;
        self.stale_done_expected = false;
        self.current_request_id = None;
        self.next_message_id = 0;
        self.confirm = None;
        self.pending_usage = None;
        self.total_input_tokens = 0;
        self.total_output_tokens = 0;
        self.total_cache_creation_input_tokens = 0;
        self.total_cache_read_input_tokens = 0;
        self.pending_new_session = false;
        self.session_id = session_id;
        self.model = model;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let app = AppState::new("s".into(), "m".into());
        assert!(!app.stale_done_expected);
    }

    #[test]
    fn test_app_state_new() {
        let app = AppState::new("test-session".into(), "deepseek-v4-flash".into());
        assert_eq!(app.session_id, "test-session");
        assert_eq!(app.model, "deepseek-v4-flash");
        assert!(app.messages.is_empty());
        assert!(app.streaming_text.is_empty());
        assert!(!app.generating);
        assert!(app.confirm.is_none());
        assert!(!app.should_quit);
        assert!(app.scroll_following);
        assert_eq!(
            app.scroll_state.offset(),
            ratatui::layout::Position::new(0, 0)
        );
    }

    #[test]
    fn test_add_message() {
        let mut app = AppState::new("s".into(), "m".into());
        app.add_message(LineType::User, "hello".into());
        assert_eq!(app.messages.len(), 1);
        assert_eq!(app.messages[0].content, "hello");
        assert_eq!(app.messages[0].line_type, LineType::User);
    }

    #[test]
    fn test_add_message_id_increments() {
        let mut app = AppState::new("s".into(), "m".into());
        app.add_message(LineType::User, "a".into());
        app.add_message(LineType::Assistant, "b".into());
        assert_eq!(app.messages[0].id, 0);
        assert_eq!(app.messages[1].id, 1);
    }

    #[test]
    fn test_add_message_version_initial() {
        let mut app = AppState::new("s".into(), "m".into());
        app.add_message(LineType::User, "hello".into());
        assert_eq!(app.messages[0].version, 0);
    }

    #[test]
    fn test_streaming_text() {
        let mut app = AppState::new("s".into(), "m".into());
        app.append_streaming("Hello ");
        app.append_streaming("world");
        assert_eq!(app.streaming_text, "Hello world");
        app.flush_streaming();
        assert!(app.streaming_text.is_empty());
        assert_eq!(app.messages.len(), 1);
        assert_eq!(app.messages[0].line_type, LineType::Assistant);
        assert_eq!(app.messages[0].content, "Hello world");
    }

    #[test]
    fn test_update_message_increments_version() {
        let mut app = AppState::new("s".into(), "m".into());
        app.add_message(LineType::Assistant, "original".into());
        let id = app.messages[0].id;
        app.update_message(id, "updated".into());
        assert_eq!(app.messages[0].version, 1);
        assert_eq!(app.messages[0].content, "updated");
    }

    #[test]
    fn test_update_message_id_not_found_does_nothing() {
        let mut app = AppState::new("s".into(), "m".into());
        app.add_message(LineType::Assistant, "original".into());
        let original_version = app.messages[0].version;
        app.update_message(999, "nope".into());
        assert_eq!(app.messages[0].version, original_version);
        assert_eq!(app.messages[0].content, "original");
    }

    #[test]
    fn test_clear_messages() {
        let mut app = AppState::new("s".into(), "m".into());
        app.add_message(LineType::User, "hello".into());
        app.add_message(LineType::Assistant, "world".into());
        assert_eq!(app.messages.len(), 2);
        app.clear_messages();
        assert!(app.messages.is_empty());
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
        let mut app = AppState::new("s".into(), "m".into());
        app.add_message(LineType::User, "hello".into());
        // 手动添加一个 cache 模拟渲染后的状态
        app.message_caches
            .push(MessageCache::from_message(&app.messages[0], 80));
        assert_eq!(app.message_caches.len(), 1);
        app.clear_messages();
        assert!(app.messages.is_empty());
        assert!(app.message_caches.is_empty());
    }

    #[test]
    fn test_add_tool_line_stores_call_id() {
        let mut app = AppState::new("s".into(), "m".into());
        app.add_tool_line(
            LineType::ToolCall {
                name: "test".into(),
            },
            "cmd".into(),
            "tc_1",
        );
        assert_eq!(app.messages[0].call_id.as_deref(), Some("tc_1"));
    }

    #[test]
    fn test_insert_tool_result_appends_to_matching_call() {
        let mut app = AppState::new("s".into(), "m".into());
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
        assert_eq!(app.messages[0].content, "cmd1\nresult1");
        assert_eq!(app.messages[1].content, "cmd2");
        assert!(matches!(
            app.messages[1].line_type,
            LineType::ToolCall { .. }
        ));
    }

    #[test]
    fn test_insert_tool_result_without_matching_call_appends() {
        let mut app = AppState::new("s".into(), "m".into());
        app.add_tool_line(
            LineType::ToolCall {
                name: "test".into(),
            },
            "cmd".into(),
            "id1",
        );
        app.insert_tool_result("nonexistent", "result".into());
        // 没有匹配的 call_id，作为新的 ToolCall 追加到末尾
        assert_eq!(app.messages.len(), 2);
        assert_eq!(app.messages[1].content, "result");
    }

    #[test]
    fn test_multiple_tool_calls_grouped() {
        let mut app = AppState::new("s".into(), "m".into());
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
        assert_eq!(app.messages[0].content, "cmd1\nresult1");
        assert_eq!(app.messages[1].content, "cmd2\nresult2");
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
}
