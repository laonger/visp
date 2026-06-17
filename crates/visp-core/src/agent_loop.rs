use std::panic::AssertUnwindSafe;
use std::sync::Arc;
use std::time::Duration;

use crate::agent::{
    cleanup_orphan_tool_uses, dump_prompt_to_file, extract_thinking_text, format_tool_args,
    llm_error_to_code, parse_user_query_marker, render_tool_guide, strip_user_query_marker,
    AgentConfig, AgentEvent, AgentLoopContext, AgentMessage, Envelope, OrchestratorMessage,
    PendingSpawn, ToolExecResult, UserQueryResult,
};
use crate::error::AgentErrorCode;
use crate::error::LlmError;
use crate::message::{
    estimate_message_tokens, Message, MessageType, Role, ToolCallRequest,
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

use futures::FutureExt;
use futures::StreamExt;
use tokio::sync::mpsc;
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

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

    // Wrap entire body in catch_unwind for panic safety.
    // On panic, session is reset to Idle before re-raising.
    let result = AssertUnwindSafe(async move {
        // Helper: convert AgentEvent to AgentMessage for global_tx forwarding
        fn event_to_msg(event: &AgentEvent) -> Option<AgentMessage> {
            match event {
                AgentEvent::TextDelta(s) => Some(AgentMessage::TextDelta(s.clone())),
                AgentEvent::ThinkingBlock(v) => Some(AgentMessage::ThinkingBlock(v.clone())),
                AgentEvent::UsageInfo { input_tokens, output_tokens, tool_calls, cache_creation_input_tokens, cache_read_input_tokens } => {
                    Some(AgentMessage::UsageInfo { input_tokens: *input_tokens, output_tokens: *output_tokens, tool_calls: *tool_calls, cache_creation_input_tokens: *cache_creation_input_tokens, cache_read_input_tokens: *cache_read_input_tokens })
                }
                AgentEvent::StatusUpdate(s) => Some(AgentMessage::StatusUpdate(s.clone())),
                AgentEvent::Error { code, message } => Some(AgentMessage::Error { code: code.clone(), message: message.clone() }),
                AgentEvent::ToolCallRequest { call_id, tool_name, arguments } => {
                    Some(AgentMessage::ToolCallRequest { call_id: call_id.clone(), tool_name: tool_name.clone(), arguments: arguments.clone() })
                }
                AgentEvent::ToolCallResult { call_id, tool_name, content, is_error } => {
                    Some(AgentMessage::ToolCallResult { call_id: call_id.clone(), tool_name: tool_name.clone(), content: content.clone(), is_error: *is_error })
                }
                AgentEvent::UserQuery { .. } => None, // oneshot not clonable
                AgentEvent::Done => Some(AgentMessage::Done),
            }
        }

        // Helper: send event, return false if receiver dropped
        macro_rules! try_send {
            ($event:expr) => {{
                let event = $event;
                // Forward to global_tx (for orchestrator)
                if let Some(ref gtx) = ctx.global_tx {
                    if let Some(msg) = event_to_msg(&event) {
                        let _ = gtx.send(Envelope {
                            session_id: ctx.session_id.clone(),
                            message: msg,
                        }).await;
                    }
                }
                // Send to tx (for CLI)
                if tx.send(event).await.is_err() {
                    let _ = sm.finish_loop(&sid, SessionStatus::Error);
                    return;
                }
            }};
        }

        // Early cancellation check before appending
        if ctx.cancel_token.is_cancelled() {
            try_send!(AgentEvent::Error {
                code: AgentErrorCode::Cancelled,
                message: "Agent loop cancelled".into(),
            });
            let _ = sm.finish_loop(&sid, SessionStatus::Error);
            return;
        }

        // Clean up orphan tool_uses from previous cancelled runs.
        // If the last assistant message has tool_calls but no corresponding
        // tool_result messages follow it, strip the tool_calls to prevent
        // Anthropic API 400 errors (tool_use without tool_result).
        cleanup_orphan_tool_uses(&mut ctx.history);

        // 1. Append user message to session store and local history
        if let Err(e) = sm.append_message(&sid, user_message.clone()) {
            let _ = tx
                .send(AgentEvent::Error {
                    code: AgentErrorCode::Internal,
                    message: format!("Failed to append user message: {e}"),
                })
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
            // a. Cancellation check
            if ctx.cancel_token.is_cancelled() {
                try_send!(AgentEvent::Error {
                    code: AgentErrorCode::Cancelled,
                    message: "Agent loop cancelled".into(),
                });
                let _ = sm.finish_loop(&sid, SessionStatus::Error);
                return;
            }

            // b. Limits check（在 LLM 调用之前）
            if iteration >= cfg.hard_limit {
                try_send!(AgentEvent::Error {
                    code: AgentErrorCode::MaxIterations,
                    message: format!(
                        "Agent loop reached hard limit ({})",
                        cfg.hard_limit
                    ),
                });
                let _ = sm.finish_loop(&sid, SessionStatus::Error);
                return;
            }
            // c. Build prompt

            let session = match sm.get(&sid) {
                Ok(s) => s,
                Err(e) => {
                    try_send!(AgentEvent::Error {
                        code: AgentErrorCode::Internal,
                        message: format!("Failed to get session: {e}"),
                    });
                    let _ = sm.finish_loop(&sid, SessionStatus::Error);
                    return;
                }
            };
            // 生成日期字符串
            let date_str = chrono::Local::now().format("%Y-%m-%d").to_string();

            // 渲染动态工具指南并追加到 system prompt
            let tool_guide = render_tool_guide(&tool_registry);
            let mut enriched_template = if tool_guide.is_empty() {
                session.system_prompt_template.clone()
            } else {
                format!("{}{}", session.system_prompt_template, tool_guide)
            };

            // Soft limit check: 注入收尾提示
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

            // 防御性清理：确保 tool_calls/tool_result 配对完整，
            // 防止因上下文裁剪导致 orphan tool_calls 被发送到 LLM 引发 400 错误。
            cleanup_orphan_tool_uses(&mut messages);
            messages.retain(|m| !m.skip_context);

            // c. Get tool definitions
            let tools = tool_registry.definitions();

            // 记录上下文大小
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

            // 调试：取消注释下方行，将完整 prompt（messages + tools）保存到 .visp/last-prompt.json
            // dump_prompt_to_file(&ctx.working_dir, &messages, &tools);

            // d. Call LLM with retry
            let stream = {
                let mut attempt = 0u32;
                loop {
                    match provider.chat_stream(&messages, &tools, &ctx.config).await {
                        Ok(s) => break s,
                        Err(e @ (LlmError::RateLimit { .. } | LlmError::Network(_))) => {
                            if attempt >= cfg.llm_retry_attempts {
                                let (code, msg) = llm_error_to_code(&e);
                                tracing::error!(
                                    session_id = %sid,
                                    error_code = ?code,
                                    error_msg = %msg,
                                    attempts = attempt + 1,
                                    "LLM provider error after retries exhausted"
                                );
                                try_send!(AgentEvent::Error { code, message: msg });
                                let _ = sm.finish_loop(&sid, SessionStatus::Error);
                                return;
                            }
                            let delay = cfg.llm_retry_base_delay_ms * (1u64 << attempt);
                            tokio::time::sleep(Duration::from_millis(delay)).await;
                            attempt += 1;
                        }
                        Err(e) => {
                            let (code, msg) = llm_error_to_code(&e);
                            tracing::error!(
                                session_id = %sid,
                                error_code = ?code,
                                error_msg = %msg,
                                "LLM provider error"
                            );
                            try_send!(AgentEvent::Error { code, message: msg });
                            let _ = sm.finish_loop(&sid, SessionStatus::Error);
                            return;
                        }
                    }
                }
            };

            // e. Collect events
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
                        try_send!(AgentEvent::Error {
                            code: AgentErrorCode::Cancelled,
                            message: "Agent loop cancelled".into(),
                        });
                        let _ = sm.finish_loop(&sid, SessionStatus::Error);
                        return;
                    }
                    event = pin_stream.next() => {
                        match event {
                            Some(Ok(ChatEvent::TextDelta(delta))) => {
                                text_buffer.push_str(&delta);
                                try_send!(AgentEvent::TextDelta(delta));
                            }
                            Some(Ok(ChatEvent::ThinkingBlock(block))) => {
                                // 流式 thinking: 保留最新的完整 block（替换而非累积）
                                thinking_blocks.clear();
                                thinking_blocks.push(block.clone());
                                try_send!(AgentEvent::ThinkingBlock(block));
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
                            // Done 表示 stream 正常结束，byte_stream_to_chat_events
                            // 始终在 stream 结束前发射 Done，后续 None 不会到达此分支
                            Some(Ok(ChatEvent::Done)) => break,
                            Some(Err(e)) => {
                                let (code, msg) = llm_error_to_code(&e);
                                try_send!(AgentEvent::Error { code, message: msg });
                                let _ = sm.finish_loop(&sid, SessionStatus::Error);
                                return;
                            }
                            // None 代表 stream 意外中断（API 超时断连等），未收到 Done 标记
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
                                try_send!(AgentEvent::Error {
                                    code: AgentErrorCode::Internal,
                                    message: "LLM stream ended unexpectedly — the response may be incomplete, check API connection".into(),
                                });
                                let _ = sm.finish_loop(&sid, SessionStatus::Error);
                                return;
                            }
                        }
                    }
                }
            }

            // f. Decide: no tool calls → check [USER_QUERY] marker or done
            if tool_calls.is_empty() {
                // Check [USER_QUERY] marker
                if let Some(marker) = parse_user_query_marker(&text_buffer) {
                    let clean_text = strip_user_query_marker(&text_buffer);
                    let thinking_text = extract_thinking_text(&thinking_blocks);

                    // Save thinking as a separate message if present
                    if let Some(ref thinking) = thinking_text {
                        let mut thinking_msg = Message::thinking(thinking.clone());
                        thinking_msg.estimated_tokens = estimate_message_tokens(&thinking_msg);
                        ctx.history.push(thinking_msg.clone());
                        if let Err(e) = sm.append_message(&sid, thinking_msg) {
                            try_send!(AgentEvent::Error {
                                code: AgentErrorCode::Internal,
                                message: format!("Failed to append thinking message: {e}"),
                            });
                            let _ = sm.finish_loop(&sid, SessionStatus::Error);
                            return;
                        }
                    }

                    // Save text message if present (keep extra_blocks for provider round-trip compat)
                    if !clean_text.is_empty() {
                        let mut text_msg = Message::assistant(clean_text.clone());
                        text_msg.extra_blocks = if thinking_blocks.is_empty() {
                            None
                        } else {
                            Some(thinking_blocks.clone())
                        };
                        text_msg.estimated_tokens = estimate_message_tokens(&text_msg);
                        ctx.history.push(text_msg.clone());
                        if let Err(e) = sm.append_message(&sid, text_msg) {
                            try_send!(AgentEvent::Error {
                                code: AgentErrorCode::Internal,
                                message: format!("Failed to append assistant message: {e}"),
                            });
                            let _ = sm.finish_loop(&sid, SessionStatus::Error);
                            return;
                        }
                    }

                    // Send UserQuery event
                    let (resp_tx, resp_rx) = oneshot::channel::<UserQueryResult>();
                    try_send!(AgentEvent::UserQuery {
                        query_id: format!("query-{}", ctx.history.len()),
                        message: marker.message.clone(),
                        options: marker.options.clone(),
                        allow_other: marker.allow_other,
                        respond: resp_tx,
                    });

                    let query_result = resp_rx.await.unwrap_or_default();

                    // Build user message from result
                    let user_msg = if query_result.selected_index >= 0
                        && (query_result.selected_index as usize) < marker.options.len()
                    {
                        let option_text = marker.options[query_result.selected_index as usize].clone();
                        Message::user(option_text)
                    } else {
                        Message::user(query_result.text)
                    };
                    ctx.history.push(user_msg.clone());
                    if let Err(e) = sm.append_message(&sid, user_msg) {
                        try_send!(AgentEvent::Error {
                            code: AgentErrorCode::Internal,
                            message: format!("Failed to append user message: {e}"),
                        });
                        let _ = sm.finish_loop(&sid, SessionStatus::Error);
                        return;
                    }

                    // Continue to next iteration
                    continue;
                }

                // No [USER_QUERY] marker: done
                if text_buffer.is_empty() && tool_calls.is_empty() && thinking_blocks.is_empty() {
                    if output_tokens > 0 {
                        // LLM consumed output tokens but produced nothing useful.
                        // This can happen when thinking mode is enabled and the thinking
                        // is redacted by the API (exceeds budget), or the output budget
                        // is entirely exhausted without producing usable content.
                        tracing::error!(
                            session_id = %sid,
                            input_tokens,
                            output_tokens,
                            "LLM returned empty response after consuming output tokens"
                        );
                        try_send!(AgentEvent::Error {
                            code: AgentErrorCode::Internal,
                            message: format!(
                                "LLM returned empty response after consuming {output_tokens} output tokens. \
                             If thinking mode is enabled, the thinking may have been redacted or \
                             the output budget exhausted. Try increasing budget_tokens or max_tokens."
                            ),
                        });
                        let _ = sm.finish_loop(&sid, SessionStatus::Error);
                        return;
                    }
                    // output_tokens == 0: no tokens consumed — likely a provider
                    // returned an empty stream, just warn and return normally
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
                let thinking_text = extract_thinking_text(&thinking_blocks);

                // Save thinking as a separate message if present
                if let Some(ref thinking) = thinking_text {
                    let mut thinking_msg = Message::thinking(thinking.clone());
                    thinking_msg.estimated_tokens = estimate_message_tokens(&thinking_msg);
                    ctx.history.push(thinking_msg.clone());
                    if let Err(e) = sm.append_message(&sid, thinking_msg) {
                        try_send!(AgentEvent::Error {
                            code: AgentErrorCode::Internal,
                            message: format!("Failed to append thinking message: {e}"),
                        });
                        let _ = sm.finish_loop(&sid, SessionStatus::Error);
                        return;
                    }
                }

                // Save text message if present (keep extra_blocks for provider round-trip compat)
                if !text_buffer.is_empty() {
                    let mut text_msg = Message::assistant(text_buffer.clone());
                    text_msg.extra_blocks = if thinking_blocks.is_empty() {
                        None
                    } else {
                        Some(thinking_blocks.clone())
                    };
                    text_msg.estimated_tokens = estimate_message_tokens(&text_msg);
                    ctx.history.push(text_msg.clone());
                    if let Err(e) = sm.append_message(&sid, text_msg) {
                        try_send!(AgentEvent::Error {
                            code: AgentErrorCode::Internal,
                            message: format!("Failed to append assistant message: {e}"),
                        });
                        let _ = sm.finish_loop(&sid, SessionStatus::Error);
                        return;
                    }
                }
                // 发送用量统计后再发送 Done
                try_send!(AgentEvent::UsageInfo {
                    input_tokens,
                    output_tokens,
                    tool_calls: total_tool_calls,
                    cache_creation_input_tokens,
                    cache_read_input_tokens,
                });
                try_send!(AgentEvent::Done);
                let _ = sm.finish_loop(&sid, SessionStatus::Completed);
                return;
            }

            // Has tool calls: append assistant message with tool_calls
            total_tool_calls += tool_calls.len() as u32;
            let mut assistant_msg = Message {
                role: Role::Assistant,
                kind: MessageType::ToolCall,
                content: text_buffer,
                tool_call_id: None,
                tool_calls: Some(tool_calls.clone()),
                skip_context: false,
                extra_blocks: if thinking_blocks.is_empty() {
                    None
                } else {
                    Some(thinking_blocks.clone())
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
            if let Err(e) = sm.append_message(&sid, assistant_msg) {
                try_send!(AgentEvent::Error {
                    code: AgentErrorCode::Internal,
                    message: format!("Failed to append assistant message: {e}"),
                });
                let _ = sm.finish_loop(&sid, SessionStatus::Error);
                return;
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
                    let first = &doom_loop_window[0];
                    let all_same = doom_loop_window.iter().all(|sig| sig == first);
                    if all_same {
                        if doom_loop_warned {
                            try_send!(AgentEvent::Error {
                                code: AgentErrorCode::StuckInLoop,
                                message: "Agent stuck in repeated tool call loop after warning".into(),
                            });
                            let _ = sm.finish_loop(&sid, SessionStatus::Error);
                            return;
                        }
                        doom_loop_warned = true;
                        doom_loop_window.clear();
                        try_send!(AgentEvent::StatusUpdate(
                            "Agent appears stuck in a loop of repeated tool calls".into()
                        ));
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
            // Store tool IDs indexed by spawn order, for error recovery when a task panics.
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

                    // Send SpawnRequest via global_tx
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
                let session_id = sid.clone();
                let working_dir = ctx.working_dir.clone();
                let tc = tc.clone();
                let sm = sm.clone();
                let permissions = ctx.permission_rules.clone();
                let sid2 = sid.clone();

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
                        // Forward to global_tx before sending to tx
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

                    // Check if tool is already approved (Always Allow)
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
                            0 => {
                                // Approve - continue
                            }
                            2 => {
                                // Always Allow
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

                    // Send result
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

            // Phase 2: Collect results (select! with support for inbox_rx)
            let task_results = if is_multi_agent {
                let mut collected: Vec<ToolExecResult> = Vec::new();
                let mut regular_done = exec_tasks.is_empty();
                let mut inbox = ctx.inbox_rx.take();

                loop {
                    // Check cancellation first
                    if ctx.cancel_token.is_cancelled() {
                        for h in &exec_tasks { h.abort(); }
                        break;
                    }

                    let has_tasks = !exec_tasks.is_empty();
                    let has_pending = !pending_spawns.is_empty();

                    if !has_tasks && !has_pending {
                        break;
                    }

                    if has_tasks && inbox.is_some() && has_pending {
                        // Both regular tasks and sub-agent results pending: select!
                        let all_tasks = std::mem::take(&mut exec_tasks);
                        let join_fut = futures::future::join_all(
                            all_tasks
                        );
                        let recv_fut = async {
                            // SAFETY: guarded by inbox.is_some() above
                            inbox.as_mut().unwrap().recv().await
                        };

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
                                        break;
                                    }
                                    None => {}
                                }
                            }
                        }
                    } else if has_tasks {
                        // Only regular tasks remaining
                        let all_tasks = std::mem::take(&mut exec_tasks);
                        let results = futures::future::join_all(
                            all_tasks
                        ).await;
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
                    } else if let Some(ref mut rx) = inbox {
                        // Only sub-agent results pending
                        match rx.recv().await {
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
                            Some(OrchestratorMessage::Cancelled) => break,
                            None => break,
                        }
                    }

                    if regular_done && pending_spawns.is_empty() {
                        break;
                    }
                }

                // Restore inbox_rx (now None since we took it)
                ctx.inbox_rx = inbox;

                collected
            } else {
                // Single-agent mode: original join_all
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

            // h. Append tool results to history (in original order)
            let mut sorted_results: Vec<ToolExecResult> = task_results;
            sorted_results.sort_by_key(|r| r.index);

            for tr in sorted_results {
                let tool_msg = Message::tool(tr.result.content, &tr.call_id);
                ctx.history.push(tool_msg.clone());
                if let Err(e) = sm.append_message(&sid, tool_msg) {
                    try_send!(AgentEvent::Error {
                        code: AgentErrorCode::Internal,
                        message: format!("Failed to append tool result: {e}"),
                    });
                    let _ = sm.finish_loop(&sid, SessionStatus::Error);
                    return;
                }
            }
            // i. Increment iteration
            iteration += 1;
        }
    })
    .catch_unwind()
    .await;

    // If panicked, reset session status
    if let Err(panic) = result {
        let _ = sm_panic.finish_loop(&sid_panic, SessionStatus::Idle);
        std::panic::resume_unwind(panic);
    }
}
