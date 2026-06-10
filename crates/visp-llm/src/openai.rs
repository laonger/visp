use async_trait::async_trait;
use futures::stream::{self, Stream, StreamExt};
use std::collections::HashMap;
use std::pin::Pin;
use visp_core::error::LlmError;
use visp_core::message::{Message, Role, ToolDefinition};
use visp_core::provider::{ChatEvent, LlmConfig, LlmProvider};

/// 构建 OpenAI API 请求体
pub fn build_openai_request(
    messages: &[Message],
    tools: &[ToolDefinition],
    config: &LlmConfig,
) -> serde_json::Value {
    let openai_messages = build_openai_messages(messages);

    let mut request = serde_json::json!({
        "model": config.model,
        "messages": openai_messages,
        "max_tokens": config.max_tokens,
        "temperature": config.temperature,
        "stream": true,
    });

    // 添加工具定义
    if !tools.is_empty() {
        let openai_tools: Vec<serde_json::Value> = tools
            .iter()
            .map(|t| {
                serde_json::json!({
                    "type": "function",
                    "function": {
                        "name": t.name,
                        "description": t.description,
                        "parameters": t.parameters,
                    }
                })
            })
            .collect();
        request["tools"] = serde_json::Value::Array(openai_tools);
    }

    // tool_choice（通过 extra 配置）
    // 支持: "auto" / "none" / "required" / 或 JSON 对象 {"type":"function","function":{"name":"..."}}
    if let Some(tool_choice) = config.extra.get("tool_choice") {
        if tool_choice.starts_with('{') {
            if let Ok(val) = serde_json::from_str::<serde_json::Value>(tool_choice) {
                request["tool_choice"] = val;
            }
        } else {
            request["tool_choice"] = serde_json::Value::String(tool_choice.clone());
        }
    }

    // 从 extra 配置读取参数
    // response_format
    if let Some(response_format) = config.extra.get("response_format")
        && response_format == "json_object"
    {
        request["response_format"] = serde_json::json!({ "type": "json_object" });
    }

    // seed
    if let Some(seed) = config.extra.get("seed")
        && let Ok(n) = seed.parse::<u64>()
    {
        request["seed"] = serde_json::json!(n);
    }

    // frequency_penalty / presence_penalty / top_p
    if let Some(val) = config.extra.get("frequency_penalty")
        && let Ok(n) = val.parse::<f64>()
    {
        request["frequency_penalty"] = serde_json::json!(n);
    }
    if let Some(val) = config.extra.get("presence_penalty")
        && let Ok(n) = val.parse::<f64>()
    {
        request["presence_penalty"] = serde_json::json!(n);
    }
    if let Some(val) = config.extra.get("top_p")
        && let Ok(n) = val.parse::<f64>()
    {
        request["top_p"] = serde_json::json!(n);
    }

    request
}

/// 构建 OpenAI 兼容的 headers
pub fn build_openai_headers(api_key: &str) -> reqwest::header::HeaderMap {
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(
        reqwest::header::CONTENT_TYPE,
        "application/json".parse().unwrap(),
    );
    headers.insert(
        reqwest::header::AUTHORIZATION,
        format!("Bearer {}", api_key).parse().unwrap(),
    );
    headers.insert(
        reqwest::header::USER_AGENT,
        "visp/0.1.0".parse().unwrap(),
    );
    headers
}

/// 将 visp-core Message 转换为 OpenAI Chat API 消息格式
///
/// 规则：
/// - System 角色使用 `role: "system"`
/// - Tool 角色使用 `role: "tool"` + `tool_call_id`
/// - User 消息 content 为字符串
/// - Assistant 消息可包含 content 和 tool_calls
pub fn build_openai_messages(messages: &[Message]) -> Vec<serde_json::Value> {
    let mut result: Vec<serde_json::Value> = Vec::new();

    for msg in messages {
        match msg.role {
            Role::System => {
                result.push(serde_json::json!({
                    "role": "system",
                    "content": msg.content,
                }));
            }
            Role::User => {
                result.push(serde_json::json!({
                    "role": "user",
                    "content": msg.content,
                }));
            }
            Role::Assistant => {
                let mut assistant_msg = serde_json::json!({
                    "role": "assistant",
                    "content": msg.content,
                });

                // 添加 tool_calls（如果有）
                if let Some(ref calls) = msg.tool_calls {
                    let tool_calls: Vec<serde_json::Value> = calls
                        .iter()
                        .map(|tc| {
                            serde_json::json!({
                                "id": tc.id,
                                "type": "function",
                                "function": {
                                    "name": tc.name,
                                    "arguments": tc.arguments,
                                }
                            })
                        })
                        .collect();
                    assistant_msg["tool_calls"] = serde_json::Value::Array(tool_calls);
                }

                result.push(assistant_msg);
            }
            Role::Tool => {
                result.push(serde_json::json!({
                    "role": "tool",
                    "tool_call_id": msg.tool_call_id.as_deref().unwrap_or(""),
                    "content": msg.content,
                }));
            }
        }
    }

    result
}

/// OpenAI SSE 流解析的内部事件
#[derive(Debug)]
pub(crate) enum OpenAiStreamEvent {
    /// 文本增量
    TextDelta(String),
    /// 工具调用开始（包含 id 和 name）
    ToolCallStart {
        index: usize,
        id: String,
        name: String,
    },
    /// 工具调用参数增量
    ToolCallDelta {
        index: usize,
        arguments: String,
    },
    /// 内容块结束（index, finish_reason）
    Stop {
        _index: usize,
        _finish_reason: Option<String>,
    },
    /// token 用量
    Usage {
        input_tokens: u32,
        output_tokens: u32,
    },
    /// 跳过（忽略事件）
    Skip,
}

/// 解析单行 OpenAI SSE data（不含 `data: ` 前缀）
///
/// OpenAI SSE 格式：
/// ```text
/// data: {"id":"...","object":"chat.completion.chunk","choices":[{"delta":{"content":"Hello"},"index":0}]}
/// data: {"id":"...","object":"chat.completion.chunk","choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_1","type":"function","function":{"name":"read_file","arguments":""}}]},"index":0}]}
/// data: [DONE]
/// ```
pub(crate) fn parse_openai_sse_data(data: &str) -> Result<OpenAiStreamEvent, LlmError> {
    if data == "[DONE]" || data.trim().is_empty() {
        return Ok(OpenAiStreamEvent::Skip);
    }

    let v: serde_json::Value = serde_json::from_str(data)
        .map_err(|e| LlmError::Stream(format!("parse openai data: {e}")))?;

    // 检查 usage
    if let Some(usage) = v.get("usage") {
        let input_tokens = usage["prompt_tokens"].as_u64().unwrap_or(0) as u32;
        let output_tokens = usage["completion_tokens"].as_u64().unwrap_or(0) as u32;
        if input_tokens > 0 || output_tokens > 0 {
            return Ok(OpenAiStreamEvent::Usage {
                input_tokens,
                output_tokens,
            });
        }
    }

    // 解析 choices
    let choices = match v.get("choices").and_then(|c| c.as_array()) {
        Some(arr) if !arr.is_empty() => arr,
        _ => return Ok(OpenAiStreamEvent::Skip),
    };

    let choice = &choices[0];
    let delta = &choice["delta"];
    let finish_reason = choice["finish_reason"].as_str().map(|s| s.to_string());
    let index = choice["index"].as_u64().unwrap_or(0) as usize;

    // 检查文本 delta
    if let Some(content) = delta.get("content").and_then(|c| c.as_str())
        && !content.is_empty()
    {
        return Ok(OpenAiStreamEvent::TextDelta(content.to_string()));
    }

    // 检查 tool_calls delta
    if let Some(tool_calls) = delta.get("tool_calls").and_then(|t| t.as_array()) {
        for tc in tool_calls {
            let tc_index = tc["index"].as_u64().unwrap_or(0) as usize;
            let func = &tc["function"];

            // 有 id 表示新的工具调用开始
            if let Some(id) = tc.get("id").and_then(|i| i.as_str())
                && !id.is_empty()
            {
                let name = func["name"].as_str().unwrap_or("").to_string();
                // arguments 在首个 delta 中总是空的，后续通过 ToolCallDelta 累积
                let _unused = func["arguments"].as_str();
                return Ok(OpenAiStreamEvent::ToolCallStart {
                    index: tc_index,
                    id: id.to_string(),
                    name,
                });
            }

            // 参数增量
            if let Some(arguments) = func.get("arguments").and_then(|a| a.as_str())
                && !arguments.is_empty()
            {
                return Ok(OpenAiStreamEvent::ToolCallDelta {
                    index: tc_index,
                    arguments: arguments.to_string(),
                });
            }
        }
    }

    // 检查 finish_reason
    if let Some(reason) = finish_reason {
        return Ok(OpenAiStreamEvent::Stop {
            _index: index,
            _finish_reason: Some(reason),
        });
    }

    Ok(OpenAiStreamEvent::Skip)
}

/// 将 OpenAI SSE 字节流转换为 ChatEvent 流
///
/// OpenAI 的 SSE 格式简单：
/// - 每条消息以 `data: ` 开头，空行分隔
/// - 流结束标记为 `data: [DONE]`
/// - 每个 data 行可能包含 usage、choices 中的 text delta 或 tool calls delta
fn byte_stream_to_chat_events(
    byte_stream: impl Stream<Item = Result<bytes::Bytes, reqwest::Error>> + Send + 'static,
) -> Pin<Box<dyn Stream<Item = Result<ChatEvent, LlmError>> + Send>> {
    struct StreamState {
        stream: Pin<Box<dyn Stream<Item = Result<bytes::Bytes, reqwest::Error>> + Send>>,
        buf: String,
        /// 累积中的工具调用: index -> (id, name, arguments)
        tool_acc: HashMap<usize, (String, String, String)>,
        /// 待发射的工具调用列表: (id, name, arguments)
        pending_tool_calls: Vec<(String, String, String)>,
        input_tokens: u32,
        output_tokens: u32,
        /// 标记流是否已结束
        stream_ended: bool,
        /// 是否已发射 UsageInfo
        usage_emitted: bool,
        /// 是否已发射 Done
        done_emitted: bool,
    }

    let state = StreamState {
        stream: Box::pin(byte_stream),
        buf: String::new(),
        tool_acc: HashMap::new(),
        pending_tool_calls: Vec::new(),
        input_tokens: 0,
        output_tokens: 0,
        stream_ended: false,
        usage_emitted: false,
        done_emitted: false,
    };

    let event_stream = stream::unfold(state, |mut state| async move {
        loop {
            // [A] 优先处理缓冲区中完整的 SSE 消息（一次一条）
            if let Some(pos) = state.buf.find("\n\n") {
                let raw = state.buf[..pos].to_string();
                state.buf = state.buf[pos + 2..].to_string();

                for line in raw.lines() {
                    if let Some(data) = line.strip_prefix("data: ") {
                        match parse_openai_sse_data(data) {
                            Ok(OpenAiStreamEvent::TextDelta(text)) => {
                                return Some((
                                    Ok(ChatEvent::TextDelta(text)),
                                    state,
                                ));
                            }
                            Ok(OpenAiStreamEvent::ToolCallStart {
                                index,
                                id,
                                name,
                            }) => {
                                state
                                    .tool_acc
                                    .insert(index, (id, name, String::new()));
                            }
                            Ok(OpenAiStreamEvent::ToolCallDelta {
                                index,
                                arguments,
                            }) => {
                                if let Some(entry) = state.tool_acc.get_mut(&index) {
                                    entry.2.push_str(&arguments);
                                }
                            }
                            Ok(OpenAiStreamEvent::Stop { .. }) => {
                                if !state.tool_acc.is_empty() {
                                    let calls: Vec<(String, String, String)> = state
                                        .tool_acc
                                        .drain()
                                        .map(|(_, (id, name, args))| (id, name, args))
                                        .collect();
                                    state.pending_tool_calls =
                                        calls.into_iter().rev().collect();
                                }
                            }
                            Ok(OpenAiStreamEvent::Usage {
                                input_tokens,
                                output_tokens,
                            }) => {
                                if input_tokens > 0 {
                                    state.input_tokens = input_tokens;
                                }
                                if output_tokens > 0 {
                                    state.output_tokens = output_tokens;
                                }
                            }
                            Ok(OpenAiStreamEvent::Skip) => {}
                            Err(e) => {
                                return Some((Err(e), state));
                            }
                        }
                    } else if line == "data: [DONE]" {
                        state.stream_ended = true;

                        if !state.tool_acc.is_empty() {
                            let calls: Vec<(String, String, String)> = state
                                .tool_acc
                                .drain()
                                .map(|(_, (id, name, args))| (id, name, args))
                                .collect();
                            state.pending_tool_calls =
                                calls.into_iter().rev().collect();
                        }
                    } else if !line.trim().is_empty() {
                        tracing::trace!(?line, "ignored non-data SSE line");
                    }
                }

                // 继续处理下一条消息
                continue;
            }

            // [B] 发射待处理的工具调用
            if let Some((id, name, args)) = state.pending_tool_calls.pop() {
                return Some((
                    Ok(ChatEvent::ToolCall {
                        id,
                        name,
                        arguments: args,
                    }),
                    state,
                ));
            }

            // [C] 处理流结束标记
            if state.stream_ended {
                if !state.usage_emitted {
                    state.usage_emitted = true;
                    return Some((
                        Ok(ChatEvent::UsageInfo {
                            input_tokens: state.input_tokens,
                            output_tokens: state.output_tokens,
                            tool_calls: state.tool_acc.len() as u32,
                        }),
                        state,
                    ));
                }
                if !state.done_emitted {
                    state.done_emitted = true;
                    return Some((Ok(ChatEvent::Done), state));
                }
                return None;
            }

            // [D] 从底层流读取更多数据
            match state.stream.next().await {
                Some(Ok(chunk)) => {
                    state.buf.push_str(&String::from_utf8_lossy(&chunk));
                    // 下一轮循环会处理缓冲区
                }
                Some(Err(e)) => {
                    return Some((
                        Err(LlmError::Network(e.to_string())),
                        state,
                    ));
                }
                None => {
                    // 流自然结束
                    state.stream_ended = true;

                    if !state.tool_acc.is_empty() {
                        let calls: Vec<(String, String, String)> = state
                            .tool_acc
                            .drain()
                            .map(|(_, (id, name, args))| (id, name, args))
                            .collect();
                        state.pending_tool_calls =
                            calls.into_iter().rev().collect();
                    }
                }
            }
        }
    });

    Box::pin(event_stream)
}

/// OpenAI API 提供器
pub struct OpenAiProvider {
    api_key: String,
    api_url: String,
}

impl OpenAiProvider {
    pub fn new(api_key: String) -> Self {
        Self {
            api_key,
            api_url: "https://api.openai.com".to_string(),
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
impl LlmProvider for OpenAiProvider {
    async fn chat_stream(
        &self,
        messages: &[Message],
        tools: &[ToolDefinition],
        config: &LlmConfig,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<ChatEvent, LlmError>> + Send>>, LlmError> {
        let url = format!(
            "{}/v1/chat/completions",
            self.api_url.trim_end_matches('/')
        );
        let body = build_openai_request(messages, tools, config);
        let headers = build_openai_headers(&self.api_key);

        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(300))
            .build()
            .map_err(|e| LlmError::Network(e.to_string()))?;
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

/// 从 HTTP headers 中解析 Retry-After 值
fn parse_retry_after(headers: &reqwest::header::HeaderMap) -> Option<u64> {
    headers
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<u64>().ok())
}

#[cfg(test)]
mod tests {
    use super::*;
    use visp_core::message::ToolCallRequest;

    // --- OpenAiProvider 构造测试 ---

    #[test]
    fn test_provider_with_base_url() {
        let provider =
            OpenAiProvider::with_base_url("test-key".into(), "https://custom.openai.com".into());
        assert_eq!(provider.api_url, "https://custom.openai.com");
    }

    #[test]
    fn test_provider_default_url() {
        let provider = OpenAiProvider::new("test-key".into());
        assert_eq!(provider.api_url, "https://api.openai.com");
    }

    // --- build_openai_headers 测试 ---

    #[test]
    fn test_build_headers() {
        let headers = build_openai_headers("sk-test123");
        assert_eq!(
            headers.get(reqwest::header::AUTHORIZATION).unwrap(),
            "Bearer sk-test123"
        );
        assert_eq!(
            headers.get(reqwest::header::CONTENT_TYPE).unwrap(),
            "application/json"
        );
        assert_eq!(
            headers.get(reqwest::header::USER_AGENT).unwrap(),
            "visp/0.1.0"
        );
    }

    // --- build_openai_messages 测试 ---

    #[test]
    fn test_build_messages_simple() {
        let msgs = vec![
            Message::system("You are a helpful assistant."),
            Message::user("Hello!"),
        ];
        let result = build_openai_messages(&msgs);
        assert_eq!(result.len(), 2);
        assert_eq!(result[0]["role"], "system");
        assert_eq!(result[0]["content"], "You are a helpful assistant.");
        assert_eq!(result[1]["role"], "user");
        assert_eq!(result[1]["content"], "Hello!");
    }

    #[test]
    fn test_build_messages_with_tool_result() {
        let msgs = vec![
            Message::user("Read the file."),
            Message {
                role: Role::Assistant,
                content: "".into(),
                tool_calls: Some(vec![ToolCallRequest {
                    id: "call_1".into(),
                    name: "read_file".into(),
                    arguments: r#"{"path":"test.txt"}"#.into(),
                }]),
                tool_call_id: None,
                extra_blocks: None,
                skip_context: false,
                estimated_tokens: 0,
            },
            Message::tool("file content", "call_1"),
        ];
        let result = build_openai_messages(&msgs);
        assert_eq!(result.len(), 3);
        assert_eq!(result[1]["role"], "assistant");
        assert_eq!(result[2]["role"], "tool");
        assert_eq!(result[2]["tool_call_id"], "call_1");
        assert_eq!(result[2]["content"], "file content");
    }

    // --- build_openai_request 测试 ---

    #[test]
    fn test_build_request_basic() {
        let msgs = vec![Message::user("Hi")];
        let config = LlmConfig {
            model: "gpt-4o".into(),
            temperature: 0.5,
            max_tokens: 100,
            ..Default::default()
        };
        let req = build_openai_request(&msgs, &[], &config);
        assert_eq!(req["model"], "gpt-4o");
        assert_eq!(req["temperature"], 0.5);
        assert_eq!(req["max_tokens"], 100);
        assert!(req["stream"].as_bool().unwrap());
        assert_eq!(req["messages"][0]["content"], "Hi");
    }

    #[test]
    fn test_build_request_with_tools() {
        let msgs = vec![Message::user("List files")];
        let tools = vec![ToolDefinition {
            name: "list_files".into(),
            description: "List files in directory".into(),
            category: "files".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string"}
                }
            }),
        }];
        let config = LlmConfig::default();
        let req = build_openai_request(&msgs, &tools, &config);
        assert!(req["tools"].is_array());
        assert_eq!(req["tools"][0]["type"], "function");
        assert_eq!(req["tools"][0]["function"]["name"], "list_files");
    }

    #[test]
    fn test_build_request_tool_choice_string() {
        let msgs = vec![Message::user("List files")];
        let tools = vec![ToolDefinition {
            name: "list_files".into(),
            description: "List files in directory".into(),
            category: "files".into(),
            parameters: serde_json::json!({ "type": "object" }),
        }];
        let mut config = LlmConfig::default();
        config
            .extra
            .insert("tool_choice".into(), "required".into());
        let req = build_openai_request(&msgs, &tools, &config);
        assert_eq!(req["tool_choice"], "required");
    }

    #[test]
    fn test_build_request_tool_choice_json() {
        let msgs = vec![Message::user("List files")];
        let tools = vec![ToolDefinition {
            name: "list_files".into(),
            description: "List files in directory".into(),
            category: "files".into(),
            parameters: serde_json::json!({ "type": "object" }),
        }];
        let mut config = LlmConfig::default();
        config.extra.insert(
            "tool_choice".into(),
            r#"{"type":"function","function":{"name":"list_files"}}"#.into(),
        );
        let req = build_openai_request(&msgs, &tools, &config);
        assert_eq!(req["tool_choice"]["type"], "function");
        assert_eq!(req["tool_choice"]["function"]["name"], "list_files");
    }

    #[test]
    fn test_build_request_tool_choice_auto() {
        let msgs = vec![Message::user("Hi")];
        let tools = vec![];
        let mut config = LlmConfig::default();
        config
            .extra
            .insert("tool_choice".into(), "auto".into());
        let req = build_openai_request(&msgs, &tools, &config);
        // 即使没有 tools，tool_choice 也应透传
        assert_eq!(req["tool_choice"], "auto");
    }

    // --- parse_openai_sse_data 测试 ---

    #[test]
    fn test_parse_text_delta() {
        let data = r#"{"id":"chatcmpl-123","object":"chat.completion.chunk","choices":[{"index":0,"delta":{"content":"Hello"},"finish_reason":null}]}"#;
        let result = parse_openai_sse_data(data).unwrap();
        match result {
            OpenAiStreamEvent::TextDelta(t) => assert_eq!(t, "Hello"),
            _ => panic!("expected TextDelta, got {:?}", result),
        }
    }

    #[test]
    fn test_parse_tool_call_start() {
        let data = r#"{"id":"chatcmpl-123","object":"chat.completion.chunk","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_abc","type":"function","function":{"name":"read_file","arguments":""}}]},"finish_reason":null}]}"#;
        let result = parse_openai_sse_data(data).unwrap();
        match result {
            OpenAiStreamEvent::ToolCallStart { id, name, .. } => {
                assert_eq!(id, "call_abc");
                assert_eq!(name, "read_file");
            }
            _ => panic!("expected ToolCallStart, got {:?}", result),
        }
    }

    #[test]
    fn test_parse_tool_call_delta() {
        let data = r#"{"id":"chatcmpl-123","object":"chat.completion.chunk","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"path\":\"te"}}]},"finish_reason":null}]}"#;
        let result = parse_openai_sse_data(data).unwrap();
        match result {
            OpenAiStreamEvent::ToolCallDelta { arguments, .. } => {
                assert_eq!(arguments, "{\"path\":\"te");
            }
            _ => panic!("expected ToolCallDelta, got {:?}", result),
        }
    }

    #[test]
    fn test_parse_finish_stop() {
        let data = r#"{"id":"chatcmpl-123","object":"chat.completion.chunk","choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}"#;
        let result = parse_openai_sse_data(data).unwrap();
        match result {
            OpenAiStreamEvent::Stop {
                _finish_reason: Some(r),
                ..
            } => assert_eq!(r, "stop"),
            _ => panic!("expected Stop, got {:?}", result),
        }
    }

    #[test]
    fn test_parse_finish_tool_calls() {
        let data = r#"{"id":"chatcmpl-123","object":"chat.completion.chunk","choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}"#;
        let result = parse_openai_sse_data(data).unwrap();
        match result {
            OpenAiStreamEvent::Stop {
                _finish_reason: Some(r),
                ..
            } => assert_eq!(r, "tool_calls"),
            _ => panic!("expected Stop, got {:?}", result),
        }
    }

    #[test]
    fn test_parse_done_marker() {
        let result = parse_openai_sse_data("[DONE]").unwrap();
        assert!(matches!(result, OpenAiStreamEvent::Skip));
    }

    #[test]
    fn test_parse_usage() {
        let data = r#"{"id":"chatcmpl-123","object":"chat.completion.chunk","choices":[],"usage":{"prompt_tokens":10,"completion_tokens":20,"total_tokens":30}}"#;
        let result = parse_openai_sse_data(data).unwrap();
        match result {
            OpenAiStreamEvent::Usage {
                input_tokens,
                output_tokens,
            } => {
                assert_eq!(input_tokens, 10);
                assert_eq!(output_tokens, 20);
            }
            _ => panic!("expected Usage, got {:?}", result),
        }
    }

    // --- parse_retry_after 测试 ---

    #[test]
    fn test_parse_retry_after_valid() {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            reqwest::header::RETRY_AFTER,
            "30".parse().unwrap(),
        );
        assert_eq!(parse_retry_after(&headers), Some(30));
    }

    #[test]
    fn test_parse_retry_after_missing() {
        let headers = reqwest::header::HeaderMap::new();
        assert_eq!(parse_retry_after(&headers), None);
    }

    // --- byte_stream_to_chat_events 测试 ---

    /// 构建单条 SSE data 行（自动追加 \n\n）
    fn sse_line(data: &str) -> String {
        format!("data: {}\n\n", data)
    }

    /// 构建 OpenAI SSE 数据行，自动 JSON 编码
    fn make_sse(val: &serde_json::Value) -> String {
        sse_line(&val.to_string())
    }

    /// OpenAI chunk（没有 tool_calls 时）
    fn make_text_chunk(content: &str) -> serde_json::Value {
        serde_json::json!({
            "id": "chatcmpl",
            "object": "chat.completion.chunk",
            "choices": [{
                "index": 0,
                "delta": { "content": content },
                "finish_reason": null
            }]
        })
    }

    /// OpenAI chunk 带 finish_reason
    fn make_stop_chunk(finish_reason: &str) -> serde_json::Value {
        serde_json::json!({
            "id": "chatcmpl",
            "object": "chat.completion.chunk",
            "choices": [{
                "index": 0,
                "delta": {},
                "finish_reason": finish_reason
            }]
        })
    }

    /// 收集 ChatEvent 流到 Vec
    async fn collect_events(
        chunks: Vec<String>,
    ) -> Vec<ChatEvent> {
        let byte_stream = futures::stream::iter(
            chunks.into_iter().map(|s| Ok(bytes::Bytes::from(s))),
        );
        let event_stream = byte_stream_to_chat_events(byte_stream);
        event_stream
            .filter_map(|e| futures::future::ready(e.ok()))
            .collect()
            .await
    }

    #[tokio::test]
    async fn test_byte_stream_single_text_then_done() {
        let sse = format!(
            "{}{}",
            make_sse(&make_text_chunk("Hello")),
            sse_line("[DONE]"),
        );
        let events = collect_events(vec![sse]).await;

        assert_eq!(events.len(), 3, "expect TextDelta + UsageInfo + Done");
        assert!(matches!(&events[0], ChatEvent::TextDelta(t) if t == "Hello"));
        assert!(matches!(&events[1], ChatEvent::UsageInfo { .. }));
        assert!(matches!(&events[2], ChatEvent::Done));
    }

    #[tokio::test]
    async fn test_byte_stream_multiple_text_deltas() {
        let sse = format!(
            "{}{}{}",
            make_sse(&make_text_chunk("Hello")),
            make_sse(&make_text_chunk(" World")),
            sse_line("[DONE]"),
        );
        let events = collect_events(vec![sse]).await;

        assert_eq!(events.len(), 4, "expect TextDelta x2 + UsageInfo + Done");
        assert!(matches!(&events[0], ChatEvent::TextDelta(t) if t == "Hello"));
        assert!(matches!(&events[1], ChatEvent::TextDelta(t) if t == " World"));
        assert!(matches!(&events[2], ChatEvent::UsageInfo { .. }));
        assert!(matches!(&events[3], ChatEvent::Done));
    }

    #[tokio::test]
    async fn test_byte_stream_tool_call() {
        // 手动构建 JSON 避免 r# 在 json! 宏中的解析问题
        let tool_start = serde_json::json!({
            "id": "chatcmpl",
            "object": "chat.completion.chunk",
            "choices": [{
                "index": 0,
                "delta": {
                    "tool_calls": [{
                        "index": 0,
                        "id": "call_abc",
                        "type": "function",
                        "function": {
                            "name": "read_file",
                            "arguments": ""
                        }
                    }]
                },
                "finish_reason": null
            }]
        });
        let tool_arg = serde_json::json!({
            "id": "chatcmpl",
            "object": "chat.completion.chunk",
            "choices": [{
                "index": 0,
                "delta": {
                    "tool_calls": [{
                        "index": 0,
                        "function": { "arguments": "{\"path\":\"test.txt\"}" }
                    }]
                },
                "finish_reason": null
            }]
        });

        let sse = format!(
            "{}{}{}{}",
            make_sse(&tool_start),
            make_sse(&tool_arg),
            make_sse(&make_stop_chunk("tool_calls")),
            sse_line("[DONE]"),
        );
        let events = collect_events(vec![sse]).await;

        assert_eq!(events.len(), 3, "expect ToolCall + UsageInfo + Done");
        match &events[0] {
            ChatEvent::ToolCall { id, name, arguments } => {
                assert_eq!(id, "call_abc");
                assert_eq!(name, "read_file");
                assert!(
                    arguments.contains("path"),
                    "arguments should contain 'path', got: {arguments}",
                );
            }
            _ => panic!("expected ToolCall, got {:?}", events[0]),
        }
        assert!(matches!(&events[1], ChatEvent::UsageInfo { .. }));
        assert!(matches!(&events[2], ChatEvent::Done));
    }

    #[tokio::test]
    async fn test_byte_stream_natural_end() {
        // 没有 [DONE] 标记，流自然结束
        let sse = make_sse(&make_text_chunk("Hello"));
        let events = collect_events(vec![sse]).await;

        assert_eq!(events.len(), 3, "expect TextDelta + UsageInfo + Done");
        assert!(matches!(&events[0], ChatEvent::TextDelta(t) if t == "Hello"));
        assert!(matches!(&events[1], ChatEvent::UsageInfo { .. }));
        assert!(matches!(&events[2], ChatEvent::Done));
    }

    #[tokio::test]
    async fn test_byte_stream_chunk_boundary() {
        // SSE 消息正文被拆在两个 HTTP chunk 中，`"Hel"` 和 `lo"}` 分属不同 chunk
        let part1 = "data: {\"id\":\"1\",\"object\":\"chat.completion.chunk\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"Hel";
        let part2 = "lo\"},\"finish_reason\":null}]}\n\ndata: [DONE]\n\n".to_string();
        let events = collect_events(vec![part1.to_string(), part2]).await;

        assert_eq!(events.len(), 3, "expect TextDelta + UsageInfo + Done");
        assert!(matches!(&events[0], ChatEvent::TextDelta(t) if t == "Hello"));
        assert!(matches!(&events[1], ChatEvent::UsageInfo { .. }));
        assert!(matches!(&events[2], ChatEvent::Done));
    }

    #[tokio::test]
    async fn test_byte_stream_empty_stream() {
        let events = collect_events(vec![]).await;

        assert_eq!(events.len(), 2, "expect UsageInfo + Done");
        assert!(matches!(&events[0], ChatEvent::UsageInfo { .. }));
        assert!(matches!(&events[1], ChatEvent::Done));
    }
}
