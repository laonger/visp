use vbw_core::message::{Message, ToolDefinition};
use vbw_core::provider::LlmConfig;

/// 构建 Anthropic API 请求体
pub fn build_anthropic_request(
    _messages: &[Message],
    _tools: &[ToolDefinition],
    _config: &LlmConfig,
) -> serde_json::Value {
    todo!()
}

/// 构建 HTTP 请求头
pub fn build_anthropic_headers(_api_key: &str) -> reqwest::header::HeaderMap {
    todo!()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_messages_basic() {
        let msgs = vec![Message::user("Hello"), Message::assistant("Hi there!")];
        let config = LlmConfig {
            model: "claude-sonnet-4-20250514".into(),
            temperature: 0.7,
            max_tokens: 4096,
            extra: Default::default(),
        };
        let req = build_anthropic_request(&msgs, &[], &config);
        assert_eq!(req["model"], "claude-sonnet-4-20250514");
        assert_eq!(req["max_tokens"], 4096);
        assert_eq!(req["temperature"], 0.7);
        assert!(req.get("system").is_none());

        let msgs_arr = req["messages"].as_array().unwrap();
        assert_eq!(msgs_arr.len(), 2);
        assert_eq!(msgs_arr[0]["role"], "user");
        assert_eq!(msgs_arr[0]["content"][0]["type"], "text");
        assert_eq!(msgs_arr[0]["content"][0]["text"], "Hello");
        assert_eq!(msgs_arr[1]["role"], "assistant");
        assert_eq!(msgs_arr[1]["content"][0]["text"], "Hi there!");
    }

    #[test]
    fn test_system_message_separated() {
        let msgs = vec![
            Message::system("You are a helpful assistant."),
            Message::user("Hello"),
        ];
        let config = LlmConfig::default();
        let req = build_anthropic_request(&msgs, &[], &config);
        assert_eq!(req["system"], "You are a helpful assistant.");
        let msgs_arr = req["messages"].as_array().unwrap();
        assert_eq!(msgs_arr.len(), 1);
        assert_eq!(msgs_arr[0]["role"], "user");
    }

    #[test]
    fn test_tool_message_merged_into_user() {
        let msgs = vec![
            Message::user("Check the weather"),
            Message::assistant("Let me look that up"),
            Message::tool("Sunny 22°C", "toolu_abc123"),
        ];
        let config = LlmConfig::default();
        let req = build_anthropic_request(&msgs, &[], &config);
        let msgs_arr = req["messages"].as_array().unwrap();
        assert_eq!(msgs_arr.len(), 2);
        // Last message should be user with text + tool_result
        let last = &msgs_arr[1];
        assert_eq!(last["role"], "user");
        let content = last["content"].as_array().unwrap();
        assert_eq!(content.len(), 2);
        assert_eq!(content[0]["type"], "text");
        assert_eq!(content[0]["text"], "Check the weather");
        assert_eq!(content[1]["type"], "tool_result");
        assert_eq!(content[1]["tool_use_id"], "toolu_abc123");
        assert_eq!(content[1]["content"], "Sunny 22°C");
    }

    #[test]
    fn test_consecutive_same_role_merged() {
        let msgs = vec![
            Message::user("Hello"),
            Message::assistant("First response"),
            Message::assistant("Second response"),
        ];
        let config = LlmConfig::default();
        let req = build_anthropic_request(&msgs, &[], &config);
        let msgs_arr = req["messages"].as_array().unwrap();
        assert_eq!(msgs_arr.len(), 2);
        assert_eq!(msgs_arr[1]["role"], "assistant");
        let content = msgs_arr[1]["content"].as_array().unwrap();
        assert_eq!(content.len(), 1);
        assert_eq!(content[0]["text"], "First response\n\nSecond response");
    }
}
