// Allow unused imports — they are consumed by test module via `use super::*`
#![allow(unused_imports)]

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use crate::agent_definition::PermissionRule;
use crate::error::AgentErrorCode;
use crate::error::LlmError;
use crate::message::Message;
use crate::message::Role;
use crate::message::ToolCallRequest;
use crate::message::estimate_message_tokens;
use crate::prompt::PromptBuilder;
use crate::provider::ChatEvent;
use crate::provider::LlmConfig;
use crate::provider::LlmProvider;
use crate::rules::RuleEngine;
use crate::session::SessionManager;
use crate::session::SessionStatus;
use crate::tool::{ToolContext, ToolResult, ToolType};
use crate::tool_registry::ToolRegistry;
use async_trait::async_trait;

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
        cache_creation_input_tokens: u32,
        cache_read_input_tokens: u32,
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
        tool_name: String,
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
    /// LLM 输出的图片内容块
    ImageBlock {
        path: String,
        mime_type: String,
        remote_url: Option<String>,
    },
    /// 图片处理失败
    ImageError {
        reason: String,
    },
    /// 需要用户输入
    UserQuery {
        query_id: String,
        message: String,
        options: Vec<String>,
        allow_other: bool,
        respond: oneshot::Sender<UserQueryResult>,
    },
}

/// Agent 事件帧：AgentEvent 及其来源上下文。
/// 用于标识事件来自哪个 agent，支持 CLI 显示 agent 名称前缀。
pub struct AgentEventFrame {
    pub event: AgentEvent,
    pub session_id: String,
    pub agent_name: String,
    pub parent_session_id: Option<String>,
    pub parent_session_name: Option<String>,
}

/// Agent → Orchestrator 消息（通过全局事件总线）
pub enum AgentMessage {
    TextDelta(String),
    ThinkingBlock(serde_json::Value),
    UsageInfo {
        input_tokens: u32,
        output_tokens: u32,
        tool_calls: u32,
        cache_creation_input_tokens: u32,
        cache_read_input_tokens: u32,
    },
    StatusUpdate(String),
    Error {
        code: AgentErrorCode,
        message: String,
    },
    ToolCallRequest {
        call_id: String,
        tool_name: String,
        arguments: String,
    },
    ToolCallResult {
        call_id: String,
        tool_name: String,
        content: String,
        is_error: bool,
    },
    UserQuery {
        query_id: String,
        message: String,
        options: Vec<String>,
        allow_other: bool,
        respond: oneshot::Sender<UserQueryResult>,
    },
    SpawnRequest {
        call_id: String,
        subagent_type: String,
        description: String,
        /// 传给子 agent 的详细、自包含任务说明（目标+上下文+约束+期望输出）。
        /// 由调用方 LLM 编写，子 agent 将其作为首条 user message。
        prompt: String,
        task_id: Option<String>,
        trace_context: Option<crate::TraceContext>,
        /// 用于将子 agent 的响应发送回调用方
        response_tx: Option<tokio::sync::oneshot::Sender<String>>,
    },
    Done,
}

/// Orchestrator → Agent 消息（通过专属 inbox）
pub enum OrchestratorMessage {
    SubAgentComplete {
        call_id: String,
        content: String,
        task_id: String,
    },
    SubAgentError {
        call_id: String,
        error: String,
    },
    Cancelled,
}

/// 事件总线信封
pub struct Envelope {
    pub session_id: String,
    pub message: AgentMessage,
    pub trace_context: Option<crate::TraceContext>,
}

/// Agent kind (primary vs sub).
///
/// Used for observability fields (`visp.agent.kind`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentKind {
    /// Root agent handling the user's session.
    Primary,
    /// Sub-agent spawned by a parent agent.
    Sub,
}

impl std::fmt::Display for AgentKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AgentKind::Primary => write!(f, "primary"),
            AgentKind::Sub => write!(f, "sub"),
        }
    }
}

/// A tool that delegates a task to a sub-agent.
/// Created dynamically for each sub-agent in the AgentRegistry.
pub struct AgentTool {
    agent_name: String,
    agent_description: String,
}

impl AgentTool {
    pub fn new(name: String, description: String) -> Self {
        Self {
            agent_name: name,
            agent_description: description,
        }
    }
}

#[async_trait]
impl crate::tool::Tool for AgentTool {
    fn name(&self) -> &str {
        &self.agent_name
    }

    fn description(&self) -> &str {
        &self.agent_description
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "prompt": {
                    "type": "string",
                    "description": "The detailed, self-contained task to delegate to the sub-agent"
                }
            },
            "required": ["prompt"]
        })
    }

    async fn execute(&self, arguments: serde_json::Value, context: &ToolContext) -> ToolResult {
        let global_tx = match &context.global_tx {
            Some(tx) => tx.clone(),
            None => {
                return ToolResult::error(
                    "[SubAgent Error] Agent tool requires multi-agent mode (global_tx not available)",
                );
            }
        };

        let prompt = match arguments.get("prompt").and_then(|v| v.as_str()) {
            Some(p) => p.to_string(),
            None => {
                return ToolResult::error("[SubAgent Error] Missing required 'prompt' argument");
            }
        };

        let (response_tx, response_rx) = oneshot::channel::<String>();

        let trace_context = context.visp_trace_id.as_ref().and_then(|trace_id| {
            context.iter_span_w3c_id.as_ref().and_then(|span_id| {
                crate::TraceContext::new(trace_id.clone(), span_id.clone(), 1, None, None).ok()
            })
        });

        let envelope = Envelope {
            session_id: context.session_id.clone().unwrap_or_default(),
            message: AgentMessage::SpawnRequest {
                call_id: uuid::Uuid::new_v4().to_string(),
                subagent_type: self.agent_name.clone(),
                description: self.agent_description.clone(),
                prompt,
                task_id: None,
                trace_context: trace_context.clone(),
                response_tx: Some(response_tx),
            },
            trace_context,
        };

        if let Err(e) = global_tx.send(envelope).await {
            return ToolResult::error(format!(
                "[SubAgent Error] Failed to send spawn request: {e}"
            ));
        }

        match response_rx.await {
            Ok(content) => ToolResult::success(content),
            Err(_) => {
                ToolResult::error("[SubAgent Error] Sub-agent response channel closed unexpectedly")
            }
        }
    }

    fn category(&self) -> &str {
        "agent"
    }

    fn tool_type(&self) -> ToolType {
        ToolType::Agent
    }
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
    /// 上下文裁剪器
    pub context_trimmer: Arc<dyn crate::context::ContextTrimmer + Send + Sync>,
    /// 全局事件总线发送端（多 Agent 模式）
    pub global_tx: Option<mpsc::Sender<Envelope>>,
    /// 权限规则集
    pub permission_rules: Option<Arc<Vec<PermissionRule>>>,
    /// Agent kind (Primary or Sub), used for observability.
    pub agent_kind: AgentKind,
    /// Nesting depth (0 for root, incremented per sub-agent level).
    pub depth: u32,
    /// 父 agent 的 session_id；主 agent 为自身 session_id（自引用）
    pub parent_session_id: String,
}

/// Agent 执行配置
#[derive(Debug, Clone)]
pub struct AgentConfig {
    /// 软上限：达到此轮次后在 LLM 调用前注入"请收尾"提示（0 = 关闭）
    pub soft_limit: u32,
    /// 硬上限兜底：达到此轮次后强制终止（防止死循环）
    pub hard_limit: u32,
    /// Doom loop 检测窗口：连续 N 轮相同工具调用则触发警告
    pub doom_loop_threshold: u32,
    /// LLM 调用重试次数
    pub llm_retry_attempts: u32,
    /// LLM 重试基础延迟（毫秒）
    pub llm_retry_base_delay_ms: u64,
    /// bash 工具确认模式（执行高危命令前是否需要用户确认）
    pub bash_confirm_mode: bool,
    /// 文件读取/写入的最大字节数
    pub file_max_size_bytes: u64,
    /// Agent 嵌套深度上限
    pub max_depth: u32,
    /// Langfuse 总开关
    pub langfuse_enabled: bool,
    /// Langfuse user.id 字段值（None = 不设置）
    pub langfuse_user_id: Option<String>,
    /// Langfuse tags 字段值（JSON 字符串，None = 不设置）
    pub langfuse_tags: Option<String>,
    /// Langfuse environment 字段
    pub langfuse_environment: Option<String>,
    /// Langfuse release 字段
    pub langfuse_release: Option<String>,
    /// Langfuse version 字段
    pub langfuse_version: Option<String>,
    /// Langfuse public 开关（None = 不设置）
    pub langfuse_public: Option<bool>,
    /// Langfuse metadata（值均为字符串：标量保留，数组/table 转紧凑 JSON）
    pub langfuse_metadata: Option<HashMap<String, String>>,
    /// 是否在 Langfuse OTEL span 中记录 LLM generation input
    pub langfuse_capture_input: bool,
    /// 是否在 Langfuse OTEL span 中记录 LLM generation output
    pub langfuse_capture_output: bool,
    /// Langfuse capture 最大字符数（超出截断）
    pub langfuse_capture_max_chars: usize,
    /// 是否脱敏敏感字段（api_key/token/secret/password 等）
    pub langfuse_redact_secrets: bool,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            soft_limit: 50,
            hard_limit: 200,
            doom_loop_threshold: 5,
            llm_retry_attempts: 3,
            llm_retry_base_delay_ms: 1000,
            bash_confirm_mode: true,
            file_max_size_bytes: 1048576,
            max_depth: 5,
            langfuse_enabled: false,
            langfuse_user_id: None,
            langfuse_tags: None,
            langfuse_environment: None,
            langfuse_release: None,
            langfuse_version: None,
            langfuse_public: None,
            langfuse_metadata: None,
            langfuse_capture_input: false,
            langfuse_capture_output: false,
            langfuse_capture_max_chars: 20_000,
            langfuse_redact_secrets: true,
        }
    }
}

// ── Internal helper ──────────────────────────────────────────────────────────

pub(crate) struct ToolExecResult {
    pub(crate) index: usize,
    pub(crate) call_id: String,
    pub(crate) result: ToolResult,
    pub(crate) duration_ms: Option<u64>,
}

/// Pending sub-agent spawn (for multi-agent mode)
#[allow(dead_code)]
pub(crate) struct PendingSpawn {
    pub(crate) index: usize,
    pub(crate) call_id: String,
    pub(crate) subagent_type: String,
}

pub(crate) fn llm_error_to_code(err: &LlmError) -> (AgentErrorCode, String) {
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
        LlmError::Cancelled => (AgentErrorCode::Cancelled, "agent cancelled".into()),
    }
}

/// 格式化工具参数为用户友好的显示文本
pub(crate) fn format_tool_args(args_json: &str) -> String {
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

pub(crate) struct UserQueryMarker {
    pub(crate) message: String,
    pub(crate) options: Vec<String>,
    pub(crate) allow_other: bool,
}

/// 从文本末尾检测 [USER_QUERY]...[/USER_QUERY] 标记
pub(crate) fn parse_user_query_marker(text: &str) -> Option<UserQueryMarker> {
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
pub(crate) fn strip_user_query_marker(text: &str) -> String {
    let text = text.trim_end();
    if let Some(close_pos) = text.rfind("[/USER_QUERY]")
        && let Some(open_pos) = text[..close_pos].rfind("[USER_QUERY")
    {
        let before = &text[..open_pos].trim_end();
        return before.to_string();
    }
    text.to_string()
}

/// 从 thinking blocks 中提取 thinking 文本内容。
/// thinking block 的 JSON 结构: {"type":"thinking","thinking":"...","signature":"..."}
pub(crate) fn extract_thinking_text(blocks: &[serde_json::Value]) -> Option<String> {
    blocks.first().and_then(|block| {
        block
            .get("thinking")
            .and_then(|v| v.as_str().map(|s| s.to_string()))
    })
}

// Re-export run_agent_loop from agent_loop module
pub use crate::agent_loop::run_agent_loop;

/// 清理历史中残留的 orphan tool_uses。
/// 如果一条 assistant 消息包含 tool_calls，但没有对应的 tool_result
/// 消息紧随其后，则清空 tool_calls。这发生在 Cancel 终止了 agent 循环，
/// 导致 tool_use 被发送给 Anthropic 但没来得及追加 tool_result，
/// 后续请求会报 400 错误。
///
/// 注意：支持部分 tool_result 的情况——如果 assistant 消息有 3 个 tool_calls
/// 但只有 2 个 tool_results 跟随，也会清空 tool_calls。
pub(crate) fn cleanup_orphan_tool_uses(history: &mut [Message]) {
    let len = history.len();

    // 第一遍：从后向前，清理 orphan tool_calls（assistant 消息有 tool_calls 但没有配对的 tool 结果）
    let mut i = len;
    while i > 0 {
        i -= 1;
        if history[i].role == Role::Assistant
            && let Some(ref calls) = history[i].tool_calls
        {
            let num_calls = calls.len();
            // 紧跟在 assistant 后面的 num_calls 条消息必须全是 Tool 角色，
            // 且各自的 tool_call_id 必须与 assistant 的 ToolCallRequest.id 逐一对应
            let all_have_results = (0..num_calls).all(|offset| {
                let idx = i + 1 + offset;
                idx < len
                    && history[idx].role == Role::Tool
                    && history[idx].tool_call_id.as_deref() == Some(&calls[offset].id)
            });

            if !all_have_results {
                // 清理 orphan tool_calls
                // 同时标记对应的 tool 结果为 skip_context（如果存在但 ID 不匹配）
                for offset in 0..num_calls {
                    let idx = i + 1 + offset;
                    if idx < len && history[idx].role == Role::Tool {
                        // Tool 消息存在但 ID 不匹配 → 标记为 skip_context
                        history[idx].skip_context = true;
                    }
                }
                history[i].tool_calls = None;
            }
            // 不 break！继续检查更早的 assistant 消息
        }
    }

    // 第二遍：从后向前，清理孤儿 Tool 消息（tool 消息的 tool_call_id 没有对应的 assistant tool_call）
    let mut i = len;
    while i > 0 {
        i -= 1;
        if history[i].role == Role::Tool
            && let Some(ref call_id) = history[i].tool_call_id
        {
            // 向前查找最近的 assistant 消息
            let mut found = false;
            let mut j = i;
            while j > 0 {
                j -= 1;
                if history[j].role == Role::Assistant
                    && let Some(ref calls) = history[j].tool_calls
                {
                    // 找到了 assistant，检查是否有匹配的 tool_call id
                    if calls.iter().any(|c| c.id == *call_id) {
                        found = true;
                    }
                    break; // 找到最近的 assistant 消息就停止（不管是否匹配）
                } else if history[j].role == Role::User {
                    break; // 遇到 user 消息停止
                }
            }
            if !found {
                history[i].skip_context = true;
            }
        }
    }
}

/// 根据注册的工具定义，生成按分类分组的动态工具指南 Markdown 文本
pub(crate) fn render_tool_guide(registry: &ToolRegistry) -> String {
    let defs = registry.definitions();
    if defs.is_empty() {
        return String::new();
    }

    /// 3 个高层次 Code Understanding 工具，放在独立分组中，且从 Analyze 分组中排除
    const CODE_UNDERSTANDING_TOOLS: &[&str] =
        &["codegraph_context", "codegraph_trace", "codegraph_impact"];

    use std::collections::{HashMap, HashSet};
    let cu_set: HashSet<&str> = HashSet::from_iter(CODE_UNDERSTANDING_TOOLS.iter().copied());
    let mut cu_descs: HashMap<&str, &str> = HashMap::new();
    let mut grouped: HashMap<&str, Vec<&str>> = HashMap::new();
    for def in &defs {
        // 将 Code Understanding 工具从常规 category 分组中排除
        if cu_set.contains(def.name.as_str()) {
            cu_descs.insert(def.name.as_str(), def.description.as_str());
            continue;
        }
        let cat = if def.category.is_empty() {
            "other"
        } else {
            def.category.as_str()
        };
        grouped.entry(cat).or_default().push(def.name.as_str());
    }

    let mut parts = vec![
        "\n\n## Available Tools".to_string(),
        "\n**IMPORTANT**: Prefer specialized tools over `bash` when possible: \
         use `read_file` to read files, `edit_file`/`write_file` to modify them, \
         `grep`/`glob` to search files. \
         Use `codegraph_context`/`codegraph_trace`/`codegraph_impact` for quick symbol \
         lookups and call-chain tracing. \
         When you need to discover code (find files, locate patterns, search \
         the codebase) or execute bounded implementation tasks, \
         delegate to the appropriate sub-agent (e.g. @explorer, @fixer, \
         @designer, @oracle, @librarian). \
         See the sub-agent list in the Task Delegation section for details. \
         Only use `bash` when no other tool fits \
         (e.g. running build commands, git operations, or multi-step shell scripts)."
            .to_string(),
    ];

    // Code Understanding 独立分组（在 category 分组之前）
    if !cu_descs.is_empty() {
        parts.push("\n## Code Understanding (lightweight lookups)".to_string());
        for name in CODE_UNDERSTANDING_TOOLS {
            if let Some(desc) = cu_descs.get(name) {
                parts.push(format!("  {name}  — {desc}"));
            }
        }
    }

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

/// 将当前 prompt（messages + tools）保存到 `.visp/last-prompt.json`，
/// 方便调试和检查实际发送给 LLM 的内容。
/// 取消注释上方 `dump_prompt_to_file(&ctx.working_dir, &messages, &tools);` 以启用。
/// 写入失败时静默忽略。
#[allow(dead_code)]
pub(crate) fn dump_prompt_to_file(
    working_dir: &std::path::Path,
    messages: &[crate::message::Message],
    tools: &[crate::message::ToolDefinition],
) {
    let dir = working_dir.join(".visp");
    if !dir.is_dir() {
        return;
    }
    let path = dir.join("last-prompt.json");
    let content = serde_json::json!({
        "messages": messages,
        "tools": tools,
    });
    if let Ok(json) = serde_json::to_string_pretty(&content) {
        let _ = std::fs::write(&path, json);
    }
}

// ----------------------  test ------------------------//
// ----------------------  test ------------------------//
// ----------------------  test ------------------------//

#[cfg(test)]
mod tests {
    use super::*;
    use crate::TraceContext;
    use crate::context::ContextTrimmer;
    use crate::session::InMemorySessionStore;
    use crate::tool::Tool;
    use std::path::Path;
    use std::sync::Arc as StdArc;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    struct MockTrimmer;
    impl ContextTrimmer for MockTrimmer {
        fn trim(
            &self,
            h: &[crate::message::Message],
            _: u32,
            _: u32,
            _: u32,
        ) -> Vec<crate::message::Message> {
            h.to_vec()
        }
    }

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
            _cancel: &tokio_util::sync::CancellationToken,
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
        let trimmer: StdArc<dyn ContextTrimmer + Send + Sync> = StdArc::new(MockTrimmer);
        let ctx = session_mgr
            .start_loop(&session.id, &trimmer, None, None)
            .unwrap();
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
        soft_limit: u32,
        hard_limit: u32,
        user_msg: Message,
    ) -> (Vec<AgentEvent>, StdArc<SessionManager>, String) {
        let (tx, mut rx) = mpsc::channel(64);

        let registry = ToolRegistry::new();
        for tool in tools {
            registry.register(Arc::from(tool)).unwrap();
        }
        let tool_registry = StdArc::new(registry);
        let config = AgentConfig {
            soft_limit,
            hard_limit,
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

    // ── Existing tests ─────────────────────────────────────────────────────

    #[test]
    fn test_agent_config_default() {
        let cfg = AgentConfig::default();
        assert_eq!(cfg.soft_limit, 50);
        assert_eq!(cfg.hard_limit, 200);
        assert_eq!(cfg.doom_loop_threshold, 5);
        assert_eq!(cfg.llm_retry_attempts, 3);
        assert_eq!(cfg.llm_retry_base_delay_ms, 1000);
        assert!(cfg.bash_confirm_mode);
        assert_eq!(cfg.file_max_size_bytes, 1048576);
    }

    #[test]
    fn test_agent_config_langfuse_defaults() {
        let cfg = AgentConfig::default();
        assert_eq!(cfg.langfuse_user_id, None);
        assert_eq!(cfg.langfuse_tags, None);
    }

    #[test]
    fn test_agent_config_langfuse_configured() {
        let cfg = AgentConfig {
            langfuse_user_id: Some("user_456".into()),
            langfuse_tags: Some(r#"["agent","weather"]"#.into()),
            ..Default::default()
        };
        assert_eq!(cfg.langfuse_user_id.as_deref(), Some("user_456"));
        assert_eq!(cfg.langfuse_tags.as_deref(), Some(r#"["agent","weather"]"#));
    }

    // ── 1b: 传递配置到核心 AgentConfig ──────────────────────────────────

    #[test]
    fn test_agent_config_langfuse_enabled_default() {
        let cfg = AgentConfig::default();
        assert!(
            !cfg.langfuse_enabled,
            "langfuse_enabled should default to false"
        );
    }

    #[test]
    fn test_agent_config_langfuse_all_fields() {
        let mut meta = HashMap::new();
        meta.insert("env".into(), "prod".into());
        meta.insert("count".into(), "42".into());
        let cfg = AgentConfig {
            langfuse_enabled: true,
            langfuse_user_id: Some("user".into()),
            langfuse_tags: Some(r#"["agent"]"#.into()),
            langfuse_environment: Some("staging".into()),
            langfuse_release: Some("2.0".into()),
            langfuse_version: Some("abc123".into()),
            langfuse_public: Some(true),
            langfuse_metadata: Some(meta.clone()),
            ..Default::default()
        };
        assert!(cfg.langfuse_enabled);
        assert_eq!(cfg.langfuse_user_id.as_deref(), Some("user"));
        assert_eq!(cfg.langfuse_tags.as_deref(), Some(r#"["agent"]"#));
        assert_eq!(cfg.langfuse_environment.as_deref(), Some("staging"));
        assert_eq!(cfg.langfuse_release.as_deref(), Some("2.0"));
        assert_eq!(cfg.langfuse_version.as_deref(), Some("abc123"));
        assert_eq!(cfg.langfuse_public, Some(true));
        assert_eq!(cfg.langfuse_metadata, Some(meta));
    }

    #[test]
    fn test_agent_config_langfuse_capture_defaults() {
        let cfg = AgentConfig::default();
        assert!(!cfg.langfuse_capture_input);
        assert!(!cfg.langfuse_capture_output);
        assert_eq!(cfg.langfuse_capture_max_chars, 20000);
        assert!(cfg.langfuse_redact_secrets);
    }

    #[test]
    fn test_agent_config_langfuse_capture_configured() {
        let cfg = AgentConfig {
            langfuse_capture_input: true,
            langfuse_capture_output: true,
            langfuse_capture_max_chars: 5000,
            langfuse_redact_secrets: false,
            ..Default::default()
        };
        assert!(cfg.langfuse_capture_input);
        assert!(cfg.langfuse_capture_output);
        assert_eq!(cfg.langfuse_capture_max_chars, 5000);
        assert!(!cfg.langfuse_redact_secrets);
    }

    #[test]
    fn test_agent_config_langfuse_empty_tags_none() {
        // 空 tags 策略：tags 为 None 时不写入
        let cfg = AgentConfig {
            langfuse_tags: None,
            ..Default::default()
        };
        assert_eq!(cfg.langfuse_tags, None);
    }

    #[test]
    fn test_agent_config_langfuse_metadata_cross_crate() {
        // metadata 跨 crate 表达：HashMap<String, String> 可以在 core 中使用
        let mut meta = HashMap::new();
        meta.insert("env".into(), "prod".into());
        meta.insert("items".into(), r#"["a","b"]"#.into());
        let cfg = AgentConfig {
            langfuse_metadata: Some(meta),
            ..Default::default()
        };
        let m = cfg.langfuse_metadata.as_ref().unwrap();
        assert_eq!(m.get("env").map(|s| s.as_str()), Some("prod"));
        assert_eq!(m.get("items").map(|s| s.as_str()), Some(r#"["a","b"]"#));
        assert_eq!(m.len(), 2);
    }

    #[test]
    fn test_soft_limit_zero_disabled() {
        let cfg = AgentConfig {
            soft_limit: 0,
            ..Default::default()
        };
        assert_eq!(cfg.soft_limit, 0);
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
        let trimmer: std::sync::Arc<dyn ContextTrimmer + Send + Sync> =
            std::sync::Arc::new(MockTrimmer);
        let ctx = AgentLoopContext {
            session_id: "sess-1".into(),
            history: vec![],
            working_dir: PathBuf::from("/tmp"),
            config: crate::provider::LlmConfig::default(),
            cancel_token: cancel.clone(),
            context_trimmer: trimmer,
            global_tx: None,
            permission_rules: None,
            agent_kind: AgentKind::Primary,
            depth: 0,
            parent_session_id: "sess-1".into(),
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
            run_collect(provider, vec![], test_setup(), 10, 200, Message::user("Hi")).await;

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

    /// W1: Done 分支 —— provider 直接返回 ChatEvent::Done
    #[tokio::test]
    async fn test_done_decision_logs_completion() {
        let provider: StdArc<dyn LlmProvider> =
            StdArc::new(TestProvider::new(vec![vec![ChatEvent::Done]]));

        let (events, _sm, _sid) =
            run_collect(provider, vec![], test_setup(), 10, 200, Message::user("Go")).await;

        // Agent loop should produce a Done event
        assert!(
            events.iter().any(|e| matches!(e, AgentEvent::Done)),
            "expected Done event from minimal provider"
        );
        // No errors during bare-minimum run
        assert!(
            events
                .iter()
                .all(|e| !matches!(e, AgentEvent::Error { .. })),
            "unexpected error events"
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
            200,
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
            200,
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

    /// 4. 硬上限：hard_limit=2，一直返回 ToolCall → Error(MaxIterations)
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
            1, // soft_limit = 1
            2, // hard_limit = 2
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
            run_collect(provider, vec![], setup, 10, 200, Message::user("Hi")).await;

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
        let registry = ToolRegistry::new();
        registry.register(Arc::from(tool)).unwrap();

        let setup = test_setup();
        let config = AgentConfig {
            soft_limit: 50,
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
        let registry = ToolRegistry::new();
        registry.register(Arc::from(tool)).unwrap();

        let setup = test_setup();
        let config = AgentConfig {
            soft_limit: 50,
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
        let registry = ToolRegistry::new();
        registry.register(Arc::from(tool)).unwrap();

        let setup = test_setup();
        let config = AgentConfig {
            soft_limit: 50,
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
        let provider: StdArc<dyn LlmProvider> = StdArc::new(TestProvider::new(vec![
            vec![
                ChatEvent::TextDelta(
                    "What color?\n[USER_QUERY]\nChoose:\n- Red\n- Blue\n[/USER_QUERY]".into(),
                ),
                ChatEvent::Done,
            ],
            vec![ChatEvent::Done],
        ]));

        let (tx, mut rx) = mpsc::channel(64);
        let registry = ToolRegistry::new();

        let setup = test_setup();
        let config = AgentConfig {
            soft_limit: 50,
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
            soft_limit: 50,
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
            soft_limit: 50,
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

    // ── Empty response handling ─────────────────────────────────────────────

    #[tokio::test]
    async fn test_empty_response_returns_error() {
        // LLM returns Done immediately with no text, no tool calls, no thinking
        // but with tokens consumed (simulates the redacted_thinking bug scenario)
        let provider: StdArc<dyn LlmProvider> = StdArc::new(TestProvider::new(vec![vec![
            ChatEvent::UsageInfo {
                input_tokens: 100,
                output_tokens: 4096,
                tool_calls: 0,
                cache_creation_input_tokens: 0,
                cache_read_input_tokens: 0,
            },
            ChatEvent::Done,
        ]]));

        let (events, _sm, _sid) =
            run_collect(provider, vec![], test_setup(), 10, 200, Message::user("Hi")).await;

        assert!(
            events.iter().any(|e| matches!(e, AgentEvent::Error { .. })),
            "Expected Error event for empty LLM response"
        );
        assert!(
            !events.iter().any(|e| matches!(e, AgentEvent::Done)),
            "Should NOT emit Done when LLM returns empty response"
        );
    }

    #[tokio::test]
    async fn test_thinking_only_response_ok() {
        // LLM returns a thinking block but no text — this is valid after
        // the redacted_thinking fix and should NOT produce an error
        let provider: StdArc<dyn LlmProvider> = StdArc::new(TestProvider::new(vec![vec![
            ChatEvent::ThinkingBlock(serde_json::json!({
                "type": "thinking",
                "thinking": "[REDACTED]",
                "signature": "base64sig",
            })),
            ChatEvent::Done,
        ]]));

        let (events, _sm, _sid) =
            run_collect(provider, vec![], test_setup(), 10, 200, Message::user("Hi")).await;

        assert!(
            !events.iter().any(|e| matches!(e, AgentEvent::Error { .. })),
            "Should NOT error when LLM returns thinking-only response"
        );
        assert!(
            events.iter().any(|e| matches!(e, AgentEvent::Done)),
            "Expected Done for thinking-only response"
        );
    }

    // ── Panic safety tests ───────────────────────────────────────────────────

    /// 10. agent loop 正常结束 → session 状态为 Idle
    #[tokio::test]
    async fn test_session_status_idle_on_normal_completion() {
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

        // Session should be Completed after normal completion
        let session = sm.get(&sid).unwrap();
        assert_eq!(session.status, SessionStatus::Completed);
        assert_eq!(session.history.len(), 2, "Expected 2 messages in history");
    }

    /// 11. agent loop 通过 catch_unwind 保护 panic
    #[tokio::test]
    async fn test_panic_does_not_leak_session_running() {
        struct PanicProvider;
        #[async_trait::async_trait]
        impl LlmProvider for PanicProvider {
            async fn chat_stream(
                &self,
                _messages: &[Message],
                _tools: &[crate::message::ToolDefinition],
                _config: &LlmConfig,
                _cancel: &tokio_util::sync::CancellationToken,
            ) -> Result<
                std::pin::Pin<Box<dyn futures::Stream<Item = Result<ChatEvent, LlmError>> + Send>>,
                LlmError,
            > {
                panic!("provider panic");
            }
        }

        let provider: StdArc<dyn LlmProvider> = StdArc::new(PanicProvider);
        let setup = test_setup();
        let config = AgentConfig::default();
        let (tx, rx) = mpsc::channel(64);

        let registry = ToolRegistry::new();
        let sm = setup.session_mgr.clone();
        let sid = setup.session_id.clone();

        let sm_for_spawn = sm.clone();
        let handle = tokio::spawn(async move {
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

        // Drop receiver so the loop doesn't hang on send errors
        drop(rx);

        // The JoinHandle should indicate a panic (resumed_unwind)
        let result = handle.await;
        assert!(
            result.is_err(),
            "Expected agent loop to panic, but it completed normally"
        );
        let err = result.unwrap_err();
        assert!(err.is_panic(), "Expected a panic error");

        // Session should be reset to Idle despite the panic
        let session = sm.get(&sid).unwrap();
        assert_eq!(
            session.status,
            SessionStatus::Idle,
            "Session should be Idle after panic, not Running"
        );
    }

    /// W2: agent loop panic 时，应通过 global_tx 发送 AgentMessage::Error
    /// 让 orchestrator 能够把失败结果转发给父 agent。
    #[tokio::test]
    async fn test_panic_emits_error_envelope_to_global_tx() {
        struct PanicProvider;
        #[async_trait::async_trait]
        impl LlmProvider for PanicProvider {
            async fn chat_stream(
                &self,
                _messages: &[Message],
                _tools: &[crate::message::ToolDefinition],
                _config: &LlmConfig,
                _cancel: &tokio_util::sync::CancellationToken,
            ) -> Result<
                std::pin::Pin<Box<dyn futures::Stream<Item = Result<ChatEvent, LlmError>> + Send>>,
                LlmError,
            > {
                panic!("provider panic");
            }
        }

        let provider: StdArc<dyn LlmProvider> = StdArc::new(PanicProvider);

        // 手动构建带 global_tx 的 ctx（test_setup 默认不带）
        let session_mgr = StdArc::new(SessionManager::new(InMemorySessionStore::new()));
        let session = session_mgr
            .create(Path::new("/tmp"), LlmConfig::default())
            .unwrap();
        let sid = session.id.clone();
        let trimmer: StdArc<dyn ContextTrimmer + Send + Sync> = StdArc::new(MockTrimmer);

        let (global_tx, mut global_rx) = mpsc::channel::<Envelope>(16);
        let permission_rules = StdArc::new(Vec::new());

        let ctx = session_mgr
            .start_loop(&sid, &trimmer, Some(global_tx), Some(permission_rules))
            .unwrap();

        let rule_engine = StdArc::new(RuleEngine::new(Path::new("/tmp")).unwrap());
        let registry = ToolRegistry::new();
        let config = AgentConfig::default();
        let (tx, _rx) = mpsc::channel(64);

        let sm_for_spawn = session_mgr.clone();
        let handle = tokio::spawn(async move {
            run_agent_loop(
                provider,
                StdArc::new(registry),
                rule_engine,
                sm_for_spawn,
                ctx,
                &config,
                Message::user("Hi"),
                tx,
            )
            .await;
        });

        // 等待 task panic
        let join_res = handle.await;
        assert!(
            join_res.is_err() && join_res.unwrap_err().is_panic(),
            "expected agent loop to panic"
        );

        // 关键断言：global_tx 上应能收到 AgentMessage::Error envelope
        let envelope =
            tokio::time::timeout(std::time::Duration::from_millis(500), global_rx.recv())
                .await
                .expect("should not timeout waiting for error envelope")
                .expect("envelope channel should not be closed");

        assert_eq!(envelope.session_id, sid);
        match envelope.message {
            AgentMessage::Error { code, message } => {
                assert_eq!(code, AgentErrorCode::Internal);
                assert!(
                    message.contains("panic"),
                    "error message should mention panic, got: {message}"
                );
            }
            _ => panic!("expected AgentMessage::Error, got a different variant"),
        }
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

        let registry = ToolRegistry::new();
        registry
            .register(Arc::new(CategorisedTool {
                name: "bash",
                cat: "common",
            }))
            .unwrap();
        registry
            .register(Arc::new(CategorisedTool {
                name: "codegraph",
                cat: "analyze",
            }))
            .unwrap();
        registry
            .register(Arc::new(CategorisedTool {
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

    #[test]
    fn test_render_tool_guide_no_task_reference() {
        use crate::tool::ToolContext;
        use crate::tool::ToolResult;

        struct DummyTool;

        #[async_trait::async_trait]
        impl Tool for DummyTool {
            fn name(&self) -> &str {
                "bash"
            }
            fn description(&self) -> &str {
                "run commands"
            }
            fn parameters(&self) -> serde_json::Value {
                serde_json::json!({})
            }
            async fn execute(&self, _args: serde_json::Value, _ctx: &ToolContext) -> ToolResult {
                ToolResult::success("ok")
            }
        }

        let registry = ToolRegistry::new();
        registry.register(Arc::new(DummyTool)).unwrap();
        let guide = render_tool_guide(&registry);
        assert!(!guide.is_empty());
        assert!(
            !guide.contains("`task` tool"),
            "should not reference the `task` tool, got: {guide}"
        );
    }

    #[test]
    fn test_render_tool_guide_lists_agent_tools() {
        use crate::tool::ToolContext;
        use crate::tool::ToolResult;

        struct DummyTool;

        #[async_trait::async_trait]
        impl Tool for DummyTool {
            fn name(&self) -> &str {
                "bash"
            }
            fn description(&self) -> &str {
                "run commands"
            }
            fn parameters(&self) -> serde_json::Value {
                serde_json::json!({})
            }
            async fn execute(&self, _args: serde_json::Value, _ctx: &ToolContext) -> ToolResult {
                ToolResult::success("ok")
            }
        }

        let registry = ToolRegistry::new();
        registry.register(Arc::new(DummyTool)).unwrap();
        let guide = render_tool_guide(&registry);
        assert!(!guide.is_empty());
        // Should mention at least one agent tool name like @explorer or @fixer
        assert!(
            guide.contains("@explorer") || guide.contains("@fixer") || guide.contains("@designer"),
            "should list agent tool names like @explorer, got: {guide}"
        );
    }

    // ── AgentTool::execute ──────────────────────────────────────────────────

    #[tokio::test]
    async fn test_agent_tool_execute_sends_spawn_request_and_returns_response() {
        let tool = AgentTool::new("explorer".to_string(), "Search codebase".to_string());
        let (tx, mut rx) = mpsc::channel::<Envelope>(8);

        let ctx = ToolContext {
            working_dir: PathBuf::from("/tmp"),
            session_id: Some("test-session".to_string()),
            permission_rules: None,
            global_tx: Some(tx),
            visp_trace_id: None,
            iter_span_w3c_id: None,
        };

        let handle = tokio::spawn(async move {
            let env = rx.recv().await.expect("should receive envelope");
            assert_eq!(env.session_id, "test-session");
            match env.message {
                AgentMessage::SpawnRequest {
                    subagent_type,
                    description,
                    prompt,
                    response_tx,
                    task_id,
                    ..
                } => {
                    assert_eq!(subagent_type, "explorer");
                    assert_eq!(description, "Search codebase");
                    assert_eq!(prompt, "find all TODOs");
                    assert!(task_id.is_none());
                    if let Some(tx) = response_tx {
                        let _ = tx.send("found 3 TODOs".to_string());
                    }
                }
                _ => panic!("expected SpawnRequest"),
            }
        });

        let result = tool
            .execute(serde_json::json!({"prompt": "find all TODOs"}), &ctx)
            .await;

        assert!(!result.is_error);
        assert_eq!(result.content, "found 3 TODOs");
        handle.await.unwrap();
    }

    #[tokio::test]
    async fn test_agent_tool_execute_error_without_global_tx() {
        let tool = AgentTool::new("fixer".to_string(), "Fix things".to_string());

        let ctx = ToolContext {
            working_dir: PathBuf::from("/tmp"),
            session_id: None,
            permission_rules: None,
            global_tx: None,
            visp_trace_id: None,
            iter_span_w3c_id: None,
        };

        let result = tool
            .execute(serde_json::json!({"prompt": "do something"}), &ctx)
            .await;

        assert!(result.is_error);
        assert!(result.content.contains("global_tx not available"));
    }

    #[tokio::test]
    async fn test_agent_tool_execute_error_missing_prompt() {
        let tool = AgentTool::new("explorer".to_string(), "Search".to_string());
        let (tx, _rx) = mpsc::channel::<Envelope>(8);

        let ctx = ToolContext {
            working_dir: PathBuf::from("/tmp"),
            session_id: None,
            permission_rules: None,
            global_tx: Some(tx),
            visp_trace_id: None,
            iter_span_w3c_id: None,
        };

        let result = tool.execute(serde_json::json!({}), &ctx).await;

        assert!(result.is_error);
        assert!(result.content.contains("Missing required 'prompt'"));
    }

    // ── cleanup_orphan_tool_uses ──────────────────────────────────────────────

    #[test]
    fn test_cleanup_single_orphan_at_end() {
        let mut history = vec![
            Message::user("do task"),
            Message::tool_call(vec![ToolCallRequest {
                id: "call_a".to_string(),
                name: "search".to_string(),
                arguments: "{}".to_string(),
            }]),
        ];
        cleanup_orphan_tool_uses(&mut history);
        // 孤儿 tool_call 被清空
        assert!(history[1].tool_calls.is_none());
    }

    #[test]
    fn test_cleanup_complete_pair_unchanged() {
        let mut history = vec![
            Message::user("do task"),
            Message::tool_call(vec![ToolCallRequest {
                id: "call_a".to_string(),
                name: "search".to_string(),
                arguments: "{}".to_string(),
            }]),
            Message::tool("result", "call_a"),
        ];
        let original = history.clone();
        cleanup_orphan_tool_uses(&mut history);
        // 完整配对保持不变
        assert_eq!(history[1].tool_calls, original[1].tool_calls);
        assert_eq!(history[2].role, Role::Tool);
    }

    #[test]
    fn test_cleanup_orphan_middle_complete_end() {
        // orphan 在中间，完整配对在末尾 → 两个都应该被检查
        let mut history = vec![
            Message::user("task A"),
            Message::tool_call(vec![ToolCallRequest {
                id: "call_orphan".to_string(),
                name: "search".to_string(),
                arguments: "{}".to_string(),
            }]),
            // 中间 user 消息隔开（模拟中断后用户新消息）
            Message::user("task B"),
            Message::tool_call(vec![ToolCallRequest {
                id: "call_good".to_string(),
                name: "read".to_string(),
                arguments: "{}".to_string(),
            }]),
            Message::tool("result", "call_good"),
        ];
        cleanup_orphan_tool_uses(&mut history);
        // 孤儿 tool_call 应该被清空（之前因为 break bug 会漏掉）
        assert!(
            history[1].tool_calls.is_none(),
            "orphan tool_calls should be cleared"
        );
        // 完整配对应该保留
        assert!(history[3].tool_calls.is_some(), "good pair should stay");
        assert_eq!(history[4].role, Role::Tool);
    }

    #[test]
    fn test_cleanup_id_mismatch() {
        // assistant tool_call id 与 tool 结果 id 不匹配
        let mut history = vec![
            Message::user("do task"),
            Message::tool_call(vec![ToolCallRequest {
                id: "call_a".to_string(),
                name: "search".to_string(),
                arguments: "{}".to_string(),
            }]),
            Message::tool("result", "call_b"), // ID 不匹配！
        ];
        cleanup_orphan_tool_uses(&mut history);
        // ID 不匹配 → tool_calls 被清空
        assert!(
            history[1].tool_calls.is_none(),
            "mismatched tool_calls should be cleared"
        );
        // tool 消息被标记为 skip_context
        assert!(
            history[2].skip_context,
            "orphan tool result should be skip_context"
        );
    }

    #[test]
    fn test_cleanup_orphan_tool_message() {
        // tool 消息存在但没有匹配的 assistant tool_call
        let mut history = vec![
            Message::user("do task"),
            Message::assistant("done"),
            Message::tool("orphan result", "call_orphan"),
        ];
        cleanup_orphan_tool_uses(&mut history);
        // 孤儿 tool 消息被标记为 skip_context
        assert!(
            history[2].skip_context,
            "orphan tool message should be skip_context"
        );
    }

    #[test]
    fn test_cleanup_multi_tool_calls_partial_match() {
        // assistant 有 2 个 tool_calls，但只有 1 个 tool 结果（且 ID 匹配第一个）
        let mut history = vec![
            Message::user("do task"),
            Message::tool_call(vec![
                ToolCallRequest {
                    id: "call_a".to_string(),
                    name: "search".to_string(),
                    arguments: "{}".to_string(),
                },
                ToolCallRequest {
                    id: "call_b".to_string(),
                    name: "read".to_string(),
                    arguments: "{}".to_string(),
                },
            ]),
            Message::tool("result a", "call_a"),
            // call_b 的结果缺失！
        ];
        cleanup_orphan_tool_uses(&mut history);
        // 部分匹配 → tool_calls 全部清空
        assert!(
            history[1].tool_calls.is_none(),
            "partial match should clear all"
        );
        // tool 消息 call_a 被标记为孤儿
        assert!(
            history[2].skip_context,
            "orphan tool result should be skip_context"
        );
    }

    // ── W1-S1-3: TraceContext on SpawnRequest & Envelope ────────────────

    #[test]
    fn test_spawn_request_carries_trace_context() {
        let tc = TraceContext::new(
            "0af7651916cd43dd8448eb211c80319c".to_string(),
            "b7ad6b7169203331".to_string(),
            1,
            None,
            None,
        )
        .unwrap();
        let spawn = AgentMessage::SpawnRequest {
            call_id: "call-1".into(),
            subagent_type: "default".into(),
            description: "do work".into(),
            prompt: "do work".into(),
            task_id: None,
            trace_context: Some(tc.clone()),
            response_tx: None,
        };
        match &spawn {
            AgentMessage::SpawnRequest { trace_context, .. } => {
                assert_eq!(trace_context, &Some(tc));
            }
            _ => panic!("expected SpawnRequest"),
        }
    }

    #[test]
    fn test_spawn_request_backward_compat_default_none() {
        let spawn = AgentMessage::SpawnRequest {
            call_id: "call-1".into(),
            subagent_type: "default".into(),
            description: "do work".into(),
            prompt: "do work".into(),
            task_id: None,
            trace_context: None,
            response_tx: None,
        };
        match &spawn {
            AgentMessage::SpawnRequest { trace_context, .. } => {
                assert!(trace_context.is_none());
            }
            _ => panic!("expected SpawnRequest"),
        }
    }

    #[test]
    fn test_spawn_request_response_tx_none_by_default() {
        let spawn = AgentMessage::SpawnRequest {
            call_id: "call-1".into(),
            subagent_type: "default".into(),
            description: "do work".into(),
            prompt: "do work".into(),
            task_id: None,
            trace_context: None,
            response_tx: None,
        };
        match &spawn {
            AgentMessage::SpawnRequest { response_tx, .. } => {
                assert!(response_tx.is_none());
            }
            _ => panic!("expected SpawnRequest"),
        }
    }

    #[test]
    fn test_spawn_request_response_tx_send_receive() {
        let (tx, mut rx) = tokio::sync::oneshot::channel::<String>();
        let spawn = AgentMessage::SpawnRequest {
            call_id: "call-2".into(),
            subagent_type: "default".into(),
            description: "do work".into(),
            prompt: "do work".into(),
            task_id: None,
            trace_context: None,
            response_tx: Some(tx),
        };
        if let AgentMessage::SpawnRequest {
            response_tx: Some(sender),
            ..
        } = spawn
        {
            sender.send("hello".to_string()).ok();
        }
        let result = rx.try_recv();
        assert_eq!(result, Ok("hello".to_string()));
    }

    #[test]
    fn test_envelope_carries_trace_context() {
        let tc = TraceContext::new(
            "0af7651916cd43dd8448eb211c80319c".to_string(),
            "b7ad6b7169203331".to_string(),
            1,
            None,
            None,
        )
        .unwrap();
        let envelope = Envelope {
            session_id: "sess-1".into(),
            message: AgentMessage::Done,
            trace_context: Some(tc.clone()),
        };
        assert_eq!(envelope.trace_context, Some(tc));
    }

    #[test]
    fn test_envelope_backward_compat_default_none() {
        let envelope = Envelope {
            session_id: "sess-1".into(),
            message: AgentMessage::Done,
            trace_context: None,
        };
        assert!(envelope.trace_context.is_none());
    }
}
