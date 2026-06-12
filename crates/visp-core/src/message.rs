use serde::{Deserialize, Serialize};

/// 消息子类型，与 role 正交
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum MessageType {
    #[serde(rename = "text")]
    Text,
    #[serde(rename = "thinking")]
    Thinking,
    #[serde(rename = "tool_call")]
    ToolCall,
    #[serde(rename = "tool_result")]
    ToolResult,
    #[serde(rename = "error")]
    Error,
    #[serde(rename = "status")]
    Status,
    #[serde(rename = "system")]
    System,
    #[serde(rename = "user")]
    User,
}

/// 对话角色
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum Role {
    #[serde(rename = "system")]
    System,
    #[serde(rename = "user")]
    User,
    #[serde(rename = "assistant")]
    Assistant,
    #[serde(rename = "tool")]
    Tool,
}

/// 工具调用（来自 LLM）
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolCallRequest {
    pub id: String,
    pub name: String,
    pub arguments: String, // JSON string
}

/// 对话中的一条消息
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Message {
    pub role: Role,
    /// 消息子类型（Text/Thinking/ToolCall/ToolResult/Error/Status/System/User）
    pub kind: MessageType,
    pub content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCallRequest>>,
    /// 额外的原始内容块（如 thinking），以 JSON Value 存储
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extra_blocks: Option<Vec<serde_json::Value>>,
    /// 标记此消息不注入后续对话的 context window（用于 /init 等系统命令）
    #[serde(default)]
    pub skip_context: bool,
    /// 估算的消息 token 数，用于 context window 管理
    #[serde(default)]
    pub estimated_tokens: u32,
}

impl Message {
    pub fn system(content: impl Into<String>) -> Self {
        let mut msg = Self {
            role: Role::System,
            kind: MessageType::System,
            content: content.into(),
            tool_call_id: None,
            tool_calls: None,
            extra_blocks: None,
            skip_context: false,
            estimated_tokens: 0,
        };
        msg.estimated_tokens = estimate_message_tokens(&msg);
        msg
    }

    pub fn user(content: impl Into<String>) -> Self {
        let mut msg = Self {
            role: Role::User,
            kind: MessageType::User,
            content: content.into(),
            tool_call_id: None,
            tool_calls: None,
            extra_blocks: None,
            skip_context: false,
            estimated_tokens: 0,
        };
        msg.estimated_tokens = estimate_message_tokens(&msg);
        msg
    }

    pub fn assistant(content: impl Into<String>) -> Self {
        let mut msg = Self {
            role: Role::Assistant,
            kind: MessageType::Text,
            content: content.into(),
            tool_call_id: None,
            tool_calls: None,
            extra_blocks: None,
            skip_context: false,
            estimated_tokens: 0,
        };
        msg.estimated_tokens = estimate_message_tokens(&msg);
        msg
    }

    pub fn tool(content: impl Into<String>, call_id: impl Into<String>) -> Self {
        let mut msg = Self {
            role: Role::Tool,
            kind: MessageType::ToolResult,
            content: content.into(),
            tool_call_id: Some(call_id.into()),
            tool_calls: None,
            extra_blocks: None,
            skip_context: false,
            estimated_tokens: 0,
        };
        msg.estimated_tokens = estimate_message_tokens(&msg);
        msg
    }

    pub fn tool_call(calls: Vec<ToolCallRequest>) -> Self {
        Self {
            role: Role::Assistant,
            kind: MessageType::ToolCall,
            content: String::new(),
            tool_call_id: None,
            tool_calls: Some(calls),
            extra_blocks: None,
            skip_context: false,
            estimated_tokens: 0,
        }
    }

    pub fn thinking(content: impl Into<String>) -> Self {
        Self {
            role: Role::Assistant,
            kind: MessageType::Thinking,
            content: content.into(),
            tool_call_id: None,
            tool_calls: None,
            extra_blocks: None,
            skip_context: false,
            estimated_tokens: 0,
        }
    }

    pub fn error(content: impl Into<String>) -> Self {
        Self {
            role: Role::System,
            kind: MessageType::Error,
            content: content.into(),
            tool_call_id: None,
            tool_calls: None,
            extra_blocks: None,
            skip_context: false,
            estimated_tokens: 0,
        }
    }

    pub fn status(content: impl Into<String>) -> Self {
        Self {
            role: Role::System,
            kind: MessageType::Status,
            content: content.into(),
            tool_call_id: None,
            tool_calls: None,
            extra_blocks: None,
            skip_context: false,
            estimated_tokens: 0,
        }
    }
}

/// 估算文本的 token 数（基于 1 token ≈ 4 字符的近似值）
pub fn estimate_tokens(text: &str) -> u32 {
    if text.is_empty() {
        return 0;
    }
    (text.len() as f64 / 4.0).ceil() as u32
}

/// 估算一条消息的 token 数，包括 role overhead 和 tool 字段
pub fn estimate_message_tokens(msg: &Message) -> u32 {
    let mut total = estimate_tokens(&msg.content) + 1; // +1 for role overhead
    if let Some(ref id) = msg.tool_call_id {
        total += estimate_tokens(id);
    }
    if let Some(ref calls) = msg.tool_calls {
        for call in calls {
            total += estimate_tokens(&call.id)
                + estimate_tokens(&call.name)
                + estimate_tokens(&call.arguments);
        }
    }
    total
}

/// 工具定义，用于 LLM function calling
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value, // JSON Schema
    pub category: String,              // 工具分类，用于动态工具指南的分组展示
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Step 1: MessageType ──

    #[test]
    fn test_message_type_serde() {
        let cases = vec![
            (MessageType::Text, "\"text\""),
            (MessageType::Thinking, "\"thinking\""),
            (MessageType::ToolCall, "\"tool_call\""),
            (MessageType::ToolResult, "\"tool_result\""),
            (MessageType::Error, "\"error\""),
            (MessageType::Status, "\"status\""),
            (MessageType::System, "\"system\""),
            (MessageType::User, "\"user\""),
        ];
        for (variant, expected) in cases {
            let json = serde_json::to_string(&variant).unwrap();
            assert_eq!(json, expected);
            let deserialized: MessageType = serde_json::from_str(expected).unwrap();
            assert_eq!(deserialized, variant);
        }
    }

    #[test]
    fn test_user_message_kind() {
        let msg = Message::user("hi");
        assert_eq!(msg.kind, MessageType::User);
    }

    #[test]
    fn test_system_message_kind() {
        let msg = Message::system("prompt");
        assert_eq!(msg.kind, MessageType::System);
    }

    #[test]
    fn test_tool_message_kind() {
        let msg = Message::tool("out", "id");
        assert_eq!(msg.kind, MessageType::ToolResult);
    }

    #[test]
    fn test_assistant_message_kind() {
        let msg = Message::assistant("text");
        assert_eq!(msg.kind, MessageType::Text);
    }

    #[test]
    fn test_tool_call_constructor() {
        let calls = vec![ToolCallRequest {
            id: "call_1".into(),
            name: "read_file".into(),
            arguments: r#"{"path":"test.txt"}"#.into(),
        }];
        let msg = Message::tool_call(calls.clone());
        assert_eq!(msg.kind, MessageType::ToolCall);
        assert_eq!(msg.role, Role::Assistant);
        assert!(msg.tool_calls.is_some());
        assert_eq!(msg.tool_calls.unwrap(), calls);
    }

    #[test]
    fn test_thinking_constructor() {
        let msg = Message::thinking("thinking content");
        assert_eq!(msg.kind, MessageType::Thinking);
        assert_eq!(msg.content, "thinking content");
    }

    #[test]
    fn test_error_constructor() {
        let msg = Message::error("something went wrong");
        assert_eq!(msg.kind, MessageType::Error);
        assert_eq!(msg.content, "something went wrong");
        assert_eq!(msg.role, Role::System);
    }

    #[test]
    fn test_status_constructor() {
        let msg = Message::status("processing...");
        assert_eq!(msg.kind, MessageType::Status);
        assert_eq!(msg.content, "processing...");
        assert_eq!(msg.role, Role::System);
    }

    // ── Existing token tests ──

    #[test]
    fn test_user_message_tokens() {
        let msg = Message::user("hello");
        assert_eq!(msg.estimated_tokens, 3); // ceil(5/4)=2 + 1 role
    }

    #[test]
    fn test_user_empty_message_tokens() {
        let msg = Message::user("");
        assert_eq!(msg.estimated_tokens, 1); // 0 + 1 role
    }

    #[test]
    fn test_tool_message_tokens() {
        let msg = Message::tool("output", "call_1");
        assert_eq!(msg.estimated_tokens, 5); // ceil(6/4)=2 + 1 role + ceil(6/4)=2 = 5
    }

    #[test]
    fn test_assistant_message_tokens() {
        let msg = Message::assistant("hi");
        assert_eq!(msg.estimated_tokens, 2); // ceil(2/4)=1 + 1 role
    }

    #[test]
    fn test_system_message_tokens() {
        let msg = Message::system("prompt");
        assert_eq!(msg.estimated_tokens, 3); // ceil(6/4)=2 + 1 role
    }

    #[test]
    fn test_estimate_tokens_non_empty() {
        assert_eq!(estimate_tokens("test"), 1); // ceil(4/4)=1
    }

    #[test]
    fn test_estimate_tokens_empty() {
        assert_eq!(estimate_tokens(""), 0);
    }

    #[test]
    fn test_estimate_tokens_longer() {
        assert_eq!(estimate_tokens("hello world"), 3); // ceil(11/4)=3
    }
}
