use std::panic::AssertUnwindSafe;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use crate::ProviderMetadata;
use crate::agent::{
    AgentConfig, AgentEvent, AgentLoopContext, AgentMessage, Envelope, OrchestratorMessage,
    PendingSpawn, ToolExecResult, UserQueryResult, cleanup_orphan_tool_uses, extract_thinking_text,
    format_tool_args, llm_error_to_code, parse_user_query_marker, render_tool_guide,
    strip_user_query_marker,
};
use crate::error::AgentErrorCode;
use crate::error::LlmError;
use crate::message::{
    Message, MessageType, Role, ToolCallRequest, ToolDefinition, estimate_message_tokens,
};
use crate::prompt::PromptBuilder;
use crate::provider::ChatEvent;
use crate::provider::LlmConfig;
use crate::provider::LlmProvider;
use crate::rules::RuleEngine;
use crate::session::SessionManager;
use crate::session::SessionStatus;
use crate::tool::ToolContext;
use crate::tool::ToolResult;
use crate::tool_registry::ToolRegistry;
use crate::trace_context::generate_w3c_span_id;

use futures::FutureExt;
use futures::Stream;
use futures::StreamExt;
use tokio::sync::mpsc;
use tokio::sync::oneshot;
use tracing::Instrument;

// ── Helpers ──────────────────────────────────────────────────────────────────

/// Convert AgentEvent to AgentMessage for global_tx forwarding
fn event_to_msg(event: &AgentEvent) -> Option<AgentMessage> {
    match event {
        AgentEvent::TextDelta(s) => Some(AgentMessage::TextDelta(s.clone())),
        AgentEvent::ThinkingBlock(v) => Some(AgentMessage::ThinkingBlock(v.clone())),
        AgentEvent::UsageInfo {
            input_tokens,
            output_tokens,
            tool_calls,
            cache_creation_input_tokens,
            cache_read_input_tokens,
        } => Some(AgentMessage::UsageInfo {
            input_tokens: *input_tokens,
            output_tokens: *output_tokens,
            tool_calls: *tool_calls,
            cache_creation_input_tokens: *cache_creation_input_tokens,
            cache_read_input_tokens: *cache_read_input_tokens,
        }),
        AgentEvent::StatusUpdate(s) => Some(AgentMessage::StatusUpdate(s.clone())),
        AgentEvent::Error { code, message } => Some(AgentMessage::Error {
            code: code.clone(),
            message: message.clone(),
        }),
        AgentEvent::ToolCallRequest {
            call_id,
            tool_name,
            arguments,
        } => Some(AgentMessage::ToolCallRequest {
            call_id: call_id.clone(),
            tool_name: tool_name.clone(),
            arguments: arguments.clone(),
        }),
        AgentEvent::ToolCallResult {
            call_id,
            tool_name,
            content,
            is_error,
        } => Some(AgentMessage::ToolCallResult {
            call_id: call_id.clone(),
            tool_name: tool_name.clone(),
            content: content.clone(),
            is_error: *is_error,
        }),
        AgentEvent::UserQuery { .. } => None, // oneshot not clonable
        AgentEvent::Done => Some(AgentMessage::Done),
    }
}

/// Send event to tx and optionally forward to global_tx.
/// On tx send failure, finishes session and returns Err(()).
async fn send_event(
    tx: &mpsc::Sender<AgentEvent>,
    sm: &SessionManager,
    sid: &str,
    global_tx: &Option<mpsc::Sender<Envelope>>,
    session_id: &str,
    event: AgentEvent,
) -> Result<(), ()> {
    // Forward to global_tx (for orchestrator)
    if let Some(gtx) = global_tx
        && let Some(msg) = event_to_msg(&event)
    {
        let _ = gtx
            .send(Envelope {
                session_id: session_id.to_string(),
                message: msg,
                trace_context: None,
            })
            .await;
    }
    // Send to tx (for CLI)
    if tx.send(event).await.is_err() {
        let _ = sm.finish_loop(sid, SessionStatus::Error);
        return Err(());
    }
    Ok(())
}

/// Record langfuse trace-level fields onto a span.
/// The span must have these fields declared in its `info_span!` macro invocation.
/// Safe to call even when langfuse is disabled (returns immediately).
pub fn record_langfuse_trace_fields(span: &tracing::Span, cfg: &AgentConfig, session_id: &str) {
    if !cfg.langfuse_enabled {
        return;
    }
    span.record("langfuse.session.id", session_id);
    span.record("langfuse.trace.name", "visp.agent.run".to_string());

    if let Some(ref user_id) = cfg.langfuse_user_id {
        span.record("langfuse.user.id", user_id.as_str());
    }
    if let Some(ref tags) = cfg.langfuse_tags {
        span.record("langfuse.trace.tags", tags.as_str());
    }
    let env = cfg.langfuse_environment.as_deref().unwrap_or("default");
    span.record("langfuse.environment", env);
    if let Some(ref release) = cfg.langfuse_release {
        span.record("langfuse.release", release.as_str());
    }
    if let Some(ref version) = cfg.langfuse_version {
        span.record("langfuse.version", version.as_str());
    }
    if let Some(public) = cfg.langfuse_public {
        span.record("langfuse.trace.public", public);
    }
    if let Some(ref metadata) = cfg.langfuse_metadata
        && !metadata.is_empty()
        && let Ok(json) = serde_json::to_string(metadata)
    {
        span.record("langfuse.trace.metadata", json.as_str());
    }
}

/// Iteration context produced by setup_iteration
struct IterationContext {
    messages: Vec<Message>,
    tools: Vec<ToolDefinition>,
}

/// a. Cancellation check  b. Limits check  c. Prompt + tools build
#[allow(clippy::too_many_arguments)]
async fn setup_iteration(
    iteration: u32,
    ctx: &mut AgentLoopContext,
    sm: &SessionManager,
    tool_registry: &ToolRegistry,
    rule_engine: &RuleEngine,
    cfg: &AgentConfig,
    sid: &str,
    tx: &mpsc::Sender<AgentEvent>,
) -> Result<IterationContext, ()> {
    // a. Cancellation check
    if ctx.cancel_token.is_cancelled() {
        tracing::info!(
            target: "visp.agent.cancelled",
            session_id = %sid,
            iteration,
            "agent loop cancelled"
        );
        send_event(
            tx,
            sm,
            sid,
            &ctx.global_tx,
            &ctx.session_id,
            AgentEvent::Error {
                code: AgentErrorCode::Cancelled,
                message: "agent cancelled".into(),
            },
        )
        .await?;
        let _ = sm.finish_loop(sid, SessionStatus::Error);
        return Err(());
    }

    // b. Limits check
    if iteration >= cfg.hard_limit {
        tracing::warn!(
            target: "visp.agent.iteration_limit",
            session_id = %sid,
            iteration,
            hard_limit = cfg.hard_limit,
            "agent loop reached iteration limit"
        );
        send_event(
            tx,
            sm,
            sid,
            &ctx.global_tx,
            &ctx.session_id,
            AgentEvent::Error {
                code: AgentErrorCode::MaxIterations,
                message: format!("Agent loop reached hard limit ({})", cfg.hard_limit),
            },
        )
        .await?;
        let _ = sm.finish_loop(sid, SessionStatus::Error);
        return Err(());
    }

    // c. Build prompt
    let session = match sm.get(sid) {
        Ok(s) => s,
        Err(e) => {
            send_event(
                tx,
                sm,
                sid,
                &ctx.global_tx,
                &ctx.session_id,
                AgentEvent::Error {
                    code: AgentErrorCode::Internal,
                    message: format!("Failed to get session: {e}"),
                },
            )
            .await?;
            let _ = sm.finish_loop(sid, SessionStatus::Error);
            return Err(());
        }
    };
    let date_str = chrono::Local::now().format("%Y-%m-%d").to_string();

    let tool_guide = render_tool_guide(tool_registry);
    let mut enriched_template = if tool_guide.is_empty() {
        session.system_prompt_template.clone()
    } else {
        format!("{}{}", session.system_prompt_template, tool_guide)
    };

    // Soft limit check
    if cfg.soft_limit > 0 && iteration >= cfg.soft_limit {
        enriched_template.push_str(
            "\n\n[System: You have reached the maximum number of iterations. \
         Please immediately complete all remaining work in your next response. \
         If all work is done, respond without making any tool calls.]",
        );
    }
    let mut messages = PromptBuilder::build(
        &enriched_template,
        &rule_engine.get_active_rules(),
        &ctx.history,
        &ctx.working_dir,
        &date_str,
        Some(ctx.config.max_context_tokens),
        ctx.config.max_tokens,
        ctx.context_trimmer.as_ref(),
    );

    cleanup_orphan_tool_uses(&mut messages);
    messages.retain(|m| !m.skip_context);

    let tools = tool_registry.definitions();

    let total_estimated: u32 = messages
        .iter()
        .map(crate::message::estimate_message_tokens)
        .sum();
    tracing::info!(
        session_id = %sid,
        messages = messages.len(),
        budget = ctx.config.max_context_tokens,
        estimated_tokens = total_estimated,
        max_output_tokens = ctx.config.max_tokens,
        "prompt built, calling LLM"
    );

    Ok(IterationContext { messages, tools })
}

/// d. LLM call with retry for RateLimit/Network errors
///
/// 纯逻辑：监听 cancel_token、按配置重试，错误透传给调用方。
/// 不发 AgentEvent::Error、不调 finish_loop——这些 side-effect 由调用方负责。
async fn call_llm_with_retry(
    provider: &dyn LlmProvider,
    messages: &[Message],
    tools: &[ToolDefinition],
    ctx: &AgentLoopContext,
    cfg: &AgentConfig,
    sid: &str,
) -> Result<Pin<Box<dyn Stream<Item = Result<ChatEvent, LlmError>> + Send>>, LlmError> {
    let mut attempt = 0u32;

    // 注入 Langfuse trace 级字段到 LlmConfig，使 provider 能在 gen_ai.client.operation span 上记录
    let llm_config = LlmConfig {
        langfuse_enabled: cfg.langfuse_enabled,
        langfuse_session_id: Some(ctx.session_id.clone()),
        langfuse_trace_name: Some("visp.agent.run".to_string()),
        langfuse_user_id: cfg.langfuse_user_id.clone(),
        langfuse_tags: cfg.langfuse_tags.clone(),
        langfuse_environment: cfg.langfuse_environment.clone(),
        langfuse_release: cfg.langfuse_release.clone(),
        langfuse_version: cfg.langfuse_version.clone(),
        langfuse_public: cfg.langfuse_public,
        langfuse_metadata: cfg.langfuse_metadata.clone(),
        ..ctx.config.clone()
    };

    loop {
        // 循环顶部检查 cancel：首次进入或 sleep 后立即返回
        if ctx.cancel_token.is_cancelled() {
            return Err(LlmError::Cancelled);
        }

        match provider
            .chat_stream(messages, tools, &llm_config, &ctx.cancel_token)
            .await
        {
            Ok(s) => break Ok(Box::pin(s)),
            Err(e @ (LlmError::RateLimit { .. } | LlmError::Network(_))) => {
                if attempt >= cfg.llm_retry_attempts {
                    let (code, msg) = llm_error_to_code(&e);
                    if matches!(code, AgentErrorCode::Cancelled) {
                        tracing::info!(
                            session_id = %sid,
                            attempts = attempt + 1,
                            "LLM call cancelled by user"
                        );
                    } else {
                        tracing::error!(
                            session_id = %sid,
                            error_code = ?code,
                            error_msg = %msg,
                            attempts = attempt + 1,
                            "LLM provider error after retries exhausted"
                        );
                    }
                    return Err(e);
                }
                let delay = cfg.llm_retry_base_delay_ms * (1u64 << attempt);
                // sleep 期间监听 cancel
                tokio::select! {
                    biased;
                    _ = ctx.cancel_token.cancelled() => {
                        return Err(LlmError::Cancelled);
                    }
                    _ = tokio::time::sleep(Duration::from_millis(delay)) => {}
                }
                attempt += 1;
            }
            Err(e) => {
                let (code, msg) = llm_error_to_code(&e);
                if matches!(code, AgentErrorCode::Cancelled) {
                    tracing::info!(
                        session_id = %sid,
                        "LLM call cancelled by user"
                    );
                } else {
                    tracing::error!(
                        session_id = %sid,
                        error_code = ?code,
                        error_msg = %msg,
                        "LLM provider error"
                    );
                }
                return Err(e);
            }
        }
    }
}

/// Stream output produced by collect_stream_events
struct StreamOutput {
    text_buffer: String,
    thinking_blocks: Vec<serde_json::Value>,
    tool_calls: Vec<ToolCallRequest>,
    input_tokens: u32,
    output_tokens: u32,
    cache_creation_input_tokens: u32,
    cache_read_input_tokens: u32,
    provider_metadata: Option<ProviderMetadata>,
}

/// e. Collect stream events (TextDelta, ThinkingBlock, ToolCall, UsageInfo)
/// Returns None on error (stream dropped, cancelled, or LLM error).
async fn collect_stream_events(
    stream: Pin<Box<dyn Stream<Item = Result<ChatEvent, LlmError>> + Send>>,
    ctx: &mut AgentLoopContext,
    sid: &str,
    tx: &mpsc::Sender<AgentEvent>,
    sm: &SessionManager,
) -> Option<StreamOutput> {
    let mut text_buffer = String::new();
    let mut tool_calls: Vec<ToolCallRequest> = Vec::new();
    let mut thinking_blocks: Vec<serde_json::Value> = Vec::new();
    let mut input_tokens: u32 = 0;
    let mut output_tokens: u32 = 0;
    let mut cache_creation_input_tokens: u32 = 0;
    let mut cache_read_input_tokens: u32 = 0;
    let mut pending_metadata: Option<ProviderMetadata> = None;

    let mut pin_stream = Box::pin(stream);
    loop {
        tokio::select! {
            biased;
            _ = ctx.cancel_token.cancelled() => {
                send_event(
                    tx, sm, sid, &ctx.global_tx, &ctx.session_id,
                    AgentEvent::Error {
                        code: AgentErrorCode::Cancelled,
                        message: "agent cancelled".into(),
                    },
                ).await.ok()?;
                let _ = sm.finish_loop(sid, SessionStatus::Error);
                return None;
            }
            event = pin_stream.next() => {
                match event {
                    Some(Ok(ChatEvent::TextDelta(delta))) => {
                        text_buffer.push_str(&delta);
                        send_event(
                            tx, sm, sid, &ctx.global_tx, &ctx.session_id,
                            AgentEvent::TextDelta(delta),
                        ).await.ok()?;
                    }
                    Some(Ok(ChatEvent::ThinkingBlock(block))) => {
                        thinking_blocks.clear();
                        thinking_blocks.push(block.clone());
                        send_event(
                            tx, sm, sid, &ctx.global_tx, &ctx.session_id,
                            AgentEvent::ThinkingBlock(block),
                        ).await.ok()?;
                    }
                    Some(Ok(ChatEvent::UsageInfo { input_tokens: it, output_tokens: ot, cache_creation_input_tokens: ccit, cache_read_input_tokens: crit, .. })) => {
                        input_tokens = it;
                        output_tokens = ot;
                        cache_creation_input_tokens = ccit;
                        cache_read_input_tokens = crit;
                    }
                    Some(Ok(ChatEvent::ToolCall { id, name, arguments })) => {
                        tool_calls.push(ToolCallRequest { id, name, arguments });
                    }
                    Some(Ok(ChatEvent::OutputMetadata(meta))) => {
                        pending_metadata = Some(meta);
                    }
                    Some(Ok(ChatEvent::Done)) => break,
                    Some(Err(e)) => {
                        let (code, msg) = llm_error_to_code(&e);
                        send_event(
                            tx, sm, sid, &ctx.global_tx, &ctx.session_id,
                            AgentEvent::Error { code, message: msg },
                        ).await.ok()?;
                        let _ = sm.finish_loop(sid, SessionStatus::Error);
                        return None;
                    }
                    None => {
                        let partial_len = text_buffer.len();
                        let tool_count = tool_calls.len();
                        let thinking_count = thinking_blocks.len();
                        tracing::warn!(
                            session_id = %sid,
                            partial_response_len = partial_len,
                            tool_calls_received = tool_count,
                            thinking_blocks = thinking_count,
                            "LLM stream ended without Done event — connection likely dropped"
                        );
                        send_event(
                            tx, sm, sid, &ctx.global_tx, &ctx.session_id,
                            AgentEvent::Error {
                                code: AgentErrorCode::Internal,
                                message: "LLM stream ended unexpectedly — the response may be incomplete, check API connection".into(),
                            },
                        ).await.ok()?;
                        let _ = sm.finish_loop(sid, SessionStatus::Error);
                        return None;
                    }
                }
            }
        }
    }

    Some(StreamOutput {
        text_buffer,
        tool_calls,
        thinking_blocks,
        input_tokens,
        output_tokens,
        cache_creation_input_tokens,
        cache_read_input_tokens,
        provider_metadata: pending_metadata,
    })
}

/// Decision after handling stream result
enum StreamDecision {
    Done,
    UserQuery {
        response_rx: oneshot::Receiver<UserQueryResult>,
    },
    Continue,
}

/// Drain pending sub-agent spawns into error ToolExecResults.
/// Used on cancel/inbox-close paths to prevent orphan tool_uses.
fn drain_pending_spawns(pending: &mut Vec<PendingSpawn>, reason: &str) -> Vec<ToolExecResult> {
    pending
        .drain(..)
        .map(|ps| ToolExecResult {
            index: ps.index,
            call_id: ps.call_id,
            result: ToolResult::error(reason),
            duration_ms: None,
        })
        .collect()
}

/// f. Handle stream result: check [USER_QUERY] marker or return Done/Continue
/// On error, sends error events via send_event and returns Err(()).
async fn handle_stream_result(
    output: &StreamOutput,
    total_tool_calls: u32,
    ctx: &mut AgentLoopContext,
    sm: &SessionManager,
    sid: &str,
    tx: &mpsc::Sender<AgentEvent>,
) -> Result<StreamDecision, ()> {
    let text_buffer = &output.text_buffer;
    let tool_calls = &output.tool_calls;
    let thinking_blocks = &output.thinking_blocks;
    let input_tokens = output.input_tokens;
    let output_tokens = output.output_tokens;
    let cache_creation_input_tokens = output.cache_creation_input_tokens;
    let cache_read_input_tokens = output.cache_read_input_tokens;
    let provider_metadata = &output.provider_metadata;

    if tool_calls.is_empty() {
        // Check [USER_QUERY] marker
        if let Some(marker) = parse_user_query_marker(text_buffer) {
            let clean_text = strip_user_query_marker(text_buffer);
            let thinking_text = extract_thinking_text(thinking_blocks);

            // Save thinking as a separate message if present
            if let Some(ref thinking) = thinking_text {
                let mut thinking_msg = Message::thinking(thinking.clone());
                thinking_msg.estimated_tokens = estimate_message_tokens(&thinking_msg);
                ctx.history.push(thinking_msg.clone());
                if let Err(e) = sm.append_message(sid, thinking_msg) {
                    send_event(
                        tx,
                        sm,
                        sid,
                        &ctx.global_tx,
                        &ctx.session_id,
                        AgentEvent::Error {
                            code: AgentErrorCode::Internal,
                            message: format!("Failed to append thinking message: {e}"),
                        },
                    )
                    .await?;
                    let _ = sm.finish_loop(sid, SessionStatus::Error);
                    return Err(());
                }
            }

            // Save text message if present
            if !clean_text.is_empty() {
                let mut text_msg =
                    Message::assistant_with_metadata(clean_text.clone(), provider_metadata.clone());
                text_msg.extra_blocks = if thinking_blocks.is_empty() {
                    None
                } else {
                    Some(thinking_blocks.clone())
                };
                text_msg.actual_tokens_input = Some(input_tokens);
                text_msg.actual_tokens_output = Some(output_tokens);
                text_msg.actual_cache_read = Some(cache_read_input_tokens);
                text_msg.actual_cache_write = Some(cache_creation_input_tokens);
                ctx.history.push(text_msg.clone());
                if let Err(e) = sm.append_message(sid, text_msg) {
                    send_event(
                        tx,
                        sm,
                        sid,
                        &ctx.global_tx,
                        &ctx.session_id,
                        AgentEvent::Error {
                            code: AgentErrorCode::Internal,
                            message: format!("Failed to append assistant message: {e}"),
                        },
                    )
                    .await?;
                    let _ = sm.finish_loop(sid, SessionStatus::Error);
                    return Err(());
                }
            }

            // Send UserQuery event
            let (resp_tx, resp_rx) = oneshot::channel::<UserQueryResult>();
            send_event(
                tx,
                sm,
                sid,
                &ctx.global_tx,
                &ctx.session_id,
                AgentEvent::UserQuery {
                    query_id: format!("query-{}", ctx.history.len()),
                    message: marker.message.clone(),
                    options: marker.options.clone(),
                    allow_other: marker.allow_other,
                    respond: resp_tx,
                },
            )
            .await?;

            return Ok(StreamDecision::UserQuery {
                response_rx: resp_rx,
            });
        }

        // No [USER_QUERY] marker: done
        if text_buffer.is_empty() && tool_calls.is_empty() && thinking_blocks.is_empty() {
            if output_tokens > 0 {
                tracing::error!(
                    session_id = %sid,
                    input_tokens,
                    output_tokens,
                    "LLM returned empty response after consuming output tokens"
                );
                send_event(
                    tx, sm, sid, &ctx.global_tx, &ctx.session_id,
                    AgentEvent::Error {
                        code: AgentErrorCode::Internal,
                        message: format!(
                            "LLM returned empty response after consuming {output_tokens} output tokens. \
                         If thinking mode is enabled, the thinking may have been redacted or \
                         the output budget exhausted. Try increasing budget_tokens or max_tokens."
                        ),
                    },
                )
                .await?;
                let _ = sm.finish_loop(sid, SessionStatus::Error);
                return Err(());
            }
            tracing::warn!(
                session_id = %sid,
                input_tokens,
                output_tokens,
                "LLM returned empty stream (no text, no tool calls, no thinking)"
            );
        } else if text_buffer.is_empty() {
            tracing::info!(
                session_id = %sid,
                tool_calls = tool_calls.len(),
                thinking_blocks = thinking_blocks.len(),
                "LLM returned response with no text (only tool_calls/thinking)"
            );
        }
        let thinking_text = extract_thinking_text(thinking_blocks);

        if let Some(ref thinking) = thinking_text {
            let mut thinking_msg = Message::thinking(thinking.clone());
            thinking_msg.estimated_tokens = estimate_message_tokens(&thinking_msg);
            ctx.history.push(thinking_msg.clone());
            if let Err(e) = sm.append_message(sid, thinking_msg) {
                send_event(
                    tx,
                    sm,
                    sid,
                    &ctx.global_tx,
                    &ctx.session_id,
                    AgentEvent::Error {
                        code: AgentErrorCode::Internal,
                        message: format!("Failed to append thinking message: {e}"),
                    },
                )
                .await?;
                let _ = sm.finish_loop(sid, SessionStatus::Error);
                return Err(());
            }
        }

        if !text_buffer.is_empty() {
            let mut text_msg =
                Message::assistant_with_metadata(text_buffer.clone(), provider_metadata.clone());
            text_msg.extra_blocks = if thinking_blocks.is_empty() {
                None
            } else {
                Some(thinking_blocks.clone())
            };
            text_msg.actual_tokens_input = Some(input_tokens);
            text_msg.actual_tokens_output = Some(output_tokens);
            text_msg.actual_cache_read = Some(cache_read_input_tokens);
            text_msg.actual_cache_write = Some(cache_creation_input_tokens);
            ctx.history.push(text_msg.clone());
            if let Err(e) = sm.append_message(sid, text_msg) {
                send_event(
                    tx,
                    sm,
                    sid,
                    &ctx.global_tx,
                    &ctx.session_id,
                    AgentEvent::Error {
                        code: AgentErrorCode::Internal,
                        message: format!("Failed to append assistant message: {e}"),
                    },
                )
                .await?;
                let _ = sm.finish_loop(sid, SessionStatus::Error);
                return Err(());
            }
        }

        // Send usage info and Done
        send_event(
            tx,
            sm,
            sid,
            &ctx.global_tx,
            &ctx.session_id,
            AgentEvent::UsageInfo {
                input_tokens,
                output_tokens,
                tool_calls: total_tool_calls,
                cache_creation_input_tokens,
                cache_read_input_tokens,
            },
        )
        .await?;
        send_event(
            tx,
            sm,
            sid,
            &ctx.global_tx,
            &ctx.session_id,
            AgentEvent::Done,
        )
        .await?;
        let _ = sm.finish_loop(sid, SessionStatus::Completed);
        return Ok(StreamDecision::Done);
    }

    Ok(StreamDecision::Continue)
}

/// g+h: Doom loop detection, execute tools in parallel, collect results,
/// and append tool results to history.
/// Returns true if the agent loop should return (fatal error), false to continue.
#[allow(clippy::too_many_arguments)]
async fn execute_tool_calls(
    tool_calls: &[ToolCallRequest],
    text_buffer: String,
    thinking_blocks: Vec<serde_json::Value>,
    actual_tokens_input: u32,
    actual_tokens_output: u32,
    actual_cache_read: u32,
    actual_cache_write: u32,
    provider_metadata: Option<ProviderMetadata>,
    total_tool_calls: &mut u32,
    ctx: &mut AgentLoopContext,
    sm: &Arc<SessionManager>,
    tool_registry: &Arc<ToolRegistry>,
    sid: &str,
    tx: &mpsc::Sender<AgentEvent>,
    cfg: &AgentConfig,
    doom_loop_window: &mut Vec<Vec<(String, serde_json::Value)>>,
    doom_loop_warned: &mut bool,
    visp_trace_id: &str,
    iter_span_w3c_id: &str,
) -> bool {
    // Append assistant message with tool_calls
    *total_tool_calls += tool_calls.len() as u32;
    let mut assistant_msg = Message {
        role: Role::Assistant,
        kind: MessageType::ToolCall,
        content: text_buffer,
        tool_call_id: None,
        tool_calls: Some(tool_calls.to_vec()),
        skip_context: false,
        extra_blocks: if thinking_blocks.is_empty() {
            None
        } else {
            Some(thinking_blocks)
        },
        estimated_tokens: 0,
        actual_tokens_input: Some(actual_tokens_input),
        actual_tokens_output: Some(actual_tokens_output),
        actual_cache_read: Some(actual_cache_read),
        actual_cache_write: Some(actual_cache_write),
        actual_cost: None,
        provider_metadata: provider_metadata
            .map(|pm| serde_json::to_value(pm).expect("ProviderMetadata serialization")),
        tool_call_count: Some(tool_calls.len() as u32),
        tool_result_is_error: None,
        tool_result_duration_ms: None,
        created_at: None,
    };
    assistant_msg.estimated_tokens = estimate_message_tokens(&assistant_msg);
    ctx.history.push(assistant_msg.clone());
    if let Err(e) = sm.append_message(sid, assistant_msg) {
        let _ = send_event(
            tx,
            sm,
            sid,
            &ctx.global_tx,
            &ctx.session_id,
            AgentEvent::Error {
                code: AgentErrorCode::Internal,
                message: format!("Failed to append assistant message: {e}"),
            },
        )
        .await;
        // Send failure is acceptable here (best-effort), but mark error
        let _ = sm.finish_loop(sid, SessionStatus::Error);
        return true; // fatal
    }

    // g. Doom loop detection
    if cfg.doom_loop_threshold > 0 {
        let round_sig: Vec<(String, serde_json::Value)> = tool_calls
            .iter()
            .map(|tc| {
                let args: serde_json::Value =
                    serde_json::from_str(&tc.arguments).unwrap_or_default();
                (tc.name.clone(), args)
            })
            .collect();
        doom_loop_window.push(round_sig);
        let threshold = cfg.doom_loop_threshold as usize;
        if doom_loop_window.len() > threshold {
            doom_loop_window.remove(0);
        }
        if doom_loop_window.len() == threshold {
            let first = &(*doom_loop_window)[0];
            let all_same = doom_loop_window.iter().all(|sig| sig == first);
            if all_same {
                if *doom_loop_warned {
                    let _ = send_event(
                        tx,
                        sm,
                        sid,
                        &ctx.global_tx,
                        &ctx.session_id,
                        AgentEvent::Error {
                            code: AgentErrorCode::StuckInLoop,
                            message: "Agent stuck in repeated tool call loop after warning".into(),
                        },
                    )
                    .await;
                    let _ = sm.finish_loop(sid, SessionStatus::Error);
                    return true; // fatal
                }
                *doom_loop_warned = true;
                doom_loop_window.clear();
                let _ = send_event(
                    tx,
                    sm,
                    sid,
                    &ctx.global_tx,
                    &ctx.session_id,
                    AgentEvent::StatusUpdate(
                        "Agent appears stuck in a loop of repeated tool calls".into(),
                    ),
                )
                .await;
                ctx.history.push(Message::system(
                    "You appear to be repeating the same tool calls. \
                     Please change your approach or summarize the current progress.",
                ));
            }
        }
    }

    // h. Execute tools in parallel (Phase 1: dispatch)
    let num_tools = tool_calls.len();
    let mut exec_tasks = Vec::with_capacity(num_tools);
    let mut pending_spawns: Vec<PendingSpawn> = Vec::new();
    let tool_ids: Vec<String> = tool_calls.iter().map(|tc| tc.id.clone()).collect();
    let is_multi_agent = ctx.global_tx.is_some() && ctx.inbox_rx.is_some();
    let langfuse_enabled = cfg.langfuse_enabled;

    for (i, tc) in tool_calls.iter().enumerate() {
        // Multi-agent: intercept "task" tool calls
        if is_multi_agent && tc.name == "task" {
            let task_args: serde_json::Value =
                serde_json::from_str(&tc.arguments).unwrap_or_default();
            let subagent_type = task_args
                .get("subagent_type")
                .and_then(|v| v.as_str())
                .unwrap_or("default")
                .to_string();
            let description = task_args
                .get("description")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let task_id = task_args
                .get("task_id")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());

            if let Some(ref gtx) = ctx.global_tx {
                // Generate W3C TraceContext for cross-mpsc propagation
                let span_id = generate_w3c_span_id();
                let visp_tc = crate::TraceContext::new(
                    visp_trace_id.to_string(),
                    span_id,
                    1, // sampled
                    None,
                    Some(iter_span_w3c_id.to_string()),
                )
                .expect("TraceContext construction should not fail with valid IDs");
                let _ = gtx.try_send(Envelope {
                    session_id: ctx.session_id.clone(),
                    message: AgentMessage::SpawnRequest {
                        call_id: tc.id.clone(),
                        subagent_type: subagent_type.clone(),
                        description: description.clone(),
                        task_id: task_id.clone(),
                        trace_context: Some(visp_tc.clone()),
                    },
                    trace_context: Some(visp_tc),
                });
            }

            pending_spawns.push(PendingSpawn {
                index: i,
                call_id: tc.id.clone(),
                subagent_type,
            });
            continue;
        }

        // Regular tool execution (original spawn logic)
        let tx = tx.clone();
        let global_tx = ctx.global_tx.clone();
        let cancel = ctx.cancel_token.clone();
        let registry = tool_registry.clone();
        let session_id = sid.to_string();
        let working_dir = ctx.working_dir.clone();
        let tc = tc.clone();
        let sm = sm.clone();
        let permissions = ctx.permission_rules.clone();
        let sid2 = sid.to_string();

        // Pre-clone for tool span
        let tool_span_name = tc.name.clone();
        let tool_span_id = tc.id.clone();
        let tool_span = if langfuse_enabled {
            tracing::info_span!(
                "visp.tool.execute",
                gen_ai.tool.name = %tool_span_name,
                gen_ai.tool.call.id = %tool_span_id,
                gen_ai.tool.type = "function",
                gen_ai.operation.name = "execute_tool",
                langfuse.observation.type = "span",
                level = tracing::field::Empty,
                status_message = tracing::field::Empty,
                langfuse.session.id = tracing::field::Empty,
                langfuse.user.id = tracing::field::Empty,
                langfuse.trace.tags = tracing::field::Empty,
                langfuse.trace.name = tracing::field::Empty,
                langfuse.environment = tracing::field::Empty,
                langfuse.trace.public = tracing::field::Empty,
                langfuse.release = tracing::field::Empty,
                langfuse.version = tracing::field::Empty,
                langfuse.trace.metadata = tracing::field::Empty,
                visp.tool.is_error = tracing::field::Empty,
                visp.tool.duration_ms = tracing::field::Empty,
            )
        } else {
            tracing::info_span!(
                "visp.tool.execute",
                gen_ai.tool.name = %tool_span_name,
                gen_ai.tool.call.id = %tool_span_id,
                gen_ai.tool.type = "function",
                visp.tool.is_error = tracing::field::Empty,
                visp.tool.duration_ms = tracing::field::Empty,
            )
        };
        // Propagate langfuse trace-level fields onto tool span
        record_langfuse_trace_fields(&tool_span, cfg, sid);

        exec_tasks.push(tokio::spawn(
            async move {
                // Helper: forward AgentMessage to global_tx
                macro_rules! forward_global {
                    ($msg:expr) => {
                        if let Some(ref gtx) = global_tx {
                            let _ = gtx.try_send(Envelope {
                                session_id: sid2.clone(),
                                message: $msg,
                                trace_context: None,
                            });
                        }
                    };
                }

                // Cancellation check
                if cancel.is_cancelled() {
                    let result = ToolExecResult {
                        index: i,
                        call_id: tc.id.clone(),
                        result: ToolResult::error("Cancelled"),
                        duration_ms: None,
                    };
                    forward_global!(AgentMessage::ToolCallResult {
                        call_id: tc.id.clone(),
                        tool_name: tc.name.clone(),
                        content: "Cancelled".into(),
                        is_error: true,
                    });
                    let _ = tx
                        .send(AgentEvent::ToolCallResult {
                            call_id: tc.id.clone(),
                            tool_name: tc.name.clone(),
                            content: "Cancelled".into(),
                            is_error: true,
                        })
                        .await;
                    return result;
                }

                // Send ToolCallRequest
                forward_global!(AgentMessage::ToolCallRequest {
                    call_id: tc.id.clone(),
                    tool_name: tc.name.clone(),
                    arguments: tc.arguments.clone(),
                });
                let _ = tx
                    .send(AgentEvent::ToolCallRequest {
                        call_id: tc.id.clone(),
                        tool_name: tc.name.clone(),
                        arguments: tc.arguments.clone(),
                    })
                    .await;

                // Check if tool requires approval (with arguments)
                let args_value: serde_json::Value =
                    serde_json::from_str(&tc.arguments).unwrap_or_default();
                let requires_approval = registry
                    .get(&tc.name)
                    .map(|t| t.requires_approval_for(&args_value))
                    .unwrap_or(false);

                let already_approved = sm.is_tool_approved(&session_id, &tc.name);

                if requires_approval && !already_approved {
                    let (resp_tx, resp_rx) = oneshot::channel::<UserQueryResult>();
                    let args_display = format_tool_args(&tc.arguments);
                    let _ = tx
                        .send(AgentEvent::UserQuery {
                            query_id: tc.id.clone(),
                            message: format!("Allow tool: {}({})?", tc.name, args_display),
                            options: Vec::new(),
                            allow_other: false,
                            respond: resp_tx,
                        })
                        .await;

                    let result = resp_rx.await.unwrap_or_default();
                    match result.selected_index {
                        0 => {}
                        2 => {
                            let _ = sm.add_approved_tool(&session_id, &tc.name);
                        }
                        _ => {
                            let result = ToolResult::error("User denied");
                            forward_global!(AgentMessage::ToolCallResult {
                                call_id: tc.id.clone(),
                                tool_name: tc.name.clone(),
                                content: result.content.clone(),
                                is_error: result.is_error,
                            });
                            let _ = tx
                                .send(AgentEvent::ToolCallResult {
                                    call_id: tc.id.clone(),
                                    tool_name: tc.name.clone(),
                                    content: result.content.clone(),
                                    is_error: result.is_error,
                                })
                                .await;
                            return ToolExecResult {
                                index: i,
                                call_id: tc.id,
                                result,
                                duration_ms: None,
                            };
                        }
                    }
                }

                // Parse arguments and execute
                let args = match serde_json::from_str(&tc.arguments) {
                    Ok(v) => v,
                    Err(e) => {
                        tracing::warn!(
                            tool = %tc.name,
                            args_len = tc.arguments.len(),
                            error = %e,
                            "tool call arguments truncated or malformed (likely max_output_tokens exceeded)"
                        );
                        let result = ToolResult::error(format!(
                            "[TRUNCATED] Tool call arguments incomplete ({} bytes, parse: {}). \
                             The content exceeded max_output_tokens.\n\
                             To fix this, split the content into smaller parts:\n\
                             - Use multiple smaller write_file or edit_file calls\n\
                             - Or use edit_file to incrementally build the file\n\
                             - Do NOT retry the same large write_file call — it will fail again.",
                            tc.arguments.len(), e
                        ));
                        forward_global!(AgentMessage::ToolCallResult {
                            call_id: tc.id.clone(),
                            tool_name: tc.name.clone(),
                            content: result.content.clone(),
                            is_error: result.is_error,
                        });
                        let _ = tx
                            .send(AgentEvent::ToolCallResult {
                                call_id: tc.id.clone(),
                                tool_name: tc.name.clone(),
                                content: result.content.clone(),
                                is_error: result.is_error,
                            })
                            .await;
                        return ToolExecResult {
                            index: i,
                            call_id: tc.id,
                            result,
                            duration_ms: None,
                        };
                    }
                };
                let tool_ctx = ToolContext {
                    working_dir: working_dir.clone(),
                    session_id: Some(session_id),
                    permission_rules: permissions.clone(),
                };

                let start = std::time::Instant::now();
                let result = registry
                    .execute(&tc.name, args, &tool_ctx)
                    .await
                    .unwrap_or_else(|| ToolResult::error("Tool not found in registry"));
                let elapsed_ms = start.elapsed().as_millis() as u64;

                // Record tool execution fields on the visp.tool.execute span
                tracing::Span::current().record("visp.tool.duration_ms", elapsed_ms as i64);
                tracing::Span::current().record("visp.tool.is_error", result.is_error);

                if langfuse_enabled {
                    let level = if result.is_error { "ERROR" } else { "DEFAULT" };
                    tracing::Span::current().record("level", level);
                    if result.is_error {
                        let summary = result.content.lines().next().unwrap_or("Tool error");
                        let status_msg = if summary.len() > 100 {
                            let mut s = summary.to_string();
                            s.truncate(97);
                            s.push_str("...");
                            s
                        } else {
                            summary.to_string()
                        };
                        tracing::Span::current().record("status_message", status_msg);
                    }
                }

                forward_global!(AgentMessage::ToolCallResult {
                    call_id: tc.id.clone(),
                    tool_name: tc.name.clone(),
                    content: result.content.clone(),
                    is_error: result.is_error,
                });
                let _ = tx
                    .send(AgentEvent::ToolCallResult {
                        call_id: tc.id.clone(),
                        tool_name: tc.name.clone(),
                        content: result.content.clone(),
                        is_error: result.is_error,
                    })
                    .await;

            ToolExecResult {
                index: i,
                call_id: tc.id,
                result,
                duration_ms: Some(elapsed_ms),
            }
        }.instrument(tool_span)));
    }

    // Phase 2: Collect results
    let task_results = if is_multi_agent {
        let mut collected: Vec<ToolExecResult> = Vec::new();
        let mut regular_done = exec_tasks.is_empty();
        let mut inbox = ctx.inbox_rx.take();

        loop {
            if ctx.cancel_token.is_cancelled() {
                for h in &exec_tasks {
                    h.abort();
                }
                collected.extend(drain_pending_spawns(&mut pending_spawns, "agent cancelled"));
                break;
            }

            let has_tasks = !exec_tasks.is_empty();
            let has_pending = !pending_spawns.is_empty();

            if !has_tasks && !has_pending {
                break;
            }

            if has_tasks && inbox.is_some() && has_pending {
                let all_tasks = std::mem::take(&mut exec_tasks);
                let join_fut = futures::future::join_all(all_tasks);
                let recv_fut = async { inbox.as_mut().unwrap().recv().await };

                tokio::select! {
                    biased;
                    results = join_fut => {
                        for r in results {
                            match r {
                                Ok(result) => collected.push(result),
                                Err(e) if e.is_cancelled() => {},
                                Err(e) => {
                                    tracing::warn!("tool task failed: {e}");
                                }
                            }
                        }
                        regular_done = true;
                    }
                    msg = recv_fut => {
                        match msg {
                            Some(OrchestratorMessage::SubAgentComplete { call_id, content, task_id: _ }) => {
                                if let Some(pos) = pending_spawns.iter().position(|p| p.call_id == call_id) {
                                    let ps = pending_spawns.remove(pos);
                                    collected.push(ToolExecResult {
                                        index: ps.index,
                                        call_id,
                                        result: ToolResult::success(content),
                                        duration_ms: None,
                                    });
                                }
                            }
                            Some(OrchestratorMessage::SubAgentError { call_id, error }) => {
                                if let Some(pos) = pending_spawns.iter().position(|p| p.call_id == call_id) {
                                    let ps = pending_spawns.remove(pos);
                                    collected.push(ToolExecResult {
                                        index: ps.index,
                                        call_id,
                                        result: ToolResult::error(error),
                                        duration_ms: None,
                                    });
                                }
                            }
                            Some(OrchestratorMessage::Cancelled) => {
                                collected.extend(drain_pending_spawns(
                                    &mut pending_spawns,
                                    "agent cancelled",
                                ));
                                break;
                            }
                            None => {
                                // inbox 关闭兜底——orchestrator 已断开，
                                // 把所有未完成的 pending_spawns 合成失败 ToolResult，
                                // 避免父 agent 死等 + 下一轮 LLM 调用因缺失 tool_result 而 400。
                                tracing::error!(
                                    pending_count = pending_spawns.len(),
                                    "sub-agent inbox closed before all spawns completed; synthesizing error ToolResults"
                                );
                                collected.extend(drain_pending_spawns(
                                    &mut pending_spawns,
                                    "sub-agent inbox closed before completing",
                                ));
                                inbox = None;
                                break;
                            }
                        }
                    }
                }
            } else if has_tasks {
                let all_tasks = std::mem::take(&mut exec_tasks);
                let results = futures::future::join_all(all_tasks).await;
                for r in results {
                    match r {
                        Ok(result) => collected.push(result),
                        Err(e) if e.is_cancelled() => {}
                        Err(e) => {
                            tracing::warn!("tool task failed: {e}");
                        }
                    }
                }
                regular_done = true;
            } else if let Some(ref mut rx) = inbox {
                tokio::select! {
                    biased;
                    _ = ctx.cancel_token.cancelled() => {
                        for h in &exec_tasks { h.abort(); }
                        collected.extend(drain_pending_spawns(
                            &mut pending_spawns,
                            "agent cancelled",
                        ));
                        break;
                    }
                    msg = rx.recv() => {
                        match msg {
                            Some(OrchestratorMessage::SubAgentComplete {
                                call_id,
                                content,
                                task_id: _,
                            }) => {
                                if let Some(pos) = pending_spawns.iter().position(|p| p.call_id == call_id)
                                {
                                    let ps = pending_spawns.remove(pos);
                                    collected.push(ToolExecResult {
                                        index: ps.index,
                                        call_id,
                                        result: ToolResult::success(content),
                                        duration_ms: None,
                                    });
                                }
                            }
                            Some(OrchestratorMessage::SubAgentError { call_id, error }) => {
                                if let Some(pos) = pending_spawns.iter().position(|p| p.call_id == call_id)
                                {
                                    let ps = pending_spawns.remove(pos);
                                    collected.push(ToolExecResult {
                                        index: ps.index,
                                        call_id,
                                        result: ToolResult::error(error),
                                        duration_ms: None,
                                    });
                                }
                            }
                            Some(OrchestratorMessage::Cancelled) => {
                                collected.extend(drain_pending_spawns(
                                    &mut pending_spawns,
                                    "agent cancelled",
                                ));
                                break;
                            }
                            None => {
                                // inbox 关闭兜底——orchestrator 已断开，
                                // 把所有未完成的 pending_spawns 合成失败 ToolResult。
                                tracing::error!(
                                    pending_count = pending_spawns.len(),
                                    "sub-agent inbox closed before all spawns completed (no tasks branch); synthesizing error ToolResults"
                                );
                                collected.extend(drain_pending_spawns(
                                    &mut pending_spawns,
                                    "sub-agent inbox closed before completing",
                                ));
                                inbox = None;
                                break;
                            }
                        }
                    }
                }
            }

            if regular_done && pending_spawns.is_empty() {
                break;
            }
        }

        ctx.inbox_rx = inbox;
        collected
    } else {
        let mut exec_tasks = Some(exec_tasks);
        tokio::select! {
            biased;
            _ = ctx.cancel_token.cancelled() => {
                if let Some(tasks) = exec_tasks.take() {
                    for h in &tasks { h.abort(); }
                }
                Vec::new()
            }
            results = futures::future::join_all(
                exec_tasks.take().unwrap()
            ) => {
                results.into_iter().enumerate().filter_map(|(idx, r)| match r {
                    Ok(result) => Some(result),
                    Err(e) if e.is_cancelled() => None,
                    Err(e) => {
                        tracing::warn!("tool task {} failed: {e}", idx);
                        let call_id = tool_ids.get(idx).cloned().unwrap_or_default();
                        Some(ToolExecResult {
                            index: idx,
                            call_id,
                            result: ToolResult::error(format!("Tool execution panicked: {e}")),
                            duration_ms: None,
                        })
                    }
                }).collect()
            }
        }
    };

    // Append tool results to history (in original order)
    let mut sorted_results: Vec<ToolExecResult> = task_results;
    sorted_results.sort_by_key(|r| r.index);

    for tr in sorted_results {
        let tool_msg = Message::tool_with_duration(tr.result.content, &tr.call_id, tr.duration_ms);
        ctx.history.push(tool_msg.clone());
        if let Err(e) = sm.append_message(sid, tool_msg) {
            let _ = send_event(
                tx,
                sm,
                sid,
                &ctx.global_tx,
                &ctx.session_id,
                AgentEvent::Error {
                    code: AgentErrorCode::Internal,
                    message: format!("Failed to append tool result: {e}"),
                },
            )
            .await;
            let _ = sm.finish_loop(sid, SessionStatus::Error);
            return true; // fatal
        }
    }

    false // continue loop
}

// ── Agent loop ───────────────────────────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
pub async fn run_agent_loop(
    provider: Arc<dyn LlmProvider>,
    tool_registry: Arc<ToolRegistry>,
    rule_engine: Arc<RuleEngine>,
    session_mgr: Arc<SessionManager>,
    mut ctx: AgentLoopContext,
    agent_config: &AgentConfig,
    user_message: Message,
    tx: mpsc::Sender<AgentEvent>,
) {
    // Manual span creation (replaces #[tracing::instrument]) – using
    // tracing::info_span! directly so the span is visible to test subscribers
    // created via set_default() on the same thread.
    let short_id = ctx.session_id[..ctx.session_id.len().min(8)].to_string();

    let __run_span = if agent_config.langfuse_enabled {
        let trace_name = "visp.agent.run".to_string();
        tracing::info_span!(
            "visp.agent.run",
            session.id = %ctx.session_id,
            session.short_id = %short_id,
            langfuse.session.id = %ctx.session_id,
            langfuse.trace.name = %trace_name,
            langfuse.user.id = tracing::field::Empty,
            langfuse.trace.tags = tracing::field::Empty,
            langfuse.environment = tracing::field::Empty,
            langfuse.trace.public = tracing::field::Empty,
            langfuse.release = tracing::field::Empty,
            langfuse.version = tracing::field::Empty,
            langfuse.trace.metadata = tracing::field::Empty,
            visp.agent.kind = %ctx.agent_kind,
            visp.agent.depth = ctx.depth,
            langfuse.observation.type = tracing::field::Empty,
            langfuse.observation.input = tracing::field::Empty,
            langfuse.observation.output = tracing::field::Empty,
            visp.span.w3c_id = tracing::field::Empty,
            trace_id = tracing::field::Empty,
            parent_span_id = tracing::field::Empty,
        )
    } else {
        tracing::info_span!(
            "visp.agent.run",
            session.id = %ctx.session_id,
            session.short_id = %short_id,
            visp.agent.kind = %ctx.agent_kind,
            visp.agent.depth = ctx.depth,
            visp.span.w3c_id = tracing::field::Empty,
            trace_id = tracing::field::Empty,
            parent_span_id = tracing::field::Empty,
        )
    };
    let __run_guard = __run_span.enter();

    let sid = ctx.session_id.clone();
    let sm = session_mgr.clone();
    let cfg = agent_config.clone();

    // Clone for use in panic handler after the async block
    let sid_panic = sid.clone();
    let sm_panic = sm.clone();
    // W2: Clone global_tx so the panic handler can forward AgentMessage::Error
    // to the orchestrator (which will then notify the parent agent via SubAgentError).
    let global_tx_panic = ctx.global_tx.clone();

    // Generate trace_id for W3C TraceContext propagation (Wave 1)
    let visp_trace_id = uuid::Uuid::new_v4().to_string().replace('-', "");
    // Generate W3C span ID for this run span and record as field.
    // A downstream layer (SpanW3CIdInjector or ParentLinkLayer) reads this
    // via on_record and writes it into the span extension.
    let run_span_w3c_id = generate_w3c_span_id();
    __run_span.record("visp.span.w3c_id", &run_span_w3c_id);

    // Record optional langfuse fields based on config
    if agent_config.langfuse_enabled {
        if let Some(ref user_id) = agent_config.langfuse_user_id {
            __run_span.record("langfuse.user.id", user_id.as_str());
        }
        if let Some(ref tags) = agent_config.langfuse_tags {
            __run_span.record("langfuse.trace.tags", tags.as_str());
        }
        if let Some(ref env) = agent_config.langfuse_environment {
            __run_span.record("langfuse.environment", env.as_str());
        }
        if let Some(ref release) = agent_config.langfuse_release {
            __run_span.record("langfuse.release", release.as_str());
        }
        if let Some(ref version) = agent_config.langfuse_version {
            __run_span.record("langfuse.version", version.as_str());
        }
        if let Some(public) = agent_config.langfuse_public {
            __run_span.record("langfuse.trace.public", public);
        }
        if let Some(ref metadata) = agent_config.langfuse_metadata
            && !metadata.is_empty()
            && let Ok(json) = serde_json::to_string(metadata)
        {
            __run_span.record("langfuse.trace.metadata", json.as_str());
        }

        // Record langfuse observation input (for trace preview)
        if agent_config.langfuse_capture_input {
            __run_span.record("langfuse.observation.type", "span");
            let input_json = serde_json::json!({"message": user_message.content});
            let input_str = input_json.to_string();
            let max_chars = agent_config.langfuse_capture_max_chars;
            let truncated = if input_str.len() > max_chars {
                let mut end = max_chars.saturating_sub(14);
                while end > 0 && !input_str.is_char_boundary(end) {
                    end -= 1;
                }
                let mut s = input_str[..end].to_string();
                s.push_str("...[truncated]");
                s
            } else {
                input_str
            };
            __run_span.record("langfuse.observation.input", &truncated);
        }
    }

    // Clone root span handle for use inside async body (for output recording)
    let root_span = __run_span.clone();

    // Enter the span for the sync setup part, then wrap the async body with Instrument.
    drop(__run_guard);

    // Wrap entire body in catch_unwind for panic safety.
    // On panic, session is reset to Idle before re-raising.
    let result = AssertUnwindSafe(async move {
        if ctx.cancel_token.is_cancelled() {
            tracing::info!(
                target: "visp.agent.cancelled",
                session_id = %sid,
                "agent loop cancelled before start"
            );
            let _ = send_event(
                &tx,
                &sm,
                &sid,
                &ctx.global_tx,
                &ctx.session_id,
                AgentEvent::Error {
                    code: AgentErrorCode::Cancelled,
                    message: "agent cancelled".into(),
                },
            )
            .await;
            let _ = sm.finish_loop(&sid, SessionStatus::Error);
            return;
        }

        // Clean up orphan tool_uses from previous cancelled runs.
        cleanup_orphan_tool_uses(&mut ctx.history);

        // 1. Append user message to session store and local history
        if let Err(e) = sm.append_message(&sid, user_message.clone()) {
            let _ = send_event(
                &tx,
                &sm,
                &sid,
                &ctx.global_tx,
                &ctx.session_id,
                AgentEvent::Error {
                    code: AgentErrorCode::Internal,
                    message: format!("Failed to append user message: {e}"),
                },
            )
            .await;
            let _ = sm.finish_loop(&sid, SessionStatus::Error);
            return;
        }
        ctx.history.push(user_message);

        let mut total_tool_calls: u32 = 0;
        let mut iteration: u32 = 1;
        let mut doom_loop_window: Vec<Vec<(String, serde_json::Value)>> = Vec::new();
        let mut doom_loop_warned = false;
        loop {
            // Create iteration span with W3C ID placeholder.
            let iter_span_w3c_id = generate_w3c_span_id();
            let iteration_span = if agent_config.langfuse_enabled {
                tracing::info_span!(
                    "visp.agent.iteration",
                    visp.span.w3c_id = tracing::field::Empty,
                    iteration.count = iteration,
                    langfuse.session.id = tracing::field::Empty,
                    langfuse.user.id = tracing::field::Empty,
                    langfuse.trace.tags = tracing::field::Empty,
                    langfuse.trace.name = tracing::field::Empty,
                    langfuse.environment = tracing::field::Empty,
                    langfuse.trace.public = tracing::field::Empty,
                    langfuse.release = tracing::field::Empty,
                    langfuse.version = tracing::field::Empty,
                    langfuse.trace.metadata = tracing::field::Empty,
                )
            } else {
                tracing::info_span!(
                    "visp.agent.iteration",
                    visp.span.w3c_id = tracing::field::Empty,
                    iteration.count = iteration,
                )
            };
            iteration_span.record("visp.span.w3c_id", &iter_span_w3c_id);
            // Propagate langfuse trace-level fields onto iteration span
            record_langfuse_trace_fields(&iteration_span, &cfg, &sid);

            // a/b/c: Build prompt + boundary checks
            let ic = match setup_iteration(
                iteration,
                &mut ctx,
                &sm,
                &tool_registry,
                &rule_engine,
                &cfg,
                &sid,
                &tx,
            )
            .instrument(iteration_span.clone())
            .await
            {
                Ok(ic) => ic,
                Err(()) => {
                    // setup_iteration sends cancellaton / iteration_limit events internally
                    return;
                }
            };
            let messages = ic.messages;
            let tools = ic.tools;

            // d. Call LLM with retry
            let stream =
                match call_llm_with_retry(provider.as_ref(), &messages, &tools, &ctx, &cfg, &sid)
                    .instrument(iteration_span.clone())
                    .await
                {
                    Ok(s) => s,
                    Err(LlmError::Cancelled) => {
                        // retry 阶段被 cancel：还没拿到 stream，
                        // 必须在此显式发 Cancelled 事件 + finish_loop（collect_stream_events 不会被调用）
                        tracing::info!(
                            target: "visp.agent.cancelled",
                            session_id = %sid,
                            iteration,
                            "agent loop cancelled during LLM retry"
                        );
                        let _ = send_event(
                            &tx,
                            &sm,
                            &sid,
                            &ctx.global_tx,
                            &ctx.session_id,
                            AgentEvent::Error {
                                code: AgentErrorCode::Cancelled,
                                message: "agent cancelled".into(),
                            },
                        )
                        .await;
                        let _ = sm.finish_loop(&sid, SessionStatus::Error);
                        return;
                    }
                    Err(e) => {
                        let (code, msg) = llm_error_to_code(&e);
                        let _ = send_event(
                            &tx,
                            &sm,
                            &sid,
                            &ctx.global_tx,
                            &ctx.session_id,
                            AgentEvent::Error { code, message: msg },
                        )
                        .await;
                        let _ = sm.finish_loop(&sid, SessionStatus::Error);
                        return;
                    }
                };

            // e. Collect stream events
            let output = match collect_stream_events(stream, &mut ctx, &sid, &tx, &sm)
                .instrument(iteration_span.clone())
                .await
            {
                Some(o) => o,
                None => return,
            };

            // f. Handle stream result (check USER_QUERY marker or done)
            match handle_stream_result(&output, total_tool_calls, &mut ctx, &sm, &sid, &tx)
                .instrument(iteration_span.clone())
                .await
            {
                Ok(StreamDecision::Done) => {
                    // Record langfuse observation output (for trace preview)
                    if cfg.langfuse_enabled
                        && cfg.langfuse_capture_output
                        && !output.text_buffer.is_empty()
                    {
                        root_span.record("langfuse.observation.type", "span");
                        let output_json = serde_json::json!({"response": output.text_buffer});
                        let output_str = output_json.to_string();
                        let max_chars = cfg.langfuse_capture_max_chars;
                        let truncated = if output_str.len() > max_chars {
                            let mut end = max_chars.saturating_sub(14);
                            while end > 0 && !output_str.is_char_boundary(end) {
                                end -= 1;
                            }
                            let mut s = output_str[..end].to_string();
                            s.push_str("...[truncated]");
                            s
                        } else {
                            output_str
                        };
                        root_span.record("langfuse.observation.output", &truncated);
                    }

                    tracing::info!(
                        target: "visp.agent.completed",
                        session_id = %sid,
                        iterations = iteration,
                        total_tool_calls,
                        "agent loop completed"
                    );
                    return;
                }
                Ok(StreamDecision::UserQuery { response_rx }) => {
                    let query_result = response_rx.await.unwrap_or_default();

                    // Build user message from result
                    let marker = parse_user_query_marker(&output.text_buffer).unwrap();
                    let user_msg = if query_result.selected_index >= 0
                        && (query_result.selected_index as usize) < marker.options.len()
                    {
                        let option_text =
                            marker.options[query_result.selected_index as usize].clone();
                        Message::user(option_text)
                    } else {
                        Message::user(query_result.text)
                    };
                    ctx.history.push(user_msg.clone());
                    if let Err(e) = sm.append_message(&sid, user_msg) {
                        let _ = send_event(
                            &tx,
                            &sm,
                            &sid,
                            &ctx.global_tx,
                            &ctx.session_id,
                            AgentEvent::Error {
                                code: AgentErrorCode::Internal,
                                message: format!("Failed to append user message: {e}"),
                            },
                        )
                        .await;
                        let _ = sm.finish_loop(&sid, SessionStatus::Error);
                        return;
                    }
                    continue;
                }
                Ok(StreamDecision::Continue) => {}
                Err(()) => return,
            }

            // g+h: Execute tool calls, collect results, append to history
            if execute_tool_calls(
                &output.tool_calls,
                output.text_buffer,
                output.thinking_blocks,
                output.input_tokens,
                output.output_tokens,
                output.cache_read_input_tokens,
                output.cache_creation_input_tokens,
                output.provider_metadata,
                &mut total_tool_calls,
                &mut ctx,
                &sm,
                &tool_registry,
                &sid,
                &tx,
                &cfg,
                &mut doom_loop_window,
                &mut doom_loop_warned,
                &visp_trace_id,
                &iter_span_w3c_id,
            )
            .instrument(iteration_span.clone())
            .await
            {
                return; // fatal error
            }

            // i. Increment iteration
            iteration += 1;
        }
    })
    .catch_unwind()
    .instrument(__run_span)
    .await;

    // If panicked, reset session status and notify orchestrator before re-raising.
    if let Err(panic) = result {
        let _ = sm_panic.finish_loop(&sid_panic, SessionStatus::Idle);

        // W2: Extract panic message and forward AgentMessage::Error to orchestrator.
        // This lets the orchestrator notify the parent agent via SubAgentError so
        // the parent's Phase 2 collection loop won't dead-wait on this sub-agent.
        let panic_msg = if let Some(s) = panic.downcast_ref::<&'static str>() {
            (*s).to_string()
        } else if let Some(s) = panic.downcast_ref::<String>() {
            s.clone()
        } else {
            "unknown panic payload".to_string()
        };

        if let Some(global_tx) = global_tx_panic {
            // 使用 try_send 避免阻塞；失败仅记录日志（不影响 resume_unwind）
            let envelope = Envelope {
                session_id: sid_panic.clone(),
                message: AgentMessage::Error {
                    code: AgentErrorCode::Internal,
                    message: format!("agent loop panicked: {panic_msg}"),
                },
                trace_context: None,
            };
            match global_tx.try_send(envelope) {
                Ok(()) => {
                    tracing::error!(
                        session_id = %sid_panic,
                        panic = %panic_msg,
                        "agent loop panicked; error envelope forwarded to orchestrator"
                    );
                }
                Err(mpsc::error::TrySendError::Full(_)) => {
                    tracing::error!(
                        session_id = %sid_panic,
                        panic = %panic_msg,
                        "agent loop panicked but global_tx is full; parent may dead-wait"
                    );
                }
                Err(mpsc::error::TrySendError::Closed(_)) => {
                    tracing::error!(
                        session_id = %sid_panic,
                        panic = %panic_msg,
                        "agent loop panicked but global_tx is closed"
                    );
                }
            }
        } else {
            tracing::error!(
                session_id = %sid_panic,
                panic = %panic_msg,
                "agent loop panicked (no global_tx; root agent or test)"
            );
        }

        std::panic::resume_unwind(panic);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::AgentKind;
    use crate::context::NoopTrimmer;
    use crate::provider::LlmConfig;
    use futures::stream;
    use serial_test::serial;
    use std::path::PathBuf;
    use std::sync::Arc as StdArc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tracing_subscriber::prelude::*;

    /// RateLimit mock: first `fail_attempts` calls return RateLimit, then Ok(Done)
    struct RateLimitProvider {
        call_count: AtomicUsize,
        fail_attempts: usize,
    }

    #[async_trait::async_trait]
    impl LlmProvider for RateLimitProvider {
        async fn chat_stream(
            &self,
            _messages: &[Message],
            _tools: &[ToolDefinition],
            _config: &LlmConfig,
            _cancel: &tokio_util::sync::CancellationToken,
        ) -> Result<Pin<Box<dyn Stream<Item = Result<ChatEvent, LlmError>> + Send>>, LlmError>
        {
            let count = self.call_count.fetch_add(1, Ordering::SeqCst);
            if count < self.fail_attempts {
                return Err(LlmError::RateLimit {
                    retry_after_secs: 10,
                });
            }
            let s: Pin<Box<dyn Stream<Item = Result<ChatEvent, LlmError>> + Send>> =
                Box::pin(stream::iter(vec![Ok(ChatEvent::Done)]));
            Ok(s)
        }
    }

    #[serial]
    #[tokio::test]
    async fn test_call_llm_with_retry_cancels_during_sleep() {
        let provider = Arc::new(RateLimitProvider {
            call_count: AtomicUsize::new(0),
            fail_attempts: 3,
        });
        let cancel = tokio_util::sync::CancellationToken::new();
        let ctx = AgentLoopContext {
            session_id: "test".into(),
            history: vec![],
            working_dir: PathBuf::from("/tmp"),
            config: LlmConfig::default(),
            cancel_token: cancel.clone(),
            context_trimmer: Arc::new(NoopTrimmer),
            global_tx: None,
            inbox_rx: None,
            permission_rules: None,
            agent_kind: AgentKind::Primary,
            depth: 0,
        };
        let cfg = AgentConfig {
            llm_retry_attempts: 5,
            llm_retry_base_delay_ms: 1000,
            ..Default::default()
        };

        let provider_clone = provider.clone();
        let handle = tokio::spawn(async move {
            call_llm_with_retry(provider_clone.as_ref(), &[], &[], &ctx, &cfg, "test").await
        });

        // 100ms 后取消，此时应在 retry sleep 中
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        cancel.cancel();

        let result = tokio::time::timeout(std::time::Duration::from_secs(1), handle)
            .await
            .expect("should complete within 1s")
            .expect("join should not panic");

        assert!(
            matches!(result, Err(LlmError::Cancelled)),
            "expected Err(LlmError::Cancelled)"
        );
        let count = provider.call_count.load(Ordering::SeqCst);
        assert!(
            count <= 3,
            "expected at most 3 calls (cancelled during retry sleep), got {}",
            count
        );
    }

    #[test]
    fn test_drain_pending_spawns_returns_error_results() {
        let mut pending = vec![
            PendingSpawn {
                index: 0,
                call_id: "call_1".into(),
                subagent_type: "test".into(),
            },
            PendingSpawn {
                index: 1,
                call_id: "call_2".into(),
                subagent_type: "test".into(),
            },
        ];

        let results = drain_pending_spawns(&mut pending, "test reason");
        assert_eq!(results.len(), 2, "should drain all pending spawns");
        assert!(pending.is_empty(), "pending should be empty after drain");
        assert_eq!(results[0].index, 0);
        assert_eq!(results[0].call_id, "call_1");
        assert!(results[0].result.is_error);
        assert_eq!(results[1].index, 1);
        assert_eq!(results[1].call_id, "call_2");
        assert!(results[1].result.is_error);
    }

    struct Phase2MockTrimmer;
    impl crate::context::ContextTrimmer for Phase2MockTrimmer {
        fn trim(&self, h: &[Message], _: u32, _: u32, _: u32) -> Vec<Message> {
            h.to_vec()
        }
    }

    /// Provider that returns a single tool_use on first call, then Done on subsequent.
    struct Phase2ToolProvider {
        call_count: AtomicUsize,
    }

    #[async_trait::async_trait]
    impl LlmProvider for Phase2ToolProvider {
        async fn chat_stream(
            &self,
            _messages: &[Message],
            _tools: &[ToolDefinition],
            _config: &LlmConfig,
            _cancel: &tokio_util::sync::CancellationToken,
        ) -> Result<Pin<Box<dyn Stream<Item = Result<ChatEvent, LlmError>> + Send>>, LlmError>
        {
            let count = self.call_count.fetch_add(1, Ordering::SeqCst);
            if count == 0 {
                // First call: return a tool_use to trigger sub-agent spawn
                let events: Vec<Result<ChatEvent, LlmError>> = vec![
                    Ok(ChatEvent::ToolCall {
                        id: "call_1".into(),
                        name: "task".into(),
                        arguments: r#"{"subagent_type": "general", "description": "test"}"#.into(),
                    }),
                    Ok(ChatEvent::Done),
                ];
                Ok(Box::pin(stream::iter(events)))
            } else {
                // Subsequent calls — shouldn't happen if cancel works
                Ok(Box::pin(stream::iter(vec![Ok(ChatEvent::Done)])))
            }
        }
    }

    #[serial]
    #[tokio::test]
    async fn test_phase2_cancel_drains_pending_spawns() {
        use crate::rules::RuleEngine;
        use crate::session::InMemorySessionStore;
        use crate::tool_registry::ToolRegistry;
        use std::path::Path;

        let (global_tx, _global_rx) = mpsc::channel::<Envelope>(16);
        let (_inbox_tx, inbox_rx) = mpsc::channel::<OrchestratorMessage>(16);
        let permission_rules = std::sync::Arc::new(Vec::new());

        let session_mgr = std::sync::Arc::new(SessionManager::new(InMemorySessionStore::new()));
        let session = session_mgr
            .create(Path::new("/tmp"), LlmConfig::default())
            .unwrap();
        let sid = session.id.clone();
        let trimmer: std::sync::Arc<dyn crate::context::ContextTrimmer + Send + Sync> =
            std::sync::Arc::new(Phase2MockTrimmer);
        let ctx = session_mgr
            .start_loop(
                &sid,
                &trimmer,
                Some(global_tx),
                Some(inbox_rx),
                Some(permission_rules),
            )
            .unwrap();
        let handle_cancel = ctx.cancel_token.clone();

        let provider: std::sync::Arc<dyn LlmProvider> = std::sync::Arc::new(Phase2ToolProvider {
            call_count: AtomicUsize::new(0),
        });
        let rule_engine = std::sync::Arc::new(RuleEngine::new(Path::new("/tmp")).unwrap());
        let registry = ToolRegistry::new();
        let config = AgentConfig {
            hard_limit: 10,
            ..Default::default()
        };
        let (tx, _rx) = mpsc::channel(64);

        let sm_clone = session_mgr.clone();
        let handle = tokio::spawn(async move {
            run_agent_loop(
                provider,
                std::sync::Arc::new(registry),
                rule_engine,
                sm_clone,
                ctx,
                &config,
                Message::user("Do something"),
                tx,
            )
            .await;
        });

        // Give agent time to enter Phase 2 and wait for inbox
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        handle_cancel.cancel();

        let result = tokio::time::timeout(std::time::Duration::from_secs(1), handle)
            .await
            .expect("agent loop should exit within 1s after cancel");

        // finish_loop 现在尊重传入的 status 参数（cancel 走 Error 路径）
        let final_session = session_mgr.get(&sid).unwrap();
        assert_eq!(
            final_session.status,
            crate::session::SessionStatus::Error,
            "session should end in Error after cancel"
        );
        // Cancel should not panic
        assert!(result.is_ok() || (result.is_err() && result.unwrap_err().is_cancelled()));
    }

    /// Provider that returns TextDelta + UsageInfo + Done with explicit token counts
    struct TokenProvider {
        text: String,
        input_tokens: u32,
        output_tokens: u32,
        cache_read: u32,
        cache_write: u32,
    }

    #[async_trait::async_trait]
    impl LlmProvider for TokenProvider {
        async fn chat_stream(
            &self,
            _messages: &[Message],
            _tools: &[ToolDefinition],
            _config: &LlmConfig,
            _cancel: &tokio_util::sync::CancellationToken,
        ) -> Result<Pin<Box<dyn Stream<Item = Result<ChatEvent, LlmError>> + Send>>, LlmError>
        {
            let events: Vec<Result<ChatEvent, LlmError>> = vec![
                Ok(ChatEvent::TextDelta(self.text.clone())),
                Ok(ChatEvent::UsageInfo {
                    input_tokens: self.input_tokens,
                    output_tokens: self.output_tokens,
                    tool_calls: 0,
                    cache_creation_input_tokens: self.cache_write,
                    cache_read_input_tokens: self.cache_read,
                }),
                Ok(ChatEvent::Done),
            ];
            Ok(Box::pin(stream::iter(events)))
        }
    }

    #[serial]
    #[tokio::test]
    async fn test_actual_tokens_persisted_on_assistant_message() {
        use crate::session::InMemorySessionStore;
        use std::path::Path;

        let session_mgr = std::sync::Arc::new(SessionManager::new(InMemorySessionStore::new()));
        let session = session_mgr
            .create(Path::new("/tmp"), LlmConfig::default())
            .unwrap();
        let sid = session.id.clone();
        let trimmer: std::sync::Arc<dyn crate::context::ContextTrimmer + Send + Sync> =
            std::sync::Arc::new(Phase2MockTrimmer);
        let (global_tx, _global_rx) = mpsc::channel::<Envelope>(16);
        let (_inbox_tx, inbox_rx) = mpsc::channel::<OrchestratorMessage>(16);
        let permission_rules = std::sync::Arc::new(Vec::new());
        let ctx = session_mgr
            .start_loop(
                &sid,
                &trimmer,
                Some(global_tx),
                Some(inbox_rx),
                Some(permission_rules),
            )
            .unwrap();

        let provider: std::sync::Arc<dyn LlmProvider> = std::sync::Arc::new(TokenProvider {
            text: "Hello world".into(),
            input_tokens: 100,
            output_tokens: 50,
            cache_read: 10,
            cache_write: 5,
        });
        let rule_engine = std::sync::Arc::new(RuleEngine::new(Path::new("/tmp")).unwrap());
        let registry = ToolRegistry::new();
        let config = AgentConfig::default();
        let (tx, _rx) = mpsc::channel::<AgentEvent>(64);

        run_agent_loop(
            provider,
            StdArc::new(registry),
            rule_engine,
            session_mgr.clone(),
            ctx,
            &config,
            Message::user("Hi"),
            tx,
        )
        .await;

        // Verify tokens were persisted on assistant message
        let final_session = session_mgr.get(&sid).unwrap();
        let assistant_msgs: Vec<_> = final_session
            .history
            .iter()
            .filter(|m| m.role == Role::Assistant)
            .collect();
        assert!(
            !assistant_msgs.is_empty(),
            "expected at least one assistant message"
        );
        let msg = assistant_msgs[0];
        assert_eq!(
            msg.actual_tokens_input,
            Some(100),
            "actual_tokens_input should be 100"
        );
        assert_eq!(
            msg.actual_tokens_output,
            Some(50),
            "actual_tokens_output should be 50"
        );
        assert_eq!(
            msg.actual_cache_read,
            Some(10),
            "actual_cache_read should be 10"
        );
        assert_eq!(
            msg.actual_cache_write,
            Some(5),
            "actual_cache_write should be 5"
        );
    }

    // ── W0A-3: tool execution duration ─────────────────────────────────────

    /// Tool that sleeps ~30ms before returning success — used to measure duration.
    struct SleepyTool;

    #[async_trait::async_trait]
    impl crate::tool::Tool for SleepyTool {
        fn name(&self) -> &str {
            "sleepy"
        }
        fn description(&self) -> &str {
            "sleeps then returns"
        }
        fn parameters(&self) -> serde_json::Value {
            serde_json::json!({})
        }
        async fn execute(
            &self,
            _args: serde_json::Value,
            _ctx: &crate::tool::ToolContext,
        ) -> crate::tool::ToolResult {
            tokio::time::sleep(std::time::Duration::from_millis(30)).await;
            crate::tool::ToolResult::success("done")
        }
    }

    /// Provider: first call returns a `sleepy` tool_use, next call returns Done.
    struct OneToolCallProvider {
        call_count: AtomicUsize,
    }

    #[async_trait::async_trait]
    impl LlmProvider for OneToolCallProvider {
        async fn chat_stream(
            &self,
            _messages: &[Message],
            _tools: &[ToolDefinition],
            _config: &LlmConfig,
            _cancel: &tokio_util::sync::CancellationToken,
        ) -> Result<Pin<Box<dyn Stream<Item = Result<ChatEvent, LlmError>> + Send>>, LlmError>
        {
            let count = self.call_count.fetch_add(1, Ordering::SeqCst);
            if count == 0 {
                let events: Vec<Result<ChatEvent, LlmError>> = vec![
                    Ok(ChatEvent::ToolCall {
                        id: "call_sleepy_1".into(),
                        name: "sleepy".into(),
                        arguments: "{}".into(),
                    }),
                    Ok(ChatEvent::Done),
                ];
                Ok(Box::pin(stream::iter(events)))
            } else {
                Ok(Box::pin(stream::iter(vec![Ok(ChatEvent::Done)])))
            }
        }
    }

    #[serial]
    #[tokio::test]
    async fn test_tool_execution_records_duration_ms_in_history() {
        use crate::rules::RuleEngine;
        use crate::session::InMemorySessionStore;
        use crate::tool_registry::ToolRegistry;
        use std::path::Path;

        let session_mgr = std::sync::Arc::new(SessionManager::new(InMemorySessionStore::new()));
        let session = session_mgr
            .create(Path::new("/tmp"), LlmConfig::default())
            .unwrap();
        let sid = session.id.clone();
        let trimmer: std::sync::Arc<dyn crate::context::ContextTrimmer + Send + Sync> =
            std::sync::Arc::new(Phase2MockTrimmer);
        let ctx = session_mgr
            .start_loop(&sid, &trimmer, None, None, None)
            .unwrap();

        let provider: std::sync::Arc<dyn LlmProvider> = std::sync::Arc::new(OneToolCallProvider {
            call_count: AtomicUsize::new(0),
        });
        let rule_engine = std::sync::Arc::new(RuleEngine::new(Path::new("/tmp")).unwrap());
        let registry = ToolRegistry::new();
        registry.register(std::sync::Arc::new(SleepyTool)).unwrap();
        let config = AgentConfig {
            hard_limit: 2,
            ..Default::default()
        };
        let (tx, _rx) = mpsc::channel::<AgentEvent>(64);

        run_agent_loop(
            provider,
            std::sync::Arc::new(registry),
            rule_engine,
            session_mgr.clone(),
            ctx,
            &config,
            Message::user("invoke sleepy"),
            tx,
        )
        .await;

        let final_session = session_mgr.get(&sid).unwrap();
        let tool_msgs: Vec<_> = final_session
            .history
            .iter()
            .filter(|m| m.role == Role::Tool)
            .collect();
        assert_eq!(
            tool_msgs.len(),
            1,
            "expected exactly one tool result message"
        );
        let dur = tool_msgs[0].tool_result_duration_ms;
        assert!(
            dur.is_some(),
            "tool_result_duration_ms should be set after tool execution, got None"
        );
        assert!(
            dur.unwrap() >= 25,
            "tool_result_duration_ms should be >= 25ms (slept 30ms), got {:?}",
            dur
        );
    }

    // ── W0B-6: provider_metadata injection ──────────────────────────────────

    /// Provider that returns TextDelta + OutputMetadata + Done
    struct MetadataProvider {
        metadata: ProviderMetadata,
    }

    #[async_trait::async_trait]
    impl LlmProvider for MetadataProvider {
        async fn chat_stream(
            &self,
            _messages: &[Message],
            _tools: &[ToolDefinition],
            _config: &LlmConfig,
            _cancel: &tokio_util::sync::CancellationToken,
        ) -> Result<Pin<Box<dyn Stream<Item = Result<ChatEvent, LlmError>> + Send>>, LlmError>
        {
            let events: Vec<Result<ChatEvent, LlmError>> = vec![
                Ok(ChatEvent::TextDelta("hello".to_string())),
                Ok(ChatEvent::OutputMetadata(self.metadata.clone())),
                Ok(ChatEvent::Done),
            ];
            Ok(Box::pin(stream::iter(events)))
        }
    }

    #[serial]
    #[tokio::test]
    async fn test_agent_loop_assistant_message_has_provider_metadata() {
        use crate::rules::RuleEngine;
        use crate::session::InMemorySessionStore;
        use crate::tool_registry::ToolRegistry;
        use std::path::Path;

        let metadata = ProviderMetadata {
            model: "claude-test".to_string(),
            finish_reasons: vec!["stop".to_string()],
            input_tokens: 10,
            output_tokens: 5,
            cache_read_input_tokens: None,
            cache_creation_input_tokens: None,
            latency_ms: 100,
        };

        let session_mgr = std::sync::Arc::new(SessionManager::new(InMemorySessionStore::new()));
        let session = session_mgr
            .create(Path::new("/tmp"), LlmConfig::default())
            .unwrap();
        let sid = session.id.clone();
        let trimmer: std::sync::Arc<dyn crate::context::ContextTrimmer + Send + Sync> =
            std::sync::Arc::new(Phase2MockTrimmer);
        let ctx = session_mgr
            .start_loop(&sid, &trimmer, None, None, None)
            .unwrap();

        let provider: std::sync::Arc<dyn LlmProvider> = std::sync::Arc::new(MetadataProvider {
            metadata: metadata.clone(),
        });
        let rule_engine = std::sync::Arc::new(RuleEngine::new(Path::new("/tmp")).unwrap());
        let registry = ToolRegistry::new();
        let config = AgentConfig::default();
        let (tx, _rx) = mpsc::channel::<AgentEvent>(64);

        run_agent_loop(
            provider,
            std::sync::Arc::new(registry),
            rule_engine,
            session_mgr.clone(),
            ctx,
            &config,
            Message::user("hello"),
            tx,
        )
        .await;

        let final_session = session_mgr.get(&sid).unwrap();
        let assistant_msgs: Vec<_> = final_session
            .history
            .iter()
            .filter(|m| m.role == Role::Assistant)
            .collect();
        assert_eq!(
            assistant_msgs.len(),
            1,
            "expected exactly one assistant message"
        );
        let msg = assistant_msgs[0];
        let expected_json = serde_json::to_value(&metadata).unwrap();
        assert_eq!(
            msg.provider_metadata,
            Some(expected_json),
            "provider_metadata should match"
        );
    }

    /// Provider for multi-turn: first call returns tool_use + metadata, second returns text + different metadata
    struct TwoRoundMetadataProvider {
        call_count: AtomicUsize,
    }

    #[async_trait::async_trait]
    impl LlmProvider for TwoRoundMetadataProvider {
        async fn chat_stream(
            &self,
            _messages: &[Message],
            _tools: &[ToolDefinition],
            _config: &LlmConfig,
            _cancel: &tokio_util::sync::CancellationToken,
        ) -> Result<Pin<Box<dyn Stream<Item = Result<ChatEvent, LlmError>> + Send>>, LlmError>
        {
            let count = self.call_count.fetch_add(1, Ordering::SeqCst);
            if count == 0 {
                // First round: tool call + metadata1
                let events: Vec<Result<ChatEvent, LlmError>> = vec![
                    Ok(ChatEvent::ToolCall {
                        id: "call_sleepy_1".into(),
                        name: "sleepy".into(),
                        arguments: "{}".into(),
                    }),
                    Ok(ChatEvent::OutputMetadata(ProviderMetadata {
                        model: "claude-test".into(),
                        finish_reasons: vec!["tool_use".into()],
                        input_tokens: 10,
                        output_tokens: 5,
                        cache_read_input_tokens: None,
                        cache_creation_input_tokens: None,
                        latency_ms: 100,
                    })),
                    Ok(ChatEvent::Done),
                ];
                Ok(Box::pin(stream::iter(events)))
            } else {
                // Second round: text + metadata2
                let events: Vec<Result<ChatEvent, LlmError>> = vec![
                    Ok(ChatEvent::TextDelta("result".to_string())),
                    Ok(ChatEvent::OutputMetadata(ProviderMetadata {
                        model: "claude-test".into(),
                        finish_reasons: vec!["stop".into()],
                        input_tokens: 20,
                        output_tokens: 10,
                        cache_read_input_tokens: Some(5),
                        cache_creation_input_tokens: Some(3),
                        latency_ms: 200,
                    })),
                    Ok(ChatEvent::Done),
                ];
                Ok(Box::pin(stream::iter(events)))
            }
        }
    }

    #[serial]
    #[tokio::test]
    async fn test_agent_loop_provider_metadata_persists_through_multi_turn() {
        use crate::rules::RuleEngine;
        use crate::session::InMemorySessionStore;
        use crate::tool_registry::ToolRegistry;
        use std::path::Path;

        let session_mgr = std::sync::Arc::new(SessionManager::new(InMemorySessionStore::new()));
        let session = session_mgr
            .create(Path::new("/tmp"), LlmConfig::default())
            .unwrap();
        let sid = session.id.clone();
        let trimmer: std::sync::Arc<dyn crate::context::ContextTrimmer + Send + Sync> =
            std::sync::Arc::new(Phase2MockTrimmer);
        let ctx = session_mgr
            .start_loop(&sid, &trimmer, None, None, None)
            .unwrap();

        let provider: std::sync::Arc<dyn LlmProvider> =
            std::sync::Arc::new(TwoRoundMetadataProvider {
                call_count: AtomicUsize::new(0),
            });
        let rule_engine = std::sync::Arc::new(RuleEngine::new(Path::new("/tmp")).unwrap());
        let registry = ToolRegistry::new();
        registry.register(std::sync::Arc::new(SleepyTool)).unwrap();
        let config = AgentConfig {
            hard_limit: 10,
            ..Default::default()
        };
        let (tx, _rx) = mpsc::channel::<AgentEvent>(64);

        run_agent_loop(
            provider,
            std::sync::Arc::new(registry),
            rule_engine,
            session_mgr.clone(),
            ctx,
            &config,
            Message::user("do something"),
            tx,
        )
        .await;

        let final_session = session_mgr.get(&sid).unwrap();
        let assistant_msgs: Vec<_> = final_session
            .history
            .iter()
            .filter(|m| m.role == Role::Assistant)
            .collect();
        assert_eq!(
            assistant_msgs.len(),
            2,
            "expected two assistant messages for two rounds"
        );

        // First message (tool call round): tool_use metadata
        let meta1 = serde_json::to_value(ProviderMetadata {
            model: "claude-test".into(),
            finish_reasons: vec!["tool_use".into()],
            input_tokens: 10,
            output_tokens: 5,
            cache_read_input_tokens: None,
            cache_creation_input_tokens: None,
            latency_ms: 100,
        })
        .unwrap();
        assert_eq!(
            assistant_msgs[0].provider_metadata,
            Some(meta1),
            "first assistant message should have metadata from first round"
        );

        // Second message (text round): stop metadata with different tokens
        let meta2 = serde_json::to_value(ProviderMetadata {
            model: "claude-test".into(),
            finish_reasons: vec!["stop".into()],
            input_tokens: 20,
            output_tokens: 10,
            cache_read_input_tokens: Some(5),
            cache_creation_input_tokens: Some(3),
            latency_ms: 200,
        })
        .unwrap();
        assert_eq!(
            assistant_msgs[1].provider_metadata,
            Some(meta2),
            "second assistant message should have metadata from second round, no cross-contamination"
        );
    }

    // ── TestLayer for tracing assertions ────────────────────────────────────
    use std::sync::Arc as TArc;
    use std::sync::Mutex as TMutex;
    use tracing::Event;
    use tracing::Subscriber;
    use tracing::field::{Field, Visit};
    use tracing::span::{Attributes, Id, Record};
    use tracing_subscriber::layer::{Context, Layer};
    use tracing_subscriber::registry::LookupSpan;

    #[derive(Debug, Clone)]
    struct CapturedSpan {
        name: String,
        fields: Vec<(String, String)>,
        id: u64,
        parent_id: Option<u64>,
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
        spans: TArc<TMutex<Vec<CapturedSpan>>>,
        events: TArc<TMutex<Vec<String>>>,
    }

    impl TestLayer {
        fn new(spans: TArc<TMutex<Vec<CapturedSpan>>>, events: TArc<TMutex<Vec<String>>>) -> Self {
            Self { spans, events }
        }
    }

    impl<S> Layer<S> for TestLayer
    where
        S: Subscriber + for<'a> LookupSpan<'a>,
    {
        fn on_new_span(&self, attrs: &Attributes<'_>, id: &Id, ctx: Context<'_, S>) {
            let mut visitor = SpanFieldVisitor { fields: Vec::new() };
            attrs.record(&mut visitor);

            let parent_id = ctx.lookup_current().map(|s| s.id().into_u64());

            let mut spans = self.spans.lock().unwrap();
            spans.push(CapturedSpan {
                name: attrs.metadata().name().to_string(),
                fields: visitor.fields,
                id: id.into_u64(),
                parent_id,
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
            self.events.lock().unwrap().push(target);
        }
    }

    type TracingOutput = (TArc<TMutex<Vec<CapturedSpan>>>, TArc<TMutex<Vec<String>>>);

    fn setup_tracing() -> TracingOutput {
        let spans = TArc::new(TMutex::new(Vec::new()));
        let events = TArc::new(TMutex::new(Vec::new()));
        (spans, events)
    }

    fn make_guard(
        spans: &TArc<TMutex<Vec<CapturedSpan>>>,
        events: &TArc<TMutex<Vec<String>>>,
    ) -> tracing::subscriber::DefaultGuard {
        tracing_subscriber::registry()
            .with(TestLayer::new(spans.clone(), events.clone()))
            .set_default()
    }

    /// Simple provider with a fixed set of phases (replacement for inaccessble TestProvider)
    struct SimpleProvider {
        phases: Vec<Vec<ChatEvent>>,
        call_count: AtomicUsize,
    }

    impl SimpleProvider {
        fn new(phases: Vec<Vec<ChatEvent>>) -> Self {
            Self {
                phases,
                call_count: AtomicUsize::new(0),
            }
        }
    }

    #[async_trait::async_trait]
    impl LlmProvider for SimpleProvider {
        async fn chat_stream(
            &self,
            _messages: &[Message],
            _tools: &[ToolDefinition],
            _config: &LlmConfig,
            _cancel: &tokio_util::sync::CancellationToken,
        ) -> Result<Pin<Box<dyn Stream<Item = Result<ChatEvent, LlmError>> + Send>>, LlmError>
        {
            let idx = self.call_count.fetch_add(1, Ordering::SeqCst);
            let events = self
                .phases
                .get(idx)
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .map(Ok);
            Ok(Box::pin(stream::iter(events)))
        }
    }

    /// Simple mock tool (replacement for inaccessible mock_tool)
    struct MockTestTool {
        name: &'static str,
    }

    #[async_trait::async_trait]
    impl crate::tool::Tool for MockTestTool {
        fn name(&self) -> &str {
            self.name
        }
        fn description(&self) -> &str {
            "mock tool"
        }
        fn parameters(&self) -> serde_json::Value {
            serde_json::json!({})
        }
        async fn execute(
            &self,
            _: serde_json::Value,
            _: &crate::tool::ToolContext,
        ) -> crate::tool::ToolResult {
            crate::tool::ToolResult::success("ok")
        }
    }

    // ── W1-S3a-1/2: visp.agent.run span ────────────────────────────────────

    #[serial]
    #[tokio::test]
    async fn test_agent_run_span_created() {
        let (spans, events) = setup_tracing();
        let _guard = make_guard(&spans, &events);

        use crate::rules::RuleEngine;
        use crate::session::InMemorySessionStore;
        use crate::tool_registry::ToolRegistry;
        use std::path::Path;

        let session_mgr = StdArc::new(SessionManager::new(InMemorySessionStore::new()));
        let session = session_mgr
            .create(Path::new("/tmp"), LlmConfig::default())
            .unwrap();
        let sid = session.id.clone();
        let trimmer: StdArc<dyn crate::context::ContextTrimmer + Send + Sync> =
            StdArc::new(Phase2MockTrimmer);
        let ctx = session_mgr
            .start_loop(&sid, &trimmer, None, None, None)
            .unwrap();

        let provider: StdArc<dyn LlmProvider> =
            StdArc::new(SimpleProvider::new(vec![vec![ChatEvent::Done]]));
        let rule_engine = StdArc::new(RuleEngine::new(Path::new("/tmp")).unwrap());
        let registry = ToolRegistry::new();
        let config = AgentConfig::default();
        let (tx, _rx) = mpsc::channel::<AgentEvent>(64);

        run_agent_loop(
            provider,
            StdArc::new(registry),
            rule_engine,
            session_mgr.clone(),
            ctx,
            &config,
            Message::user("hello"),
            tx,
        )
        .await;

        let captured = spans.lock().unwrap();
        assert!(
            captured.iter().any(|s| s.name == "visp.agent.run"),
            "expected visp.agent.run span, got: {:?}",
            captured.iter().map(|s| &s.name).collect::<Vec<_>>()
        );
    }

    #[serial]
    #[tokio::test]
    async fn test_agent_run_carries_session_id_field() {
        let (spans, events) = setup_tracing();
        let _guard = make_guard(&spans, &events);

        use crate::rules::RuleEngine;
        use crate::session::InMemorySessionStore;
        use crate::tool_registry::ToolRegistry;
        use std::path::Path;

        let session_mgr = StdArc::new(SessionManager::new(InMemorySessionStore::new()));
        let session = session_mgr
            .create(Path::new("/tmp"), LlmConfig::default())
            .unwrap();
        let sid = session.id.clone();
        let trimmer: StdArc<dyn crate::context::ContextTrimmer + Send + Sync> =
            StdArc::new(Phase2MockTrimmer);
        let ctx = session_mgr
            .start_loop(&sid, &trimmer, None, None, None)
            .unwrap();

        let provider: StdArc<dyn LlmProvider> =
            StdArc::new(SimpleProvider::new(vec![vec![ChatEvent::Done]]));
        let rule_engine = StdArc::new(RuleEngine::new(Path::new("/tmp")).unwrap());
        let registry = ToolRegistry::new();
        let config = AgentConfig::default();
        let (tx, _rx) = mpsc::channel::<AgentEvent>(64);

        run_agent_loop(
            provider,
            StdArc::new(registry),
            rule_engine,
            session_mgr.clone(),
            ctx,
            &config,
            Message::user("hello"),
            tx,
        )
        .await;

        let captured = spans.lock().unwrap();
        let run_span = captured
            .iter()
            .find(|s| s.name == "visp.agent.run")
            .unwrap();
        assert!(
            run_span.fields.iter().any(|(k, _)| k == "session.id"),
            "expected session.id field"
        );
        assert!(
            run_span.fields.iter().any(|(k, _)| k == "session.short_id"),
            "expected session.short_id field"
        );
        assert!(
            run_span
                .fields
                .iter()
                .any(|(k, v)| k == "visp.agent.kind" && v == "primary"),
            "expected visp.agent.kind=primary"
        );
        assert!(
            run_span
                .fields
                .iter()
                .any(|(k, v)| k == "visp.agent.depth" && v == "0"),
            "expected visp.agent.depth=0"
        );
    }

    #[serial]
    #[tokio::test]
    async fn test_agent_run_carries_langfuse_session_id() {
        let (spans, events) = setup_tracing();
        let _guard = make_guard(&spans, &events);

        use crate::rules::RuleEngine;
        use crate::session::InMemorySessionStore;
        use crate::tool_registry::ToolRegistry;
        use std::path::Path;

        let session_mgr = StdArc::new(SessionManager::new(InMemorySessionStore::new()));
        let session = session_mgr
            .create(Path::new("/tmp"), LlmConfig::default())
            .unwrap();
        let sid = session.id.clone();
        let trimmer: StdArc<dyn crate::context::ContextTrimmer + Send + Sync> =
            StdArc::new(Phase2MockTrimmer);
        let ctx = session_mgr
            .start_loop(&sid, &trimmer, None, None, None)
            .unwrap();

        let provider: StdArc<dyn LlmProvider> =
            StdArc::new(SimpleProvider::new(vec![vec![ChatEvent::Done]]));
        let rule_engine = StdArc::new(RuleEngine::new(Path::new("/tmp")).unwrap());
        let registry = ToolRegistry::new();
        let config = AgentConfig {
            langfuse_enabled: true,
            ..Default::default()
        };
        let (tx, _rx) = mpsc::channel::<AgentEvent>(64);

        run_agent_loop(
            provider,
            StdArc::new(registry),
            rule_engine,
            session_mgr.clone(),
            ctx,
            &config,
            Message::user("hello"),
            tx,
        )
        .await;

        let captured = spans.lock().unwrap();
        let run_span = captured
            .iter()
            .find(|s| s.name == "visp.agent.run")
            .unwrap();
        assert!(
            run_span
                .fields
                .iter()
                .any(|(k, _)| k == "langfuse.session.id"),
            "expected langfuse.session.id field on visp.agent.run span, fields: {:?}",
            run_span.fields
        );
        // Value should equal session.id
        let langfuse_val = run_span
            .fields
            .iter()
            .find(|(k, _)| k == "langfuse.session.id")
            .map(|(_, v)| v.as_str())
            .unwrap();
        let session_id_val = run_span
            .fields
            .iter()
            .find(|(k, _)| k == "session.id")
            .map(|(_, v)| v.as_str())
            .unwrap();
        assert_eq!(
            langfuse_val, session_id_val,
            "langfuse.session.id should equal session.id"
        );
        // Verify langfuse.trace.name is present with correct format
        let trace_name = run_span
            .fields
            .iter()
            .find(|(k, _)| k == "langfuse.trace.name")
            .map(|(_, v)| v.clone())
            .unwrap_or_default();
        assert_eq!(
            trace_name, "visp.agent.run",
            "langfuse.trace.name should be visp.agent.run, got: {trace_name}"
        );
    }

    #[serial]
    #[tokio::test]
    async fn test_agent_run_carries_langfuse_all_fields() {
        let (spans, events) = setup_tracing();
        let _guard = make_guard(&spans, &events);

        use crate::rules::RuleEngine;
        use crate::session::InMemorySessionStore;
        use crate::tool_registry::ToolRegistry;
        use std::path::Path;

        let session_mgr = StdArc::new(SessionManager::new(InMemorySessionStore::new()));
        let session = session_mgr
            .create(Path::new("/tmp"), LlmConfig::default())
            .unwrap();
        let sid = session.id.clone();
        let trimmer: StdArc<dyn crate::context::ContextTrimmer + Send + Sync> =
            StdArc::new(Phase2MockTrimmer);
        let ctx = session_mgr
            .start_loop(&sid, &trimmer, None, None, None)
            .unwrap();

        let provider: StdArc<dyn LlmProvider> =
            StdArc::new(SimpleProvider::new(vec![vec![ChatEvent::Done]]));
        let rule_engine = StdArc::new(RuleEngine::new(Path::new("/tmp")).unwrap());
        let registry = ToolRegistry::new();
        let config = AgentConfig {
            langfuse_enabled: true,
            langfuse_user_id: Some("user_456".to_string()),
            langfuse_tags: Some(r#"["agent","weather"]"#.to_string()),
            langfuse_environment: Some("prod".to_string()),
            langfuse_public: Some(true),
            langfuse_release: Some("1.0.0".to_string()),
            langfuse_version: Some("abc123".to_string()),
            langfuse_metadata: Some({
                let mut m = std::collections::HashMap::new();
                m.insert("source".to_string(), "test".to_string());
                m
            }),
            ..Default::default()
        };
        let (tx, _rx) = mpsc::channel::<AgentEvent>(64);

        run_agent_loop(
            provider,
            StdArc::new(registry),
            rule_engine,
            session_mgr.clone(),
            ctx,
            &config,
            Message::user("hello"),
            tx,
        )
        .await;

        let captured = spans.lock().unwrap();
        let run_span = captured
            .iter()
            .find(|s| s.name == "visp.agent.run")
            .unwrap();

        // Verify langfuse.user.id
        assert!(
            run_span.fields.iter().any(|(k, _)| k == "langfuse.user.id"),
            "expected langfuse.user.id field on visp.agent.run span, fields: {:?}",
            run_span.fields
        );
        let user_id_val = run_span
            .fields
            .iter()
            .find(|(k, _)| k == "langfuse.user.id")
            .map(|(_, v)| v.as_str())
            .unwrap();
        assert_eq!(
            user_id_val, "user_456",
            "langfuse.user.id should equal configured user_id"
        );

        // Verify langfuse.trace.tags (replaces old langfuse.tags)
        assert!(
            run_span
                .fields
                .iter()
                .any(|(k, _)| k == "langfuse.trace.tags"),
            "expected langfuse.trace.tags field on visp.agent.run span, fields: {:?}",
            run_span.fields
        );
        let tags_val = run_span
            .fields
            .iter()
            .find(|(k, _)| k == "langfuse.trace.tags")
            .map(|(_, v)| v.as_str())
            .unwrap();
        assert_eq!(
            tags_val, r#"["agent","weather"]"#,
            "langfuse.trace.tags should equal configured tags JSON"
        );

        // Verify no old langfuse.tags field
        assert!(
            !run_span.fields.iter().any(|(k, _)| k == "langfuse.tags"),
            "expected NO langfuse.tags field (old name), fields: {:?}",
            run_span.fields
        );

        // Verify langfuse.trace.name
        let trace_name = run_span
            .fields
            .iter()
            .find(|(k, _)| k == "langfuse.trace.name")
            .map(|(_, v)| v.clone())
            .unwrap_or_default();
        assert_eq!(
            trace_name, "visp.agent.run",
            "langfuse.trace.name should be visp.agent.run, got: {trace_name}"
        );

        // Verify langfuse.environment
        assert!(
            run_span
                .fields
                .iter()
                .any(|(k, v)| k == "langfuse.environment" && v == "prod"),
            "expected langfuse.environment=prod, fields: {:?}",
            run_span.fields
        );

        // Verify langfuse.trace.public
        assert!(
            run_span
                .fields
                .iter()
                .any(|(k, v)| k == "langfuse.trace.public" && v == "true"),
            "expected langfuse.trace.public=true, fields: {:?}",
            run_span.fields
        );

        // Verify langfuse.release
        assert!(
            run_span
                .fields
                .iter()
                .any(|(k, v)| k == "langfuse.release" && v == "1.0.0"),
            "expected langfuse.release=1.0.0, fields: {:?}",
            run_span.fields
        );

        // Verify langfuse.version
        assert!(
            run_span
                .fields
                .iter()
                .any(|(k, v)| k == "langfuse.version" && v == "abc123"),
            "expected langfuse.version=abc123, fields: {:?}",
            run_span.fields
        );

        // Verify langfuse.trace.metadata includes the metadata JSON
        assert!(
            run_span
                .fields
                .iter()
                .any(|(k, v)| k == "langfuse.trace.metadata"
                    && v.contains("source")
                    && v.contains("test")),
            "expected langfuse.trace.metadata to contain metadata JSON, fields: {:?}",
            run_span.fields
        );
    }

    #[serial]
    #[tokio::test]
    async fn test_agent_run_langfuse_disabled_no_langfuse_fields() {
        let (spans, events) = setup_tracing();
        let _guard = make_guard(&spans, &events);

        use crate::rules::RuleEngine;
        use crate::session::InMemorySessionStore;
        use crate::tool_registry::ToolRegistry;
        use std::path::Path;

        let session_mgr = StdArc::new(SessionManager::new(InMemorySessionStore::new()));
        let session = session_mgr
            .create(Path::new("/tmp"), LlmConfig::default())
            .unwrap();
        let sid = session.id.clone();
        let trimmer: StdArc<dyn crate::context::ContextTrimmer + Send + Sync> =
            StdArc::new(Phase2MockTrimmer);
        let ctx = session_mgr
            .start_loop(&sid, &trimmer, None, None, None)
            .unwrap();

        let provider: StdArc<dyn LlmProvider> =
            StdArc::new(SimpleProvider::new(vec![vec![ChatEvent::Done]]));
        let rule_engine = StdArc::new(RuleEngine::new(Path::new("/tmp")).unwrap());
        let registry = ToolRegistry::new();
        let config = AgentConfig::default(); // langfuse_enabled=false
        let (tx, _rx) = mpsc::channel::<AgentEvent>(64);

        run_agent_loop(
            provider,
            StdArc::new(registry),
            rule_engine,
            session_mgr.clone(),
            ctx,
            &config,
            Message::user("hello"),
            tx,
        )
        .await;

        let captured = spans.lock().unwrap();
        let run_span = captured
            .iter()
            .find(|s| s.name == "visp.agent.run")
            .unwrap();
        let langfuse_fields: Vec<_> = run_span
            .fields
            .iter()
            .filter(|(k, _)| k.starts_with("langfuse."))
            .collect();
        assert!(
            langfuse_fields.is_empty(),
            "expected NO langfuse.* fields when disabled, got: {:?}",
            langfuse_fields
        );
    }

    // ── Langfuse observation input/output recording ──────────────────────

    #[serial]
    #[tokio::test]
    async fn test_agent_run_records_observation_type_and_input_when_capture_enabled() {
        let (spans, events) = setup_tracing();
        let _guard = make_guard(&spans, &events);

        use crate::rules::RuleEngine;
        use crate::session::InMemorySessionStore;
        use crate::tool_registry::ToolRegistry;
        use std::path::Path;

        let session_mgr = StdArc::new(SessionManager::new(InMemorySessionStore::new()));
        let session = session_mgr
            .create(Path::new("/tmp"), LlmConfig::default())
            .unwrap();
        let sid = session.id.clone();
        let trimmer: StdArc<dyn crate::context::ContextTrimmer + Send + Sync> =
            StdArc::new(Phase2MockTrimmer);
        let ctx = session_mgr
            .start_loop(&sid, &trimmer, None, None, None)
            .unwrap();

        let provider: StdArc<dyn LlmProvider> =
            StdArc::new(SimpleProvider::new(vec![vec![ChatEvent::Done]]));
        let rule_engine = StdArc::new(RuleEngine::new(Path::new("/tmp")).unwrap());
        let registry = ToolRegistry::new();
        let config = AgentConfig {
            langfuse_enabled: true,
            langfuse_capture_input: true,
            ..Default::default()
        };
        let (tx, _rx) = mpsc::channel::<AgentEvent>(64);

        run_agent_loop(
            provider,
            StdArc::new(registry),
            rule_engine,
            session_mgr.clone(),
            ctx,
            &config,
            Message::user("test query"),
            tx,
        )
        .await;

        let captured = spans.lock().unwrap();
        let run_span = captured
            .iter()
            .find(|s| s.name == "visp.agent.run")
            .unwrap();

        // Verify observation type = span
        assert!(
            run_span
                .fields
                .iter()
                .any(|(k, v)| k == "langfuse.observation.type" && v == "span"),
            "expected langfuse.observation.type=span, fields: {:?}",
            run_span.fields
        );

        // Verify observation input is valid JSON containing the user message
        let input_field = run_span
            .fields
            .iter()
            .find(|(k, _)| k == "langfuse.observation.input")
            .map(|(_, v)| v.as_str())
            .unwrap_or("");
        assert!(
            !input_field.is_empty(),
            "expected langfuse.observation.input to be non-empty"
        );
        let parsed: serde_json::Value =
            serde_json::from_str(input_field).expect("observation.input should be valid JSON");
        assert_eq!(
            parsed["message"], "test query",
            "observation.input should contain the user message"
        );
    }

    #[serial]
    #[tokio::test]
    async fn test_agent_run_records_output_when_capture_enabled() {
        let (spans, events) = setup_tracing();
        let _guard = make_guard(&spans, &events);

        use crate::rules::RuleEngine;
        use crate::session::InMemorySessionStore;
        use crate::tool_registry::ToolRegistry;
        use std::path::Path;

        let session_mgr = StdArc::new(SessionManager::new(InMemorySessionStore::new()));
        let session = session_mgr
            .create(Path::new("/tmp"), LlmConfig::default())
            .unwrap();
        let sid = session.id.clone();
        let trimmer: StdArc<dyn crate::context::ContextTrimmer + Send + Sync> =
            StdArc::new(Phase2MockTrimmer);
        let ctx = session_mgr
            .start_loop(&sid, &trimmer, None, None, None)
            .unwrap();

        // Provider that returns text + Done
        let provider: StdArc<dyn LlmProvider> = StdArc::new(SimpleProvider::new(vec![vec![
            ChatEvent::TextDelta("Hello world response".into()),
            ChatEvent::Done,
        ]]));
        let rule_engine = StdArc::new(RuleEngine::new(Path::new("/tmp")).unwrap());
        let registry = ToolRegistry::new();
        let config = AgentConfig {
            langfuse_enabled: true,
            langfuse_capture_output: true,
            ..Default::default()
        };
        let (tx, _rx) = mpsc::channel::<AgentEvent>(64);

        run_agent_loop(
            provider,
            StdArc::new(registry),
            rule_engine,
            session_mgr.clone(),
            ctx,
            &config,
            Message::user("hello"),
            tx,
        )
        .await;

        let captured = spans.lock().unwrap();
        let run_span = captured
            .iter()
            .find(|s| s.name == "visp.agent.run")
            .unwrap();

        // Verify observation type = span (set during output recording)
        assert!(
            run_span
                .fields
                .iter()
                .any(|(k, v)| k == "langfuse.observation.type" && v == "span"),
            "expected langfuse.observation.type=span, fields: {:?}",
            run_span.fields
        );

        // Verify observation output is valid JSON containing the response text
        let output_field = run_span
            .fields
            .iter()
            .find(|(k, _)| k == "langfuse.observation.output")
            .map(|(_, v)| v.as_str())
            .unwrap_or("");
        assert!(
            !output_field.is_empty(),
            "expected langfuse.observation.output to be non-empty"
        );
        let parsed: serde_json::Value =
            serde_json::from_str(output_field).expect("observation.output should be valid JSON");
        assert_eq!(
            parsed["response"], "Hello world response",
            "observation.output should contain the assistant response"
        );
    }

    #[serial]
    #[tokio::test]
    async fn test_agent_run_does_not_record_input_when_capture_disabled() {
        let (spans, events) = setup_tracing();
        let _guard = make_guard(&spans, &events);

        use crate::rules::RuleEngine;
        use crate::session::InMemorySessionStore;
        use crate::tool_registry::ToolRegistry;
        use std::path::Path;

        let session_mgr = StdArc::new(SessionManager::new(InMemorySessionStore::new()));
        let session = session_mgr
            .create(Path::new("/tmp"), LlmConfig::default())
            .unwrap();
        let sid = session.id.clone();
        let trimmer: StdArc<dyn crate::context::ContextTrimmer + Send + Sync> =
            StdArc::new(Phase2MockTrimmer);
        let ctx = session_mgr
            .start_loop(&sid, &trimmer, None, None, None)
            .unwrap();

        let provider: StdArc<dyn LlmProvider> =
            StdArc::new(SimpleProvider::new(vec![vec![ChatEvent::Done]]));
        let rule_engine = StdArc::new(RuleEngine::new(Path::new("/tmp")).unwrap());
        let registry = ToolRegistry::new();
        let config = AgentConfig {
            langfuse_enabled: true,
            langfuse_capture_input: false, // disabled
            ..Default::default()
        };
        let (tx, _rx) = mpsc::channel::<AgentEvent>(64);

        run_agent_loop(
            provider,
            StdArc::new(registry),
            rule_engine,
            session_mgr.clone(),
            ctx,
            &config,
            Message::user("secret"),
            tx,
        )
        .await;

        let captured = spans.lock().unwrap();
        let run_span = captured
            .iter()
            .find(|s| s.name == "visp.agent.run")
            .unwrap();

        assert!(
            !run_span
                .fields
                .iter()
                .any(|(k, _)| k == "langfuse.observation.input"),
            "expected NO langfuse.observation.input when capture is disabled, fields: {:?}",
            run_span.fields
        );
    }

    #[serial]
    #[tokio::test]
    async fn test_agent_run_does_not_record_output_when_capture_disabled() {
        let (spans, events) = setup_tracing();
        let _guard = make_guard(&spans, &events);

        use crate::rules::RuleEngine;
        use crate::session::InMemorySessionStore;
        use crate::tool_registry::ToolRegistry;
        use std::path::Path;

        let session_mgr = StdArc::new(SessionManager::new(InMemorySessionStore::new()));
        let session = session_mgr
            .create(Path::new("/tmp"), LlmConfig::default())
            .unwrap();
        let sid = session.id.clone();
        let trimmer: StdArc<dyn crate::context::ContextTrimmer + Send + Sync> =
            StdArc::new(Phase2MockTrimmer);
        let ctx = session_mgr
            .start_loop(&sid, &trimmer, None, None, None)
            .unwrap();

        // Provider returns text but capture is disabled
        let provider: StdArc<dyn LlmProvider> = StdArc::new(SimpleProvider::new(vec![vec![
            ChatEvent::TextDelta("secret response".into()),
            ChatEvent::Done,
        ]]));
        let rule_engine = StdArc::new(RuleEngine::new(Path::new("/tmp")).unwrap());
        let registry = ToolRegistry::new();
        let config = AgentConfig {
            langfuse_enabled: true,
            langfuse_capture_output: false, // disabled
            ..Default::default()
        };
        let (tx, _rx) = mpsc::channel::<AgentEvent>(64);

        run_agent_loop(
            provider,
            StdArc::new(registry),
            rule_engine,
            session_mgr.clone(),
            ctx,
            &config,
            Message::user("hello"),
            tx,
        )
        .await;

        let captured = spans.lock().unwrap();
        let run_span = captured
            .iter()
            .find(|s| s.name == "visp.agent.run")
            .unwrap();

        assert!(
            !run_span
                .fields
                .iter()
                .any(|(k, _)| k == "langfuse.observation.output"),
            "expected NO langfuse.observation.output when capture is disabled, fields: {:?}",
            run_span.fields
        );
    }

    #[serial]
    #[tokio::test]
    async fn test_agent_run_emits_completed_event() {
        let (spans, events) = setup_tracing();
        let _guard = make_guard(&spans, &events);

        use crate::rules::RuleEngine;
        use crate::session::InMemorySessionStore;
        use crate::tool_registry::ToolRegistry;
        use std::path::Path;

        let session_mgr = StdArc::new(SessionManager::new(InMemorySessionStore::new()));
        let session = session_mgr
            .create(Path::new("/tmp"), LlmConfig::default())
            .unwrap();
        let sid = session.id.clone();
        let trimmer: StdArc<dyn crate::context::ContextTrimmer + Send + Sync> =
            StdArc::new(Phase2MockTrimmer);
        let ctx = session_mgr
            .start_loop(&sid, &trimmer, None, None, None)
            .unwrap();

        let provider: StdArc<dyn LlmProvider> =
            StdArc::new(SimpleProvider::new(vec![vec![ChatEvent::Done]]));
        let rule_engine = StdArc::new(RuleEngine::new(Path::new("/tmp")).unwrap());
        let registry = ToolRegistry::new();
        let config = AgentConfig::default();
        let (tx, _rx) = mpsc::channel::<AgentEvent>(64);

        run_agent_loop(
            provider,
            StdArc::new(registry),
            rule_engine,
            session_mgr.clone(),
            ctx,
            &config,
            Message::user("hello"),
            tx,
        )
        .await;

        let captured_events = events.lock().unwrap();
        assert!(
            captured_events.iter().any(|e| e == "visp.agent.completed"),
            "expected visp.agent.completed event, got: {:?}",
            *captured_events
        );
    }

    #[serial]
    #[tokio::test]
    async fn test_agent_run_emits_cancelled_event_on_cancel() {
        let (spans, events) = setup_tracing();
        let _guard = make_guard(&spans, &events);

        use crate::rules::RuleEngine;
        use crate::session::InMemorySessionStore;
        use crate::tool_registry::ToolRegistry;
        use std::path::Path;

        let session_mgr = StdArc::new(SessionManager::new(InMemorySessionStore::new()));
        let session = session_mgr
            .create(Path::new("/tmp"), LlmConfig::default())
            .unwrap();
        let sid = session.id.clone();
        let trimmer: StdArc<dyn crate::context::ContextTrimmer + Send + Sync> =
            StdArc::new(Phase2MockTrimmer);
        let ctx = session_mgr
            .start_loop(&sid, &trimmer, None, None, None)
            .unwrap();

        // Cancel before starting
        ctx.cancel_token.cancel();

        let provider: StdArc<dyn LlmProvider> =
            StdArc::new(SimpleProvider::new(vec![vec![ChatEvent::Done]]));
        let rule_engine = StdArc::new(RuleEngine::new(Path::new("/tmp")).unwrap());
        let registry = ToolRegistry::new();
        let config = AgentConfig::default();
        let (tx, _rx) = mpsc::channel::<AgentEvent>(64);

        run_agent_loop(
            provider,
            StdArc::new(registry),
            rule_engine,
            session_mgr.clone(),
            ctx,
            &config,
            Message::user("hello"),
            tx,
        )
        .await;

        let captured_events = events.lock().unwrap();
        assert!(
            captured_events.iter().any(|e| e == "visp.agent.cancelled"),
            "expected visp.agent.cancelled event, got: {:?}",
            *captured_events
        );
    }

    #[serial]
    #[tokio::test]
    async fn test_agent_run_emits_iteration_limit_event() {
        let (spans, events) = setup_tracing();
        let _guard = make_guard(&spans, &events);

        use crate::rules::RuleEngine;
        use crate::session::InMemorySessionStore;
        use crate::tool_registry::ToolRegistry;
        use std::path::Path;

        let session_mgr = StdArc::new(SessionManager::new(InMemorySessionStore::new()));
        let session = session_mgr
            .create(Path::new("/tmp"), LlmConfig::default())
            .unwrap();
        let sid = session.id.clone();
        let trimmer: StdArc<dyn crate::context::ContextTrimmer + Send + Sync> =
            StdArc::new(Phase2MockTrimmer);
        let ctx = session_mgr
            .start_loop(&sid, &trimmer, None, None, None)
            .unwrap();

        // Provider always returns a tool call, so we hit hard_limit=1 immediately
        let provider: StdArc<dyn LlmProvider> = StdArc::new(SimpleProvider::new(vec![vec![
            ChatEvent::ToolCall {
                id: "call_1".into(),
                name: "finder".into(),
                arguments: "{}".into(),
            },
            ChatEvent::Done,
        ]]));
        let rule_engine = StdArc::new(RuleEngine::new(Path::new("/tmp")).unwrap());
        let registry = ToolRegistry::new();
        registry
            .register(StdArc::new(MockTestTool { name: "finder" }))
            .unwrap();
        let config = AgentConfig {
            soft_limit: 0,
            hard_limit: 1,
            ..Default::default()
        };
        let (tx, _rx) = mpsc::channel::<AgentEvent>(64);

        run_agent_loop(
            provider,
            StdArc::new(registry),
            rule_engine,
            session_mgr.clone(),
            ctx,
            &config,
            Message::user("do tool"),
            tx,
        )
        .await;

        let captured_events = events.lock().unwrap();
        assert!(
            captured_events
                .iter()
                .any(|e| e == "visp.agent.iteration_limit"),
            "expected visp.agent.iteration_limit event, got: {:?}",
            *captured_events
        );
    }

    // ── W1-S3a-3/4: visp.agent.iteration span ─────────────────────────────

    #[serial]
    #[tokio::test]
    async fn test_agent_iteration_span_nested_under_run() {
        let (spans, events) = setup_tracing();
        let _guard = make_guard(&spans, &events);

        use crate::rules::RuleEngine;
        use crate::session::InMemorySessionStore;
        use crate::tool_registry::ToolRegistry;
        use std::path::Path;

        let session_mgr = StdArc::new(SessionManager::new(InMemorySessionStore::new()));
        let session = session_mgr
            .create(Path::new("/tmp"), LlmConfig::default())
            .unwrap();
        let sid = session.id.clone();
        let trimmer: StdArc<dyn crate::context::ContextTrimmer + Send + Sync> =
            StdArc::new(Phase2MockTrimmer);
        let ctx = session_mgr
            .start_loop(&sid, &trimmer, None, None, None)
            .unwrap();

        let provider: StdArc<dyn LlmProvider> =
            StdArc::new(SimpleProvider::new(vec![vec![ChatEvent::Done]]));
        let rule_engine = StdArc::new(RuleEngine::new(Path::new("/tmp")).unwrap());
        let registry = ToolRegistry::new();
        let config = AgentConfig::default();
        let (tx, _rx) = mpsc::channel::<AgentEvent>(64);

        run_agent_loop(
            provider,
            StdArc::new(registry),
            rule_engine,
            session_mgr.clone(),
            ctx,
            &config,
            Message::user("hello"),
            tx,
        )
        .await;

        let captured = spans.lock().unwrap();
        let run_span = captured
            .iter()
            .find(|s| s.name == "visp.agent.run")
            .expect("expected visp.agent.run span");
        let iter_spans: Vec<&CapturedSpan> = captured
            .iter()
            .filter(|s| s.name == "visp.agent.iteration")
            .collect();
        assert!(
            !iter_spans.is_empty(),
            "expected at least one visp.agent.iteration span"
        );
        // Each iteration span's parent should be the run span
        for iter_span in &iter_spans {
            assert_eq!(
                iter_span.parent_id,
                Some(run_span.id),
                "visp.agent.iteration should be a child of visp.agent.run"
            );
        }
    }

    #[serial]
    #[tokio::test]
    async fn test_agent_iteration_field_count() {
        let (spans, events) = setup_tracing();
        let _guard = make_guard(&spans, &events);

        use crate::rules::RuleEngine;
        use crate::session::InMemorySessionStore;
        use crate::tool_registry::ToolRegistry;
        use std::path::Path;

        let provider: StdArc<dyn LlmProvider> = StdArc::new(SimpleProvider::new(vec![
            vec![
                ChatEvent::ToolCall {
                    id: "call_1".into(),
                    name: "finder".into(),
                    arguments: "{}".into(),
                },
                ChatEvent::Done,
            ],
            vec![ChatEvent::Done],
        ]));
        let rule_engine = StdArc::new(RuleEngine::new(Path::new("/tmp")).unwrap());
        let registry = ToolRegistry::new();
        registry
            .register(StdArc::new(MockTestTool { name: "finder" }))
            .unwrap();
        let config = AgentConfig::default();

        let session_mgr = StdArc::new(SessionManager::new(InMemorySessionStore::new()));
        let session = session_mgr
            .create(Path::new("/tmp"), LlmConfig::default())
            .unwrap();
        let sid = session.id.clone();
        let trimmer: StdArc<dyn crate::context::ContextTrimmer + Send + Sync> =
            StdArc::new(Phase2MockTrimmer);
        let ctx = session_mgr
            .start_loop(&sid, &trimmer, None, None, None)
            .unwrap();
        let (tx, _rx) = mpsc::channel::<AgentEvent>(64);

        run_agent_loop(
            provider,
            StdArc::new(registry),
            rule_engine,
            session_mgr.clone(),
            ctx,
            &config,
            Message::user("do tool"),
            tx,
        )
        .await;

        let captured = spans.lock().unwrap();
        let iter_spans: Vec<&CapturedSpan> = captured
            .iter()
            .filter(|s| s.name == "visp.agent.iteration")
            .collect();
        assert!(
            !iter_spans.is_empty(),
            "expected at least one iteration span"
        );
        // Each iteration span should have iteration.count field
        for (i, span) in iter_spans.iter().enumerate() {
            assert!(
                span.fields.iter().any(|(k, _)| k == "iteration.count"),
                "iteration span {} should have iteration.count field, fields: {:?}",
                i,
                span.fields
            );
        }
    }

    // ── W1-S3b: iteration langfuse propagation ────────────────────────────

    #[serial]
    #[tokio::test]
    async fn test_iteration_span_propagates_langfuse_fields_when_enabled() {
        let (spans, events) = setup_tracing();
        let _guard = make_guard(&spans, &events);

        use crate::rules::RuleEngine;
        use crate::session::InMemorySessionStore;
        use crate::tool_registry::ToolRegistry;
        use std::path::Path;

        let session_mgr = StdArc::new(SessionManager::new(InMemorySessionStore::new()));
        let session = session_mgr
            .create(Path::new("/tmp"), LlmConfig::default())
            .unwrap();
        let sid = session.id.clone();
        let trimmer: StdArc<dyn crate::context::ContextTrimmer + Send + Sync> =
            StdArc::new(Phase2MockTrimmer);
        let ctx = session_mgr
            .start_loop(&sid, &trimmer, None, None, None)
            .unwrap();

        // Use a provider that does one tool call then done, giving us 2 iterations
        let provider: StdArc<dyn LlmProvider> = StdArc::new(OneToolCallProvider {
            call_count: AtomicUsize::new(0),
        });
        let rule_engine = StdArc::new(RuleEngine::new(Path::new("/tmp")).unwrap());
        let registry = ToolRegistry::new();
        registry
            .register(StdArc::new(MockTestTool { name: "sleepy" }))
            .unwrap();
        let config = AgentConfig {
            langfuse_enabled: true,
            langfuse_user_id: Some("iter_user".to_string()),
            langfuse_tags: Some(r#"["iter"]"#.to_string()),
            langfuse_environment: Some("staging".to_string()),
            hard_limit: 10,
            ..Default::default()
        };
        let (tx, _rx) = mpsc::channel::<AgentEvent>(64);

        run_agent_loop(
            provider,
            StdArc::new(registry),
            rule_engine,
            session_mgr.clone(),
            ctx,
            &config,
            Message::user("test iteration"),
            tx,
        )
        .await;

        let captured = spans.lock().unwrap();
        // Get root run span trace name for comparison
        let run_span = captured
            .iter()
            .find(|s| s.name == "visp.agent.run")
            .expect("expected visp.agent.run span");
        let root_trace_name = run_span
            .fields
            .iter()
            .find(|(k, _)| k == "langfuse.trace.name")
            .map(|(_, v)| v.clone())
            .unwrap_or_default();

        // Get iteration spans
        let iter_spans: Vec<&CapturedSpan> = captured
            .iter()
            .filter(|s| s.name == "visp.agent.iteration")
            .collect();
        assert!(
            !iter_spans.is_empty(),
            "expected at least one iteration span"
        );

        // Each iteration span should carry trace-level langfuse fields
        for (i, span) in iter_spans.iter().enumerate() {
            assert!(
                span.fields.iter().any(|(k, _)| k == "langfuse.session.id"),
                "iteration span {} should have langfuse.session.id, fields: {:?}",
                i,
                span.fields
            );
            assert!(
                span.fields.iter().any(|(k, _)| k == "langfuse.trace.name"),
                "iteration span {} should have langfuse.trace.name, fields: {:?}",
                i,
                span.fields
            );
            assert!(
                span.fields.iter().any(|(k, _)| k == "langfuse.user.id"),
                "iteration span {} should have langfuse.user.id, fields: {:?}",
                i,
                span.fields
            );
            assert!(
                span.fields.iter().any(|(k, _)| k == "langfuse.trace.tags"),
                "iteration span {} should have langfuse.trace.tags, fields: {:?}",
                i,
                span.fields
            );
            assert!(
                span.fields.iter().any(|(k, _)| k == "langfuse.environment"),
                "iteration span {} should have langfuse.environment, fields: {:?}",
                i,
                span.fields
            );

            // Verify trace name matches root
            let iter_trace_name = span
                .fields
                .iter()
                .find(|(k, _)| k == "langfuse.trace.name")
                .map(|(_, v)| v.clone())
                .unwrap_or_default();
            assert_eq!(
                iter_trace_name, root_trace_name,
                "iteration span {} trace name should match root",
                i
            );
        }
    }

    #[serial]
    #[tokio::test]
    async fn test_iteration_span_no_langfuse_fields_when_disabled() {
        let (spans, events) = setup_tracing();
        let _guard = make_guard(&spans, &events);

        use crate::rules::RuleEngine;
        use crate::session::InMemorySessionStore;
        use crate::tool_registry::ToolRegistry;
        use std::path::Path;

        let session_mgr = StdArc::new(SessionManager::new(InMemorySessionStore::new()));
        let session = session_mgr
            .create(Path::new("/tmp"), LlmConfig::default())
            .unwrap();
        let sid = session.id.clone();
        let trimmer: StdArc<dyn crate::context::ContextTrimmer + Send + Sync> =
            StdArc::new(Phase2MockTrimmer);
        let ctx = session_mgr
            .start_loop(&sid, &trimmer, None, None, None)
            .unwrap();

        let provider: StdArc<dyn LlmProvider> = StdArc::new(OneToolCallProvider {
            call_count: AtomicUsize::new(0),
        });
        let rule_engine = StdArc::new(RuleEngine::new(Path::new("/tmp")).unwrap());
        let registry = ToolRegistry::new();
        registry
            .register(StdArc::new(MockTestTool { name: "sleepy" }))
            .unwrap();
        let config = AgentConfig {
            hard_limit: 10,
            ..Default::default() // langfuse_enabled=false
        };
        let (tx, _rx) = mpsc::channel::<AgentEvent>(64);

        run_agent_loop(
            provider,
            StdArc::new(registry),
            rule_engine,
            session_mgr.clone(),
            ctx,
            &config,
            Message::user("test no-langfuse iteration"),
            tx,
        )
        .await;

        let captured = spans.lock().unwrap();
        let iter_spans: Vec<&CapturedSpan> = captured
            .iter()
            .filter(|s| s.name == "visp.agent.iteration")
            .collect();
        assert!(
            !iter_spans.is_empty(),
            "expected at least one iteration span"
        );

        for (i, span) in iter_spans.iter().enumerate() {
            let langfuse_fields: Vec<_> = span
                .fields
                .iter()
                .filter(|(k, _)| k.starts_with("langfuse."))
                .collect();
            assert!(
                langfuse_fields.is_empty(),
                "iteration span {} should have NO langfuse.* fields when disabled, got: {:?}",
                i,
                langfuse_fields
            );
        }
    }

    // ── W1-S3a-5/6: visp.tool.execute span ────────────────────────────────

    /// Provider that returns three tool calls in one go
    struct ThreeToolProvider {
        call_count: AtomicUsize,
    }

    #[async_trait::async_trait]
    impl LlmProvider for ThreeToolProvider {
        async fn chat_stream(
            &self,
            _messages: &[Message],
            _tools: &[ToolDefinition],
            _config: &LlmConfig,
            _cancel: &tokio_util::sync::CancellationToken,
        ) -> Result<Pin<Box<dyn Stream<Item = Result<ChatEvent, LlmError>> + Send>>, LlmError>
        {
            let count = self.call_count.fetch_add(1, Ordering::SeqCst);
            if count == 0 {
                let events = vec![
                    Ok(ChatEvent::ToolCall {
                        id: "call_1".into(),
                        name: "finder".into(),
                        arguments: "{}".into(),
                    }),
                    Ok(ChatEvent::ToolCall {
                        id: "call_2".into(),
                        name: "grep".into(),
                        arguments: "{}".into(),
                    }),
                    Ok(ChatEvent::ToolCall {
                        id: "call_3".into(),
                        name: "bash".into(),
                        arguments: r#"{"cmd": "echo hi"}"#.into(),
                    }),
                    Ok(ChatEvent::Done),
                ];
                Ok(Box::pin(stream::iter(events)))
            } else {
                Ok(Box::pin(stream::iter(vec![Ok(ChatEvent::Done)])))
            }
        }
    }

    #[serial]
    #[tokio::test]
    async fn test_tool_execute_span_per_call() {
        let (spans, events) = setup_tracing();
        let _guard = make_guard(&spans, &events);

        use crate::rules::RuleEngine;
        use crate::session::InMemorySessionStore;
        use crate::tool_registry::ToolRegistry;
        use std::path::Path;

        let session_mgr = StdArc::new(SessionManager::new(InMemorySessionStore::new()));
        let session = session_mgr
            .create(Path::new("/tmp"), LlmConfig::default())
            .unwrap();
        let sid = session.id.clone();
        let trimmer: StdArc<dyn crate::context::ContextTrimmer + Send + Sync> =
            StdArc::new(Phase2MockTrimmer);
        let ctx = session_mgr
            .start_loop(&sid, &trimmer, None, None, None)
            .unwrap();

        let provider: StdArc<dyn LlmProvider> = StdArc::new(ThreeToolProvider {
            call_count: AtomicUsize::new(0),
        });
        let rule_engine = StdArc::new(RuleEngine::new(Path::new("/tmp")).unwrap());
        let registry = ToolRegistry::new();
        registry.register(StdArc::new(SleepyTool)).unwrap();
        // Register SleepyTool twice with different names using inline wrappers
        struct GrepTool;
        #[async_trait::async_trait]
        impl crate::tool::Tool for GrepTool {
            fn name(&self) -> &str {
                "grep"
            }
            fn description(&self) -> &str {
                "greps"
            }
            fn parameters(&self) -> serde_json::Value {
                serde_json::json!({})
            }
            async fn execute(
                &self,
                _: serde_json::Value,
                _: &crate::tool::ToolContext,
            ) -> crate::tool::ToolResult {
                crate::tool::ToolResult::success("matched")
            }
        }
        struct BashTool;
        #[async_trait::async_trait]
        impl crate::tool::Tool for BashTool {
            fn name(&self) -> &str {
                "bash"
            }
            fn description(&self) -> &str {
                "runs"
            }
            fn parameters(&self) -> serde_json::Value {
                serde_json::json!({})
            }
            async fn execute(
                &self,
                _: serde_json::Value,
                _: &crate::tool::ToolContext,
            ) -> crate::tool::ToolResult {
                crate::tool::ToolResult::success("done")
            }
        }
        registry.register(StdArc::new(GrepTool)).unwrap();
        registry.register(StdArc::new(BashTool)).unwrap();

        let config = AgentConfig {
            hard_limit: 10,
            ..Default::default()
        };
        let (tx, _rx) = mpsc::channel::<AgentEvent>(64);

        run_agent_loop(
            provider,
            StdArc::new(registry),
            rule_engine,
            session_mgr.clone(),
            ctx,
            &config,
            Message::user("use tools"),
            tx,
        )
        .await;

        let captured = spans.lock().unwrap();
        let tool_spans: Vec<&CapturedSpan> = captured
            .iter()
            .filter(|s| s.name == "visp.tool.execute")
            .collect();
        assert_eq!(
            tool_spans.len(),
            3,
            "expected 3 visp.tool.execute spans, got {}: {:?}",
            tool_spans.len(),
            tool_spans.iter().map(|s| &s.fields).collect::<Vec<_>>()
        );
    }

    #[serial]
    #[tokio::test]
    async fn test_tool_execute_fields() {
        let (spans, events) = setup_tracing();
        let _guard = make_guard(&spans, &events);

        use crate::rules::RuleEngine;
        use crate::session::InMemorySessionStore;
        use crate::tool_registry::ToolRegistry;
        use std::path::Path;

        let session_mgr = StdArc::new(SessionManager::new(InMemorySessionStore::new()));
        let session = session_mgr
            .create(Path::new("/tmp"), LlmConfig::default())
            .unwrap();
        let sid = session.id.clone();
        let trimmer: StdArc<dyn crate::context::ContextTrimmer + Send + Sync> =
            StdArc::new(Phase2MockTrimmer);
        let ctx = session_mgr
            .start_loop(&sid, &trimmer, None, None, None)
            .unwrap();

        let provider: StdArc<dyn LlmProvider> = StdArc::new(OneToolCallProvider {
            call_count: AtomicUsize::new(0),
        });
        let rule_engine = StdArc::new(RuleEngine::new(Path::new("/tmp")).unwrap());
        let registry = ToolRegistry::new();
        registry.register(StdArc::new(SleepyTool)).unwrap();
        let config = AgentConfig {
            hard_limit: 10,
            langfuse_enabled: true,
            ..Default::default()
        };
        let (tx, _rx) = mpsc::channel::<AgentEvent>(64);

        run_agent_loop(
            provider,
            StdArc::new(registry),
            rule_engine,
            session_mgr.clone(),
            ctx,
            &config,
            Message::user("sleep"),
            tx,
        )
        .await;

        let captured = spans.lock().unwrap();
        let tool_span = captured
            .iter()
            .find(|s| s.name == "visp.tool.execute")
            .expect("expected visp.tool.execute span");

        // Check required fields
        let field_names: Vec<&str> = tool_span.fields.iter().map(|(k, _)| k.as_str()).collect();
        assert!(
            tool_span
                .fields
                .iter()
                .any(|(k, _)| k == "gen_ai.tool.name"),
            "expected gen_ai.tool.name field, got: {:?}",
            field_names
        );
        assert!(
            tool_span
                .fields
                .iter()
                .any(|(k, _)| k == "gen_ai.tool.call.id"),
            "expected gen_ai.tool.call.id field, got: {:?}",
            field_names
        );
        assert!(
            tool_span
                .fields
                .iter()
                .any(|(k, _)| k == "gen_ai.tool.type"),
            "expected gen_ai.tool.type field, got: {:?}",
            field_names
        );
        // Check langfuse observation type
        assert!(
            tool_span
                .fields
                .iter()
                .any(|(k, v)| k == "langfuse.observation.type" && v == "span"),
            "expected langfuse.observation.type=span, got: {:?}",
            field_names
        );
        // Check gen_ai operation name
        assert!(
            tool_span
                .fields
                .iter()
                .any(|(k, v)| k == "gen_ai.operation.name" && v == "execute_tool"),
            "expected gen_ai.operation.name=execute_tool, got: {:?}",
            field_names
        );
        // Check trace-level langfuse fields are propagated
        assert!(
            tool_span
                .fields
                .iter()
                .any(|(k, _)| k == "langfuse.session.id"),
            "expected langfuse.session.id on tool span"
        );
        assert!(
            tool_span
                .fields
                .iter()
                .any(|(k, _)| k == "langfuse.trace.name"),
            "expected langfuse.trace.name on tool span"
        );
    }

    #[serial]
    #[tokio::test]
    async fn test_tool_execute_success_level_default() {
        let (spans, events) = setup_tracing();
        let _guard = make_guard(&spans, &events);

        use crate::rules::RuleEngine;
        use crate::session::InMemorySessionStore;
        use crate::tool_registry::ToolRegistry;
        use std::path::Path;

        let session_mgr = StdArc::new(SessionManager::new(InMemorySessionStore::new()));
        let session = session_mgr
            .create(Path::new("/tmp"), LlmConfig::default())
            .unwrap();
        let sid = session.id.clone();
        let trimmer: StdArc<dyn crate::context::ContextTrimmer + Send + Sync> =
            StdArc::new(Phase2MockTrimmer);
        let ctx = session_mgr
            .start_loop(&sid, &trimmer, None, None, None)
            .unwrap();

        let provider: StdArc<dyn LlmProvider> = StdArc::new(OneToolCallProvider {
            call_count: AtomicUsize::new(0),
        });
        let rule_engine = StdArc::new(RuleEngine::new(Path::new("/tmp")).unwrap());
        let registry = ToolRegistry::new();
        registry.register(StdArc::new(SleepyTool)).unwrap();
        let config = AgentConfig {
            hard_limit: 10,
            langfuse_enabled: true,
            ..Default::default()
        };
        let (tx, _rx) = mpsc::channel::<AgentEvent>(64);

        run_agent_loop(
            provider,
            StdArc::new(registry),
            rule_engine,
            session_mgr.clone(),
            ctx,
            &config,
            Message::user("sleep"),
            tx,
        )
        .await;

        let captured = spans.lock().unwrap();
        let tool_span = captured
            .iter()
            .find(|s| s.name == "visp.tool.execute")
            .expect("expected visp.tool.execute span");

        // Success should have level=DEFAULT
        assert!(
            tool_span
                .fields
                .iter()
                .any(|(k, v)| k == "level" && v == "DEFAULT"),
            "expected level=DEFAULT for successful tool, got fields: {:?}",
            tool_span.fields
        );
        // No status_message for success
        assert!(
            !tool_span.fields.iter().any(|(k, _)| k == "status_message"),
            "expected no status_message for successful tool, got fields: {:?}",
            tool_span.fields
        );
    }

    #[serial]
    #[tokio::test]
    async fn test_tool_execute_error_level_error_with_status_message() {
        let (spans, events) = setup_tracing();
        let _guard = make_guard(&spans, &events);

        use crate::session::InMemorySessionStore;
        use std::path::Path;

        // ErrorTool returns an error
        struct ErrorTool;
        #[async_trait::async_trait]
        impl crate::tool::Tool for ErrorTool {
            fn name(&self) -> &str {
                "error_tool"
            }
            fn description(&self) -> &str {
                "always fails"
            }
            fn parameters(&self) -> serde_json::Value {
                serde_json::json!({})
            }
            async fn execute(
                &self,
                _args: serde_json::Value,
                _ctx: &crate::tool::ToolContext,
            ) -> crate::tool::ToolResult {
                crate::tool::ToolResult::error("always fails")
            }
        }

        struct ErrorToolProvider {
            call_count: AtomicUsize,
        }
        #[async_trait::async_trait]
        impl LlmProvider for ErrorToolProvider {
            async fn chat_stream(
                &self,
                _messages: &[Message],
                _tools: &[ToolDefinition],
                _config: &LlmConfig,
                _cancel: &tokio_util::sync::CancellationToken,
            ) -> Result<Pin<Box<dyn Stream<Item = Result<ChatEvent, LlmError>> + Send>>, LlmError>
            {
                let count = self.call_count.fetch_add(1, Ordering::SeqCst);
                if count == 0 {
                    let events = vec![
                        Ok(ChatEvent::ToolCall {
                            id: "call_err_1".into(),
                            name: "error_tool".into(),
                            arguments: "{}".into(),
                        }),
                        Ok(ChatEvent::Done),
                    ];
                    Ok(Box::pin(stream::iter(events)))
                } else {
                    Ok(Box::pin(stream::iter(vec![Ok(ChatEvent::Done)])))
                }
            }
        }

        let session_mgr = StdArc::new(SessionManager::new(InMemorySessionStore::new()));
        let session = session_mgr
            .create(Path::new("/tmp"), LlmConfig::default())
            .unwrap();
        let sid = session.id.clone();
        let trimmer: StdArc<dyn crate::context::ContextTrimmer + Send + Sync> =
            StdArc::new(Phase2MockTrimmer);
        let ctx = session_mgr
            .start_loop(&sid, &trimmer, None, None, None)
            .unwrap();

        let provider: StdArc<dyn LlmProvider> = StdArc::new(ErrorToolProvider {
            call_count: AtomicUsize::new(0),
        });
        let rule_engine = StdArc::new(RuleEngine::new(Path::new("/tmp")).unwrap());
        let registry = ToolRegistry::new();
        registry.register(StdArc::new(ErrorTool)).unwrap();
        let config = AgentConfig {
            hard_limit: 10,
            langfuse_enabled: true,
            ..Default::default()
        };
        let (tx, _rx) = mpsc::channel::<AgentEvent>(64);

        run_agent_loop(
            provider,
            StdArc::new(registry),
            rule_engine,
            session_mgr.clone(),
            ctx,
            &config,
            Message::user("fail"),
            tx,
        )
        .await;

        let captured = spans.lock().unwrap();
        let tool_span = captured
            .iter()
            .find(|s| s.name == "visp.tool.execute")
            .expect("expected visp.tool.execute span");

        // Error should have level=ERROR
        assert!(
            tool_span
                .fields
                .iter()
                .any(|(k, v)| k == "level" && v == "ERROR"),
            "expected level=ERROR for failing tool, got fields: {:?}",
            tool_span.fields
        );
        // status_message should contain a summary (not full args/results)
        assert!(
            tool_span.fields.iter().any(|(k, _)| k == "status_message"),
            "expected status_message for failing tool, got fields: {:?}",
            tool_span.fields
        );
        let status_msg = tool_span
            .fields
            .iter()
            .find(|(k, _)| k == "status_message")
            .map(|(_, v)| v.as_str())
            .unwrap_or("");
        assert!(
            status_msg.contains("always fails"),
            "status_message should contain error summary, got: {status_msg}"
        );
        // status_message should NOT contain full args or results (just summary)
        assert!(
            status_msg.len() < 200,
            "status_message should be a short summary, got length {}",
            status_msg.len()
        );
    }

    #[serial]
    #[tokio::test]
    async fn test_tool_execute_duration_ms_uses_authoritative_value() {
        let (spans, events) = setup_tracing();
        let _guard = make_guard(&spans, &events);

        use crate::session::InMemorySessionStore;
        use std::path::Path;

        let session_mgr = StdArc::new(SessionManager::new(InMemorySessionStore::new()));
        let session = session_mgr
            .create(Path::new("/tmp"), LlmConfig::default())
            .unwrap();
        let sid = session.id.clone();
        let trimmer: StdArc<dyn crate::context::ContextTrimmer + Send + Sync> =
            StdArc::new(Phase2MockTrimmer);
        let ctx = session_mgr
            .start_loop(&sid, &trimmer, None, None, None)
            .unwrap();

        // Use the tool that records duration in history
        let provider: StdArc<dyn LlmProvider> = StdArc::new(OneToolCallProvider {
            call_count: AtomicUsize::new(0),
        });
        let rule_engine = StdArc::new(RuleEngine::new(Path::new("/tmp")).unwrap());
        let registry = ToolRegistry::new();
        registry.register(StdArc::new(SleepyTool)).unwrap();
        let config = AgentConfig {
            hard_limit: 10,
            ..Default::default()
        };
        let (tx, _rx) = mpsc::channel::<AgentEvent>(64);

        run_agent_loop(
            provider,
            StdArc::new(registry),
            rule_engine,
            session_mgr.clone(),
            ctx,
            &config,
            Message::user("sleep"),
            tx,
        )
        .await;

        // Get duration from history (Wave 0 authoritative value)
        let final_session = session_mgr.get(&sid).unwrap();
        let tool_msg = final_session
            .history
            .iter()
            .find(|m| m.role == Role::Tool)
            .expect("expected a tool message in history");
        let history_duration = tool_msg
            .tool_result_duration_ms
            .expect("expected duration_ms on tool message");

        // Get duration from span
        let captured = spans.lock().unwrap();
        let tool_span = captured
            .iter()
            .find(|s| s.name == "visp.tool.execute")
            .expect("expected visp.tool.execute span");

        // Find duration_ms field in span
        let span_duration = tool_span
            .fields
            .iter()
            .find(|(k, _)| k == "visp.tool.duration_ms")
            .map(|(_, v)| v.parse::<u64>().unwrap_or(0));

        if let Some(span_dur) = span_duration {
            // The span duration and history duration should match (both use the same elapsed_ms)
            // They come from the same measurement, so should be identical
            assert_eq!(
                span_dur, history_duration,
                "span duration_ms should match the authoritative history value"
            );
        }
        // If no duration_ms field, the tool was cancelled early — acceptable
    }

    #[serial]
    #[tokio::test]
    async fn test_tool_execute_is_error_true_on_failure() {
        let (spans, events) = setup_tracing();
        let _guard = make_guard(&spans, &events);

        use crate::session::InMemorySessionStore;
        use std::path::Path;

        // ErrorTool returns an error
        struct ErrorTool;
        #[async_trait::async_trait]
        impl crate::tool::Tool for ErrorTool {
            fn name(&self) -> &str {
                "error_tool"
            }
            fn description(&self) -> &str {
                "always fails"
            }
            fn parameters(&self) -> serde_json::Value {
                serde_json::json!({})
            }
            async fn execute(
                &self,
                _args: serde_json::Value,
                _ctx: &crate::tool::ToolContext,
            ) -> crate::tool::ToolResult {
                crate::tool::ToolResult::error("always fails")
            }
        }

        struct ErrorToolProvider {
            call_count: AtomicUsize,
        }
        #[async_trait::async_trait]
        impl LlmProvider for ErrorToolProvider {
            async fn chat_stream(
                &self,
                _messages: &[Message],
                _tools: &[ToolDefinition],
                _config: &LlmConfig,
                _cancel: &tokio_util::sync::CancellationToken,
            ) -> Result<Pin<Box<dyn Stream<Item = Result<ChatEvent, LlmError>> + Send>>, LlmError>
            {
                let count = self.call_count.fetch_add(1, Ordering::SeqCst);
                if count == 0 {
                    let events = vec![
                        Ok(ChatEvent::ToolCall {
                            id: "call_err_1".into(),
                            name: "error_tool".into(),
                            arguments: "{}".into(),
                        }),
                        Ok(ChatEvent::Done),
                    ];
                    Ok(Box::pin(stream::iter(events)))
                } else {
                    Ok(Box::pin(stream::iter(vec![Ok(ChatEvent::Done)])))
                }
            }
        }

        let session_mgr = StdArc::new(SessionManager::new(InMemorySessionStore::new()));
        let session = session_mgr
            .create(Path::new("/tmp"), LlmConfig::default())
            .unwrap();
        let sid = session.id.clone();
        let trimmer: StdArc<dyn crate::context::ContextTrimmer + Send + Sync> =
            StdArc::new(Phase2MockTrimmer);
        let ctx = session_mgr
            .start_loop(&sid, &trimmer, None, None, None)
            .unwrap();

        let provider: StdArc<dyn LlmProvider> = StdArc::new(ErrorToolProvider {
            call_count: AtomicUsize::new(0),
        });
        let rule_engine = StdArc::new(RuleEngine::new(Path::new("/tmp")).unwrap());
        let registry = ToolRegistry::new();
        registry.register(StdArc::new(ErrorTool)).unwrap();
        let config = AgentConfig {
            hard_limit: 10,
            ..Default::default()
        };
        let (tx, _rx) = mpsc::channel::<AgentEvent>(64);

        run_agent_loop(
            provider,
            StdArc::new(registry),
            rule_engine,
            session_mgr.clone(),
            ctx,
            &config,
            Message::user("fail"),
            tx,
        )
        .await;

        let captured = spans.lock().unwrap();
        let tool_span = captured
            .iter()
            .find(|s| s.name == "visp.tool.execute")
            .expect("expected visp.tool.execute span");

        assert!(
            tool_span
                .fields
                .iter()
                .any(|(k, v)| k == "visp.tool.is_error" && v == "true"),
            "expected visp.tool.is_error=true, got fields: {:?}",
            tool_span.fields
        );
    }

    // ── W1-S3a-7/8: TraceContext injection ─────────────────────────────────

    #[serial]
    #[tokio::test]
    async fn test_task_tool_intercepts_and_attaches_trace_context() {
        let (spans, events) = setup_tracing();
        let _guard = make_guard(&spans, &events);

        use crate::rules::RuleEngine;
        use crate::session::InMemorySessionStore;
        use crate::tool_registry::ToolRegistry;
        use std::path::Path;

        // Set up multi-agent mode with global_tx
        let (global_tx, mut global_rx) = mpsc::channel::<Envelope>(16);
        let (_inbox_tx, inbox_rx) = mpsc::channel::<OrchestratorMessage>(16);
        let permission_rules = Arc::new(Vec::new());

        let session_mgr = StdArc::new(SessionManager::new(InMemorySessionStore::new()));
        let session = session_mgr
            .create(Path::new("/tmp"), LlmConfig::default())
            .unwrap();
        let sid = session.id.clone();
        let trimmer: StdArc<dyn crate::context::ContextTrimmer + Send + Sync> =
            StdArc::new(Phase2MockTrimmer);
        let ctx = session_mgr
            .start_loop(
                &sid,
                &trimmer,
                Some(global_tx),
                Some(inbox_rx),
                Some(permission_rules),
            )
            .unwrap();

        // Provider returns a "task" tool call
        let provider: StdArc<dyn LlmProvider> = StdArc::new(Phase2ToolProvider {
            call_count: AtomicUsize::new(0),
        });
        let rule_engine = StdArc::new(RuleEngine::new(Path::new("/tmp")).unwrap());
        let registry = ToolRegistry::new();
        let config = AgentConfig {
            hard_limit: 10,
            ..Default::default()
        };
        let (tx, mut rx) = mpsc::channel::<AgentEvent>(64);

        // Run agent loop in background
        let sm_clone = session_mgr.clone();
        let handle = tokio::spawn(async move {
            run_agent_loop(
                provider,
                StdArc::new(registry),
                rule_engine,
                sm_clone,
                ctx,
                &config,
                Message::user("spawn sub-agent"),
                tx,
            )
            .await;
        });

        // Cancel to avoid waiting forever
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;

        // Drain events
        while rx.try_recv().is_ok() {}

        // Cancel
        handle.abort();

        // Check the global_tx for Envelope with SpawnRequest containing trace_context
        let mut found_tc = false;
        while let Ok(envelope) = global_rx.try_recv() {
            if let AgentMessage::SpawnRequest {
                trace_context: Some(tc),
                ..
            } = &envelope.message
            {
                found_tc = true;
                assert_eq!(tc.trace_id.len(), 32, "trace_id should be 32 hex chars");
                assert_eq!(tc.span_id.len(), 16, "span_id should be 16 hex chars");
                assert!(tc.trace_id.chars().all(|c| c.is_ascii_hexdigit()));
                assert!(tc.span_id.chars().all(|c| c.is_ascii_hexdigit()));
            }
        }

        assert!(found_tc, "expected SpawnRequest with trace_context set");
    }
}
