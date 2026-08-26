use async_trait::async_trait;
use futures::stream::{self, Stream, StreamExt};
use std::collections::{HashMap, VecDeque};
use std::pin::Pin;
use tracing::Instrument;
use tracing::field;
use visp_config::LlmConfig;
use visp_core::error::LlmError;
use visp_core::message::{Message, MessageType, Role, ToolDefinition};
use visp_core::provider::{ChatEvent, LlmProvider};

use crate::image_util;
use crate::util::{build_client, parse_retry_after};

/// 检查 tool_call arguments 是否为合法 JSON（空字符串视为合法）。
/// 用于识别 max_tokens 截断导致的不完整 JSON，避免畸形 tool_call 污染会话历史。
fn is_valid_json_args(s: &str) -> bool {
    if s.is_empty() {
        return true;
    }
    serde_json::from_str::<serde_json::Value>(s).is_ok()
}

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
        "stream_options": {"include_usage": true},
    });

    // 添加工具定义（use_tool = false 时不携带）
    if config.use_tool && !tools.is_empty() {
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
            match serde_json::from_str::<serde_json::Value>(tool_choice) {
                Ok(val) => {
                    request["tool_choice"] = val;
                }
                Err(e) => {
                    tracing::warn!("invalid tool_choice JSON {:?}: {e}", tool_choice);
                }
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
    if let Some(seed) = config.extra.get("seed") {
        match seed.parse::<u64>() {
            Ok(n) => {
                request["seed"] = serde_json::json!(n);
            }
            Err(e) => {
                tracing::warn!("invalid seed value {:?}: {e}", seed);
            }
        }
    }

    // frequency_penalty / presence_penalty / top_p
    if let Some(val) = config.extra.get("frequency_penalty") {
        match val.parse::<f64>() {
            Ok(n) => {
                request["frequency_penalty"] = serde_json::json!(n);
            }
            Err(e) => {
                tracing::warn!("invalid frequency_penalty value {:?}: {e}", val);
            }
        }
    }
    if let Some(val) = config.extra.get("presence_penalty") {
        match val.parse::<f64>() {
            Ok(n) => {
                request["presence_penalty"] = serde_json::json!(n);
            }
            Err(e) => {
                tracing::warn!("invalid presence_penalty value {:?}: {e}", val);
            }
        }
    }
    if let Some(val) = config.extra.get("top_p") {
        match val.parse::<f64>() {
            Ok(n) => {
                request["top_p"] = serde_json::json!(n);
            }
            Err(e) => {
                tracing::warn!("invalid top_p value {:?}: {e}", val);
            }
        }
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
    headers.insert(reqwest::header::USER_AGENT, "visp/0.1.0".parse().unwrap());
    headers
}

/// 将 visp-core Message 转换为 OpenAI Chat API 消息格式
///
/// 规则：
/// - System 角色使用 `role: "system"`
/// - Tool 角色使用 `role: "tool"` + `tool_call_id`
/// - User 消息 content 为字符串
/// - Assistant 消息可包含 content、tool_calls 和 extra_blocks 中的扩展字段
///   （如 thinking，部分 OpenAI 兼容模型支持）
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
                if msg.images.is_empty() {
                    result.push(serde_json::json!({
                        "role": "user",
                        "content": msg.content,
                    }));
                } else {
                    // Multimodal: text + images
                    let mut content: Vec<serde_json::Value> = Vec::new();
                    if !msg.content.is_empty() {
                        content.push(serde_json::json!({
                            "type": "text",
                            "text": msg.content,
                        }));
                    }
                    for img in &msg.images {
                        let data_uri = format!("data:{};base64,{}", img.mime_type, img.base64);
                        content.push(serde_json::json!({
                            "type": "image_url",
                            "image_url": { "url": data_uri },
                        }));
                    }
                    result.push(serde_json::json!({
                        "role": "user",
                        "content": content,
                    }));
                }
            }
            Role::Assistant => {
                let content: serde_json::Value = if msg.content.is_empty()
                    && msg.tool_calls.as_ref().is_some_and(|c| !c.is_empty())
                {
                    // OpenAI 规范：纯 tool_calls 消息 content 应为 null
                    serde_json::Value::Null
                } else if msg.kind == MessageType::Thinking {
                    // Thinking-only message: reasoning goes to reasoning_content,
                    // but content must be a non-null string (DeepSeek API requires
                    // content or tool_calls to be set).
                    serde_json::Value::String(String::new())
                } else {
                    serde_json::Value::String(msg.content.clone())
                };
                let mut assistant_msg = serde_json::json!({
                    "role": "assistant",
                    "content": content,
                });

                // 添加 tool_calls（如果有且非空 — OpenAI 拒绝空数组）
                if let Some(ref calls) = msg.tool_calls
                    && !calls.is_empty()
                {
                    let tool_calls: Vec<serde_json::Value> = calls
                        .iter()
                        .map(|tc| {
                            let args = if is_valid_json_args(&tc.arguments) {
                                tc.arguments.clone()
                            } else {
                                "{}".to_string()
                            };
                            serde_json::json!({
                                "id": tc.id,
                                "type": "function",
                                "function": {
                                    "name": tc.name,
                                    "arguments": args,
                                }
                            })
                        })
                        .collect();
                    assistant_msg["tool_calls"] = serde_json::Value::Array(tool_calls);
                }

                // 合并 extra_blocks（如 thinking）到 assistant message 顶层字段
                // 部分 OpenAI 兼容模型支持这些扩展字段（如 DeepSeek 的 reasoning_content）
                // 跳过 OpenAI 保留字段，避免意外覆盖
                const RESERVED_FIELDS: &[&str] = &["type", "role", "content", "tool_calls"];
                if let Some(ref blocks) = msg.extra_blocks {
                    for block in blocks {
                        if let Some(obj) = block.as_object() {
                            // Check if this is a thinking block and extract the reasoning text
                            let block_type = obj.get("type").and_then(|v| v.as_str());
                            let thinking_text = obj.get("thinking").and_then(|v| v.as_str());
                            if block_type == Some("thinking") {
                                if let Some(text) = thinking_text {
                                    assistant_msg["reasoning_content"] =
                                        serde_json::Value::String(text.to_string());
                                }
                                continue;
                            }
                            // For non-thinking blocks, merge keys as before
                            for (key, val) in obj {
                                if !RESERVED_FIELDS.contains(&key.as_str()) {
                                    assistant_msg[key] = val.clone();
                                }
                            }
                        }
                    }
                }

                result.push(assistant_msg);
            }
            Role::Tool => {
                let tool_call_id = msg.tool_call_id.as_deref().unwrap_or_else(|| {
                    tracing::warn!("Tool message without tool_call_id");
                    ""
                });
                result.push(serde_json::json!({
                    "role": "tool",
                    "tool_call_id": tool_call_id,
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
    /// 推理/思考内容增量（部分 OpenAI 兼容模型如 DeepSeek 支持）
    ReasoningDelta(String),
    /// 图片内容块（来自 LLM 输出的 image_url）
    ImageBlock {
        /// base64 数据（不含 `data:` 前缀），远程 URL 时为 None
        data: Option<String>,
        /// MIME 类型（远程 URL 时为空字符串）
        mime_type: String,
        /// 远程 URL（base64 时为 None）
        remote_url: Option<String>,
    },
    /// 工具调用开始（包含 id 和 name）
    ToolCallStart {
        index: usize,
        id: String,
        name: String,
    },
    /// 工具调用参数增量
    ToolCallDelta { index: usize, arguments: String },
    /// 内容块结束，携带 finish_reason
    /// fields 保留用于调试，match 时用 `..` 忽略
    #[allow(dead_code)]
    Finish {
        index: usize,
        reason: Option<String>,
    },
    /// token 用量
    Usage {
        input_tokens: u32,
        output_tokens: u32,
        cache_creation_input_tokens: u32,
        cache_read_input_tokens: u32,
    },
    /// 流结束标记 `[DONE]`
    StreamEnd,
    /// 跳过（忽略事件）
    Skip,
}

/// 解析单行 OpenAI SSE data（不含 `data: ` 前缀）
///
/// 返回事件列表：常规情况下单元素；`delta.content` 为数组时（OpenAI
/// 图片输出格式）可能返回多个事件（文本 + 图片）。
///
/// OpenAI SSE 格式：
/// ```text
/// data: {"id":"...","object":"chat.completion.chunk","choices":[{"delta":{"content":"Hello"},"index":0}]}
/// data: {"id":"...","object":"chat.completion.chunk","choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_1","type":"function","function":{"name":"read_file","arguments":""}}]},"index":0}]}
/// data: [DONE]
/// ```
pub(crate) fn parse_openai_sse_data(data: &str) -> Result<Vec<OpenAiStreamEvent>, LlmError> {
    if data == "[DONE]" {
        return Ok(vec![OpenAiStreamEvent::StreamEnd]);
    }
    if data.trim().is_empty() {
        return Ok(vec![OpenAiStreamEvent::Skip]);
    }

    let v: serde_json::Value = serde_json::from_str(data)
        .map_err(|e| LlmError::Stream(format!("parse openai data: {e}")))?;

    // 检查 usage（跳过 "usage": null，部分 provider 在非最终 chunk 中发送 null）
    if let Some(usage) = v.get("usage").filter(|u| !u.is_null()) {
        let input_tokens = usage["prompt_tokens"].as_u64().unwrap_or(0) as u32;
        let output_tokens = usage["completion_tokens"].as_u64().unwrap_or(0) as u32;
        // Try Anthropic-style cache fields first (some OpenAI-compatible providers use them)
        let cache_creation = usage
            .get("cache_creation_input_tokens")
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as u32;
        // OpenAI standard: usage.prompt_tokens_details.cached_tokens
        let cache_read = usage
            .get("prompt_tokens_details")
            .and_then(|d| d.get("cached_tokens"))
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as u32;
        // Also try Anthropic-style field as fallback
        let cache_read = if cache_read == 0 {
            usage
                .get("cache_read_input_tokens")
                .and_then(|v| v.as_u64())
                .unwrap_or(0) as u32
        } else {
            cache_read
        };
        if input_tokens > 0 || output_tokens > 0 {
            return Ok(vec![OpenAiStreamEvent::Usage {
                input_tokens,
                output_tokens,
                cache_creation_input_tokens: cache_creation,
                cache_read_input_tokens: cache_read,
            }]);
        }
    }

    // 解析 choices
    let choices = match v.get("choices").and_then(|c| c.as_array()) {
        Some(arr) if !arr.is_empty() => arr,
        _ => return Ok(vec![OpenAiStreamEvent::Skip]),
    };

    let choice = &choices[0];
    let delta = &choice["delta"];
    let finish_reason = choice["finish_reason"].as_str().map(|s| s.to_string());
    let index = choice["index"].as_u64().unwrap_or(0) as usize;

    // 检查文本/图片 delta（content 可以是字符串或数组）
    if let Some(content) = delta.get("content") {
        // 字符串形式（常规流式文本）
        if let Some(text) = content.as_str()
            && !text.is_empty()
        {
            return Ok(vec![OpenAiStreamEvent::TextDelta(text.to_string())]);
        }

        // 数组形式（OpenAI 图片输出：text + image_url 块）
        if let Some(items) = content.as_array() {
            let mut image_events: Vec<OpenAiStreamEvent> = Vec::new();
            let mut text_parts: Vec<String> = Vec::new();
            for item in items {
                match item.get("type").and_then(|t| t.as_str()) {
                    Some("text") => {
                        if let Some(t) = item.get("text").and_then(|t| t.as_str()) {
                            text_parts.push(t.to_string());
                        }
                    }
                    Some("image_url") => {
                        let url = item
                            .get("image_url")
                            .and_then(|u| u.get("url"))
                            .and_then(|u| u.as_str())
                            .unwrap_or("");
                        if url.starts_with("data:") {
                            // data URI：提取 mime + base64，由调用方保存到磁盘
                            if let Some((mime, base64_data)) = image_util::parse_data_uri(url) {
                                image_events.push(OpenAiStreamEvent::ImageBlock {
                                    data: Some(base64_data),
                                    mime_type: mime,
                                    remote_url: None,
                                });
                            }
                        } else if url.starts_with("http://") || url.starts_with("https://") {
                            // 远程 URL：不下载，透传给上层
                            image_events.push(OpenAiStreamEvent::ImageBlock {
                                data: None,
                                mime_type: String::new(),
                                remote_url: Some(url.to_string()),
                            });
                        }
                    }
                    _ => {}
                }
            }
            if !text_parts.is_empty() {
                image_events.insert(0, OpenAiStreamEvent::TextDelta(text_parts.join("")));
            }
            if !image_events.is_empty() {
                return Ok(image_events);
            }
        }
    }

    // 检查 reasoning_content（DeepSeek 等模型发送推理内容）
    if let Some(reasoning) = delta.get("reasoning_content").and_then(|c| c.as_str())
        && !reasoning.is_empty()
    {
        return Ok(vec![OpenAiStreamEvent::ReasoningDelta(
            reasoning.to_string(),
        )]);
    }
    // 部分提供商使用 "reasoning" 字段
    if let Some(reasoning) = delta.get("reasoning").and_then(|c| c.as_str())
        && !reasoning.is_empty()
    {
        return Ok(vec![OpenAiStreamEvent::ReasoningDelta(
            reasoning.to_string(),
        )]);
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
                return Ok(vec![OpenAiStreamEvent::ToolCallStart {
                    index: tc_index,
                    id: id.to_string(),
                    name,
                }]);
            }

            // 参数增量
            if let Some(arguments) = func.get("arguments").and_then(|a| a.as_str())
                && !arguments.is_empty()
            {
                return Ok(vec![OpenAiStreamEvent::ToolCallDelta {
                    index: tc_index,
                    arguments: arguments.to_string(),
                }]);
            }
        }
    }

    // 检查 finish_reason
    if let Some(reason) = finish_reason {
        return Ok(vec![OpenAiStreamEvent::Finish {
            index,
            reason: Some(reason),
        }]);
    }

    Ok(vec![OpenAiStreamEvent::Skip])
}

/// 按字符边界安全截取，最多取 `max_chars` 个字符。
///
/// 用于日志预览，避免在 UTF-8 多字节字符（如中文）中间切片导致 panic。
/// 返回切片，不分配新内存。
fn truncate_for_log(s: &str, max_chars: usize) -> &str {
    match s.char_indices().nth(max_chars) {
        Some((idx, _)) => &s[..idx],
        None => s,
    }
}

/// 将 OpenAI SSE 字节流转换为 ChatEvent 流
///
/// OpenAI 的 SSE 格式简单：
/// - 每条消息以 `data: ` 开头，空行分隔
/// - 流结束标记为 `data: [DONE]`
/// - 每个 data 行可能包含 usage、choices 中的 text delta 或 tool calls delta
#[allow(clippy::too_many_arguments)]
fn byte_stream_to_chat_events(
    byte_stream: impl Stream<Item = Result<bytes::Bytes, reqwest::Error>> + Send + 'static,
    start_time: std::time::Instant,
    span: tracing::Span,
    request_model: String,
    project_path: String,
    langfuse_capture_output: bool,
    langfuse_capture_max_chars: usize,
    langfuse_redact_secrets: bool,
) -> Pin<Box<dyn Stream<Item = Result<ChatEvent, LlmError>> + Send>> {
    struct StreamState {
        stream: Pin<Box<dyn Stream<Item = Result<bytes::Bytes, reqwest::Error>> + Send>>,
        buf: String,
        /// 跨 chunk 的不完整 UTF-8 尾字节缓冲（修复多字节字符被 TCP chunk 切坏的问题）
        pending_bytes: Vec<u8>,
        /// 累积中的工具调用: index -> (id, name, arguments)
        tool_acc: HashMap<usize, (String, String, String)>,
        /// 待发射的工具调用列表: (id, name, arguments)
        pending_tool_calls: Vec<(String, String, String)>,
        /// 同一条 SSE 行产生的多个事件缓存（逐条发射）
        pending_events: VecDeque<OpenAiStreamEvent>,
        /// 图片保存目录（base64 图片落盘的项目路径）
        project_path: String,
        /// 已完成的工具调用数量（用于 UsageInfo）
        tool_call_count: u32,
        /// 累积的推理/思考内容（来自 reasoning_content / reasoning 字段）
        reasoning_text: String,
        input_tokens: u32,
        output_tokens: u32,
        cache_creation_input_tokens: u32,
        cache_read_input_tokens: u32,
        /// 响应模型名称（从 chunk 顶层 model 字段提取）
        model: String,
        /// 请求时的模型名称（用于 response model 为空时的 fallback）
        request_model: String,
        /// 结束原因（从 Finish 事件提取）
        finish_reason: String,
        /// 请求开始时刻（用于计算端到端 latency）
        state_start_time: std::time::Instant,
        /// 标记流是否已结束
        stream_ended: bool,
        /// 是否已发射 UsageInfo
        usage_emitted: bool,
        /// 是否已发射 OutputMetadata
        metadata_emitted: bool,
        /// 是否已发射 Done
        done_emitted: bool,
        /// gen_ai.client.operation span
        span: tracing::Span,
        /// 是否已发射首 token 事件
        first_content_emitted: bool,
        /// Langfuse 输出捕获配置
        langfuse_capture_output: bool,
        langfuse_capture_max_chars: usize,
        langfuse_redact_secrets: bool,
        /// 累积的输出文本（用于 langfuse.observation.output）
        accumulated_output: String,
        /// 本轮是否已发射过 TextDelta（用于判断截断时是否需补提示文本）
        had_text_output: bool,
        /// 待发射的截断提示文本（finish_reason='length' 丢弃畸形 tool_call 且无文本时设置）
        pending_hint: Option<String>,
    }

    /// 将累积的工具调用（tool_acc）转移到 pending_tool_calls。
    /// finish_reason='length' 截断的 tool_call arguments 可能是不完整 JSON，
    /// 转移时校验合法性，丢弃畸形的，避免污染会话历史导致后续请求 400。
    fn flush_tool_acc(state: &mut StreamState) {
        if !state.tool_acc.is_empty() {
            let calls: Vec<(String, String, String)> = state
                .tool_acc
                .drain()
                .filter(|(_, (_, _, args))| is_valid_json_args(args))
                .map(|(_, (id, name, args))| (id, name, args))
                .collect();
            state.tool_call_count = calls.len() as u32;
            state.pending_tool_calls = calls.into_iter().rev().collect();
        }
    }

    /// 标记首 token 已到达（幂等，仅首次有效）
    fn emit_first_token(state: &mut StreamState) {
        if !state.first_content_emitted {
            state.first_content_emitted = true;
            tracing::debug!(target: "gen_ai.client.first_token", "first token received");
        }
    }

    /// 处理单个 OpenAiStreamEvent，返回需要发射的 ChatEvent（仅更新内部状态则返回 None）
    fn handle_stream_event(state: &mut StreamState, event: OpenAiStreamEvent) -> Option<ChatEvent> {
        match event {
            OpenAiStreamEvent::TextDelta(text) => {
                if state.langfuse_capture_output {
                    state.accumulated_output.push_str(&text);
                }
                state.had_text_output = true;
                emit_first_token(state);
                Some(ChatEvent::TextDelta(text))
            }
            OpenAiStreamEvent::ReasoningDelta(text) => {
                state.reasoning_text.push_str(&text);
                // 立即发射 ThinkingBlock 实现流式显示（不等待 text delta）
                let block = serde_json::json!({
                    "type": "thinking",
                    "thinking": state.reasoning_text.clone(),
                });
                emit_first_token(state);
                Some(ChatEvent::ThinkingBlock(block))
            }
            OpenAiStreamEvent::ImageBlock {
                data,
                mime_type,
                remote_url,
            } => {
                emit_first_token(state);
                if let Some(data) = data {
                    // base64 图片：保存到磁盘，失败时发射 ImageError
                    match image_util::save_base64_image(&data, &mime_type, &state.project_path) {
                        Ok(path) => Some(ChatEvent::ImageBlock {
                            path,
                            mime_type,
                            remote_url: None,
                        }),
                        Err(e) => Some(ChatEvent::ImageError {
                            reason: e.to_string(),
                        }),
                    }
                } else {
                    // 远程 URL 图片：不下载，直接透传
                    remote_url.map(|remote_url| ChatEvent::ImageBlock {
                        path: String::new(),
                        mime_type: String::new(),
                        remote_url: Some(remote_url),
                    })
                }
            }
            OpenAiStreamEvent::ToolCallStart { index, id, name } => {
                tracing::debug!(index, %id, %name, "ToolCallStart");
                state.tool_acc.insert(index, (id, name, String::new()));
                None
            }
            OpenAiStreamEvent::ToolCallDelta { index, arguments } => {
                if let Some(entry) = state.tool_acc.get_mut(&index) {
                    entry.2.push_str(&arguments);
                } else {
                    tracing::warn!(
                        index,
                        delta_len = arguments.len(),
                        "ToolCallDelta for unknown tool index (ToolCallStart not received?)"
                    );
                }
                None
            }
            OpenAiStreamEvent::Finish { reason, .. } => {
                if let Some(ref r) = reason {
                    state.finish_reason = r.clone();
                    tracing::debug!(
                        finish_reason = %r,
                        had_text_output = state.had_text_output,
                        reasoning_len = state.reasoning_text.len(),
                        tool_acc_count = state.tool_acc.len(),
                        "OpenAI stream: received finish_reason"
                    );
                    if r == "content_filter" {
                        tracing::warn!("OpenAI response blocked by content filter");
                    }
                    if r == "length" && !state.tool_acc.is_empty() {
                        tracing::warn!(
                            tool_count = state.tool_acc.len(),
                            "finish_reason='length' — tool call arguments likely truncated (max_output_tokens exceeded)"
                        );
                    }
                }
                // 转移 tool_acc（校验丢弃畸形 arguments）
                let had_tool_acc = !state.tool_acc.is_empty();
                flush_tool_acc(state);
                // finish_reason='length' 且丢弃后无 tool_call 且本轮无文本：
                // 补一条提示文本，保证 assistant 消息非空且语义可追踪
                if state.finish_reason == "length"
                    && had_tool_acc
                    && state.pending_tool_calls.is_empty()
                    && !state.had_text_output
                {
                    state.pending_hint = Some(
                        "[tool call truncated due to max_tokens limit, please retry]".to_string(),
                    );
                }
                // 收到 finish_reason 即标记流结束，无需等待底层流关闭
                state.stream_ended = true;
                None
            }
            OpenAiStreamEvent::Usage {
                input_tokens,
                output_tokens,
                cache_creation_input_tokens,
                cache_read_input_tokens,
            } => {
                if input_tokens > 0 {
                    state.input_tokens = input_tokens;
                }
                if output_tokens > 0 {
                    state.output_tokens = output_tokens;
                }
                if cache_creation_input_tokens > 0 {
                    state.cache_creation_input_tokens = cache_creation_input_tokens;
                }
                if cache_read_input_tokens > 0 {
                    state.cache_read_input_tokens = cache_read_input_tokens;
                }
                None
            }
            OpenAiStreamEvent::Skip => None,
            OpenAiStreamEvent::StreamEnd => {
                state.stream_ended = true;
                flush_tool_acc(state);
                None
            }
        }
    }

    let state = StreamState {
        stream: Box::pin(byte_stream),
        buf: String::new(),
        pending_bytes: Vec::new(),
        tool_acc: HashMap::new(),
        pending_tool_calls: Vec::new(),
        pending_events: VecDeque::new(),
        project_path,
        tool_call_count: 0,
        reasoning_text: String::new(),
        input_tokens: 0,
        output_tokens: 0,
        cache_creation_input_tokens: 0,
        cache_read_input_tokens: 0,
        model: String::new(),
        request_model,
        finish_reason: String::new(),
        state_start_time: start_time,
        stream_ended: false,
        usage_emitted: false,
        metadata_emitted: false,
        done_emitted: false,
        span,
        first_content_emitted: false,
        langfuse_capture_output,
        langfuse_capture_max_chars,
        langfuse_redact_secrets,
        accumulated_output: String::new(),
        had_text_output: false,
        pending_hint: None,
    };

    let event_stream = stream::unfold(state, |mut state| {
        let span = state.span.clone();
        async move {
            loop {
                // [A0] 发射缓存的待处理事件（一条 SSE 行可能产生多个事件）
                if let Some(event) = state.pending_events.pop_front() {
                    if let Some(chat_event) = handle_stream_event(&mut state, event) {
                        return Some((Ok(chat_event), state));
                    }
                    continue;
                }

                // [A] 优先处理缓冲区中完整的 SSE 消息（一次一条）
                if let Some(pos) = state.buf.find("\n\n") {
                    let raw = state.buf[..pos].to_string();
                    state.buf = state.buf[pos + 2..].to_string();

                    for line in raw.lines() {
                        if let Some(data) = line.strip_prefix("data: ") {
                            tracing::trace!(
                                target: "visp.sse.openai",
                                data,
                                "raw SSE data line"
                            );
                            // 从 chunk 顶层提取 model 字段
                            if let Ok(v) = serde_json::from_str::<serde_json::Value>(data)
                                && let Some(m) = v.get("model").and_then(|m| m.as_str())
                                && !m.is_empty()
                            {
                                state.model = m.to_string();
                            }
                            // 解析为事件列表；多个事件缓存到 pending_events 逐条发射
                            match parse_openai_sse_data(data) {
                                Ok(events) => {
                                    state.pending_events.extend(events);
                                }
                                Err(e) => {
                                    return Some((Err(e), state));
                                }
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
                    emit_first_token(&mut state);
                    tracing::debug!(
                        %name,
                        args_len = args.len(),
                        args_preview = %truncate_for_log(&args, 200),
                        "emitting ToolCall"
                    );
                    return Some((
                        Ok(ChatEvent::ToolCall {
                            id,
                            name,
                            arguments: args,
                        }),
                        state,
                    ));
                }

                // [B'] 发射截断提示文本（finish_reason='length' 丢弃畸形 tool_call 且无文本时）
                if let Some(hint) = state.pending_hint.take() {
                    emit_first_token(&mut state);
                    return Some((Ok(ChatEvent::TextDelta(hint)), state));
                }

                // [C] 处理流结束标记
                if state.stream_ended {
                    if !state.usage_emitted {
                        state.usage_emitted = true;
                        return Some((
                            Ok(ChatEvent::UsageInfo {
                                input_tokens: state.input_tokens,
                                output_tokens: state.output_tokens,
                                tool_calls: state.tool_call_count,
                                cache_creation_input_tokens: state.cache_creation_input_tokens,
                                cache_read_input_tokens: state.cache_read_input_tokens,
                            }),
                            state,
                        ));
                    }
                    if !state.metadata_emitted {
                        state.metadata_emitted = true;
                        let latency = state.state_start_time.elapsed().as_millis() as u64;
                        let finish_reasons = if state.finish_reason.is_empty() {
                            vec![]
                        } else {
                            vec![state.finish_reason.clone()]
                        };

                        // 在 span 上 record usage / model / finish_reasons / cost
                        // Cast u32 → i64: tracing-opentelemetry's Visit impl only handles
                        // record_i64 (not record_u64), so u32/u64 values would fall through
                        // to record_debug and be exported as String("100") instead of I64(100).
                        state
                            .span
                            .record("gen_ai.usage.input_tokens", state.input_tokens as i64);
                        state
                            .span
                            .record("gen_ai.usage.output_tokens", state.output_tokens as i64);
                        // OpenAI 不写 cache 字段
                        if state.finish_reason == "length" {
                            state.span.record("visp.llm.token_limit_hit", true);
                        }
                        let finish_reasons_str =
                            serde_json::to_string(&finish_reasons).unwrap_or_default();
                        state
                            .span
                            .record("gen_ai.response.finish_reasons", &finish_reasons_str);
                        // Fallback to request model if response didn't include one
                        // (some OpenAI-compatible providers don't include "model" in SSE chunks)
                        let effective_model = if state.model.is_empty() {
                            &state.request_model
                        } else {
                            &state.model
                        };
                        state.span.record("gen_ai.response.model", effective_model);

                        // Langfuse generation capture: record output if enabled
                        let raw_output_len = state.accumulated_output.len();
                        if state.langfuse_capture_output && raw_output_len > 0 {
                            let sanitized = crate::sanitize::format_langfuse_output(
                                &state.accumulated_output,
                                state.langfuse_capture_max_chars,
                                state.langfuse_redact_secrets,
                            );
                            state.span.record("langfuse.observation.output", &sanitized);
                        }

                        return Some((
                            Ok(ChatEvent::OutputMetadata(visp_core::ProviderMetadata {
                                model: effective_model.clone(),
                                finish_reasons,
                                input_tokens: state.input_tokens,
                                output_tokens: state.output_tokens,
                                cache_read_input_tokens: if state.cache_read_input_tokens > 0 {
                                    Some(state.cache_read_input_tokens)
                                } else {
                                    None
                                },
                                cache_creation_input_tokens: if state.cache_creation_input_tokens
                                    > 0
                                {
                                    Some(state.cache_creation_input_tokens)
                                } else {
                                    None
                                },
                                latency_ms: latency,
                            })),
                            state,
                        ));
                    }
                    if !state.done_emitted {
                        state.done_emitted = true;
                        tracing::debug!(
                            target: "gen_ai.client.completed",
                            input_tokens = state.input_tokens,
                            output_tokens = state.output_tokens,
                            model = %state.model,
                            "LLM request completed"
                        );
                        return Some((Ok(ChatEvent::Done), state));
                    }
                    return None;
                }

                // [D] 从底层流读取更多数据
                match state.stream.next().await {
                    Some(Ok(chunk)) => {
                        state.pending_bytes.extend_from_slice(&chunk);
                        // 只解码完整的 UTF-8 部分，不完整的尾字节留到下一个 chunk
                        let safe_end = match std::str::from_utf8(&state.pending_bytes) {
                            Ok(_) => state.pending_bytes.len(),
                            Err(e) => e.valid_up_to(),
                        };
                        if safe_end > 0 {
                            state.buf.push_str(&String::from_utf8_lossy(
                                &state.pending_bytes[..safe_end],
                            ));
                            state.pending_bytes = state.pending_bytes[safe_end..].to_vec();
                        }
                    }
                    Some(Err(e)) => {
                        return Some((Err(LlmError::Network(e.to_string())), state));
                    }
                    None => {
                        tracing::debug!(
                            model = %state.model,
                            finish_reason = %state.finish_reason,
                            had_text_output = state.had_text_output,
                            reasoning_len = state.reasoning_text.len(),
                            tool_acc_count = state.tool_acc.len(),
                            "OpenAI stream: byte stream ended (None)"
                        );
                        // 流自然结束
                        state.stream_ended = true;
                        flush_tool_acc(&mut state);
                    }
                }
            }
        }
        .instrument(span)
    });

    Box::pin(event_stream)
}

/// 检查 base_url 是否已经包含版本路径段（如 /v1, /v3 等）。
/// 如果用户提供了带版本的自定义 base_url（例如火山引擎 Ark 的 /api/plan/v3），
/// 则不重复追加 /v1 前缀。
fn is_versioned_base_url(url: &str) -> bool {
    url.rsplit('/')
        .next()
        .is_some_and(|seg| seg.starts_with('v') && seg[1..].chars().all(|c| c.is_ascii_digit()))
}

/// OpenAI API 提供器
pub struct OpenAiProvider {
    api_key: String,
    api_url: String,
    client: reqwest::Client,
}

impl OpenAiProvider {
    pub fn new(api_key: String) -> Self {
        Self {
            api_key,
            api_url: "https://api.openai.com".to_string(),
            client: build_client(),
        }
    }

    pub fn with_base_url(api_key: String, base_url: String) -> Self {
        Self {
            api_key,
            api_url: base_url,
            client: build_client(),
        }
    }

    /// 调用文生图 API（/images/generations），返回 ImageBlock 事件流。
    ///
    /// 从 messages 中提取最后一条 user 消息作为 prompt，
    /// 发送非流式请求，将返回的图片 URL 包装为 ChatEvent::ImageBlock。
    async fn image_generate(
        &self,
        messages: &[Message],
        config: &LlmConfig,
        cancel: &tokio_util::sync::CancellationToken,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<ChatEvent, LlmError>> + Send>>, LlmError> {
        use visp_core::message::Role;

        // 1. 从 messages 提取最后一条 user 消息作为 prompt
        let prompt: String = messages
            .iter()
            .rev()
            .find(|m| m.role == Role::User)
            .map(|m| m.content.clone())
            .ok_or_else(|| LlmError::Api {
                status: 400,
                message: "No user message found for image generation prompt".to_string(),
            })?;

        // 2. 构建请求体与 URL（OpenAI 兼容 /images/generations）
        let mut body = serde_json::json!({
            "model": config.model,
            "prompt": prompt,
            "response_format": "url",
        });
        // 从 config.extra 透传可选参数
        for key in &["size", "output_format", "watermark"] {
            if let Some(val) = config.extra.get(*key) {
                body[key] = serde_json::Value::String(val.clone());
            }
        }
        let base = self.api_url.trim_end_matches('/');
        let url = if is_versioned_base_url(base) {
            format!("{base}/images/generations")
        } else {
            format!("{base}/v1/images/generations")
        };

        // 3. 构建请求头
        let headers = build_openai_headers(&self.api_key);

        // 6. 发送请求
        tracing::debug!(url = %url, model = %config.model, "image generation request");
        let send_fut = self.client.post(&url).headers(headers).json(&body).send();
        let response = tokio::select! {
            biased;
            _ = cancel.cancelled() => return Err(LlmError::Cancelled),
            resp = send_fut => resp.map_err(|e| LlmError::Network(e.to_string()))?,
        };

        let status = response.status();
        if !status.is_success() {
            let body_text = response.text().await.unwrap_or_default();
            return Err(LlmError::Api {
                status: status.as_u16(),
                message: format!("Image generation API error: {}", body_text),
            });
        }

        // 7. 解析响应 JSON
        let resp_json: serde_json::Value = response.json().await.map_err(|e| LlmError::Api {
            status: 502,
            message: format!("Failed to parse image generation response: {}", e),
        })?;

        let image_url = resp_json
            .get("data")
            .and_then(|d| d.get(0))
            .and_then(|item| item.get("url"))
            .and_then(|u| u.as_str())
            .ok_or_else(|| LlmError::Api {
                status: 502,
                message: format!("No image URL in response: {}", resp_json),
            })?
            .to_string();

        // 8. 构建事件流：ImageBlock + Done
        let events = vec![
            Ok(ChatEvent::ImageBlock {
                path: String::new(),
                mime_type: String::new(),
                remote_url: Some(image_url),
            }),
            Ok(ChatEvent::Done),
        ];

        Ok(Box::pin(stream::iter(events)))
    }
}

#[async_trait]
impl LlmProvider for OpenAiProvider {
    async fn chat_stream(
        &self,
        messages: &[Message],
        tools: &[ToolDefinition],
        config: &LlmConfig,
        cancel: &tokio_util::sync::CancellationToken,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<ChatEvent, LlmError>> + Send>>, LlmError> {
        // Provider name from config (falls back to "openai" for backward compat)
        let provider_name = config.provider.as_deref().unwrap_or("openai");
        // 创建 gen_ai.client.operation span
        let span = tracing::info_span!(
            "gen_ai.client.operation",
            gen_ai.system = field::Empty,
            gen_ai.request.model = %config.model,
            gen_ai.operation.name = "chat",
            gen_ai.provider.name = %provider_name,
            gen_ai.request.max_tokens = field::Empty,
            gen_ai.request.temperature = field::Empty,
            visp.llm.attempt = 0u64,
            gen_ai.usage.input_tokens = field::Empty,
            gen_ai.usage.output_tokens = field::Empty,
            gen_ai.response.finish_reasons = field::Empty,
            gen_ai.response.model = field::Empty,
            visp.llm.token_limit_hit = field::Empty,
            langfuse.observation.type = field::Empty,
            langfuse.observation.input = field::Empty,
            visp.tools.definitions = field::Empty,
            langfuse.observation.output = field::Empty,
            langfuse.session.id = field::Empty,
            langfuse.trace.name = field::Empty,
            langfuse.user.id = field::Empty,
            langfuse.trace.tags = field::Empty,
            langfuse.environment = field::Empty,
            langfuse.release = field::Empty,
            langfuse.version = field::Empty,
            langfuse.trace.public = field::Empty,
            langfuse.trace.metadata = field::Empty,
            gen_ai.client.base_url = field::Empty,
        );
        span.record("gen_ai.system", "openai");
        span.record("gen_ai.request.max_tokens", config.max_tokens as i64);
        span.record("gen_ai.request.temperature", config.temperature);
        span.record("gen_ai.client.base_url", self.api_url.as_str());

        // Langfuse trace-level fields: record when enabled
        if config.langfuse_enabled {
            if let Some(ref val) = config.langfuse_session_id {
                span.record("langfuse.session.id", val.as_str());
            }
            if let Some(ref val) = config.langfuse_trace_name {
                span.record("langfuse.trace.name", val.as_str());
            }
            if let Some(ref val) = config.langfuse_user_id {
                span.record("langfuse.user.id", val.as_str());
            }
            if let Some(ref val) = config.langfuse_tags {
                span.record("langfuse.trace.tags", val.as_str());
            }
            if let Some(ref val) = config.langfuse_environment {
                span.record("langfuse.environment", val.as_str());
            }
            if let Some(ref val) = config.langfuse_release {
                span.record("langfuse.release", val.as_str());
            }
            if let Some(ref val) = config.langfuse_version {
                span.record("langfuse.version", val.as_str());
            }
            if let Some(public) = config.langfuse_public {
                span.record("langfuse.trace.public", public);
            }
            if let Some(ref metadata) = config.langfuse_metadata
                && !metadata.is_empty()
                && let Ok(json) = serde_json::to_string(metadata)
            {
                span.record("langfuse.trace.metadata", json.as_str());
            }
        }

        // 文生图模型：走 /images/generations 端点
        if config.image_generation {
            return self.image_generate(messages, config, cancel).await;
        }

        let base = self.api_url.trim_end_matches('/');
        let url = if is_versioned_base_url(base) {
            format!("{base}/chat/completions")
        } else {
            format!("{base}/v1/chat/completions")
        };
        let body = build_openai_request(messages, tools, config);
        let headers = build_openai_headers(&self.api_key);

        // Langfuse generation capture: record input if enabled
        let capture_enabled = config.langfuse_capture_input || config.langfuse_capture_output;
        if capture_enabled {
            span.record("langfuse.observation.type", "generation");
        }
        if config.langfuse_capture_input {
            // Record only the messages array, not the full request body
            let input = body.get("messages").unwrap_or(&body);
            let sanitized = crate::sanitize::format_langfuse_input(
                input,
                config.langfuse_capture_max_chars,
                config.langfuse_redact_secrets,
            );
            span.record("langfuse.observation.input", &sanitized);

            // Record tools as a separate attribute
            if let Some(tools_val) = body.get("tools") {
                let tools_str = serde_json::to_string(tools_val).unwrap_or_default();
                let tools_sanitized = crate::sanitize::sanitize_and_truncate(
                    &tools_str,
                    config.langfuse_capture_max_chars,
                    config.langfuse_redact_secrets,
                );
                span.record("visp.tools.definitions", &tools_sanitized);
            }
        }

        tracing::debug!(url = %url, model = %config.model, "OpenAI request");
        let start_time = std::time::Instant::now();
        let send_fut = self.client.post(&url).headers(headers).json(&body).send();
        let response = tokio::select! {
            biased;
            _ = cancel.cancelled() => return Err(LlmError::Cancelled),
            resp = send_fut => resp.map_err(|e| LlmError::Network(e.to_string()))?,
        };

        let status = response.status();
        if status.is_success() {
            let byte_stream = response.bytes_stream();
            // 图片保存目录：优先取 config.extra 的 project_path，缺省用系统临时目录
            let project_path = config
                .extra
                .get("project_path")
                .cloned()
                .unwrap_or_else(|| std::env::temp_dir().to_string_lossy().into_owned());
            Ok(byte_stream_to_chat_events(
                byte_stream,
                start_time,
                span,
                config.model.clone(),
                project_path,
                config.langfuse_capture_output,
                config.langfuse_capture_max_chars,
                config.langfuse_redact_secrets,
            ))
        } else if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
            span.in_scope(|| {
                tracing::error!(target: "gen_ai.client.error", error_type = "rate_limit", "rate limit exceeded");
            });
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
            span.in_scope(|| {
                tracing::error!(target: "gen_ai.client.error", error_type = "api_error", status = status.as_u16(), "API error");
            });
            let body_text = response.text().await.unwrap_or_default();
            Err(LlmError::Api {
                status: status.as_u16(),
                message: body_text,
            })
        }
    }
}

#[cfg(test)]
#[path = "openai_tests.rs"]
mod tests;
