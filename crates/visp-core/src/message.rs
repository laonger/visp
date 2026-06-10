use serde::{Deserialize, Serialize};

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
