#![allow(dead_code)]

use ratatui::{
    style::{Color, Style},
    text::Line,
};

#[derive(Debug, Clone, PartialEq)]
pub enum LineType {
    User,
    Assistant,
    ToolCall,
    ToolResult,
    Error,
    Status,
}

#[derive(Debug, Clone)]
pub struct ChatLine {
    pub id: u64,
    pub version: u64,
    pub line_type: LineType,
    pub content: String,
}

#[derive(Debug, Clone)]
pub struct ConfirmState {
    pub query_id: String,
    pub message: String,
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
        let (fg, bg) = match msg.line_type {
            LineType::User => (Color::Cyan, Color::from_u32(0x001A3A5E)),
            LineType::Assistant => (Color::White, Color::from_u32(0x001A1A2E)),
            LineType::ToolCall => (Color::Yellow, Color::from_u32(0x001A1A2E)),
            LineType::ToolResult => (Color::DarkGray, Color::from_u32(0x00222222)),
            LineType::Error => (Color::Red, Color::from_u32(0x001A1A2E)),
            LineType::Status => (Color::Gray, Color::from_u32(0x001A1A2E)),
        };
        let style = Style::default().fg(fg).bg(bg);

        let mut lines: Vec<Line<'static>> = Vec::new();

        if msg.line_type == LineType::User {
            lines.push(Line::styled(" ".repeat(width as usize), style));
        }

        let wrapped = wrap_text(&msg.content, width);
        let display_lines = if msg.line_type == LineType::ToolResult && wrapped.len() > 5 {
            let mut truncated: Vec<String> = wrapped.into_iter().take(4).collect();
            truncated.push(format!("... [truncated, {}B]", msg.content.len()));
            truncated
        } else {
            wrapped
        };

        for dl in display_lines {
            lines.push(Line::styled(
                if dl.is_empty() {
                    " ".repeat(width as usize)
                } else {
                    pad_to_width(&dl, width as usize)
                },
                style,
            ));
        }

        if msg.line_type == LineType::User {
            lines.push(Line::styled(" ".repeat(width as usize), style));
        }

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

fn wrap_text(text: &str, screen_width: u16) -> Vec<String> {
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
            let mut w: usize = 0;
            let mut end = start;
            for (i, &c) in chars[start..].iter().enumerate() {
                let cw: usize = if c > '\u{2000}' { 2 } else { 1 };
                if w + cw > sw && w > 0 {
                    end = start + i;
                    break;
                }
                w += cw;
                end = start + i + 1;
            }
            result.push(chars[start..end].iter().collect());
            start = end;
        }
    }
    result
}

fn pad_to_width(s: &str, width: usize) -> String {
    let len: usize = s.chars().map(|c| if c > '\u{2000}' { 2 } else { 1 }).sum();
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

    // 输入
    pub textarea: tui_textarea::TextArea<'static>,
    pub input_history: Vec<String>,
    pub history_index: Option<usize>,

    // 状态
    pub generating: bool,
    pub needs_render: bool,
    pub last_scroll_time: Option<std::time::Instant>,
    pub next_message_id: u64,
    pub confirm: Option<ConfirmState>,
    pub model: String,
    pub session_id: String,
    pub should_quit: bool,
}

impl AppState {
    pub fn new(session_id: String, model: String) -> Self {
        let mut textarea = tui_textarea::TextArea::default();
        textarea.set_placeholder_text("Type your message...");
        Self {
            messages: Vec::new(),
            message_caches: Vec::new(),
            streaming_text: String::new(),
            scroll_following: true,
            scroll_state: tui_scrollview::ScrollViewState::default(),
            textarea,
            input_history: Vec::new(),
            history_index: None,
            generating: false,
            needs_render: true,
            last_scroll_time: None,
            next_message_id: 0,
            confirm: None,
            model,
            session_id,
            should_quit: false,
        }
    }

    pub fn add_message(&mut self, line_type: LineType, content: String) {
        let id = self.next_message_id;
        self.next_message_id += 1;
        self.messages.push(ChatLine {
            id,
            version: 0,
            line_type,
            content,
        });
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

    pub fn update_message(&mut self, id: u64, content: String) {
        if let Some(msg) = self.messages.iter_mut().find(|m| m.id == id) {
            msg.content = content;
            msg.version += 1;
        }
    }

    pub fn clear_messages(&mut self) {
        self.messages.clear();
        self.message_caches.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        };
        let cache = MessageCache::from_message(&msg, 80);
        // User 消息上下各有一行空行，至少 3 行
        assert!(cache.line_count >= 3);
    }

    #[test]
    fn test_cache_tool_result_truncation() {
        let msg = ChatLine {
            id: 0,
            version: 0,
            line_type: LineType::ToolResult,
            content: "line1\nline2\nline3\nline4\nline5\nline6\nline7".into(),
        };
        let cache = MessageCache::from_message(&msg, 80);
        // 截断为 5 行（4+省略）
        assert_eq!(cache.line_count, 5);
    }

    #[test]
    fn test_clear_messages_also_clears_caches() {
        let mut app = AppState::new("s".into(), "m".into());
        app.add_message(LineType::User, "hello".into());
        // 手动添加一个 cache 模拟渲染后的状态
        app.message_caches.push(MessageCache::from_message(&app.messages[0], 80));
        assert_eq!(app.message_caches.len(), 1);
        app.clear_messages();
        assert!(app.messages.is_empty());
        assert!(app.message_caches.is_empty());
    }
}
