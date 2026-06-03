use async_trait::async_trait;
use futures::stream::{self, Stream, StreamExt};
use std::pin::Pin;
use vbw_core::error::LlmError;
use vbw_core::message::{Message, Role, ToolDefinition};
use vbw_core::provider::{ChatEvent, LlmConfig, LlmProvider};

/// 构建 Anthropic API 请求体
pub fn build_anthropic_request(
    messages: &[Message],
    tools: &[ToolDefinition],
    config: &LlmConfig,
) -> serde_json::Value {
    // 1. 提取 system 文本
    let system_text: String = messages
        .iter()
        .filter(|m| m.role == Role::System)
        .map(|m| m.content.as_str())
        .collect::<Vec<_>>()
        .join("\n\n");

    // 2. 转换非 system 消息
    let non_system: Vec<&Message> = messages.iter().filter(|m| m.role != Role::System).collect();
    let anthropic_messages = build_anthropic_messages(&non_system);

    // 3. 转换 tool definitions
    let anthropic_tools: Vec<serde_json::Value> = tools
        .iter()
        .map(|t| {
            serde_json::json!({
                "name": t.name,
                "description": t.description,
                "input_schema": t.parameters,
            })
        })
        .collect();

    // 4. 构建请求
    let mut request = serde_json::json!({
        "model": config.model,
        "messages": anthropic_messages,
        "max_tokens": config.max_tokens,
        "temperature": config.temperature,
    });

    if !system_text.is_empty() {
        request["system"] = serde_json::Value::String(system_text);
    }

    if !anthropic_tools.is_empty() {
        request["tools"] = serde_json::Value::Array(anthropic_tools);
    }

    // Anthropic API 流式请求
    request["stream"] = serde_json::Value::Bool(true);

    request
}

/// 构建请求头: Content-Type, x-api-key, anthropic-version
pub fn build_anthropic_headers(api_key: &str) -> reqwest::header::HeaderMap {
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(
        reqwest::header::CONTENT_TYPE,
        "application/json".parse().unwrap(),
    );
    headers.insert("x-api-key", api_key.parse().unwrap());
    headers.insert("anthropic-version", "2023-06-01".parse().unwrap());
    headers
}

/// 将 vbw-core 消息转换为 Anthropic Messages API 格式
///
/// 规则：
/// - tool role 合并到最近一条 user 消息的 tool_result content block
/// - 连续同角色消息合并（文本用 \n\n 拼接）
/// - 消息按时间顺序排列，包含 tool_result 的 user 消息会移动到末尾
fn build_anthropic_messages(messages: &[&Message]) -> Vec<serde_json::Value> {
    let mut result: Vec<serde_json::Value> = Vec::new();

    for msg in messages {
        if msg.role == Role::Tool {
            // 找到最近一条 user 消息，添加 tool_result content block
            if let Some(pos) = result.iter().rposition(|m| m["role"] == "user") {
                let tool_result = serde_json::json!({
                    "type": "tool_result",
                    "tool_use_id": msg.tool_call_id.as_deref().unwrap_or(""),
                    "content": msg.content,
                });
                result[pos]["content"]
                    .as_array_mut()
                    .unwrap()
                    .push(tool_result);

                // 若 user 不在末尾，移到末尾（tool 消息在时间线上在 assistant 之后）
                if pos != result.len() - 1 {
                    let user_msg = result.remove(pos);
                    result.push(user_msg);
                }
            } else {
                // 没有前置 user 消息时创建新的 user 消息
                result.push(serde_json::json!({
                    "role": "user",
                    "content": [{
                        "type": "tool_result",
                        "tool_use_id": msg.tool_call_id.as_deref().unwrap_or(""),
                        "content": msg.content,
                    }]
                }));
            }
            continue;
        }

        // User / Assistant 消息
        let role_str = match msg.role {
            Role::User => "user",
            Role::Assistant => "assistant",
            _ => unreachable!(),
        };

        // 构建 content blocks：text + tool_use（如果有）
        let mut content_blocks: Vec<serde_json::Value> = Vec::new();

        // text block
        if !msg.content.is_empty() {
            content_blocks.push(serde_json::json!({
                "type": "text",
                "text": msg.content,
            }));
        }

        // tool_use blocks（仅 assistant 消息有 tool_calls）
        if let Some(ref calls) = msg.tool_calls {
            for call in calls {
                let input: serde_json::Value =
                    serde_json::from_str(&call.arguments).unwrap_or_default();
                content_blocks.push(serde_json::json!({
                    "type": "tool_use",
                    "id": call.id,
                    "name": call.name,
                    "input": input,
                }));
            }
        }

        // 检查是否与上一条同角色消息合并
        if let Some(last) = result.last_mut()
            && last["role"] == role_str
            && !content_blocks.is_empty()
        {
            // 合并：将新 text 拼接到上一条的最后一个 text block
            let existing = last["content"].as_array_mut().unwrap();
            for block in content_blocks {
                if block["type"] == "text"
                    && existing
                        .last()
                        .map(|b| b["type"] == "text")
                        .unwrap_or(false)
                {
                    let last_text = existing.last_mut().unwrap();
                    let new_text = format!(
                        "{}\n\n{}",
                        last_text["text"].as_str().unwrap_or(""),
                        block["text"].as_str().unwrap_or("")
                    );
                    last_text["text"] = serde_json::Value::String(new_text);
                } else {
                    existing.push(block);
                }
            }
            continue;
        }

        // 创建新消息
        result.push(serde_json::json!({
            "role": role_str,
            "content": content_blocks,
        }));
    }

    result
}

/// Anthropic SSE 事件解析
///
/// 将 SSE 事件名和 JSON data 映射为 ChatEvent。
/// - `content_block_delta` + `text_delta` → `ChatEvent::TextDelta`
/// - `content_block_start` + `tool_use` → `ChatEvent::ToolCall`
/// - `message_stop` → `ChatEvent::Done`
/// - 其他事件（ping, message_start, content_block_stop, message_delta 等）→ `None`
pub fn parse_anthropic_event(event_name: &str, data: &str) -> Result<Option<ChatEvent>, LlmError> {
    match event_name {
        "message_stop" => Ok(Some(ChatEvent::Done)),
        "content_block_delta" => {
            let v: serde_json::Value = serde_json::from_str(data)
                .map_err(|e| LlmError::Stream(format!("parse content_block_delta: {e}")))?;
            if v["delta"]["type"] == "text_delta" {
                let text = v["delta"]["text"].as_str().unwrap_or("").to_string();
                Ok(Some(ChatEvent::TextDelta(text)))
            } else {
                Ok(None)
            }
        }
        "content_block_start" => {
            let v: serde_json::Value = serde_json::from_str(data)
                .map_err(|e| LlmError::Stream(format!("parse content_block_start: {e}")))?;
            if v["content_block"]["type"] == "tool_use" {
                let id = v["content_block"]["id"].as_str().unwrap_or("").to_string();
                let name = v["content_block"]["name"]
                    .as_str()
                    .unwrap_or("")
                    .to_string();
                let arguments = v["content_block"]["input"].to_string();
                Ok(Some(ChatEvent::ToolCall {
                    id,
                    name,
                    arguments,
                }))
            } else {
                Ok(None)
            }
        }
        _ => Ok(None),
    }
}

/// 从 HTTP 响应头解析 `Retry-After` 秒数
pub fn parse_retry_after(headers: &reqwest::header::HeaderMap) -> Option<u64> {
    headers
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<u64>().ok())
}

/// Anthropic API 提供器
pub struct AnthropicProvider {
    api_key: String,
    api_url: String,
}

impl AnthropicProvider {
    pub fn new(api_key: String) -> Self {
        Self {
            api_key,
            api_url: "https://api.anthropic.com".to_string(),
        }
    }
}

#[async_trait]
impl LlmProvider for AnthropicProvider {
    async fn chat_stream(
        &self,
        messages: &[Message],
        tools: &[ToolDefinition],
        config: &LlmConfig,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<ChatEvent, LlmError>> + Send>>, LlmError> {
        let url = format!("{}/v1/messages", self.api_url.trim_end_matches('/'));
        let body = build_anthropic_request(messages, tools, config);
        let headers = build_anthropic_headers(&self.api_key);

        let client = reqwest::Client::new();
        let response = client
            .post(&url)
            .headers(headers)
            .json(&body)
            .send()
            .await
            .map_err(|e| LlmError::Network(e.to_string()))?;

        let status = response.status();
        if status.is_success() {
            let byte_stream = response.bytes_stream();
            Ok(byte_stream_to_chat_events(byte_stream))
        } else if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
            let retry_after = parse_retry_after(response.headers()).unwrap_or(60);
            Err(LlmError::RateLimit {
                retry_after_secs: retry_after,
            })
        } else if status == reqwest::StatusCode::UNAUTHORIZED
            || status == reqwest::StatusCode::FORBIDDEN
        {
            let body_text = response.text().await.unwrap_or_default();
            Err(LlmError::Auth(body_text))
        } else {
            let body_text = response.text().await.unwrap_or_default();
            Err(LlmError::Api {
                status: status.as_u16(),
                message: body_text,
            })
        }
    }
}

/// 将 `reqwest` 的字节流转换为 `ChatEvent` 流
///
/// 累积字节直到遇到 `\n\n` 分隔符，然后用 `parse_sse_events` 解析
/// 每个完整的 SSE 事件，最后用 `parse_anthropic_event` 映射为 ChatEvent。
fn byte_stream_to_chat_events(
    byte_stream: impl Stream<Item = Result<bytes::Bytes, reqwest::Error>> + Send + 'static,
) -> Pin<Box<dyn Stream<Item = Result<ChatEvent, LlmError>> + Send>> {
    let buf = String::new();
    let stream = Box::pin(byte_stream);

    let event_stream = stream::unfold((stream, buf), |(mut stream, mut buf)| async move {
        loop {
            // 尝试从缓冲区提取一个完整的 SSE 事件 (以 \n\n 分隔)
            if let Some(pos) = buf.find("\n\n") {
                let chunk = buf[..pos].to_string();
                buf = buf[pos + 2..].to_string();

                let sse_events = crate::streaming::parse_sse_events(&chunk);
                for sse in sse_events {
                    let event_name = sse.event.as_deref().unwrap_or("");
                    let data = sse.data.as_deref().unwrap_or("");
                    match parse_anthropic_event(event_name, data) {
                        Ok(Some(chat_event)) => {
                            return Some((Ok(chat_event), (stream, buf)));
                        }
                        Ok(None) => continue,
                        Err(e) => {
                            return Some((Err(e), (stream, buf)));
                        }
                    }
                }
                continue;
            }

            // 需要更多数据
            match stream.next().await {
                Some(Ok(bytes)) => {
                    if let Ok(s) = std::str::from_utf8(&bytes) {
                        buf.push_str(s);
                    } else {
                        return Some((
                            Err(LlmError::Stream("Invalid UTF-8 in stream".into())),
                            (stream, buf),
                        ));
                    }
                }
                Some(Err(e)) => {
                    return Some((Err(LlmError::Stream(e.to_string())), (stream, buf)));
                }
                None => {
                    // 流结束，处理剩余的 buffer
                    if !buf.is_empty() {
                        let sse_events = crate::streaming::parse_sse_events(&buf);
                        buf.clear();
                        for sse in sse_events {
                            let event_name = sse.event.as_deref().unwrap_or("");
                            let data = sse.data.as_deref().unwrap_or("");
                            match parse_anthropic_event(event_name, data) {
                                Ok(Some(chat_event)) => {
                                    return Some((Ok(chat_event), (stream, buf)));
                                }
                                Ok(None) => continue,
                                Err(e) => {
                                    return Some((Err(e), (stream, buf)));
                                }
                            }
                        }
                    }
                    return None;
                }
            }
        }
    });

    Box::pin(event_stream)
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- parse_anthropic_event 测试 ---

    #[test]
    fn test_parse_text_delta() {
        let data = r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hello"}}"#;
        let result = parse_anthropic_event("content_block_delta", data);
        let event = result.unwrap().expect("expected Some event");
        assert!(matches!(&event, ChatEvent::TextDelta(t) if t == "Hello"));
    }

    #[test]
    fn test_parse_tool_use() {
        let data = r#"{"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"toolu_123","name":"get_weather","input":{"city":"Tokyo"}}}"#;
        let result = parse_anthropic_event("content_block_start", data);
        let event = result.unwrap().expect("expected Some event");
        match &event {
            ChatEvent::ToolCall {
                id,
                name,
                arguments,
            } => {
                assert_eq!(id, "toolu_123");
                assert_eq!(name, "get_weather");
                assert_eq!(arguments, r#"{"city":"Tokyo"}"#);
            }
            _ => panic!("expected ToolCall, got {:?}", event),
        }
    }

    #[test]
    fn test_parse_message_stop() {
        let result = parse_anthropic_event("message_stop", r#"{"type":"message_stop"}"#);
        let event = result.unwrap().expect("expected Some event");
        assert!(matches!(event, ChatEvent::Done));
    }

    #[test]
    fn test_parse_unknown_event_returns_none() {
        let result = parse_anthropic_event("ping", "{}");
        assert!(result.unwrap().is_none());
    }

    #[test]
    fn test_parse_content_block_start_text_returns_none() {
        let data =
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#;
        let result = parse_anthropic_event("content_block_start", data);
        assert!(result.unwrap().is_none());
    }

    // --- parse_retry_after 测试 ---

    #[test]
    fn test_parse_429_rate_limit() {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            reqwest::header::RETRY_AFTER,
            reqwest::header::HeaderValue::from_static("30"),
        );
        assert_eq!(parse_retry_after(&headers), Some(30));
    }

    #[test]
    fn test_parse_retry_after_missing() {
        let headers = reqwest::header::HeaderMap::new();
        assert_eq!(parse_retry_after(&headers), None);
    }

    #[test]
    fn test_parse_retry_after_invalid() {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            reqwest::header::RETRY_AFTER,
            reqwest::header::HeaderValue::from_static("not-a-number"),
        );
        assert_eq!(parse_retry_after(&headers), None);
    }

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
