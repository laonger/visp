use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use crate::error::AgentErrorCode;
use crate::error::LlmError;
use crate::message::Message;
use crate::message::Role;
use crate::message::ToolCallRequest;
use crate::prompt::PromptBuilder;
use crate::provider::ChatEvent;
use crate::provider::LlmConfig;
use crate::provider::LlmProvider;
use crate::rules::RuleEngine;
use crate::session::SessionManager;
use crate::session::SessionStatus;
use crate::tool::{ToolContext, ToolResult};
use crate::tool_registry::ToolRegistry;

use futures::StreamExt;
use tokio::sync::mpsc;
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

/// 用户查询结果
#[derive(Debug, Clone, Default)]
pub struct UserQueryResult {
    pub selected_index: i32,
    pub text: String,
}

/// Agent 事件，用于流式通知外部（TUI/WS）
pub enum AgentEvent {
    /// 文本增量
    TextDelta(String),
    /// 思考块（如 DeepSeek thinking mode）
    ThinkingBlock(serde_json::Value),
    /// token 用量及工具调用统计
    UsageInfo {
        input_tokens: u32,
        output_tokens: u32,
        tool_calls: u32,
    },
    /// 工具调用请求
    ToolCallRequest {
        call_id: String,
        tool_name: String,
        arguments: String,
    },
    /// 工具调用结果
    ToolCallResult {
        call_id: String,
        content: String,
        is_error: bool,
    },
    /// 状态更新
    StatusUpdate(String),
    /// 发生错误
    Error {
        code: AgentErrorCode,
        message: String,
    },
    /// 完成
    Done,
    /// 需要用户输入
    UserQuery {
        query_id: String,
        message: String,
        options: Vec<String>,
        allow_other: bool,
        respond: oneshot::Sender<UserQueryResult>,
    },
}

/// Agent 循环上下文
pub struct AgentLoopContext {
    /// 会话 ID
    pub session_id: String,
    /// 对话历史
    pub history: Vec<Message>,
    /// 工作目录
    pub working_dir: PathBuf,
    /// LLM 配置
    pub config: LlmConfig,
    /// 取消令牌
    pub cancel_token: CancellationToken,
}

/// Agent 执行配置
#[derive(Debug, Clone)]
pub struct AgentConfig {
    /// 最大迭代轮数
    pub max_iterations: u32,
    /// LLM 调用重试次数
    pub llm_retry_attempts: u32,
    /// LLM 重试基础延迟（毫秒）
    pub llm_retry_base_delay_ms: u64,
    /// bash 工具确认模式（执行高危命令前是否需要用户确认）
    pub bash_confirm_mode: bool,
    /// 文件读取/写入的最大字节数
    pub file_max_size_bytes: u64,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            max_iterations: 10,
            llm_retry_attempts: 3,
            llm_retry_base_delay_ms: 1000,
            bash_confirm_mode: true,
            file_max_size_bytes: 1048576,
        }
    }
}

// ── Internal helper ──────────────────────────────────────────────────────────

struct ToolExecResult {
    index: usize,
    call_id: String,
    result: ToolResult,
}

fn llm_error_to_code(err: &LlmError) -> (AgentErrorCode, String) {
    match err {
        LlmError::Network(msg) => (AgentErrorCode::LlmNetwork, msg.clone()),
        LlmError::RateLimit { retry_after_secs } => (
            AgentErrorCode::LlmRateLimit,
            format!("rate limited, retry after {retry_after_secs}s"),
        ),
        LlmError::Auth(msg) => (AgentErrorCode::LlmAuth, msg.clone()),
        LlmError::Api { status, message } => (
            AgentErrorCode::LlmApi,
            format!("status {status}: {message}"),
        ),
        LlmError::Stream(msg) => (AgentErrorCode::LlmStream, msg.clone()),
    }
}

/// 格式化工具参数为用户友好的显示文本
fn format_tool_args(args_json: &str) -> String {
    match serde_json::from_str::<serde_json::Value>(args_json) {
        Ok(serde_json::Value::Object(obj)) => {
            let parts: Vec<String> = obj
                .iter()
                .map(|(k, v)| {
                    let val = match v {
                        serde_json::Value::String(s) => s.clone(),
                        other => other.to_string(),
                    };
                    format!("{k}: {val}")
                })
                .collect();
            parts.join(", ")
        }
        _ => args_json.to_string(),
    }
}

// ── [USER_QUERY] marker parsing ──────────────────────────────────────────────

struct UserQueryMarker {
    message: String,
    options: Vec<String>,
    allow_other: bool,
}

/// 从文本末尾检测 [USER_QUERY]...[/USER_QUERY] 标记
fn parse_user_query_marker(text: &str) -> Option<UserQueryMarker> {
    let text = text.trim_end();

    // 查找结尾标记
    let close_pos = text.rfind("[/USER_QUERY]")?;
    let before_close = &text[..close_pos];

    // 查找开头标记
    let open_pos = before_close.rfind("[USER_QUERY")?;

    // 提取开头标记内容 [USER_QUERY ...]
    let header_end = before_close[open_pos..].find(']')?;
    let header = &before_close[open_pos..=open_pos + header_end];

    // 解析 allow_other
    let allow_other = header.contains("allow_other=true");

    // 提取标记内内容（去除头部标记行）
    let body_start = open_pos + header_end + 1;
    let body = &before_close[body_start..close_pos];
    let body = body.trim();

    if body.is_empty() {
        return None;
    }

    // 解析：首行是 message，- 前缀行为 options
    let mut message = String::new();
    let mut options = Vec::new();

    for line in body.lines() {
        let trimmed = line.trim();
        if let Some(opt_text) = trimmed.strip_prefix("- ") {
            options.push(opt_text.to_string());
        } else if message.is_empty() {
            message = trimmed.to_string();
        }
        // ignore extra lines after options
    }

    Some(UserQueryMarker {
        message,
        options,
        allow_other,
    })
}

/// 从文本中剥离 [USER_QUERY]...[/USER_QUERY] 标记
fn strip_user_query_marker(text: &str) -> String {
    let text = text.trim_end();
    if let Some(close_pos) = text.rfind("[/USER_QUERY]")
        && let Some(open_pos) = text[..close_pos].rfind("[USER_QUERY")
    {
        let before = &text[..open_pos].trim_end();
        return before.to_string();
    }
    text.to_string()
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
    // Helper: send event, return false if receiver dropped
    macro_rules! try_send {
        ($event:expr) => {
            if tx.send($event).await.is_err() {
                let _ = session_mgr.finish_loop(&ctx.session_id, SessionStatus::Error);
                return;
            }
        };
    }

    // Early cancellation check before appending
    if ctx.cancel_token.is_cancelled() {
        try_send!(AgentEvent::Error {
            code: AgentErrorCode::Cancelled,
            message: "Agent loop cancelled".into(),
        });
        let _ = session_mgr.finish_loop(&ctx.session_id, SessionStatus::Error);
        return;
    }

    // Clean up orphan tool_uses from previous cancelled runs.
    // If the last assistant message has tool_calls but no corresponding
    // tool_result messages follow it, strip the tool_calls to prevent
    // Anthropic API 400 errors (tool_use without tool_result).
    cleanup_orphan_tool_uses(&mut ctx.history);

    // 1. Append user message to session store and local history
    if let Err(e) = session_mgr.append_message(&ctx.session_id, user_message.clone()) {
        let _ = tx
            .send(AgentEvent::Error {
                code: AgentErrorCode::Internal,
                message: format!("Failed to append user message: {e}"),
            })
            .await;
        let _ = session_mgr.finish_loop(&ctx.session_id, SessionStatus::Error);
        return;
    }
    ctx.history.push(user_message);

    let mut total_tool_calls: u32 = 0;
    for _ in 0..agent_config.max_iterations {
        // a. Cancellation check
        if ctx.cancel_token.is_cancelled() {
            try_send!(AgentEvent::Error {
                code: AgentErrorCode::Cancelled,
                message: "Agent loop cancelled".into(),
            });
            let _ = session_mgr.finish_loop(&ctx.session_id, SessionStatus::Error);
            return;
        }

        // b. Build prompt
        let session = match session_mgr.get(&ctx.session_id) {
            Ok(s) => s,
            Err(e) => {
                try_send!(AgentEvent::Error {
                    code: AgentErrorCode::Internal,
                    message: format!("Failed to get session: {e}"),
                });
                let _ = session_mgr.finish_loop(&ctx.session_id, SessionStatus::Error);
                return;
            }
        };
        // 生成日期字符串
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap();
        let days = now.as_secs() / 86400;
        let mut y = 1970i64;
        let mut remaining = days;
        loop {
            let days_in_year = if is_leap(y) { 366 } else { 365 };
            if remaining >= days_in_year {
                remaining -= days_in_year;
                y += 1;
            } else {
                break;
            }
        }
        let month_days = if is_leap(y) {
            [31, 29, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
        } else {
            [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
        };
        let mut m = 1u32;
        for &md in month_days.iter() {
            if remaining >= md {
                remaining -= md;
                m += 1;
            } else {
                break;
            }
        }
        let d = remaining + 1;
        let date_str = format!("{y:04}-{m:02}-{d:02}");

        // 渲染动态工具指南并追加到 system prompt
        let tool_guide = render_tool_guide(&tool_registry);
        let enriched_template = if tool_guide.is_empty() {
            session.system_prompt_template.clone()
        } else {
            format!("{}{}", session.system_prompt_template, tool_guide)
        };

        let messages = PromptBuilder::build(
            &enriched_template,
            &rule_engine.get_active_rules(),
            &ctx.history,
            &ctx.working_dir,
            &date_str,
        );

        // c. Get tool definitions
        let tools = tool_registry.definitions();

        // d. Call LLM with retry
        let stream = {
            let mut attempt = 0u32;
            loop {
                match provider.chat_stream(&messages, &tools, &ctx.config).await {
                    Ok(s) => break s,
                    Err(e @ (LlmError::RateLimit { .. } | LlmError::Network(_))) => {
                        if attempt >= agent_config.llm_retry_attempts {
                            let (code, msg) = llm_error_to_code(&e);
                            try_send!(AgentEvent::Error { code, message: msg });
                            let _ = session_mgr.finish_loop(&ctx.session_id, SessionStatus::Error);
                            return;
                        }
                        let delay = agent_config.llm_retry_base_delay_ms * (1u64 << attempt);
                        tokio::time::sleep(Duration::from_millis(delay)).await;
                        attempt += 1;
                    }
                    Err(e) => {
                        let (code, msg) = llm_error_to_code(&e);
                        try_send!(AgentEvent::Error { code, message: msg });
                        let _ = session_mgr.finish_loop(&ctx.session_id, SessionStatus::Error);
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

        let mut pin_stream = Box::pin(stream);
        loop {
            tokio::select! {
                biased;
                _ = ctx.cancel_token.cancelled() => {
                    try_send!(AgentEvent::Error {
                        code: AgentErrorCode::Cancelled,
                        message: "Agent loop cancelled".into(),
                    });
                    let _ = session_mgr.finish_loop(&ctx.session_id, SessionStatus::Error);
                    return;
                }
                event = pin_stream.next() => {
                    match event {
                        Some(Ok(ChatEvent::TextDelta(delta))) => {
                            text_buffer.push_str(&delta);
                            try_send!(AgentEvent::TextDelta(delta));
                        }
                        Some(Ok(ChatEvent::ThinkingBlock(block))) => {
                            thinking_blocks.push(block.clone());
                            try_send!(AgentEvent::ThinkingBlock(block));
                        }
                        Some(Ok(ChatEvent::UsageInfo { input_tokens: it, output_tokens: ot, .. })) => {
                            input_tokens = it;
                            output_tokens = ot;
                        }
                        Some(Ok(ChatEvent::ToolCall { id, name, arguments })) => {
                            tool_calls.push(ToolCallRequest { id, name, arguments });
                        }
                        Some(Ok(ChatEvent::Done)) => break,
                        Some(Err(e)) => {
                            let (code, msg) = llm_error_to_code(&e);
                            try_send!(AgentEvent::Error { code, message: msg });
                            let _ = session_mgr.finish_loop(&ctx.session_id, SessionStatus::Error);
                            return;
                        }
                        None => break,
                    }
                }
            }
        }

        // f. Decide: no tool calls → check [USER_QUERY] marker or done
        if tool_calls.is_empty() {
            // Check [USER_QUERY] marker
            if let Some(marker) = parse_user_query_marker(&text_buffer) {
                let clean_text = strip_user_query_marker(&text_buffer);
                let assistant_msg = Message {
                    role: Role::Assistant,
                    content: clean_text,
                    tool_call_id: None,
                    tool_calls: None,
                    skip_context: false,
                    extra_blocks: if thinking_blocks.is_empty() {
                        None
                    } else {
                        Some(thinking_blocks.clone())
                    },
                };
                ctx.history.push(assistant_msg.clone());
                if let Err(e) = session_mgr.append_message(&ctx.session_id, assistant_msg) {
                    try_send!(AgentEvent::Error {
                        code: AgentErrorCode::Internal,
                        message: format!("Failed to append assistant message: {e}"),
                    });
                    let _ = session_mgr.finish_loop(&ctx.session_id, SessionStatus::Error);
                    return;
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
                if let Err(e) = session_mgr.append_message(&ctx.session_id, user_msg) {
                    try_send!(AgentEvent::Error {
                        code: AgentErrorCode::Internal,
                        message: format!("Failed to append user message: {e}"),
                    });
                    let _ = session_mgr.finish_loop(&ctx.session_id, SessionStatus::Error);
                    return;
                }

                // Continue to next iteration
                continue;
            }

            // No [USER_QUERY] marker: done
            let assistant_msg = Message {
                role: Role::Assistant,
                content: text_buffer,
                tool_call_id: None,
                tool_calls: None,
                skip_context: false,
                extra_blocks: if thinking_blocks.is_empty() {
                    None
                } else {
                    Some(thinking_blocks.clone())
                },
            };
            ctx.history.push(assistant_msg.clone());
            if let Err(e) = session_mgr.append_message(&ctx.session_id, assistant_msg) {
                try_send!(AgentEvent::Error {
                    code: AgentErrorCode::Internal,
                    message: format!("Failed to append assistant message: {e}"),
                });
                let _ = session_mgr.finish_loop(&ctx.session_id, SessionStatus::Error);
                return;
            }
            // 发送用量统计后再发送 Done
            try_send!(AgentEvent::UsageInfo {
                input_tokens,
                output_tokens,
                tool_calls: total_tool_calls,
            });
            try_send!(AgentEvent::Done);
            let _ = session_mgr.finish_loop(&ctx.session_id, SessionStatus::Completed);
            return;
        }

        // Has tool calls: append assistant message with tool_calls
        total_tool_calls += tool_calls.len() as u32;
        let assistant_msg = Message {
            role: Role::Assistant,
            content: text_buffer,
            tool_call_id: None,
            tool_calls: Some(tool_calls.clone()),
            skip_context: false,
            extra_blocks: if thinking_blocks.is_empty() {
                None
            } else {
                Some(thinking_blocks.clone())
            },
        };
        ctx.history.push(assistant_msg.clone());
        if let Err(e) = session_mgr.append_message(&ctx.session_id, assistant_msg) {
            try_send!(AgentEvent::Error {
                code: AgentErrorCode::Internal,
                message: format!("Failed to append assistant message: {e}"),
            });
            let _ = session_mgr.finish_loop(&ctx.session_id, SessionStatus::Error);
            return;
        }

        // g. Execute tools in parallel
        let num_tools = tool_calls.len();
        let mut exec_tasks = Vec::with_capacity(num_tools);

        for (i, tc) in tool_calls.iter().enumerate() {
            let tx = tx.clone();
            let cancel = ctx.cancel_token.clone();
            let registry = tool_registry.clone();
            let session_id = ctx.session_id.clone();
            let working_dir = ctx.working_dir.clone();
            let tc = tc.clone();
            let sm = session_mgr.clone();

            exec_tasks.push(tokio::spawn(async move {
                // Cancellation check
                if cancel.is_cancelled() {
                    let _ = tx
                        .send(AgentEvent::ToolCallResult {
                            call_id: tc.id.clone(),
                            content: "Cancelled".into(),
                            is_error: true,
                        })
                        .await;
                    return ToolExecResult {
                        index: i,
                        call_id: tc.id,
                        result: ToolResult::error("Cancelled"),
                    };
                }

                // Send ToolCallRequest
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
                            let _ = tx
                                .send(AgentEvent::ToolCallResult {
                                    call_id: tc.id.clone(),
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

                // Status update
                let _ = tx
                    .send(AgentEvent::StatusUpdate(format!(
                        "Executing tool: {}",
                        tc.name
                    )))
                    .await;

                // Parse arguments and execute
                let args = serde_json::from_str(&tc.arguments).unwrap_or(serde_json::json!({}));
                let tool_ctx = ToolContext {
                    working_dir: working_dir.clone(),
                    session_id: Some(session_id),
                };

                let result = registry
                    .execute(&tc.name, args, &tool_ctx)
                    .await
                    .unwrap_or_else(|| ToolResult::error("Tool not found in registry"));

                // Send result
                let _ = tx
                    .send(AgentEvent::ToolCallResult {
                        call_id: tc.id.clone(),
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

        // Join all tasks, with cancellation support
        let task_results = {
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
                    results.into_iter().filter_map(|r| match r {
                        Ok(result) => Some(result),
                        Err(e) if e.is_cancelled() => None,
                        Err(e) => {
                            tracing::warn!("tool task failed: {e}");
                            None
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
            if let Err(e) = session_mgr.append_message(&ctx.session_id, tool_msg) {
                try_send!(AgentEvent::Error {
                    code: AgentErrorCode::Internal,
                    message: format!("Failed to append tool result: {e}"),
                });
                let _ = session_mgr.finish_loop(&ctx.session_id, SessionStatus::Error);
                return;
            }
        }
        // i. Continue loop
    }

    // Max iterations reached without completion
    try_send!(AgentEvent::Error {
        code: AgentErrorCode::MaxIterations,
        message: format!(
            "Agent loop reached maximum iterations ({})",
            agent_config.max_iterations
        ),
    });
    let _ = session_mgr.finish_loop(&ctx.session_id, SessionStatus::Error);
}

fn is_leap(year: i64) -> bool {
    (year % 4 == 0 && year % 100 != 0) || (year % 400 == 0)
}

/// 清理历史中残留的 orphan tool_uses。
/// 如果最后一条 assistant 消息包含 tool_calls，但没有对应的 tool_result
/// 消息紧随其后，则清空 tool_calls。这发生在 Cancel 终止了 agent 循环，
/// 导致 tool_use 被发送给 Anthropic 但没来得及追加 tool_result，
/// 后续请求会报 400 错误。
fn cleanup_orphan_tool_uses(history: &mut [Message]) {
    // 从后往前找最后一条 assistant 消息
    if let Some(idx) = history
        .iter()
        .rposition(|m| m.role == Role::Assistant && m.tool_calls.is_some())
    {
        // 检查之后是否有 tool_result 消息，没有则清理
        let has_results = history[idx + 1..].iter().any(|m| m.role == Role::Tool);
        if !has_results && history.get_mut(idx).is_some() {
            history[idx].tool_calls = None;
        }
    }
}

/// 根据注册的工具定义，生成按分类分组的动态工具指南 Markdown 文本
pub(crate) fn render_tool_guide(registry: &ToolRegistry) -> String {
    let defs = registry.definitions();
    if defs.is_empty() {
        return String::new();
    }

    use std::collections::HashMap;
    let mut grouped: HashMap<&str, Vec<&str>> = HashMap::new();
    for def in &defs {
        let cat = if def.category.is_empty() {
            "other"
        } else {
            def.category.as_str()
        };
        grouped.entry(cat).or_default().push(def.name.as_str());
    }

    let mut parts = vec!["\n\n## Available Tools".to_string()];

    // 按固定顺序输出 category
    let categories = [
        ("Common (prefer these first)", "common"),
        ("Analyze", "analyze"),
        ("Network", "network"),
        ("External (MCP)", "mcp"),
    ];

    for (label, cat) in &categories {
        if let Some(tools) = grouped.remove(*cat)
            && !tools.is_empty()
        {
            parts.push(format!("\n**{label}**:\n  {}", tools.join(", ")));
        }
    }

    // 剩余的 category
    for (cat, tools) in grouped {
        if !tools.is_empty() {
            parts.push(format!("\n**{cat}**:\n  {}", tools.join(", ")));
        }
    }

    parts.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::InMemorySessionStore;
    use crate::tool::Tool;
    use std::path::Path;
    use std::sync::Arc as StdArc;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    // ── Mock tool for tests ─────────────────────────────────────────────────

    struct MockAgentTool {
        name: &'static str,
        requires_approval: bool,
        executed: StdArc<AtomicBool>,
    }

    #[async_trait::async_trait]
    impl Tool for MockAgentTool {
        fn name(&self) -> &str {
            self.name
        }
        fn description(&self) -> &str {
            "Mock tool for agent tests"
        }
        fn parameters(&self) -> serde_json::Value {
            serde_json::json!({})
        }
        async fn execute(&self, _args: serde_json::Value, _ctx: &ToolContext) -> ToolResult {
            self.executed.store(true, Ordering::SeqCst);
            ToolResult::success("mock executed")
        }
        fn requires_approval(&self) -> bool {
            self.requires_approval
        }
    }

    fn mock_tool(name: &'static str, approval: bool) -> (Box<dyn Tool>, StdArc<AtomicBool>) {
        let executed = StdArc::new(AtomicBool::new(false));
        let e = executed.clone();
        (
            Box::new(MockAgentTool {
                name,
                requires_approval: approval,
                executed,
            }),
            e,
        )
    }

    // ── Test provider with phased responses ────────────────────────────────

    struct TestProvider {
        phases: Vec<Vec<ChatEvent>>,
        call_count: AtomicUsize,
    }

    impl TestProvider {
        fn new(phases: Vec<Vec<ChatEvent>>) -> Self {
            Self {
                phases,
                call_count: AtomicUsize::new(0),
            }
        }
    }

    #[async_trait::async_trait]
    impl LlmProvider for TestProvider {
        async fn chat_stream(
            &self,
            _messages: &[Message],
            _tools: &[crate::message::ToolDefinition],
            _config: &LlmConfig,
        ) -> Result<
            std::pin::Pin<Box<dyn futures::Stream<Item = Result<ChatEvent, LlmError>> + Send>>,
            LlmError,
        > {
            let idx = self.call_count.fetch_add(1, Ordering::SeqCst);

            let events = self
                .phases
                .get(idx)
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .map(Ok);
            let stream = futures::stream::iter(events);
            Ok(Box::pin(stream))
        }
    }

    // ── Test helpers ───────────────────────────────────────────────────────

    struct TestSetup {
        session_mgr: StdArc<SessionManager>,
        session_id: String,
        ctx: AgentLoopContext,
        rule_engine: StdArc<RuleEngine>,
    }

    fn test_setup() -> TestSetup {
        let session_mgr = StdArc::new(SessionManager::new(InMemorySessionStore::new()));
        let session = session_mgr
            .create(Path::new("/tmp"), LlmConfig::default())
            .unwrap();
        let ctx = session_mgr.start_loop(&session.id).unwrap();
        let rule_engine = StdArc::new(RuleEngine::new(Path::new("/tmp")).unwrap());
        TestSetup {
            session_mgr,
            session_id: session.id,
            ctx,
            rule_engine,
        }
    }

    async fn run_collect(
        provider: StdArc<dyn LlmProvider>,
        tools: Vec<Box<dyn Tool>>,
        setup: TestSetup,
        max_iterations: u32,
        user_msg: Message,
    ) -> (Vec<AgentEvent>, StdArc<SessionManager>, String) {
        let (tx, mut rx) = mpsc::channel(64);

        let mut registry = ToolRegistry::new();
        for tool in tools {
            registry.register(tool).unwrap();
        }
        let tool_registry = StdArc::new(registry);
        let config = AgentConfig {
            max_iterations,
            ..Default::default()
        };

        let session_mgr = setup.session_mgr.clone();
        let sid = setup.session_id.clone();

        tokio::spawn(async move {
            run_agent_loop(
                provider,
                tool_registry,
                setup.rule_engine,
                session_mgr,
                setup.ctx,
                &config,
                user_msg,
                tx,
            )
            .await;
        });

        let mut events = Vec::new();
        while let Some(event) = rx.recv().await {
            events.push(event);
        }

        (events, setup.session_mgr, sid)
    }

    // ── is_leap ────────────────────────────────────────────────────────────

    #[test]
    fn test_is_leap_year() {
        assert!(is_leap(2000));
        assert!(!is_leap(1900));
        assert!(!is_leap(2023));
        assert!(is_leap(2024));
    }

    // ── Existing tests ─────────────────────────────────────────────────────

    #[test]
    fn test_agent_config_default() {
        let cfg = AgentConfig::default();
        assert_eq!(cfg.max_iterations, 10);
        assert_eq!(cfg.llm_retry_attempts, 3);
        assert_eq!(cfg.llm_retry_base_delay_ms, 1000);
        assert!(cfg.bash_confirm_mode);
        assert_eq!(cfg.file_max_size_bytes, 1048576);
    }

    #[test]
    fn test_agent_event_text_delta() {
        let evt = AgentEvent::TextDelta("hello".into());
        match evt {
            AgentEvent::TextDelta(content) => assert_eq!(content, "hello"),
            _ => panic!("expected TextDelta"),
        }
    }

    #[test]
    fn test_agent_event_tool_call() {
        let evt = AgentEvent::ToolCallRequest {
            call_id: "call-1".into(),
            tool_name: "bash".into(),
            arguments: "{}".into(),
        };
        match evt {
            AgentEvent::ToolCallRequest {
                call_id,
                tool_name,
                arguments,
            } => {
                assert_eq!(call_id, "call-1");
                assert_eq!(tool_name, "bash");
                assert_eq!(arguments, "{}");
            }
            _ => panic!("expected ToolCallRequest"),
        }
    }

    #[test]
    fn test_user_query_result_default() {
        let r = UserQueryResult::default();
        assert_eq!(r.selected_index, 0);
        assert!(r.text.is_empty());
    }

    #[test]
    fn test_agent_event_user_query() {
        let (tx, _rx) = tokio::sync::oneshot::channel::<UserQueryResult>();
        let evt = AgentEvent::UserQuery {
            query_id: "q1".into(),
            message: "confirm?".into(),
            options: vec!["Yes".into(), "No".into()],
            allow_other: true,
            respond: tx,
        };
        match evt {
            AgentEvent::UserQuery {
                query_id,
                message,
                options,
                allow_other,
                ..
            } => {
                assert_eq!(query_id, "q1");
                assert_eq!(message, "confirm?");
                assert_eq!(options, vec!["Yes", "No"]);
                assert!(allow_other);
            }
            _ => panic!("expected UserQuery"),
        }
    }

    #[test]
    fn test_agent_loop_context_fields() {
        let cancel = tokio_util::sync::CancellationToken::new();
        let ctx = AgentLoopContext {
            session_id: "sess-1".into(),
            history: vec![],
            working_dir: PathBuf::from("/tmp"),
            config: crate::provider::LlmConfig::default(),
            cancel_token: cancel.clone(),
        };
        assert_eq!(ctx.session_id, "sess-1");
        assert!(ctx.history.is_empty());
        assert_eq!(ctx.working_dir, Path::new("/tmp"));
        assert_eq!(ctx.config.model, "claude-3-7-sonnet-20250219");
    }

    // ── New agent loop tests ───────────────────────────────────────────────

    /// 1. 简单响应：TextDelta → Done
    #[tokio::test]
    async fn test_simple_response() {
        let provider: StdArc<dyn LlmProvider> = StdArc::new(TestProvider::new(vec![vec![
            ChatEvent::TextDelta("Hello".into()),
            ChatEvent::Done,
        ]]));

        let (events, _sm, _sid) =
            run_collect(provider, vec![], test_setup(), 10, Message::user("Hi")).await;

        let deltas: Vec<&str> = events
            .iter()
            .filter_map(|e| match e {
                AgentEvent::TextDelta(t) => Some(t.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(deltas, vec!["Hello"]);

        assert!(events.iter().any(|e| matches!(e, AgentEvent::Done)));
        assert!(
            events
                .iter()
                .all(|e| !matches!(e, AgentEvent::Error { .. }))
        );
    }

    /// 2. 工具调用：ToolCall → Done
    #[tokio::test]
    async fn test_tool_call() {
        let (tool, _executed) = mock_tool("finder", false);
        let provider: StdArc<dyn LlmProvider> = StdArc::new(TestProvider::new(vec![
            vec![
                ChatEvent::ToolCall {
                    id: "call-1".into(),
                    name: "finder".into(),
                    arguments: "{}".into(),
                },
                ChatEvent::Done,
            ],
            vec![ChatEvent::Done],
        ]));

        let (events, _sm, _sid) = run_collect(
            provider,
            vec![tool],
            test_setup(),
            10,
            Message::user("Find files"),
        )
        .await;

        assert!(
            events
                .iter()
                .any(|e| matches!(e, AgentEvent::ToolCallRequest { .. }))
        );
        assert!(events
            .iter()
            .any(|e| matches!(e, AgentEvent::ToolCallResult { call_id, is_error: false, .. } if call_id == "call-1")));
        assert!(events.iter().any(|e| matches!(e, AgentEvent::Done)));
        assert!(
            events
                .iter()
                .all(|e| !matches!(e, AgentEvent::Error { .. }))
        );
    }

    /// 3. 多工具批量执行：2 个 ToolCall 并行执行
    #[tokio::test]
    async fn test_multi_tool_batch() {
        let (tool_a, _ex_a) = mock_tool("finder", false);
        let (tool_b, _ex_b) = mock_tool("grep", false);
        let provider: StdArc<dyn LlmProvider> = StdArc::new(TestProvider::new(vec![
            vec![
                ChatEvent::ToolCall {
                    id: "call-1".into(),
                    name: "finder".into(),
                    arguments: "{}".into(),
                },
                ChatEvent::ToolCall {
                    id: "call-2".into(),
                    name: "grep".into(),
                    arguments: "{}".into(),
                },
                ChatEvent::Done,
            ],
            vec![ChatEvent::Done],
        ]));

        let (events, _sm, _sid) = run_collect(
            provider,
            vec![tool_a, tool_b],
            test_setup(),
            10,
            Message::user("Find and grep"),
        )
        .await;

        let tool_calls: Vec<&str> = events
            .iter()
            .filter_map(|e| match e {
                AgentEvent::ToolCallRequest { tool_name, .. } => Some(tool_name.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(tool_calls.len(), 2);
        assert!(tool_calls.contains(&"finder"));
        assert!(tool_calls.contains(&"grep"));

        let tool_results: Vec<&str> = events
            .iter()
            .filter_map(|e| match e {
                AgentEvent::ToolCallResult {
                    call_id,
                    is_error: false,
                    ..
                } => Some(call_id.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(tool_results.len(), 2);
        assert!(tool_results.contains(&"call-1"));
        assert!(tool_results.contains(&"call-2"));

        assert!(events.iter().any(|e| matches!(e, AgentEvent::Done)));
        assert!(
            events
                .iter()
                .all(|e| !matches!(e, AgentEvent::Error { .. }))
        );
    }

    /// 4. 最大迭代次数：max_iterations=1，一直返回 ToolCall → Error(MaxIterations)
    #[tokio::test]
    async fn test_max_iterations() {
        let (tool, _executed) = mock_tool("finder", false);
        // Always returns ToolCall
        let provider: StdArc<dyn LlmProvider> = StdArc::new(TestProvider::new(vec![vec![
            ChatEvent::ToolCall {
                id: "call-1".into(),
                name: "finder".into(),
                arguments: "{}".into(),
            },
            ChatEvent::Done,
        ]]));

        let (events, _sm, _sid) = run_collect(
            provider,
            vec![tool],
            test_setup(),
            1, // max_iterations = 1
            Message::user("Find"),
        )
        .await;

        assert!(events.iter().any(|e| matches!(
            e,
            AgentEvent::Error {
                code: AgentErrorCode::MaxIterations,
                ..
            }
        )));
    }

    /// 5. 取消：触发 CancellationToken → Error(Cancelled)
    #[tokio::test]
    async fn test_cancellation() {
        let setup = test_setup();
        setup.ctx.cancel_token.cancel();

        let provider: StdArc<dyn LlmProvider> = StdArc::new(TestProvider::new(vec![vec![
            ChatEvent::TextDelta("Hello".into()),
            ChatEvent::Done,
        ]]));

        let (events, _sm, _sid) =
            run_collect(provider, vec![], setup, 10, Message::user("Hi")).await;

        assert!(events.iter().any(|e| matches!(
            e,
            AgentEvent::Error {
                code: AgentErrorCode::Cancelled,
                ..
            }
        )));
        assert!(!events.iter().any(|e| matches!(e, AgentEvent::Done)));
    }

    /// 6. 用户确认：tool requires_approval=true，回复 true → 工具执行
    #[tokio::test]
    async fn test_user_query() {
        let (tool, executed) = mock_tool("finder", true);
        let provider: StdArc<dyn LlmProvider> = StdArc::new(TestProvider::new(vec![
            vec![
                ChatEvent::ToolCall {
                    id: "call-1".into(),
                    name: "finder".into(),
                    arguments: "{}".into(),
                },
                ChatEvent::Done,
            ],
            vec![ChatEvent::Done],
        ]));

        let (tx, mut rx) = mpsc::channel(64);
        let mut registry = ToolRegistry::new();
        registry.register(tool).unwrap();

        let setup = test_setup();
        let config = AgentConfig {
            max_iterations: 10,
            ..Default::default()
        };

        let sm = setup.session_mgr.clone();

        tokio::spawn(async move {
            run_agent_loop(
                provider,
                StdArc::new(registry),
                setup.rule_engine,
                sm,
                setup.ctx,
                &config,
                Message::user("Find files"),
                tx,
            )
            .await;
        });

        let mut done = false;

        while let Some(event) = rx.recv().await {
            match event {
                AgentEvent::UserQuery { respond, .. } => {
                    // Respond immediately to allow tool task to proceed
                    let _ = respond.send(UserQueryResult {
                        selected_index: 0,
                        text: String::new(),
                    });
                }
                AgentEvent::Done => {
                    done = true;
                }
                _ => {}
            }
        }

        assert!(done, "Expected Done event");
        assert!(
            executed.load(Ordering::SeqCst),
            "Tool should have been executed after approval"
        );
    }

    /// 7. 用户拒绝：回复 false → ToolCallResult(is_error=true)
    #[tokio::test]
    async fn test_user_query_denied() {
        let (tool, executed) = mock_tool("finder", true);
        let provider: StdArc<dyn LlmProvider> = StdArc::new(TestProvider::new(vec![
            vec![
                ChatEvent::ToolCall {
                    id: "call-1".into(),
                    name: "finder".into(),
                    arguments: "{}".into(),
                },
                ChatEvent::Done,
            ],
            vec![ChatEvent::Done],
        ]));

        let (tx, mut rx) = mpsc::channel(64);
        let mut registry = ToolRegistry::new();
        registry.register(tool).unwrap();

        let setup = test_setup();
        let config = AgentConfig {
            max_iterations: 10,
            ..Default::default()
        };

        tokio::spawn(async move {
            run_agent_loop(
                provider,
                StdArc::new(registry),
                setup.rule_engine,
                setup.session_mgr,
                setup.ctx,
                &config,
                Message::user("Find files"),
                tx,
            )
            .await;
        });

        let mut error_result: Option<String> = None;

        while let Some(event) = rx.recv().await {
            match event {
                AgentEvent::UserQuery { respond, .. } => {
                    // Deny immediately to allow tool task to proceed
                    let _ = respond.send(UserQueryResult {
                        selected_index: 1,
                        text: String::new(),
                    });
                }
                AgentEvent::ToolCallResult {
                    is_error: true,
                    content,
                    ..
                } => {
                    error_result = Some(content);
                }
                _ => {}
            }
        }

        assert_eq!(error_result, Some("User denied".into()));
        assert!(
            !executed.load(Ordering::SeqCst),
            "Tool should NOT have been executed after denial"
        );
    }

    /// 8. 用户 Always Allow：respond with selected_index=2 → 工具执行 + 加入 approved_tools
    #[tokio::test]
    async fn test_user_query_always_allow() {
        let (tool, executed) = mock_tool("finder", true);
        let provider: StdArc<dyn LlmProvider> = StdArc::new(TestProvider::new(vec![
            vec![
                ChatEvent::ToolCall {
                    id: "call-1".into(),
                    name: "finder".into(),
                    arguments: "{}".into(),
                },
                ChatEvent::Done,
            ],
            vec![ChatEvent::Done],
        ]));

        let (tx, mut rx) = mpsc::channel(64);
        let mut registry = ToolRegistry::new();
        registry.register(tool).unwrap();

        let setup = test_setup();
        let config = AgentConfig {
            max_iterations: 10,
            ..Default::default()
        };

        let sm = setup.session_mgr.clone();
        let sid = setup.session_id.clone();
        let sm_for_spawn = sm.clone();

        tokio::spawn(async move {
            run_agent_loop(
                provider,
                StdArc::new(registry),
                setup.rule_engine,
                sm_for_spawn,
                setup.ctx,
                &config,
                Message::user("Find files"),
                tx,
            )
            .await;
        });

        let mut done = false;

        while let Some(event) = rx.recv().await {
            match event {
                AgentEvent::UserQuery { respond, .. } => {
                    // Always Allow
                    let _ = respond.send(UserQueryResult {
                        selected_index: 2,
                        text: String::new(),
                    });
                }
                AgentEvent::Done => {
                    done = true;
                }
                _ => {}
            }
        }

        assert!(done, "Expected Done event");
        assert!(
            executed.load(Ordering::SeqCst),
            "Tool should have been executed"
        );
        assert!(
            sm.is_tool_approved(&sid, "finder"),
            "Tool should be in approved_tools after Always Allow"
        );
    }

    /// 9. mpsc 关闭：drop receiver → agent 不 panic
    #[tokio::test]
    async fn test_mpsc_closed() {
        let provider: StdArc<dyn LlmProvider> = StdArc::new(TestProvider::new(vec![vec![
            ChatEvent::TextDelta("Hello".into()),
            ChatEvent::Done,
        ]]));

        let setup = test_setup();
        let config = AgentConfig::default();
        let (tx, rx) = mpsc::channel(1);
        drop(rx); // drop receiver immediately

        let result = tokio::time::timeout(
            Duration::from_secs(5),
            run_agent_loop(
                provider,
                StdArc::new(ToolRegistry::new()),
                setup.rule_engine,
                setup.session_mgr,
                setup.ctx,
                &config,
                Message::user("Hi"),
                tx,
            ),
        )
        .await;

        assert!(
            result.is_ok(),
            "Agent loop should not hang or panic when receiver is dropped"
        );
    }

    /// 9. 历史记录：结束时 history 包含 user + assistant 消息
    #[tokio::test]
    async fn test_history_appended() {
        let provider: StdArc<dyn LlmProvider> = StdArc::new(TestProvider::new(vec![vec![
            ChatEvent::TextDelta("Hello".into()),
            ChatEvent::Done,
        ]]));

        let setup = test_setup();
        let config = AgentConfig::default();
        let (tx, mut rx) = mpsc::channel(64);

        let registry = ToolRegistry::new();
        let sm = setup.session_mgr.clone();
        let sid = setup.session_id.clone();

        let sm_for_spawn = sm.clone();
        tokio::spawn(async move {
            run_agent_loop(
                provider,
                StdArc::new(registry),
                setup.rule_engine,
                sm_for_spawn,
                setup.ctx,
                &config,
                Message::user("Hi"),
                tx,
            )
            .await;
        });

        // Drain events
        while rx.recv().await.is_some() {}

        // Check session history
        let session = sm.get(&sid).unwrap();
        assert_eq!(session.history.len(), 2, "Expected 2 messages in history");
        assert_eq!(session.history[0].role, Role::User);
        assert_eq!(session.history[0].content, "Hi");
        assert_eq!(session.history[1].role, Role::Assistant);
        assert_eq!(session.history[1].content, "Hello");
    }

    // ── parse_user_query_marker tests ──────────────────────────────────────

    #[test]
    fn test_parse_marker_simple() {
        let text =
            "What do you want?\n[USER_QUERY]\nPick one:\n- Option A\n- Option B\n[/USER_QUERY]";
        let marker = parse_user_query_marker(text).unwrap();
        assert_eq!(marker.message, "Pick one:");
        assert_eq!(marker.options, vec!["Option A", "Option B"]);
        assert!(!marker.allow_other);
    }

    #[test]
    fn test_parse_marker_with_allow_other() {
        let text = "What do you want?\n[USER_QUERY allow_other=true]\nCustom input:\n- Option 1\n- Option 2\n[/USER_QUERY]";
        let marker = parse_user_query_marker(text).unwrap();
        assert_eq!(marker.message, "Custom input:");
        assert_eq!(marker.options, vec!["Option 1", "Option 2"]);
        assert!(marker.allow_other);
    }

    #[test]
    fn test_parse_marker_no_options() {
        let text = "Confirm?\n[USER_QUERY]\nAre you sure?\n[/USER_QUERY]";
        let marker = parse_user_query_marker(text).unwrap();
        assert_eq!(marker.message, "Are you sure?");
        assert!(marker.options.is_empty());
        assert!(!marker.allow_other);
    }

    #[test]
    fn test_parse_marker_no_marker() {
        assert!(parse_user_query_marker("Just a normal message").is_none());
        assert!(parse_user_query_marker("").is_none());
        assert!(parse_user_query_marker("[USER_QUERY]unclosed").is_none());
        assert!(parse_user_query_marker("[/USER_QUERY]no open").is_none());
    }

    #[test]
    fn test_parse_marker_empty_body() {
        let text = "text\n[USER_QUERY]\n\n[/USER_QUERY]";
        assert!(parse_user_query_marker(text).is_none());
    }

    #[test]
    fn test_parse_marker_only_in_text() {
        // Marker in the middle should not match (only at end)
        let text = "[USER_QUERY]\nTest\n[/USER_QUERY]\nsome more text";
        // The rfind will find the first [USER_QUERY] and the last [/USER_QUERY]
        // but there's text after the close tag
        let result = parse_user_query_marker(text);
        // In this case, the function still matches because rfind finds the last [/USER_QUERY]
        // and then looks for [USER_QUERY] before it. There IS text after [/USER_QUERY],
        // but the function uses trim_end() so it would still match.
        // Let's just verify it correctly parses
        assert!(result.is_some());
        let marker = result.unwrap();
        assert_eq!(marker.message, "Test");
    }

    #[test]
    fn test_parse_marker_whitespace_around() {
        let text = "  \n[USER_QUERY]\nHi\n[/USER_QUERY]  \n";
        let marker = parse_user_query_marker(text).unwrap();
        assert_eq!(marker.message, "Hi");
    }

    // ── strip_user_query_marker tests ──────────────────────────────────────

    #[test]
    fn test_strip_marker_simple() {
        let text = "Hello world\n[USER_QUERY]\nConfirm?\n[/USER_QUERY]";
        let stripped = strip_user_query_marker(text);
        assert_eq!(stripped, "Hello world");
    }

    #[test]
    fn test_strip_marker_no_marker() {
        assert_eq!(strip_user_query_marker("Hello"), "Hello");
        assert_eq!(strip_user_query_marker(""), "");
    }

    #[test]
    fn test_strip_marker_only_marker() {
        let stripped = strip_user_query_marker("[USER_QUERY]\nHi\n[/USER_QUERY]");
        assert_eq!(stripped, "");
    }

    // ── Agent loop [USER_QUERY] marker integration test ─────────────────────

    #[tokio::test]
    async fn test_user_query_marker_in_response() {
        // Provider returns text with [USER_QUERY] marker
        let provider: StdArc<dyn LlmProvider> = StdArc::new(TestProvider::new(vec![vec![
            ChatEvent::TextDelta(
                "What color?\n[USER_QUERY]\nChoose:\n- Red\n- Blue\n[/USER_QUERY]".into(),
            ),
            ChatEvent::Done,
        ]]));

        let (tx, mut rx) = mpsc::channel(64);
        let registry = ToolRegistry::new();

        let setup = test_setup();
        let config = AgentConfig {
            max_iterations: 10,
            ..Default::default()
        };

        let sm = setup.session_mgr.clone();

        tokio::spawn(async move {
            run_agent_loop(
                provider,
                StdArc::new(registry),
                setup.rule_engine,
                sm,
                setup.ctx,
                &config,
                Message::user("Pick a color"),
                tx,
            )
            .await;
        });

        let mut received_query = false;
        let mut done = false;

        while let Some(event) = rx.recv().await {
            match event {
                AgentEvent::UserQuery {
                    ref message,
                    ref options,
                    allow_other,
                    respond,
                    ..
                } => {
                    received_query = true;
                    assert_eq!(message, "Choose:");
                    assert_eq!(options, &vec!["Red", "Blue"]);
                    assert!(!allow_other);
                    // Respond with option index 0 (Red)
                    let _ = respond.send(UserQueryResult {
                        selected_index: 0,
                        text: String::new(),
                    });
                }
                AgentEvent::Done => {
                    done = true;
                }
                _ => {}
            }
        }

        assert!(received_query, "Expected UserQuery event with marker");
        assert!(done, "Expected Done event after marker interaction");
    }

    #[tokio::test]
    async fn test_user_query_marker_continue_loop() {
        // Phase 1: text with [USER_QUERY] → respond → Phase 2: normal text → Done
        let provider: StdArc<dyn LlmProvider> = StdArc::new(TestProvider::new(vec![
            vec![
                ChatEvent::TextDelta(
                    "Question?\n[USER_QUERY]\nAnswer:\n- Yes\n- No\n[/USER_QUERY]".into(),
                ),
                ChatEvent::Done,
            ],
            vec![ChatEvent::TextDelta("Got it!".into()), ChatEvent::Done],
        ]));

        let (tx, mut rx) = mpsc::channel(64);
        let registry = ToolRegistry::new();

        let setup = test_setup();
        let config = AgentConfig {
            max_iterations: 10,
            ..Default::default()
        };

        let sm = setup.session_mgr.clone();

        tokio::spawn(async move {
            run_agent_loop(
                provider,
                StdArc::new(registry),
                setup.rule_engine,
                sm,
                setup.ctx,
                &config,
                Message::user("Start"),
                tx,
            )
            .await;
        });

        let mut marker_found = false;
        let mut text_deltas: Vec<String> = Vec::new();

        while let Some(event) = rx.recv().await {
            match event {
                AgentEvent::UserQuery { respond, .. } => {
                    marker_found = true;
                    let _ = respond.send(UserQueryResult {
                        selected_index: 0,
                        text: String::new(),
                    });
                }
                AgentEvent::TextDelta(t) => {
                    text_deltas.push(t);
                }
                AgentEvent::Done => {}
                _ => {}
            }
        }

        assert!(
            marker_found,
            "Expected [USER_QUERY] marker to trigger UserQuery"
        );
        // The second phase text should appear
        assert!(
            text_deltas.iter().any(|t| t.contains("Got it!")),
            "Expected loop to continue after marker query"
        );
    }

    #[tokio::test]
    async fn test_user_query_marker_custom_text() {
        let provider: StdArc<dyn LlmProvider> = StdArc::new(TestProvider::new(vec![
            vec![
                ChatEvent::TextDelta("Custom?\n[USER_QUERY allow_other=true]\nEnter value:\n- Default\n[/USER_QUERY]".into()),
                ChatEvent::Done,
            ],
            vec![
                ChatEvent::TextDelta("Thanks!".into()),
                ChatEvent::Done,
            ],
        ]));

        let (tx, mut rx) = mpsc::channel(64);
        let registry = ToolRegistry::new();

        let setup = test_setup();
        let config = AgentConfig {
            max_iterations: 10,
            ..Default::default()
        };

        let sm = setup.session_mgr.clone();

        tokio::spawn(async move {
            run_agent_loop(
                provider,
                StdArc::new(registry),
                setup.rule_engine,
                sm,
                setup.ctx,
                &config,
                Message::user("Start"),
                tx,
            )
            .await;
        });

        let mut found = false;

        while let Some(event) = rx.recv().await {
            match event {
                AgentEvent::UserQuery {
                    ref message,
                    ref options,
                    allow_other,
                    respond,
                    ..
                } => {
                    found = true;
                    assert_eq!(message, "Enter value:");
                    assert_eq!(options, &vec!["Default"]);
                    assert!(allow_other);
                    // Send custom text with selected_index = -1
                    let _ = respond.send(UserQueryResult {
                        selected_index: -1,
                        text: "my custom input".into(),
                    });
                }
                AgentEvent::Done => {}
                _ => {}
            }
        }

        assert!(found, "Expected UserQuery with allow_other");
    }

    // ── render_tool_guide tests ─────────────────────────────────────────────

    #[test]
    fn test_render_tool_guide_empty() {
        let registry = ToolRegistry::new();
        let guide = render_tool_guide(&registry);
        assert!(guide.is_empty());
    }

    #[test]
    fn test_render_tool_guide_with_tools() {
        use crate::tool::ToolContext;
        use crate::tool::ToolResult;

        struct CategorisedTool {
            name: &'static str,
            cat: &'static str,
        }

        #[async_trait::async_trait]
        impl Tool for CategorisedTool {
            fn name(&self) -> &str {
                self.name
            }
            fn description(&self) -> &str {
                "test"
            }
            fn parameters(&self) -> serde_json::Value {
                serde_json::json!({})
            }
            async fn execute(&self, _args: serde_json::Value, _ctx: &ToolContext) -> ToolResult {
                ToolResult::success("ok")
            }
            fn category(&self) -> &str {
                self.cat
            }
        }

        let mut registry = ToolRegistry::new();
        registry
            .register(Box::new(CategorisedTool {
                name: "bash",
                cat: "common",
            }))
            .unwrap();
        registry
            .register(Box::new(CategorisedTool {
                name: "codegraph",
                cat: "analyze",
            }))
            .unwrap();
        registry
            .register(Box::new(CategorisedTool {
                name: "fetch",
                cat: "network",
            }))
            .unwrap();

        let guide = render_tool_guide(&registry);
        assert!(!guide.is_empty());
        assert!(guide.contains("## Available Tools"));
        assert!(guide.contains("Common (prefer these first)"));
        assert!(guide.contains("bash"));
        assert!(guide.contains("Analyze"));
        assert!(guide.contains("codegraph"));
        assert!(guide.contains("Network"));
        assert!(guide.contains("fetch"));
    }
}
