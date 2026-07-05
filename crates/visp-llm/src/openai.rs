use async_trait::async_trait;
use futures::stream::{self, Stream, StreamExt};
use std::collections::HashMap;
use std::pin::Pin;
use tracing::Instrument;
use tracing::field;
use visp_core::error::LlmError;
use visp_core::message::{Message, Role, ToolDefinition};
use visp_core::provider::{ChatEvent, LlmConfig, LlmProvider};

use crate::util::{build_client, parse_retry_after};

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
                result.push(serde_json::json!({
                    "role": "user",
                    "content": msg.content,
                }));
            }
            Role::Assistant => {
                let content: serde_json::Value =
                    if msg.content.is_empty() && msg.tool_calls.is_some() {
                        // OpenAI 规范：纯 tool_calls 消息 content 应为 null
                        serde_json::Value::Null
                    } else {
                        serde_json::Value::String(msg.content.clone())
                    };
                let mut assistant_msg = serde_json::json!({
                    "role": "assistant",
                    "content": content,
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

                // 合并 extra_blocks（如 thinking）到 assistant message 顶层字段
                // 部分 OpenAI 兼容模型支持这些扩展字段
                // 跳过 OpenAI 保留字段，避免意外覆盖
                const RESERVED_FIELDS: &[&str] = &["type", "role", "content", "tool_calls"];
                if let Some(ref blocks) = msg.extra_blocks {
                    for block in blocks {
                        if let Some(obj) = block.as_object() {
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
/// OpenAI SSE 格式：
/// ```text
/// data: {"id":"...","object":"chat.completion.chunk","choices":[{"delta":{"content":"Hello"},"index":0}]}
/// data: {"id":"...","object":"chat.completion.chunk","choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_1","type":"function","function":{"name":"read_file","arguments":""}}]},"index":0}]}
/// data: [DONE]
/// ```
pub(crate) fn parse_openai_sse_data(data: &str) -> Result<OpenAiStreamEvent, LlmError> {
    if data == "[DONE]" {
        return Ok(OpenAiStreamEvent::StreamEnd);
    }
    if data.trim().is_empty() {
        return Ok(OpenAiStreamEvent::Skip);
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
            return Ok(OpenAiStreamEvent::Usage {
                input_tokens,
                output_tokens,
                cache_creation_input_tokens: cache_creation,
                cache_read_input_tokens: cache_read,
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

    // 检查 reasoning_content（DeepSeek 等模型发送推理内容）
    if let Some(reasoning) = delta.get("reasoning_content").and_then(|c| c.as_str())
        && !reasoning.is_empty()
    {
        return Ok(OpenAiStreamEvent::ReasoningDelta(reasoning.to_string()));
    }
    // 部分提供商使用 "reasoning" 字段
    if let Some(reasoning) = delta.get("reasoning").and_then(|c| c.as_str())
        && !reasoning.is_empty()
    {
        return Ok(OpenAiStreamEvent::ReasoningDelta(reasoning.to_string()));
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
        return Ok(OpenAiStreamEvent::Finish {
            index,
            reason: Some(reason),
        });
    }

    Ok(OpenAiStreamEvent::Skip)
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
    langfuse_capture_output: bool,
    langfuse_capture_max_chars: usize,
    langfuse_redact_secrets: bool,
) -> Pin<Box<dyn Stream<Item = Result<ChatEvent, LlmError>> + Send>> {
    struct StreamState {
        stream: Pin<Box<dyn Stream<Item = Result<bytes::Bytes, reqwest::Error>> + Send>>,
        buf: String,
        /// 累积中的工具调用: index -> (id, name, arguments)
        tool_acc: HashMap<usize, (String, String, String)>,
        /// 待发射的工具调用列表: (id, name, arguments)
        pending_tool_calls: Vec<(String, String, String)>,
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
    }

    /// 将累积的工具调用（tool_acc）转移到 pending_tool_calls
    fn flush_tool_acc(state: &mut StreamState) {
        if !state.tool_acc.is_empty() {
            state.tool_call_count = state.tool_acc.len() as u32;
            let calls: Vec<(String, String, String)> = state
                .tool_acc
                .drain()
                .map(|(_, (id, name, args))| (id, name, args))
                .collect();
            state.pending_tool_calls = calls.into_iter().rev().collect();
        }
    }

    /// 标记首 token 已到达（幂等，仅首次有效）
    fn emit_first_token(state: &mut StreamState) {
        if !state.first_content_emitted {
            state.first_content_emitted = true;
            tracing::info!(target: "gen_ai.client.first_token", "first token received");
        }
    }

    let state = StreamState {
        stream: Box::pin(byte_stream),
        buf: String::new(),
        tool_acc: HashMap::new(),
        pending_tool_calls: Vec::new(),
        tool_call_count: 0,
        reasoning_text: String::new(),
        input_tokens: 0,
        output_tokens: 0,
        cache_creation_input_tokens: 0,
        cache_read_input_tokens: 0,
        model: String::new(),
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
    };

    let event_stream = stream::unfold(state, |mut state| {
        let span = state.span.clone();
        async move {
        loop {
            // [A] 优先处理缓冲区中完整的 SSE 消息（一次一条）
            if let Some(pos) = state.buf.find("\n\n") {
                let raw = state.buf[..pos].to_string();
                state.buf = state.buf[pos + 2..].to_string();

                for line in raw.lines() {
                    if let Some(data) = line.strip_prefix("data: ") {
                        // 从 chunk 顶层提取 model 字段
                        if let Ok(v) = serde_json::from_str::<serde_json::Value>(data)
                            && let Some(m) = v.get("model").and_then(|m| m.as_str())
                            && !m.is_empty()
                        {
                            state.model = m.to_string();
                        }
                        match parse_openai_sse_data(data) {
                            Ok(OpenAiStreamEvent::TextDelta(text)) => {
                                if state.langfuse_capture_output {
                                    state.accumulated_output.push_str(&text);
                                }
                                emit_first_token(&mut state);
                                return Some((Ok(ChatEvent::TextDelta(text)), state));
                            }
                            Ok(OpenAiStreamEvent::ReasoningDelta(text)) => {
                                state.reasoning_text.push_str(&text);
                                // 立即发射 ThinkingBlock 实现流式显示（不等待 text delta）
                                let block = serde_json::json!({
                                    "type": "thinking",
                                    "thinking": state.reasoning_text.clone(),
                                });
                                emit_first_token(&mut state);
                                return Some((Ok(ChatEvent::ThinkingBlock(block)), state));
                            }
                            Ok(OpenAiStreamEvent::ToolCallStart { index, id, name }) => {
                                tracing::debug!(index, %id, %name, "ToolCallStart");
                                state.tool_acc.insert(index, (id, name, String::new()));
                            }
                            Ok(OpenAiStreamEvent::ToolCallDelta { index, arguments }) => {
                                if let Some(entry) = state.tool_acc.get_mut(&index) {
                                    entry.2.push_str(&arguments);
                                } else {
                                    tracing::warn!(
                                        index,
                                        delta_len = arguments.len(),
                                        "ToolCallDelta for unknown tool index (ToolCallStart not received?)"
                                    );
                                }
                            }
                            Ok(OpenAiStreamEvent::Finish { reason, .. }) => {
                                if let Some(ref r) = reason {
                                    state.finish_reason = r.clone();
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
                                if !state.tool_acc.is_empty() {
                                    state.tool_call_count = state.tool_acc.len() as u32;
                                    let calls: Vec<(String, String, String)> = state
                                        .tool_acc
                                        .drain()
                                        .map(|(_, (id, name, args))| (id, name, args))
                                        .collect();
                                    state.pending_tool_calls = calls.into_iter().rev().collect();
                                }
                                // 收到 finish_reason 即标记流结束，无需等待底层流关闭
                                state.stream_ended = true;
                            }
                            Ok(OpenAiStreamEvent::Usage {
                                input_tokens,
                                output_tokens,
                                cache_creation_input_tokens,
                                cache_read_input_tokens,
                            }) => {
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
                            }
                            Ok(OpenAiStreamEvent::Skip) => {}
                            Ok(OpenAiStreamEvent::StreamEnd) => {
                                state.stream_ended = true;
                                flush_tool_acc(&mut state);
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
                    state.span.record("gen_ai.usage.input_tokens", state.input_tokens as i64);
                    state.span.record("gen_ai.usage.output_tokens", state.output_tokens as i64);
                    // OpenAI 不写 cache 字段
                    if state.finish_reason == "length" {
                        state.span.record("visp.llm.token_limit_hit", true);
                    }
                    let finish_reasons_str =
                        serde_json::to_string(&finish_reasons).unwrap_or_default();
                    state.span.record("gen_ai.response.finish_reasons", &finish_reasons_str);
                    state.span.record("gen_ai.response.model", &state.model);
                    let cost = crate::cost::openai_cost_usd(&state.model, state.input_tokens, state.output_tokens);
                    state.span.record("visp.llm.cost_usd", cost);

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
                            model: state.model.clone(),
                            finish_reasons,
                            input_tokens: state.input_tokens,
                            output_tokens: state.output_tokens,
                            cache_read_input_tokens: if state.cache_read_input_tokens > 0 {
                                Some(state.cache_read_input_tokens)
                            } else {
                                None
                            },
                            cache_creation_input_tokens: if state.cache_creation_input_tokens > 0 {
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
                    tracing::info!(
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
                    state.buf.push_str(&String::from_utf8_lossy(&chunk));
                    // 下一轮循环会处理缓冲区
                }
                Some(Err(e)) => {
                    return Some((Err(LlmError::Network(e.to_string())), state));
                }
                None => {
                    // 流自然结束
                    state.stream_ended = true;
                    flush_tool_acc(&mut state);
                }
            }
        }
        }.instrument(span)
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
        // 创建 gen_ai.client.operation span
        let span = tracing::info_span!(
            "gen_ai.client.operation",
            gen_ai.system = field::Empty,
            gen_ai.request.model = %config.model,
            gen_ai.operation.name = "chat",
            gen_ai.provider.name = "openai",
            gen_ai.request.max_tokens = field::Empty,
            gen_ai.request.temperature = field::Empty,
            visp.llm.attempt = 0u64,
            gen_ai.usage.input_tokens = field::Empty,
            gen_ai.usage.output_tokens = field::Empty,
            gen_ai.response.finish_reasons = field::Empty,
            gen_ai.response.model = field::Empty,
            visp.llm.cost_usd = field::Empty,
            visp.llm.token_limit_hit = field::Empty,
            langfuse.observation.type = field::Empty,
            langfuse.observation.input = field::Empty,
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
        );
        span.record("gen_ai.system", "openai");
        span.record("gen_ai.request.max_tokens", config.max_tokens as i64);
        span.record("gen_ai.request.temperature", config.temperature);

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
            let sanitized = crate::sanitize::format_langfuse_input(
                &body,
                config.langfuse_capture_max_chars,
                config.langfuse_redact_secrets,
            );
            span.record("langfuse.observation.input", &sanitized);
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
            Ok(byte_stream_to_chat_events(
                byte_stream,
                start_time,
                span,
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
            Message::tool_call(vec![ToolCallRequest {
                id: "call_1".into(),
                name: "read_file".into(),
                arguments: r#"{"path":"test.txt"}"#.into(),
            }]),
            Message::tool("file content", "call_1"),
        ];
        let result = build_openai_messages(&msgs);
        assert_eq!(result.len(), 3);
        assert_eq!(result[1]["role"], "assistant");
        assert_eq!(result[2]["role"], "tool");
        assert_eq!(result[2]["tool_call_id"], "call_1");
        assert_eq!(result[2]["content"], "file content");
    }

    #[test]
    fn test_build_messages_with_extra_blocks() {
        let msgs = vec![
            Message::user("Think step by step"),
            Message {
                role: Role::Assistant,
                content: "Let me think".into(),
                kind: visp_core::message::MessageType::Text,
                tool_calls: None,
                tool_call_id: None,
                tool_call_count: None,
                extra_blocks: Some(vec![serde_json::json!({
                    "type": "thinking",
                    "thinking": "I need to reason about this",
                    "signature": "sig_123",
                })]),
                skip_context: false,
                estimated_tokens: 0,
                actual_tokens_input: None,
                actual_tokens_output: None,
                actual_cache_read: None,
                actual_cache_write: None,
                actual_cost: None,
                provider_metadata: None,
                tool_result_is_error: None,
                tool_result_duration_ms: None,
                created_at: None,
            },
        ];
        let result = build_openai_messages(&msgs);
        assert_eq!(result.len(), 2);
        let assistant = &result[1];
        assert_eq!(assistant["role"], "assistant");
        assert_eq!(assistant["content"], "Let me think");
        assert_eq!(assistant["thinking"], "I need to reason about this");
        assert_eq!(assistant["signature"], "sig_123");
        // "type" 字段应被跳过，不出现在消息中
        assert!(assistant.get("type").is_none());
    }

    #[test]
    fn test_extra_blocks_does_not_overwrite_reserved_fields() {
        let msgs = vec![
            Message::user("Try to override fields"),
            Message {
                role: Role::Assistant,
                content: "Original content".into(),
                kind: visp_core::message::MessageType::ToolCall,
                tool_calls: Some(vec![ToolCallRequest {
                    id: "call_1".into(),
                    name: "read_file".into(),
                    arguments: "{}".into(),
                }]),
                tool_call_id: None,
                tool_call_count: None,
                extra_blocks: Some(vec![serde_json::json!({
                    "role": "user",
                    "content": "malicious content",
                    "tool_calls": "should not appear",
                    "name": "should not appear",
                    "thinking": "this is fine",
                })]),
                skip_context: false,
                estimated_tokens: 0,
                actual_tokens_input: None,
                actual_tokens_output: None,
                actual_cache_read: None,
                actual_cache_write: None,
                actual_cost: None,
                provider_metadata: None,
                tool_result_is_error: None,
                tool_result_duration_ms: None,
                created_at: None,
            },
        ];
        let result = build_openai_messages(&msgs);
        let assistant = &result[1];
        // 保留字段不能被覆盖
        assert_eq!(assistant["role"], "assistant");
        assert_eq!(assistant["content"], "Original content");
        assert!(assistant["tool_calls"].is_array());
        // 非保留字段应被合并
        assert_eq!(assistant["thinking"], "this is fine");
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
        assert_eq!(req["stream_options"]["include_usage"].as_bool(), Some(true));
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
        config.extra.insert("tool_choice".into(), "required".into());
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
        config.extra.insert("tool_choice".into(), "auto".into());
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
            OpenAiStreamEvent::Finish {
                reason: Some(r), ..
            } => assert_eq!(r, "stop"),
            _ => panic!("expected Finish, got {:?}", result),
        }
    }

    #[test]
    fn test_parse_finish_tool_calls() {
        let data = r#"{"id":"chatcmpl-123","object":"chat.completion.chunk","choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}"#;
        let result = parse_openai_sse_data(data).unwrap();
        match result {
            OpenAiStreamEvent::Finish {
                reason: Some(r), ..
            } => assert_eq!(r, "tool_calls"),
            _ => panic!("expected Finish, got {:?}", result),
        }
    }

    #[test]
    fn test_parse_done_marker() {
        let result = parse_openai_sse_data("[DONE]").unwrap();
        assert!(matches!(result, OpenAiStreamEvent::StreamEnd));
    }

    #[test]
    fn test_parse_usage() {
        let data = r#"{"id":"chatcmpl-123","object":"chat.completion.chunk","choices":[],"usage":{"prompt_tokens":10,"completion_tokens":20,"total_tokens":30}}"#;
        let result = parse_openai_sse_data(data).unwrap();
        match result {
            OpenAiStreamEvent::Usage {
                input_tokens,
                output_tokens,
                cache_creation_input_tokens,
                cache_read_input_tokens,
                ..
            } => {
                assert_eq!(input_tokens, 10);
                assert_eq!(output_tokens, 20);
                assert_eq!(cache_creation_input_tokens, 0);
                assert_eq!(cache_read_input_tokens, 0);
            }
            _ => panic!("expected Usage, got {:?}", result),
        }
    }

    #[test]
    fn test_parse_null_usage_skipped() {
        // Some providers (e.g. Ark/volcengine) send "usage": null in every chunk
        // until the final usage-only chunk. null usage should NOT produce a Usage event.
        let data = r#"{"id":"1","choices":[{"delta":{"content":"hi"},"index":0}],"usage":null}"#;
        let result = parse_openai_sse_data(data).unwrap();
        match result {
            OpenAiStreamEvent::TextDelta(text) => assert_eq!(text, "hi"),
            _ => panic!("expected TextDelta, got {:?}", result),
        }
    }

    #[test]
    fn test_parse_usage_with_cache() {
        // OpenAI 标准格式：prompt_tokens_details.cached_tokens
        let data = r#"{"id":"chatcmpl-123","object":"chat.completion.chunk","choices":[],"usage":{"prompt_tokens":100,"completion_tokens":50,"total_tokens":150,"prompt_tokens_details":{"cached_tokens":40}}}"#;
        let result = parse_openai_sse_data(data).unwrap();
        match result {
            OpenAiStreamEvent::Usage {
                input_tokens,
                output_tokens,
                cache_creation_input_tokens,
                cache_read_input_tokens,
            } => {
                assert_eq!(input_tokens, 100);
                assert_eq!(output_tokens, 50);
                assert_eq!(cache_creation_input_tokens, 0);
                assert_eq!(cache_read_input_tokens, 40);
            }
            _ => panic!("expected Usage, got {:?}", result),
        }
    }

    // --- parse_retry_after 测试 ---

    #[test]
    fn test_parse_retry_after_valid() {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(reqwest::header::RETRY_AFTER, "30".parse().unwrap());
        assert_eq!(crate::util::parse_retry_after(&headers), Some(30));
    }

    #[test]
    fn test_parse_retry_after_missing() {
        let headers = reqwest::header::HeaderMap::new();
        assert_eq!(crate::util::parse_retry_after(&headers), None);
    }

    #[test]
    fn test_parse_reasoning_content_delta() {
        let data = r#"{"id":"chatcmpl-123","object":"chat.completion.chunk","choices":[{"index":0,"delta":{"reasoning_content":"Step 1: think"},"finish_reason":null}]}"#;
        let result = parse_openai_sse_data(data).unwrap();
        match result {
            OpenAiStreamEvent::ReasoningDelta(t) => assert_eq!(t, "Step 1: think"),
            _ => panic!("expected ReasoningDelta, got {:?}", result),
        }
    }

    #[test]
    fn test_parse_reasoning_field_delta() {
        // Some providers use "reasoning" instead of "reasoning_content"
        let data = r#"{"id":"chatcmpl-123","object":"chat.completion.chunk","choices":[{"index":0,"delta":{"reasoning":"deep thinking..."},"finish_reason":null}]}"#;
        let result = parse_openai_sse_data(data).unwrap();
        match result {
            OpenAiStreamEvent::ReasoningDelta(t) => assert_eq!(t, "deep thinking..."),
            _ => panic!("expected ReasoningDelta, got {:?}", result),
        }
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
    async fn collect_events(chunks: Vec<String>) -> Vec<ChatEvent> {
        let byte_stream =
            futures::stream::iter(chunks.into_iter().map(|s| Ok(bytes::Bytes::from(s))));
        let span = tracing::Span::current();
        let event_stream = byte_stream_to_chat_events(
            byte_stream,
            std::time::Instant::now(),
            span,
            false,
            20000,
            true,
        );
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

        assert_eq!(
            events.len(),
            4,
            "expect TextDelta + UsageInfo + OutputMetadata + Done"
        );
        assert!(matches!(&events[0], ChatEvent::TextDelta(t) if t == "Hello"));
        assert!(matches!(&events[1], ChatEvent::UsageInfo { .. }));
        assert!(matches!(&events[2], ChatEvent::OutputMetadata(_)));
        assert!(matches!(&events[3], ChatEvent::Done));
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

        assert_eq!(
            events.len(),
            5,
            "expect TextDelta x2 + UsageInfo + OutputMetadata + Done"
        );
        assert!(matches!(&events[0], ChatEvent::TextDelta(t) if t == "Hello"));
        assert!(matches!(&events[1], ChatEvent::TextDelta(t) if t == " World"));
        assert!(matches!(&events[2], ChatEvent::UsageInfo { .. }));
        assert!(matches!(&events[3], ChatEvent::OutputMetadata(_)));
        assert!(matches!(&events[4], ChatEvent::Done));
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

        assert_eq!(
            events.len(),
            4,
            "expect ToolCall + UsageInfo + OutputMetadata + Done"
        );
        match &events[0] {
            ChatEvent::ToolCall {
                id,
                name,
                arguments,
            } => {
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
        assert!(matches!(&events[2], ChatEvent::OutputMetadata(_)));
        assert!(matches!(&events[3], ChatEvent::Done));
    }

    #[tokio::test]
    async fn test_byte_stream_natural_end() {
        // 没有 [DONE] 标记，流自然结束
        let sse = make_sse(&make_text_chunk("Hello"));
        let events = collect_events(vec![sse]).await;

        assert_eq!(
            events.len(),
            4,
            "expect TextDelta + UsageInfo + OutputMetadata + Done"
        );
        assert!(matches!(&events[0], ChatEvent::TextDelta(t) if t == "Hello"));
        assert!(matches!(&events[1], ChatEvent::UsageInfo { .. }));
        assert!(matches!(&events[2], ChatEvent::OutputMetadata(_)));
        assert!(matches!(&events[3], ChatEvent::Done));
    }

    #[tokio::test]
    async fn test_byte_stream_chunk_boundary() {
        // SSE 消息正文被拆在两个 HTTP chunk 中，`"Hel"` 和 `lo"}` 分属不同 chunk
        let part1 = "data: {\"id\":\"1\",\"object\":\"chat.completion.chunk\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"Hel";
        let part2 = "lo\"},\"finish_reason\":null}]}\n\ndata: [DONE]\n\n".to_string();
        let events = collect_events(vec![part1.to_string(), part2]).await;

        assert_eq!(
            events.len(),
            4,
            "expect TextDelta + UsageInfo + OutputMetadata + Done"
        );
        assert!(matches!(&events[0], ChatEvent::TextDelta(t) if t == "Hello"));
        assert!(matches!(&events[1], ChatEvent::UsageInfo { .. }));
        assert!(matches!(&events[2], ChatEvent::OutputMetadata(_)));
        assert!(matches!(&events[3], ChatEvent::Done));
    }

    #[tokio::test]
    async fn test_byte_stream_empty_stream() {
        let events = collect_events(vec![]).await;

        assert_eq!(events.len(), 3, "expect UsageInfo + OutputMetadata + Done");
        assert!(matches!(&events[0], ChatEvent::UsageInfo { .. }));
        assert!(matches!(&events[1], ChatEvent::OutputMetadata(_)));
        assert!(matches!(&events[2], ChatEvent::Done));
    }

    #[tokio::test]
    async fn test_byte_stream_reasoning_then_text() {
        // reasoning_content chunks come first, then text, then stop
        let reasoning1 = serde_json::json!({
            "id": "chatcmpl",
            "object": "chat.completion.chunk",
            "choices": [{
                "index": 0,
                "delta": { "reasoning_content": "Step 1... " },
                "finish_reason": null
            }]
        });
        let reasoning2 = serde_json::json!({
            "id": "chatcmpl",
            "object": "chat.completion.chunk",
            "choices": [{
                "index": 0,
                "delta": { "reasoning_content": "Step 2..." },
                "finish_reason": null
            }]
        });
        let sse = format!(
            "{}{}{}{}",
            make_sse(&reasoning1),
            make_sse(&reasoning2),
            make_sse(&make_text_chunk("The answer is 42.")),
            sse_line("[DONE]"),
        );
        let events = collect_events(vec![sse]).await;

        // Expect: ThinkingBlock (step1) + ThinkingBlock (step1+step2) + TextDelta + UsageInfo + OutputMetadata + Done
        assert_eq!(
            events.len(),
            6,
            "expect 2 ThinkingBlocks + TextDelta + UsageInfo + OutputMetadata + Done in streaming mode"
        );
        match &events[0] {
            ChatEvent::ThinkingBlock(block) => {
                assert_eq!(block["type"], "thinking");
                assert_eq!(block["thinking"], "Step 1... ");
            }
            _ => panic!("expected first ThinkingBlock, got {:?}", events[0]),
        }
        match &events[1] {
            ChatEvent::ThinkingBlock(block) => {
                assert_eq!(block["type"], "thinking");
                assert_eq!(block["thinking"], "Step 1... Step 2...");
            }
            _ => panic!("expected second ThinkingBlock, got {:?}", events[1]),
        }
        assert!(matches!(&events[2], ChatEvent::TextDelta(t) if t == "The answer is 42."));
        assert!(matches!(&events[3], ChatEvent::UsageInfo { .. }));
        assert!(matches!(&events[4], ChatEvent::OutputMetadata(_)));
        assert!(matches!(&events[5], ChatEvent::Done));
    }

    #[tokio::test]
    async fn test_byte_stream_reasoning_only() {
        // Model outputs ONLY reasoning content, no text (the reported bug scenario)
        let reasoning = serde_json::json!({
            "id": "chatcmpl",
            "object": "chat.completion.chunk",
            "choices": [{
                "index": 0,
                "delta": { "reasoning_content": "All tokens spent on reasoning..." },
                "finish_reason": null
            }]
        });
        // Include usage to simulate real API behavior with token counts
        let usage_chunk = serde_json::json!({
            "id": "chatcmpl",
            "object": "chat.completion.chunk",
            "choices": [],
            "usage": {
                "prompt_tokens": 100,
                "completion_tokens": 4096,
                "total_tokens": 4196
            }
        });
        let sse = format!(
            "{}{}{}",
            make_sse(&reasoning),
            make_sse(&usage_chunk),
            sse_line("[DONE]"),
        );
        let events = collect_events(vec![sse]).await;

        // Expect: ThinkingBlock + UsageInfo + OutputMetadata + Done (no TextDelta)
        assert_eq!(
            events.len(),
            4,
            "expect ThinkingBlock + UsageInfo + OutputMetadata + Done"
        );
        match &events[0] {
            ChatEvent::ThinkingBlock(block) => {
                assert_eq!(block["type"], "thinking");
                assert_eq!(block["thinking"], "All tokens spent on reasoning...");
            }
            _ => panic!("expected ThinkingBlock, got {:?}", events[0]),
        }
        assert!(matches!(&events[1], ChatEvent::UsageInfo { .. }));
        assert!(matches!(&events[2], ChatEvent::OutputMetadata(_)));
        assert!(matches!(&events[3], ChatEvent::Done));
    }

    // --- truncate_for_log 测试（回归：UTF-8 char boundary panic） ---

    #[test]
    fn truncate_for_log_ascii_below_limit_returns_full() {
        assert_eq!(truncate_for_log("hello", 200), "hello");
    }

    #[test]
    fn truncate_for_log_ascii_above_limit_truncates() {
        let s = "a".repeat(300);
        let out = truncate_for_log(&s, 200);
        assert_eq!(out.chars().count(), 200);
    }

    #[test]
    fn truncate_for_log_chinese_does_not_panic() {
        // 67 个汉字 = 201 字节，触发原 bug：bytes 198..201 在 '析' 中间
        let s = "分析".repeat(100);
        let out = truncate_for_log(&s, 200);
        // 200 字符（每个 3 字节）= 600 字节
        assert_eq!(out.chars().count(), 200);
    }

    #[test]
    fn truncate_for_log_at_exact_char_count() {
        // 边界：字符数刚好等于上限
        let s = "中文";
        assert_eq!(truncate_for_log(s, 2), "中文");
    }

    #[test]
    fn truncate_for_log_mixed_ascii_and_chinese() {
        let s = "abc中文def";
        let out = truncate_for_log(s, 4);
        assert_eq!(out, "abc中");
        assert_eq!(out.chars().count(), 4);
    }

    #[test]
    fn truncate_for_log_zero_limit() {
        assert_eq!(truncate_for_log("中文", 0), "");
    }

    // ── Tracing / gen_ai.client.operation span tests ────────────────────────

    use std::sync::{Arc, Mutex};
    use tracing::Event;
    use tracing::Subscriber;
    use tracing::field::{Field, Visit};
    use tracing::span::{Attributes, Id, Record};
    use tracing_subscriber::layer::{Context, Layer};
    use tracing_subscriber::prelude::*;
    use tracing_subscriber::registry::LookupSpan;

    #[derive(Debug, Clone)]
    struct CapturedSpan {
        name: String,
        fields: Vec<(String, String)>,
        id: u64,
    }

    struct SpanFieldVisitor {
        fields: Vec<(String, String)>,
    }

    impl Visit for SpanFieldVisitor {
        fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
            self.fields
                .push((field.name().to_string(), format!("{value:?}")));
        }
        fn record_str(&mut self, field: &Field, value: &str) {
            self.fields
                .push((field.name().to_string(), value.to_string()));
        }
        fn record_i64(&mut self, field: &Field, value: i64) {
            self.fields
                .push((field.name().to_string(), value.to_string()));
        }
        fn record_u64(&mut self, field: &Field, value: u64) {
            self.fields
                .push((field.name().to_string(), value.to_string()));
        }
        fn record_bool(&mut self, field: &Field, value: bool) {
            self.fields
                .push((field.name().to_string(), value.to_string()));
        }
    }

    struct TestLayer {
        spans: Arc<Mutex<Vec<CapturedSpan>>>,
        events: Arc<Mutex<Vec<(String, String)>>>,
    }

    impl TestLayer {
        fn new(
            spans: Arc<Mutex<Vec<CapturedSpan>>>,
            events: Arc<Mutex<Vec<(String, String)>>>,
        ) -> Self {
            Self { spans, events }
        }
    }

    impl<S> Layer<S> for TestLayer
    where
        S: Subscriber + for<'a> LookupSpan<'a>,
    {
        fn on_new_span(&self, attrs: &Attributes<'_>, id: &Id, _ctx: Context<'_, S>) {
            let mut visitor = SpanFieldVisitor { fields: Vec::new() };
            attrs.record(&mut visitor);
            self.spans.lock().unwrap().push(CapturedSpan {
                name: attrs.metadata().name().to_string(),
                fields: visitor.fields,
                id: id.into_u64(),
            });
        }

        fn on_record(&self, id: &Id, values: &Record<'_>, _ctx: Context<'_, S>) {
            let mut visitor = SpanFieldVisitor { fields: Vec::new() };
            values.record(&mut visitor);
            let mut spans = self.spans.lock().unwrap();
            if let Some(span) = spans.iter_mut().find(|s| s.id == id.into_u64()) {
                span.fields.extend(visitor.fields);
            }
        }

        fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
            let target = event.metadata().target().to_string();
            let name = event.metadata().name().to_string();
            self.events.lock().unwrap().push((target, name));
        }
    }

    #[allow(clippy::type_complexity)]
    fn setup_tracing() -> (
        Arc<Mutex<Vec<CapturedSpan>>>,
        Arc<Mutex<Vec<(String, String)>>>,
    ) {
        let spans = Arc::new(Mutex::new(Vec::new()));
        let events = Arc::new(Mutex::new(Vec::new()));
        (spans, events)
    }

    fn make_guard(
        spans: &Arc<Mutex<Vec<CapturedSpan>>>,
        events: &Arc<Mutex<Vec<(String, String)>>>,
    ) -> tracing::subscriber::DefaultGuard {
        tracing_subscriber::registry()
            .with(TestLayer::new(spans.clone(), events.clone()))
            .set_default()
    }

    /// 生成一组完整的 OpenAI SSE 事件（包含 usage 和 finish_reason）
    fn make_openai_complete_sse(model: &str, finish_reason: &str) -> String {
        let text_chunk = serde_json::json!({
            "id": "chatcmpl",
            "object": "chat.completion.chunk",
            "choices": [{
                "index": 0,
                "delta": { "content": "Hello" },
                "finish_reason": null
            }]
        });
        let usage_chunk = serde_json::json!({
            "id": "chatcmpl",
            "object": "chat.completion.chunk",
            "choices": [],
            "model": model,
            "usage": {
                "prompt_tokens": 100,
                "completion_tokens": 50,
                "total_tokens": 150
            }
        });
        let stop_chunk = serde_json::json!({
            "id": "chatcmpl",
            "object": "chat.completion.chunk",
            "choices": [{
                "index": 0,
                "delta": {},
                "finish_reason": finish_reason
            }]
        });

        fn sse_line(data: &str) -> String {
            format!("data: {}\n\n", data)
        }

        format!(
            "{}{}{}",
            sse_line(&text_chunk.to_string()),
            sse_line(&usage_chunk.to_string()),
            sse_line(&stop_chunk.to_string()),
        )
    }

    #[test]
    fn test_gen_ai_client_operation_span_created_openai() {
        let (spans, _events) = setup_tracing();
        let _guard = make_guard(&spans, &_events);

        let span = tracing::info_span!(
            "gen_ai.client.operation",
            gen_ai.system = tracing::field::Empty,
            gen_ai.request.model = "gpt-4o",
            gen_ai.operation.name = "chat",
            gen_ai.provider.name = "openai",
            gen_ai.request.max_tokens = tracing::field::Empty,
            gen_ai.request.temperature = tracing::field::Empty,
            visp.llm.attempt = 0u64,
            gen_ai.usage.input_tokens = tracing::field::Empty,
            gen_ai.usage.output_tokens = tracing::field::Empty,
            gen_ai.response.finish_reasons = tracing::field::Empty,
            gen_ai.response.model = tracing::field::Empty,
            visp.llm.cost_usd = tracing::field::Empty,
            visp.llm.token_limit_hit = tracing::field::Empty,
        );
        span.record("gen_ai.request.max_tokens", 4096i64);
        span.record("gen_ai.request.temperature", 0.7f64);

        drop(_guard);
        let spans = spans.lock().unwrap();
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].name, "gen_ai.client.operation");
    }

    #[test]
    fn test_gen_ai_request_fields_at_span_start_openai() {
        let (spans, _events) = setup_tracing();
        let _guard = make_guard(&spans, &_events);

        let span = tracing::info_span!(
            "gen_ai.client.operation",
            gen_ai.system = tracing::field::Empty,
            gen_ai.request.model = "gpt-4o",
            gen_ai.operation.name = "chat",
            gen_ai.provider.name = "openai",
            gen_ai.request.max_tokens = tracing::field::Empty,
            gen_ai.request.temperature = tracing::field::Empty,
            visp.llm.attempt = 0u64,
            gen_ai.usage.input_tokens = tracing::field::Empty,
            gen_ai.usage.output_tokens = tracing::field::Empty,
            gen_ai.response.finish_reasons = tracing::field::Empty,
            gen_ai.response.model = tracing::field::Empty,
            visp.llm.cost_usd = tracing::field::Empty,
            visp.llm.token_limit_hit = tracing::field::Empty,
        );

        span.record("gen_ai.system", "openai");
        span.record("gen_ai.request.max_tokens", 4096i64);
        span.record("gen_ai.request.temperature", 0.7f64);

        drop(_guard);
        let spans = spans.lock().unwrap();
        assert_eq!(spans.len(), 1);
        let fields = &spans[0].fields;
        assert!(
            fields
                .iter()
                .any(|(k, v)| k == "gen_ai.request.model" && v == "gpt-4o")
        );
        assert!(
            fields
                .iter()
                .any(|(k, v)| k == "gen_ai.operation.name" && v == "chat")
        );
        assert!(
            fields
                .iter()
                .any(|(k, v)| k == "visp.llm.attempt" && v == "0")
        );
        assert!(
            fields
                .iter()
                .any(|(k, v)| k == "gen_ai.system" && v == "openai")
        );
        assert!(
            fields
                .iter()
                .any(|(k, v)| k == "gen_ai.provider.name" && v == "openai")
        );
        assert!(
            fields
                .iter()
                .any(|(k, v)| k == "gen_ai.request.max_tokens" && v == "4096")
        );
        assert!(
            fields
                .iter()
                .any(|(k, v)| k == "gen_ai.request.temperature" && v == "0.7")
        );
    }

    #[tokio::test]
    async fn test_gen_ai_usage_fields_recorded_on_completion_openai() {
        let (spans, _events) = setup_tracing();
        let _guard = make_guard(&spans, &_events);

        let span = tracing::info_span!(
            "gen_ai.client.operation",
            gen_ai.system = tracing::field::Empty,
            gen_ai.request.model = "gpt-4o",
            gen_ai.operation.name = "chat",
            gen_ai.provider.name = "openai",
            gen_ai.request.max_tokens = tracing::field::Empty,
            gen_ai.request.temperature = tracing::field::Empty,
            visp.llm.attempt = 0u64,
            gen_ai.usage.input_tokens = tracing::field::Empty,
            gen_ai.usage.output_tokens = tracing::field::Empty,
            gen_ai.response.finish_reasons = tracing::field::Empty,
            gen_ai.response.model = tracing::field::Empty,
            visp.llm.cost_usd = tracing::field::Empty,
            visp.llm.token_limit_hit = tracing::field::Empty,
        );
        span.record("gen_ai.request.max_tokens", 4096i64);
        span.record("gen_ai.request.temperature", 0.7f64);

        let sse = make_openai_complete_sse("gpt-4o", "stop");
        let byte_stream =
            futures::stream::iter(vec![sse].into_iter().map(|s| Ok(bytes::Bytes::from(s))));
        let start = std::time::Instant::now();
        let event_stream = byte_stream_to_chat_events(byte_stream, start, span, false, 20000, true);
        let _: Vec<ChatEvent> = event_stream
            .filter_map(|e| futures::future::ready(e.ok()))
            .collect()
            .await;

        drop(_guard);

        let spans = spans.lock().unwrap();
        assert_eq!(spans.len(), 1);
        let fields = &spans[0].fields;
        assert!(
            fields
                .iter()
                .any(|(k, v)| k == "gen_ai.usage.input_tokens" && v == "100")
        );
        assert!(
            fields
                .iter()
                .any(|(k, v)| k == "gen_ai.usage.output_tokens" && v == "50")
        );
        assert!(
            fields
                .iter()
                .any(|(k, v)| k == "gen_ai.response.finish_reasons" && v == "[\"stop\"]")
        );
        assert!(
            fields
                .iter()
                .any(|(k, v)| k == "gen_ai.response.model" && v == "gpt-4o")
        );

        // OpenAI span 不应包含 cache 字段
        assert!(
            !fields
                .iter()
                .any(|(k, _)| k == "gen_ai.usage.cache_read_input_tokens")
        );
        assert!(
            !fields
                .iter()
                .any(|(k, _)| k == "gen_ai.usage.cache_creation_input_tokens")
        );
        assert!(
            !fields
                .iter()
                .any(|(k, _)| k == "gen_ai.usage.cache_read.input_tokens")
        );
        assert!(
            !fields
                .iter()
                .any(|(k, _)| k == "gen_ai.usage.cache_creation.input_tokens")
        );

        // cost_usd 应为正数
        assert!(
            fields
                .iter()
                .any(|(k, v)| k == "visp.llm.cost_usd" && v.parse::<f64>().unwrap_or(0.0) > 0.0)
        );
    }

    #[tokio::test]
    async fn test_openai_client_first_token_event() {
        let (spans, events) = setup_tracing();
        let _guard = make_guard(&spans, &events);

        let span = tracing::info_span!(
            "gen_ai.client.operation",
            gen_ai.usage.input_tokens = tracing::field::Empty,
            gen_ai.usage.output_tokens = tracing::field::Empty,
            gen_ai.response.model = tracing::field::Empty,
            visp.llm.cost_usd = tracing::field::Empty,
        );

        let sse = make_openai_complete_sse("gpt-4o", "stop");
        let byte_stream =
            futures::stream::iter(vec![sse].into_iter().map(|s| Ok(bytes::Bytes::from(s))));
        let start = std::time::Instant::now();
        let event_stream = byte_stream_to_chat_events(byte_stream, start, span, false, 20000, true);
        let _: Vec<ChatEvent> = event_stream
            .filter_map(|e| futures::future::ready(e.ok()))
            .collect()
            .await;

        drop(_guard);
        let evts = events.lock().unwrap();
        assert!(
            evts.iter().any(|(t, _)| t == "gen_ai.client.first_token"),
            "should find first_token event"
        );
    }

    #[tokio::test]
    async fn test_openai_client_completed_event() {
        let (spans, events) = setup_tracing();
        let _guard = make_guard(&spans, &events);

        let span = tracing::info_span!(
            "gen_ai.client.operation",
            gen_ai.usage.input_tokens = tracing::field::Empty,
            gen_ai.usage.output_tokens = tracing::field::Empty,
            gen_ai.response.model = tracing::field::Empty,
            visp.llm.cost_usd = tracing::field::Empty,
        );

        let sse = make_openai_complete_sse("gpt-4o", "stop");
        let byte_stream =
            futures::stream::iter(vec![sse].into_iter().map(|s| Ok(bytes::Bytes::from(s))));
        let start = std::time::Instant::now();
        let event_stream = byte_stream_to_chat_events(byte_stream, start, span, false, 20000, true);
        let _: Vec<ChatEvent> = event_stream
            .filter_map(|e| futures::future::ready(e.ok()))
            .collect()
            .await;

        drop(_guard);
        let evts = events.lock().unwrap();
        assert!(
            evts.iter().any(|(t, _)| t == "gen_ai.client.completed"),
            "should find completed event; found: {:?}",
            evts,
        );
    }

    #[test]
    fn test_gen_ai_client_retry_event_emitted_openai() {
        let (_spans, events) = setup_tracing();
        let _guard = make_guard(&_spans, &events);

        tracing::warn!(
            target: "gen_ai.client.retry",
            reason = "rate_limit",
            "retrying LLM request"
        );

        drop(_guard);
        let evts = events.lock().unwrap();
        assert!(
            evts.iter().any(|(t, _)| t == "gen_ai.client.retry"),
            "should find retry event"
        );
    }

    #[test]
    fn test_gen_ai_provider_name_is_openai() {
        let (spans, _events) = setup_tracing();
        let _guard = make_guard(&spans, &_events);

        let _span = tracing::info_span!(
            "gen_ai.client.operation",
            gen_ai.provider.name = "openai",
            gen_ai.operation.name = "chat",
        );

        drop(_guard);
        let spans = spans.lock().unwrap();
        assert!(
            spans[0]
                .fields
                .iter()
                .any(|(k, v)| k == "gen_ai.provider.name" && v == "openai")
        );
    }

    #[tokio::test]
    async fn test_openai_finish_reason_length_stays_length() {
        let (spans, _events) = setup_tracing();
        let _guard = make_guard(&spans, &_events);

        let span = tracing::info_span!(
            "gen_ai.client.operation",
            gen_ai.response.finish_reasons = tracing::field::Empty,
            visp.llm.token_limit_hit = tracing::field::Empty,
        );

        let sse = make_openai_complete_sse("gpt-4o", "length");
        let byte_stream =
            futures::stream::iter(vec![sse].into_iter().map(|s| Ok(bytes::Bytes::from(s))));
        let start = std::time::Instant::now();
        let event_stream = byte_stream_to_chat_events(byte_stream, start, span, false, 20000, true);
        let _: Vec<ChatEvent> = event_stream
            .filter_map(|e| futures::future::ready(e.ok()))
            .collect()
            .await;

        drop(_guard);
        let spans = spans.lock().unwrap();
        let fields = &spans[0].fields;

        assert!(
            fields
                .iter()
                .any(|(k, v)| k == "gen_ai.response.finish_reasons" && v == "[\"length\"]"),
            "OpenAI length should stay as length"
        );
        assert!(
            fields
                .iter()
                .any(|(k, v)| k == "visp.llm.token_limit_hit" && v == "true"),
            "token_limit_hit should be true for length"
        );
    }

    #[tokio::test]
    async fn test_openai_finish_reason_stop_stays_stop() {
        let (spans, _events) = setup_tracing();
        let _guard = make_guard(&spans, &_events);

        let span = tracing::info_span!(
            "gen_ai.client.operation",
            gen_ai.response.finish_reasons = tracing::field::Empty,
            visp.llm.token_limit_hit = tracing::field::Empty,
        );

        let sse = make_openai_complete_sse("gpt-4o", "stop");
        let byte_stream =
            futures::stream::iter(vec![sse].into_iter().map(|s| Ok(bytes::Bytes::from(s))));
        let start = std::time::Instant::now();
        let event_stream = byte_stream_to_chat_events(byte_stream, start, span, false, 20000, true);
        let _: Vec<ChatEvent> = event_stream
            .filter_map(|e| futures::future::ready(e.ok()))
            .collect()
            .await;

        drop(_guard);
        let spans = spans.lock().unwrap();
        let fields = &spans[0].fields;

        assert!(
            fields
                .iter()
                .any(|(k, v)| k == "gen_ai.response.finish_reasons" && v == "[\"stop\"]")
        );
        // stop 不应设置 token_limit_hit
        let token_limit_entry = fields.iter().find(|(k, _)| k == "visp.llm.token_limit_hit");
        assert!(token_limit_entry.is_none() || token_limit_entry.unwrap().1 == "false");
    }

    #[tokio::test]
    async fn test_openai_no_cache_fields() {
        let (spans, _events) = setup_tracing();
        let _guard = make_guard(&spans, &_events);

        let span = tracing::info_span!(
            "gen_ai.client.operation",
            gen_ai.usage.input_tokens = tracing::field::Empty,
            gen_ai.usage.output_tokens = tracing::field::Empty,
        );

        let sse = make_openai_complete_sse("gpt-4o", "stop");
        let byte_stream =
            futures::stream::iter(vec![sse].into_iter().map(|s| Ok(bytes::Bytes::from(s))));
        let start = std::time::Instant::now();
        let event_stream = byte_stream_to_chat_events(byte_stream, start, span, false, 20000, true);
        let _: Vec<ChatEvent> = event_stream
            .filter_map(|e| futures::future::ready(e.ok()))
            .collect()
            .await;

        drop(_guard);
        let spans = spans.lock().unwrap();
        let fields = &spans[0].fields;

        assert!(
            !fields
                .iter()
                .any(|(k, _)| k.starts_with("gen_ai.usage.cache")),
            "OpenAI span should not have any cache fields, got: {:?}",
            fields
        );
    }

    #[test]
    fn test_openai_cost_usd_computed_from_usage() {
        use crate::cost::openai_cost_usd;
        // gpt-4o: $2.5/MTok input, $10/MTok output
        let cost = openai_cost_usd("gpt-4o", 1000, 500);
        let expected = (1000.0 / 1_000_000.0 * 2.5) + (500.0 / 1_000_000.0 * 10.0);
        assert!((cost - expected).abs() < 1e-10);
    }
}
