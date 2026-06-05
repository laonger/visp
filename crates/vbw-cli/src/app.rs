#![allow(dead_code)]

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

pub struct AppState {
    // 对话
    pub messages: Vec<ChatLine>,
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
}
