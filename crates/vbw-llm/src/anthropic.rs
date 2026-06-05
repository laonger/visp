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
            let tool_result = serde_json::json!({
                "type": "tool_result",
                "tool_use_id": msg.tool_call_id.as_deref().unwrap_or(""),
                "content": msg.content,
            });
            // 如果上一条消息已经是 user(tool_result)，追加到同一消息
            // (Anthropic 要求所有 tool_result 在同一 user 消息中)
            if let Some(last) = result.last_mut()
                && last["role"] == "user"
                && last["content"]
                    .as_array()
                    .map_or(false, |a| a.iter().all(|b| b["type"] == "tool_result"))
            {
                last["content"].as_array_mut().unwrap().push(tool_result);
            } else {
                result.push(serde_json::json!({
                    "role": "user",
                    "content": [tool_result],
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

        // 构建 content blocks：extra_blocks + text + tool_use
        let mut content_blocks: Vec<serde_json::Value> = Vec::new();

        // 来自 API 的额外内容块（如 thinking），原样保留
        if let Some(ref blocks) = msg.extra_blocks {
            content_blocks.extend(blocks.iter().cloned());
        }

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

/// SSE 解析中间结果
#[derive(Debug)]
pub(crate) enum ParsedEvent {
    Emit(ChatEvent),
    /// 工具输入增量: (index, tool_id, tool_name, partial_input_json)
    ToolInputDelta {
        index: u64,
        id: String,
        name: String,
        partial: String,
    },
    /// 内容块结束（工具或思考）: index
    BlockStop {
        index: u64,
    },
    /// 思考增量: (index, partial_text, signature)
    ThinkingDelta {
        index: u64,
        partial: String,
        signature: String,
    },
    Skip,
}

/// Anthropic SSE 事件解析（带工具输入增量累积）
///
/// 将 SSE 事件名和 JSON data 映射为 ParsedEvent。
/// - `content_block_delta` + `text_delta` → `Emit(ChatEvent::TextDelta)`
/// - `content_block_delta` + `input_json_delta` → `ToolInputDelta { index, .. }`
/// - `content_block_start` + `tool_use` → `ToolInputDelta { id, name, partial: initial_input }`
/// - `content_block_stop` → `ToolBlockStop { index }`
/// - `message_stop` → `Emit(ChatEvent::Done)`
/// - 其他事件 → `Skip`
pub fn parse_anthropic_event(event_name: &str, data: &str) -> Result<ParsedEvent, LlmError> {
    match event_name {
        "message_stop" => Ok(ParsedEvent::Emit(ChatEvent::Done)),
        "content_block_delta" => {
            let v: serde_json::Value = serde_json::from_str(data)
                .map_err(|e| LlmError::Stream(format!("parse content_block_delta: {e}")))?;
            let delta_type = v["delta"]["type"].as_str().unwrap_or("");
            let index = v["index"].as_u64().unwrap_or(0);
            match delta_type {
                "text_delta" => {
                    let text = v["delta"]["text"].as_str().unwrap_or("").to_string();
                    Ok(ParsedEvent::Emit(ChatEvent::TextDelta(text)))
                }
                "input_json_delta" => {
                    let partial = v["delta"]["partial_json"]
                        .as_str()
                        .unwrap_or("")
                        .to_string();
                    Ok(ParsedEvent::ToolInputDelta {
                        index,
                        id: String::new(),
                        name: String::new(),
                        partial,
                    })
                }
                _ => Ok(ParsedEvent::Skip),
            }
        }
        "content_block_start" => {
            let v: serde_json::Value = serde_json::from_str(data)
                .map_err(|e| LlmError::Stream(format!("parse content_block_start: {e}")))?;
            let index = v["index"].as_u64().unwrap_or(0);
            let block_type = v["content_block"]["type"].as_str().unwrap_or("");
            match block_type {
                "tool_use" => {
                    let id = v["content_block"]["id"].as_str().unwrap_or("").to_string();
                    let name = v["content_block"]["name"]
                        .as_str()
                        .unwrap_or("")
                        .to_string();
                    // 流式下 content_block_start 的 input 通常为 {}，真正的参数通过 input_json_delta 发送
                    // 舍弃初始 input，从空字符串开始累积
                    Ok(ParsedEvent::ToolInputDelta {
                        index,
                        id,
                        name,
                        partial: String::new(),
                    })
                }
                "thinking" => {
                    let signature = v["content_block"]["signature"]
                        .as_str()
                        .unwrap_or("")
                        .to_string();
                    let thinking = v["content_block"]["thinking"]
                        .as_str()
                        .unwrap_or("")
                        .to_string();
                    Ok(ParsedEvent::ThinkingDelta {
                        index,
                        partial: thinking,
                        signature,
                    })
                }
                _ => Ok(ParsedEvent::Skip),
            }
        }
        "content_block_delta" => {
            let v: serde_json::Value = serde_json::from_str(data)
                .map_err(|e| LlmError::Stream(format!("parse content_block_delta: {e}")))?;
            let delta_type = v["delta"]["type"].as_str().unwrap_or("");
            let index = v["index"].as_u64().unwrap_or(0);
            match delta_type {
                "text_delta" => {
                    let text = v["delta"]["text"].as_str().unwrap_or("").to_string();
                    Ok(ParsedEvent::Emit(ChatEvent::TextDelta(text)))
                }
                "input_json_delta" => {
                    let partial = v["delta"]["partial_json"]
                        .as_str()
                        .unwrap_or("")
                        .to_string();
                    Ok(ParsedEvent::ToolInputDelta {
                        index,
                        id: String::new(),
                        name: String::new(),
                        partial,
                    })
                }
                "thinking_delta" => {
                    let partial = v["delta"]["thinking"].as_str().unwrap_or("").to_string();
                    Ok(ParsedEvent::ThinkingDelta {
                        index,
                        partial,
                        signature: String::new(),
                    })
                }
                _ => Ok(ParsedEvent::Skip),
            }
        }
        "content_block_stop" => {
            let v: serde_json::Value = serde_json::from_str(data)
                .map_err(|e| LlmError::Stream(format!("parse content_block_stop: {e}")))?;
            let index = v["index"].as_u64().unwrap_or(0);
            // 同时触发 thinking 和 tool_use 的 flush（由 byte_stream 按 index 区分）
            Ok(ParsedEvent::BlockStop { index })
        }
        _ => Ok(ParsedEvent::Skip),
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

    pub fn with_base_url(api_key: String, base_url: String) -> Self {
        Self {
            api_key,
            api_url: base_url,
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
    use std::collections::HashMap;

    struct ToolAcc {
        name: String,
        input: String,
    }
    let tools: HashMap<String, ToolAcc> = HashMap::new();
    let stream = Box::pin(byte_stream);
    let buf = String::new();

    let event_stream = stream::unfold(
        (stream, buf, tools),
        |(mut stream, mut buf, mut tools)| async move {
            loop {
                if let Some(pos) = buf.find("\n\n") {
                    let chunk = buf[..pos].to_string();
                    buf = buf[pos + 2..].to_string();

                    let sse_events = crate::streaming::parse_sse_events(&chunk);
                    for sse in sse_events {
                        let event_name = sse.event.as_deref().unwrap_or("");
                        let data = sse.data.as_deref().unwrap_or("");
                        match parse_anthropic_event(event_name, data) {
                            Ok(ParsedEvent::Emit(chat_event)) => {
                                return Some((Ok(chat_event), (stream, buf, tools)));
                            }
                            Ok(ParsedEvent::ThinkingDelta {
                                index,
                                partial,
                                signature,
                            }) => {
                                let key = format!("thinking_{}", index);
                                let entry = tools.entry(key).or_insert_with(|| ToolAcc {
                                    name: String::new(),
                                    input: String::new(),
                                });
                                if !signature.is_empty() {
                                    entry.name = signature;
                                }
                                entry.input.push_str(&partial);
                            }
                            Ok(ParsedEvent::Emit(chat_event)) => {
                                return Some((Ok(chat_event), (stream, buf, tools)));
                            }
                            Ok(ParsedEvent::ToolInputDelta {
                                index,
                                id,
                                name,
                                partial,
                            }) => {
                                let key = index.to_string();
                                if !name.is_empty() {
                                    // content_block_start: new tool
                                    tools.insert(
                                        key,
                                        ToolAcc {
                                            name,
                                            input: partial,
                                        },
                                    );
                                } else if let Some(acc) = tools.get_mut(&key) {
                                    // input_json_delta: append
                                    acc.input.push_str(&partial);
                                }
                            }
                            Ok(ParsedEvent::BlockStop { index }) => {
                                // flush thinking block
                                let tkey = format!("thinking_{}", index);
                                if let Some(acc) = tools.remove(&tkey) {
                                    let block = serde_json::json!({
                                        "type": "thinking",
                                        "thinking": acc.input,
                                        "signature": acc.name,
                                    });
                                    return Some((
                                        Ok(ChatEvent::ThinkingBlock(block)),
                                        (stream, buf, tools),
                                    ));
                                }
                                // flush tool block
                                let key = index.to_string();
                                if let Some(acc) = tools.remove(&key) {
                                    let evt = ChatEvent::ToolCall {
                                        id: format!("tool_call_{}", index),
                                        name: acc.name,
                                        arguments: acc.input,
                                    };
                                    return Some((Ok(evt), (stream, buf, tools)));
                                }
                            }
                            Ok(ParsedEvent::Skip) => continue,
                            Err(e) => {
                                return Some((Err(e), (stream, buf, tools)));
                            }
                        }
                    }
                    continue;
                }

                match stream.next().await {
                    Some(Ok(bytes)) => {
                        if let Ok(s) = std::str::from_utf8(&bytes) {
                            buf.push_str(s);
                        } else {
                            return Some((
                                Err(LlmError::Stream("Invalid UTF-8".into())),
                                (stream, buf, tools),
                            ));
                        }
                    }
                    Some(Err(e)) => {
                        return Some((Err(LlmError::Stream(e.to_string())), (stream, buf, tools)));
                    }
                    None => {
                        // 流结束，处理剩余 buffer
                        if !buf.is_empty() {
                            let sse_events = crate::streaming::parse_sse_events(&buf);
                            for sse in sse_events {
                                let event_name = sse.event.as_deref().unwrap_or("");
                                let data = sse.data.as_deref().unwrap_or("");
                                match parse_anthropic_event(event_name, data) {
                                    Ok(ParsedEvent::Emit(chat_event)) => {
                                        return Some((Ok(chat_event), (stream, buf, tools)));
                                    }
                                    Ok(_) => continue,
                                    Err(e) => {
                                        return Some((Err(e), (stream, buf, tools)));
                                    }
                                }
                            }
                        }
                        return None;
                    }
                }
            }
        },
    );

    Box::pin(event_stream)
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- AnthropicProvider 构造测试 ---

    #[test]
    fn test_provider_with_base_url() {
        let provider =
            AnthropicProvider::with_base_url("test-key".into(), "https://custom.api.com".into());
        assert_eq!(provider.api_url, "https://custom.api.com");
    }

    #[test]
    fn test_provider_default_url() {
        let provider = AnthropicProvider::new("test-key".into());
        assert_eq!(provider.api_url, "https://api.anthropic.com");
    }

    // --- parse_anthropic_event 测试 ---

    #[test]
    fn test_parse_text_delta() {
        let data = r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hello"}}"#;
        let result = parse_anthropic_event("content_block_delta", data).unwrap();
        match result {
            ParsedEvent::Emit(ChatEvent::TextDelta(t)) => assert_eq!(t, "Hello"),
            _ => panic!("expected TextDelta, got {:?}", result),
        }
    }

    #[test]
    fn test_parse_tool_use_start() {
        let data = r#"{"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"toolu_123","name":"get_weather","input":{"city":"Tokyo"}}}"#;
        let result = parse_anthropic_event("content_block_start", data).unwrap();
        match result {
            ParsedEvent::ToolInputDelta {
                index,
                id,
                name,
                partial,
            } => {
                assert_eq!(id, "toolu_123");
                assert_eq!(name, "get_weather");
                assert_eq!(index, 0);
                assert!(
                    partial.is_empty(),
                    "content_block_start tool input should be discarded in streaming mode"
                );
            }
            _ => panic!("expected ToolInputDelta, got {:?}", result),
        }
    }

    #[test]
    fn test_parse_message_stop() {
        let result = parse_anthropic_event("message_stop", r#"{"type":"message_stop"}"#).unwrap();
        match result {
            ParsedEvent::Emit(ChatEvent::Done) => {}
            _ => panic!("expected Done, got {:?}", result),
        }
    }

    #[test]
    fn test_parse_unknown_event_returns_skip() {
        let result = parse_anthropic_event("ping", "{}").unwrap();
        assert!(matches!(result, ParsedEvent::Skip));
    }

    #[test]
    fn test_parse_text_block_start_returns_skip() {
        let data =
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#;
        let result = parse_anthropic_event("content_block_start", data).unwrap();
        assert!(matches!(result, ParsedEvent::Skip));
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
    fn test_tool_message_creates_new_user() {
        let msgs = vec![
            Message::user("Check the weather"),
            Message::assistant("Let me look that up"),
            Message::tool("Sunny 22°C", "toolu_abc123"),
        ];
        let config = LlmConfig::default();
        let req = build_anthropic_request(&msgs, &[], &config);
        let msgs_arr = req["messages"].as_array().unwrap();
        // tool 消息创建独立 user 消息，3 条：user, assistant, user(tool_result)
        assert_eq!(msgs_arr.len(), 3);
        assert_eq!(msgs_arr[0]["role"], "user");
        assert_eq!(msgs_arr[1]["role"], "assistant");
        assert_eq!(msgs_arr[2]["role"], "user");
        let content = msgs_arr[2]["content"].as_array().unwrap();
        assert_eq!(content.len(), 1);
        assert_eq!(content[0]["type"], "tool_result");
        assert_eq!(content[0]["tool_use_id"], "toolu_abc123");
        assert_eq!(content[0]["content"], "Sunny 22°C");
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
