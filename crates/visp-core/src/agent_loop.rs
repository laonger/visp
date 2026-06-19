use std::panic::AssertUnwindSafe;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

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
use crate::provider::LlmProvider;
use crate::rules::RuleEngine;
use crate::session::SessionManager;
use crate::session::SessionStatus;
use crate::tool::ToolContext;
use crate::tool::ToolResult;
use crate::tool_registry::ToolRegistry;

use futures::FutureExt;
use futures::Stream;
use futures::StreamExt;
use tokio::sync::mpsc;
use tokio::sync::oneshot;

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
    loop {
        // 循环顶部检查 cancel：首次进入或 sleep 后立即返回
        if ctx.cancel_token.is_cancelled() {
            return Err(LlmError::Cancelled);
        }

        match provider
            .chat_stream(messages, tools, &ctx.config, &ctx.cancel_token)
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
                let mut text_msg = Message::assistant(clean_text.clone());
                text_msg.extra_blocks = if thinking_blocks.is_empty() {
                    None
                } else {
                    Some(thinking_blocks.clone())
                };
                text_msg.estimated_tokens = estimate_message_tokens(&text_msg);
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
            let mut text_msg = Message::assistant(text_buffer.clone());
            text_msg.extra_blocks = if thinking_blocks.is_empty() {
                None
            } else {
                Some(thinking_blocks.clone())
            };
            text_msg.estimated_tokens = estimate_message_tokens(&text_msg);
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
    total_tool_calls: &mut u32,
    ctx: &mut AgentLoopContext,
    sm: &Arc<SessionManager>,
    tool_registry: &Arc<ToolRegistry>,
    sid: &str,
    tx: &mpsc::Sender<AgentEvent>,
    cfg: &AgentConfig,
    doom_loop_window: &mut Vec<Vec<(String, serde_json::Value)>>,
    doom_loop_warned: &mut bool,
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
        actual_tokens_input: None,
        actual_tokens_output: None,
        actual_cache_read: None,
        actual_cache_write: None,
        actual_cost: None,
        provider_metadata: None,
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
                let _ = gtx.try_send(Envelope {
                    session_id: ctx.session_id.clone(),
                    message: AgentMessage::SpawnRequest {
                        call_id: tc.id.clone(),
                        subagent_type: subagent_type.clone(),
                        description: description.clone(),
                        task_id: task_id.clone(),
                    },
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

        exec_tasks.push(tokio::spawn(async move {
            // Helper: forward AgentMessage to global_tx
            macro_rules! forward_global {
                ($msg:expr) => {
                    if let Some(ref gtx) = global_tx {
                        let _ = gtx.try_send(Envelope {
                            session_id: sid2.clone(),
                            message: $msg,
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
                    };
                }
            };
            let tool_ctx = ToolContext {
                working_dir: working_dir.clone(),
                session_id: Some(session_id),
                permission_rules: permissions.clone(),
            };

            let result = registry
                .execute(&tc.name, args, &tool_ctx)
                .await
                .unwrap_or_else(|| ToolResult::error("Tool not found in registry"));

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
            }
        }));
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
        let tool_msg = Message::tool(tr.result.content, &tr.call_id);
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
    let sid = ctx.session_id.clone();
    let sm = session_mgr.clone();
    let cfg = agent_config.clone();

    // Clone for use in panic handler after the async block
    let sid_panic = sid.clone();
    let sm_panic = sm.clone();
    // W2: Clone global_tx so the panic handler can forward AgentMessage::Error
    // to the orchestrator (which will then notify the parent agent via SubAgentError).
    let global_tx_panic = ctx.global_tx.clone();

    // Wrap entire body in catch_unwind for panic safety.
    // On panic, session is reset to Idle before re-raising.
    let result = AssertUnwindSafe(async move {
        // Early cancellation check before appending
        if ctx.cancel_token.is_cancelled() {
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
            .await
            {
                Ok(ic) => ic,
                Err(()) => return,
            };
            let messages = ic.messages;
            let tools = ic.tools;

            // d. Call LLM with retry
            let stream =
                match call_llm_with_retry(provider.as_ref(), &messages, &tools, &ctx, &cfg, &sid)
                    .await
                {
                    Ok(s) => s,
                    Err(LlmError::Cancelled) => {
                        // retry 阶段被 cancel：还没拿到 stream，
                        // 必须在此显式发 Cancelled 事件 + finish_loop（collect_stream_events 不会被调用）
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
            let output = match collect_stream_events(stream, &mut ctx, &sid, &tx, &sm).await {
                Some(o) => o,
                None => return,
            };

            // f. Handle stream result (check USER_QUERY marker or done)
            match handle_stream_result(&output, total_tool_calls, &mut ctx, &sm, &sid, &tx).await {
                Ok(StreamDecision::Done) => {
                    tracing::info!(
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
                &mut total_tool_calls,
                &mut ctx,
                &sm,
                &tool_registry,
                &sid,
                &tx,
                &cfg,
                &mut doom_loop_window,
                &mut doom_loop_warned,
            )
            .await
            {
                return; // fatal error
            }

            // i. Increment iteration
            iteration += 1;
        }
    })
    .catch_unwind()
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
    use crate::context::NoopTrimmer;
    use crate::provider::LlmConfig;
    use futures::stream;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};

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
            .start_loop_v2(&sid, &trimmer, global_tx, inbox_rx, permission_rules)
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

        // finish_loop 总是重置到 Idle（_status 参数被忽略）
        let final_session = session_mgr.get(&sid).unwrap();
        assert_eq!(
            final_session.status,
            crate::session::SessionStatus::Idle,
            "session should end in Idle after cancel (finish_loop always resets to Idle)"
        );
        // Cancel should not panic
        assert!(result.is_ok() || (result.is_err() && result.unwrap_err().is_cancelled()));
    }
}
