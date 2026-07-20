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
    request_model: String,
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
        pending_bytes: Vec::new(),
        tool_acc: HashMap::new(),
        pending_tool_calls: Vec::new(),
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
                    // Fallback to request model if response didn't include one
                    // (some OpenAI-compatible providers don't include "model" in SSE chunks)
                    let effective_model = if state.model.is_empty() {
                        &state.request_model
                    } else {
                        &state.model
                    };
                    state.span.record("gen_ai.response.model", effective_model);
                    let cost = crate::cost::openai_cost_usd(effective_model, state.input_tokens, state.output_tokens);
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
                            model: effective_model.clone(),
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
                    state.pending_bytes.extend_from_slice(&chunk);
                    // 只解码完整的 UTF-8 部分，不完整的尾字节留到下一个 chunk
                    let safe_end = match std::str::from_utf8(&state.pending_bytes) {
                        Ok(_) => state.pending_bytes.len(),
                        Err(e) => e.valid_up_to(),
                    };
                    if safe_end > 0 {
                        state
                            .buf
                            .push_str(&String::from_utf8_lossy(&state.pending_bytes[..safe_end]));
                        state.pending_bytes = state.pending_bytes[safe_end..].to_vec();
                    }
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
            visp.llm.cost_usd = field::Empty,
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
            Ok(byte_stream_to_chat_events(
                byte_stream,
                start_time,
                span,
                config.model.clone(),
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
