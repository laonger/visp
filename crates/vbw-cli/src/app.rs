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
    pub scroll_offset: usize,
    pub scroll_following: bool,

    // 输入
    pub textarea: tui_textarea::TextArea<'static>,
    pub input_history: Vec<String>,
    pub history_index: Option<usize>,

    // 状态
    pub generating: bool,
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
            scroll_offset: 0,
            scroll_following: true,
            textarea,
            input_history: Vec::new(),
            history_index: None,
            generating: false,
            confirm: None,
            model,
            session_id,
            should_quit: false,
        }
    }

    pub fn add_message(&mut self, line_type: LineType, content: String) {
        self.messages.push(ChatLine { line_type, content });
    }

    pub fn append_streaming(&mut self, delta: &str) {
        self.streaming_text.push_str(delta);
    }

    pub fn flush_streaming(&mut self) {
        if !self.streaming_text.is_empty() {
            self.messages.push(ChatLine {
                line_type: LineType::Assistant,
                content: std::mem::take(&mut self.streaming_text),
            });
        }
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
}
