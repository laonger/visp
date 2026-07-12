//! Orchestrator — 多 Agent 运行时编排器
//!
//! 职责：
//! - 从 global_rx 接收 Envelope（Agent → Orchestrator）
//! - 从 grpc_rx 接收 ClientMessage（用户输入/查询响应）
//! - 转发 Agent 事件到 grpc_tx（→ CLI）
//! - 管理子 Agent 的创建（spawn_sub_agent）、销毁（handle_done）、取消（cancel_agent）
//! - 管理 pending_queries 将用户响应路由到对应 agent

use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use tracing;
use tracing::Instrument;

use visp_core::agent::run_agent_loop;
use visp_core::agent::{
    AgentConfig, AgentEvent, AgentEventFrame, AgentKind, AgentMessage, Envelope,
    OrchestratorMessage, UserQueryResult,
};
use visp_core::agent_definition::{AgentDefinition, merge_permissions};
use visp_core::agent_registry::AgentRegistry;
use visp_core::context::ContextTrimmer;
use visp_core::error::{AgentErrorCode, SessionError};
use visp_core::message::Message;
use visp_core::provider::{LlmProvider, ModelInfo};
use visp_core::rules::RuleEngine;
use visp_core::session::{SessionManager, SessionStatus, SubSessionParams};
use visp_core::tool::ToolType;
use visp_core::tool_registry::ToolRegistry;

use crate::active_agent::{ActiveAgent, ActiveAgentRegistry};

/// 根据 allowed_sub_agents 筛选子 Agent 的工具列表
fn filter_tools_for_sub_agent(
    tool_registry: &ToolRegistry,
    allowed_sub_agents: &[String],
) -> Arc<ToolRegistry> {
    let filtered = ToolRegistry::new();
    for name in tool_registry.names() {
        if let Some(tool) = tool_registry.get(&name) {
            let should_include = match tool.tool_type() {
                ToolType::Agent => {
                    if allowed_sub_agents.is_empty() {
                        false
                    } else {
                        allowed_sub_agents.iter().any(|a| a == tool.name())
                    }
                }
                _ => true,
            };
            if should_include {
                // register 返回 Err 仅当名称重复——此处不会发生
                let _ = filtered.register(tool);
            }
        }
    }
    Arc::new(filtered)
}

/// 从 AgentRegistry 动态构建子 agent 列表（用于注入 system prompt）
fn build_subagent_prompt(registry: &AgentRegistry) -> String {
    let subs: Vec<&AgentDefinition> = registry
        .list_subagents()
        .into_iter()
        .filter(|a| a.name != "default")
        .collect();

    if subs.is_empty() {
        return String::new();
    }

    let mut prompt = String::from("\n## Delegation Guidelines\n\n");
    prompt.push_str("Available sub-agents (use via the `task` tool):\n");
    for def in subs {
        prompt.push_str(&format!("  - `{}` — {}\n", def.name, def.description));
    }
    prompt
}

/// 取消信号
pub struct CancelSignal;

/// CLI → 服务器的消息
#[derive(Debug, Clone)]
pub enum ClientMessage {
    UserInput {
        session_id: String,
        text: String,
    },
    UserQueryResponse {
        query_id: String,
        selected_index: i32,
        text: String,
    },
    /// 取消正在运行的 agent
    Cancel {
        session_id: String,
    },
}

/// Orchestrator 错误
#[derive(Debug, thiserror::Error)]
pub enum OrchestratorError {
    #[error("agent not found: {0}")]
    AgentNotFound(String),
    #[error("session error: {0}")]
    Session(#[from] SessionError),
    #[error("provider not found for key: {0}")]
    ProviderNotFound(String),
    #[error("max depth exceeded: {0}")]
    MaxDepthExceeded(u32),
}

/// 多 Agent 运行时编排器
pub struct Orchestrator {
    // ── 通道 ─────────────────────────────────────────────────
    cancel_rx: mpsc::Receiver<CancelSignal>,
    global_rx: mpsc::Receiver<Envelope>,
    global_tx: mpsc::Sender<Envelope>,
    grpc_rx: mpsc::Receiver<ClientMessage>,
    grpc_tx: mpsc::Sender<AgentEventFrame>,

    // ── 状态 ─────────────────────────────────────────────────
    active_agents: ActiveAgentRegistry,
    pending_queries: HashMap<String, (String, oneshot::Sender<UserQueryResult>)>,
    /// 持有 sub-agent run_agent_loop 的 JoinHandle，便于诊断与未来扩展（如 abort）。
    /// key = sub session_id；handle 仅作引用持有，drop 不会 abort。
    sub_agent_handles: HashMap<String, JoinHandle<()>>,
    /// 待处理的 oneshot 响应通道，key 为 session_id
    pending_responses: HashMap<String, tokio::sync::oneshot::Sender<String>>,

    // ── 共享依赖 ─────────────────────────────────────────────
    session_mgr: Arc<SessionManager>,
    agent_registry: Arc<AgentRegistry>,
    tool_registry: Arc<ToolRegistry>,
    rule_engine: Arc<RuleEngine>,
    agent_config: AgentConfig,
    context_trimmer: Arc<dyn ContextTrimmer + Send + Sync>,
    providers: HashMap<String, Arc<dyn LlmProvider>>,
    default_provider_key: String,
    /// 模型元信息表（key 同 providers），用于 agent 级别模型覆盖时解析 API model 字符串
    model_infos: HashMap<String, ModelInfo>,
}

impl Orchestrator {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        cancel_rx: mpsc::Receiver<CancelSignal>,
        global_rx: mpsc::Receiver<Envelope>,
        global_tx: mpsc::Sender<Envelope>,
        grpc_rx: mpsc::Receiver<ClientMessage>,
        grpc_tx: mpsc::Sender<AgentEventFrame>,
        session_mgr: Arc<SessionManager>,
        agent_registry: Arc<AgentRegistry>,
        tool_registry: Arc<ToolRegistry>,
        rule_engine: Arc<RuleEngine>,
        agent_config: AgentConfig,
        context_trimmer: Arc<dyn ContextTrimmer + Send + Sync>,
        providers: HashMap<String, Arc<dyn LlmProvider>>,
        default_provider_key: String,
        model_infos: HashMap<String, ModelInfo>,
    ) -> Self {
        Self {
            cancel_rx,
            global_rx,
            global_tx,
            grpc_rx,
            grpc_tx,
            active_agents: ActiveAgentRegistry::new(),
            pending_queries: HashMap::new(),
            sub_agent_handles: HashMap::new(),
            pending_responses: HashMap::new(),
            session_mgr,
            agent_registry,
            tool_registry,
            rule_engine,
            agent_config,
            context_trimmer,
            providers,
            default_provider_key,
            model_infos,
        }
    }

    // ── 主循环 ───────────────────────────────────────────────

    /// 启动 Orchestrator 主循环
    /// 使用 biased select 保证取消信号优先处理
    pub async fn run(&mut self) {
        loop {
            tokio::select! {
                biased;
                Some(_signal) = self.cancel_rx.recv() => {
                    tracing::info!("orchestrator received cancel signal, shutting down");
                    for agent in self.active_agents.agents_cloned() {
                        self.cancel_agent(&agent.session_id);
                    }
                    break;
                }
                Some(envelope) = self.global_rx.recv() => {
                    self.handle_agent_message(envelope).await;
                }
                Some(msg) = self.grpc_rx.recv() => {
                    self.handle_client_message(msg).await;
                }
                else => break,
            }
        }
    }

    // ── 消息处理 ─────────────────────────────────────────────

    /// 处理来自 Agent 的消息（公开，供测试用）
    pub async fn handle_agent_message(&mut self, envelope: Envelope) {
        let session_id = envelope.session_id.clone();
        match envelope.message {
            AgentMessage::TextDelta(_) => {
                // TextDelta 已由 run_agent_loop 通过 tx（= grpc_tx）直接送达 CLI
                // 此处不重复转发
            }
            AgentMessage::ThinkingBlock(_) => {
                // 不在 V1 中转发
            }
            AgentMessage::UsageInfo { .. } => {
                // UsageInfo 已由 run_agent_loop 直接送达 CLI
            }
            AgentMessage::StatusUpdate(_) => {
                // StatusUpdate 已由 run_agent_loop 直接送达 CLI
            }
            AgentMessage::Error { code, message } => {
                // W2: 错误退出（含 panic 转发）走专属路径，向父 agent 投递 SubAgentError，
                // 而非 handle_done 默认的 SubAgentComplete 空内容路径。
                self.handle_agent_error(&session_id, code, message).await;
            }
            AgentMessage::ToolCallRequest { .. } => {
                // ToolCallRequest 已由工具执行任务直接送达 CLI
            }
            AgentMessage::ToolCallResult { .. } => {
                // ToolCallResult 已由工具执行任务直接送达 CLI
            }
            AgentMessage::UserQuery {
                query_id,
                message,
                options,
                allow_other,
                respond,
            } => {
                self.pending_queries
                    .insert(query_id.clone(), (session_id.clone(), respond));
                // Look up agent context for the frame
                let agent_name = self
                    .active_agents
                    .get(&session_id)
                    .map(|a| a.agent_name.clone())
                    .unwrap_or_default();
                let parent_session_id = self
                    .active_agents
                    .get(&session_id)
                    .and_then(|a| a.parent_session_id.clone());
                let parent_session_name = parent_session_id
                    .as_ref()
                    .and_then(|pid| self.active_agents.get(pid).map(|a| a.agent_name.clone()));
                let _ = self
                    .grpc_tx
                    .send(AgentEventFrame {
                        event: AgentEvent::UserQuery {
                            query_id,
                            message,
                            options,
                            allow_other,
                            respond: oneshot::channel().0,
                        },
                        session_id: session_id.clone(),
                        agent_name,
                        parent_session_id,
                        parent_session_name,
                    })
                    .await;
            }
            AgentMessage::SpawnRequest {
                call_id,
                subagent_type,
                description,
                prompt,
                task_id,
                trace_context,
                response_tx,
            } => {
                // 优先使用 SpawnRequest 携带的 TraceContext（由父 agent 的
                // agent_loop 用父 trace_id 构造），确保子 agent 跨 mpsc
                // 边界继承父 trace_id。仅当未携带时才回退到从当前 span 提取
                // （OTel 不活跃时走 UUID fallback）。
                let trace_context =
                    trace_context.unwrap_or_else(crate::observability::extract_trace_context);
                self.spawn_sub_agent(
                    &envelope.session_id,
                    &call_id,
                    &subagent_type,
                    &description,
                    &prompt,
                    task_id.as_deref(),
                    Some(trace_context),
                    response_tx,
                )
                .await;
            }
            AgentMessage::Done => {
                self.handle_done(&session_id).await;
            }
        }
    }

    /// 处理来自 CLI 的消息（公开，供测试用）
    pub async fn handle_client_message(&mut self, msg: ClientMessage) {
        match msg {
            ClientMessage::UserInput { session_id, text } => {
                match self.session_mgr.get(&session_id) {
                    Ok(session) if session.parent_id.is_none() => {
                        // 恢复场景主 session 可能是 Completed/Error，重置为 Idle 再启动
                        if session.status != SessionStatus::Idle {
                            let _ = self
                                .session_mgr
                                .finish_loop(&session_id, SessionStatus::Idle);
                        }
                        self.start_main_agent(&session_id, &text).await;
                    }
                    _ => {}
                }
            }
            ClientMessage::UserQueryResponse {
                query_id,
                selected_index,
                text,
            } => {
                if let Some((_session_id, respond)) = self.pending_queries.remove(&query_id) {
                    let _ = respond.send(UserQueryResult {
                        selected_index,
                        text,
                    });
                }
            }
            ClientMessage::Cancel { session_id } => {
                tracing::info!(%session_id, "cancelling agent by user request");
                self.cancel_agent(&session_id);
            }
        }
    }

    // ── Agent 生命周期 ───────────────────────────────────────

    /// 启动主 Agent（根，无 parent）
    async fn start_main_agent(&mut self, session_id: &str, user_message: &str) {
        let agent_name = match self.session_mgr.get(session_id) {
            Ok(s) => s.agent_name.clone(),
            Err(e) => {
                tracing::error!(session_id, error = %e, "failed to get session");
                return;
            }
        };

        let agent_def = match self.agent_registry.get(&agent_name) {
            Some(a) => a.clone(),
            None => {
                tracing::error!(agent_name, "agent definition not found");
                return;
            }
        };

        // Append agent-specific system prompt (from .visp/agents/*.md)
        if !agent_def.system_prompt.is_empty()
            && let Err(e) = self
                .session_mgr
                .append_system_prompt_template(session_id, &agent_def.system_prompt)
        {
            tracing::warn!(
                session_id,
                error = %e,
                "failed to append agent system prompt"
            );
        }

        // Append dynamic sub-agent delegation guidelines
        let subagent_prompt = build_subagent_prompt(&self.agent_registry);
        if !subagent_prompt.is_empty()
            && let Err(e) = self
                .session_mgr
                .append_system_prompt_template(session_id, &subagent_prompt)
        {
            tracing::warn!(
                session_id,
                error = %e,
                "failed to append subagent list to system prompt"
            );
        }

        // Create inbox
        let (inbox_tx, _inbox_rx) = mpsc::channel(64);

        // Register active agent
        self.active_agents.register(ActiveAgent {
            session_id: session_id.to_string(),
            parent_session_id: None,
            agent_name: agent_name.clone(),
            cancel_token: Default::default(),
            inbox: inbox_tx,
            pending_call_id: None,
            started_at: std::time::Instant::now(),
        });

        // Apply agent-level model/temperature overrides to the session config.
        // This ensures that when an agent definition specifies a `model` key,
        // the actual API model string is used rather than the session's default.
        {
            let mut sub_config = match self.session_mgr.get(session_id) {
                Ok(s) => s.config.clone(),
                Err(e) => {
                    tracing::error!(session_id, error = %e, "failed to get session for config override");
                    return;
                }
            };

            if let Some(info) = self.resolve_model_info(Some(&agent_def)) {
                sub_config.model = info.model.clone();
                sub_config.model_key = agent_def.model.clone();
                sub_config.provider = info.provider.clone();
                if let Some(t) = info.temperature {
                    sub_config.temperature = t;
                }
                if let Some(mt) = info.max_tokens {
                    sub_config.max_tokens = mt;
                }
                if let Some(mct) = info.max_context_tokens {
                    sub_config.max_context_tokens = mct;
                }
                tracing::debug!(
                    session_id,
                    agent = %agent_name,
                    model = %sub_config.model,
                    "applied agent model override"
                );
            }

            if let Some(temp) = agent_def.temperature {
                sub_config.temperature = temp as f64;
            }

            if let Err(e) = self.session_mgr.update_config(session_id, sub_config) {
                tracing::warn!(
                    session_id,
                    error = %e,
                    "failed to apply agent config overrides"
                );
            }
        }

        // Resolve provider
        let provider = match self.resolve_provider(Some(&agent_def), session_id) {
            Some(p) => p,
            None => {
                tracing::error!(agent_name, "no provider available for main agent");
                return;
            }
        };

        // Permission rules (root agent: use agent default)
        let permissions = merge_permissions(&[], &[], &agent_def.permission);

        // Create loop context
        let ctx = match self.session_mgr.start_loop(
            session_id,
            &self.context_trimmer,
            Some(self.global_tx.clone()),
            Some(Arc::new(permissions)),
        ) {
            Ok(ctx) => ctx,
            Err(e) => {
                tracing::error!(session_id, error = %e, "start_loop_v2 failed");
                return;
            }
        };

        let msg = Message::user(user_message);

        // Create forwarding task: agent_tx → grpc_tx with session context
        let (agent_tx, mut agent_rx) = mpsc::channel::<AgentEvent>(64);
        let grpc_tx = self.grpc_tx.clone();
        let sid = session_id.to_string();
        let agent_name = agent_name.clone();
        tokio::spawn(async move {
            while let Some(event) = agent_rx.recv().await {
                if grpc_tx
                    .send(AgentEventFrame {
                        event,
                        session_id: sid.clone(),
                        agent_name: agent_name.clone(),
                        parent_session_id: None,
                        parent_session_name: None,
                    })
                    .await
                    .is_err()
                {
                    break;
                }
            }
        });

        let provider = provider.clone();
        let tool_registry = self.tool_registry.clone();
        let rule_engine = self.rule_engine.clone();
        let session_mgr = self.session_mgr.clone();
        let mut config = self.agent_config.clone();

        // Apply agent-level steps override as hard_limit
        if let Some(steps) = agent_def.steps {
            config.hard_limit = steps;
            if config.soft_limit == 0 || config.soft_limit >= steps {
                config.soft_limit = steps.saturating_mul(4) / 5;
            }
        }

        tokio::spawn(async move {
            run_agent_loop(
                provider,
                tool_registry,
                rule_engine,
                session_mgr,
                ctx,
                &config,
                msg,
                agent_tx,
            )
            .await;
        });
    }

    /// 启动子 Agent（响应 SpawnRequest）
    #[allow(clippy::too_many_arguments)]
    async fn spawn_sub_agent(
        &mut self,
        parent_session_id: &str,
        call_id: &str,
        subagent_type: &str,
        description: &str,
        prompt: &str,
        _task_id: Option<&str>,
        trace_context: Option<visp_core::TraceContext>,
        response_tx: Option<tokio::sync::oneshot::Sender<String>>,
    ) {
        // 1. Depth check
        let depth = self.active_agents.compute_depth(parent_session_id);
        if depth >= self.agent_config.max_depth {
            tracing::warn!(
                parent_session_id,
                depth,
                max = self.agent_config.max_depth,
                "max depth exceeded"
            );
            self.send_sub_agent_error(parent_session_id, call_id, "Max depth exceeded")
                .await;
            return;
        }

        // 2. Look up agent definition
        let agent_def = match self.agent_registry.get(subagent_type) {
            Some(a) => a.clone(),
            None => {
                tracing::error!(subagent_type, "subagent definition not found");
                self.send_sub_agent_error(
                    parent_session_id,
                    call_id,
                    &format!("Unknown subagent type: {subagent_type}"),
                )
                .await;
                return;
            }
        };

        // 3. Get parent session for permission inheritance, project_path, config
        let parent_session = match self.session_mgr.get(parent_session_id) {
            Ok(s) => s,
            Err(e) => {
                tracing::error!(parent_session_id, error = %e, "parent session not found");
                return;
            }
        };

        // 4. Merge permissions: parent session deny → parent agent deny → subagent rules
        let parent_agent_def = self.agent_registry.get(&parent_session.agent_name);
        let parent_agent_permission = parent_agent_def
            .map(|a| a.permission.as_slice())
            .unwrap_or(&[]);
        let merged_rules = merge_permissions(
            &parent_session.permission,
            parent_agent_permission,
            &agent_def.permission,
        );

        // 5. Generate session ID — pure UUID, parent/agent stored in session table
        let sub_session_id = uuid::Uuid::new_v4().to_string();

        // 6. Create sub session
        let sub_session = match self.session_mgr.create_sub(SubSessionParams {
            parent_id: Some(parent_session_id.to_string()),
            agent_name: subagent_type.to_string(),
            permission: merged_rules.clone(),
            session_id: Some(sub_session_id.clone()),
            project_path: parent_session.project_path.clone(),
            config: parent_session.config.clone(),
        }) {
            Ok(s) => s,
            Err(e) => {
                tracing::error!(error = %e, "create_sub failed");
                self.send_sub_agent_error(
                    parent_session_id,
                    call_id,
                    "Failed to create sub session",
                )
                .await;
                return;
            }
        };
        let sub_session_id = sub_session.id.clone();

        // 6.5. Apply agent-level model/temperature overrides
        //
        // The sub-session was created with `parent_session.config.clone()`.
        // If the agent definition specifies a `model` key, we resolve it to
        // the actual API model string and update the session's LlmConfig.
        // Similarly, `temperature` from the agent definition overrides the
        // inherited value.
        {
            let mut sub_config = sub_session.config.clone();

            // Resolve model info from agent_def.model key
            if let Some(info) = self.resolve_model_info(Some(&agent_def)) {
                sub_config.model = info.model.clone();
                sub_config.model_key = agent_def.model.clone();
                sub_config.provider = info.provider.clone();
                // Apply model-level defaults if the parent config didn't set them explicitly
                if let Some(t) = info.temperature {
                    sub_config.temperature = t;
                }
                if let Some(mt) = info.max_tokens {
                    sub_config.max_tokens = mt;
                }
                if let Some(mct) = info.max_context_tokens {
                    sub_config.max_context_tokens = mct;
                }
                tracing::debug!(
                    sub_session_id,
                    agent = %subagent_type,
                    model = %sub_config.model,
                    "applied agent model override"
                );
            }

            // Agent-level temperature override takes precedence over model default
            if let Some(temp) = agent_def.temperature {
                sub_config.temperature = temp as f64;
            }

            // Persist the updated config to the session
            if let Err(e) = self.session_mgr.update_config(&sub_session_id, sub_config) {
                tracing::warn!(
                    sub_session_id,
                    error = %e,
                    "failed to apply agent config overrides"
                );
            }
        }

        // Append agent-specific system prompt (from .visp/agents/*.md)
        if !agent_def.system_prompt.is_empty()
            && let Err(e) = self
                .session_mgr
                .append_system_prompt_template(&sub_session_id, &agent_def.system_prompt)
        {
            tracing::warn!(
                sub_session_id,
                error = %e,
                "failed to append agent system prompt"
            );
        }

        // 7. Create inbox + register active agent
        let (inbox_tx, _inbox_rx) = mpsc::channel(64);
        self.active_agents.register(ActiveAgent {
            session_id: sub_session_id.clone(),
            parent_session_id: Some(parent_session_id.to_string()),
            agent_name: subagent_type.to_string(),
            cancel_token: Default::default(),
            inbox: inbox_tx,
            pending_call_id: Some(call_id.to_string()),
            started_at: std::time::Instant::now(),
        });

        // 8. Start loop context (consumes inbox_rx)
        let mut ctx = match self.session_mgr.start_loop(
            &sub_session_id,
            &self.context_trimmer,
            Some(self.global_tx.clone()),
            Some(Arc::new(merged_rules)),
        ) {
            Ok(ctx) => ctx,
            Err(e) => {
                tracing::error!(session_id = sub_session_id, error = %e, "start_loop failed for sub agent");
                self.active_agents.remove(&sub_session_id);
                self.send_sub_agent_error(
                    parent_session_id,
                    call_id,
                    "Failed to start sub agent loop",
                )
                .await;
                return;
            }
        };
        // Set agent kind and depth for observability
        ctx.agent_kind = AgentKind::Sub;
        ctx.depth = depth;

        // 9. Resolve provider
        let provider = match self.resolve_provider(Some(&agent_def), &sub_session_id) {
            Some(p) => p,
            None => {
                tracing::error!(subagent_type, "no provider available");
                self.active_agents.remove(&sub_session_id);
                self.send_sub_agent_error(parent_session_id, call_id, "No provider available")
                    .await;
                return;
            }
        };

        // 10. Send initial user message with the task prompt (fallback to description).
        //     `prompt` is the self-contained task written by the parent agent for the
        //     sub-agent; `description` is only a short label, used when prompt is empty.
        let task_msg = if prompt.is_empty() {
            description
        } else {
            prompt
        };
        let msg = Message::user(task_msg);

        // Create forwarding task: agent_tx → grpc_tx with session context
        let (agent_tx, mut agent_rx) = mpsc::channel::<AgentEvent>(64);
        let grpc_tx = self.grpc_tx.clone();
        let sid = sub_session_id.clone();
        let agent_name = subagent_type.to_string();
        let parent_sid = Some(parent_session_id.to_string());
        let parent_name = self
            .active_agents
            .get(parent_session_id)
            .map(|a| a.agent_name.clone());
        tokio::spawn(async move {
            while let Some(event) = agent_rx.recv().await {
                if grpc_tx
                    .send(AgentEventFrame {
                        event,
                        session_id: sid.clone(),
                        agent_name: agent_name.clone(),
                        parent_session_id: parent_sid.clone(),
                        parent_session_name: parent_name.clone(),
                    })
                    .await
                    .is_err()
                {
                    break;
                }
            }
        });

        let provider = provider.clone();
        let tool_registry = filter_tools_for_sub_agent(
            &self.tool_registry,
            &agent_def.allowed_sub_agents,
        );
        let rule_engine = self.rule_engine.clone();
        let session_mgr = self.session_mgr.clone();
        let mut config = self.agent_config.clone();

        // Apply agent-level steps override as hard_limit for this sub-agent
        if let Some(steps) = agent_def.steps {
            config.hard_limit = steps;
            // Also set soft_limit to 80% of hard_limit if it would exceed hard_limit
            if config.soft_limit == 0 || config.soft_limit >= steps {
                config.soft_limit = steps.saturating_mul(4) / 5;
            }
        }

        // ── W1-S3c: 创建 visp.subagent.spawn span ─────────────────────────
        let spawn_span = tracing::info_span!(
            "visp.subagent.spawn",
            visp.subagent.name = %subagent_type,
            visp.subagent.session_id = %sub_session_id,
            visp.subagent.call_id = %call_id,
            visp.subagent.task_id = tracing::field::Empty,
            visp.subagent.depth = depth,
            visp.agent.parent_session_id = %ctx.parent_session_id,
            trace_id = tracing::field::Empty,
            parent_span_id = tracing::field::Empty,
            trace_state = tracing::field::Empty,
            // Langfuse trace-level fields
            langfuse.session.id = tracing::field::Empty,
            langfuse.user.id = tracing::field::Empty,
            langfuse.trace.tags = tracing::field::Empty,
            langfuse.trace.name = tracing::field::Empty,
            langfuse.environment = tracing::field::Empty,
            langfuse.release = tracing::field::Empty,
            langfuse.version = tracing::field::Empty,
            langfuse.trace.public = tracing::field::Empty,
            langfuse.trace.metadata = tracing::field::Empty,
        );

        // W2-S5: Rebuild OTel parent Context from TraceContext and set it
        // on spawn_span. This must happen before the first .enter()/.in_scope(),
        // which is before tokio::spawn + .instrument().
        if let Some(tc) = trace_context.as_ref()
            && let Some(parent_ctx) = crate::observability::rebuild_parent_context(tc)
        {
            use tracing_opentelemetry::OpenTelemetrySpanExt;
            let _ = spawn_span.set_parent(parent_ctx);
        }

        // 通过 span field 传递 TraceContext（ParentLinkLayer 在 on_record 中读取）
        if let Some(tc) = trace_context.as_ref() {
            spawn_span.record("trace_id", tc.trace_id.as_str());
            if let Some(ref psid) = tc.parent_span_id {
                spawn_span.record("parent_span_id", psid.as_str());
            }
            if let Some(ref ts) = tc.trace_state {
                spawn_span.record("trace_state", ts.as_str());
            }
        }

        // 记录 Langfuse trace 级字段（仅 enabled 时写入）
        // 使用 parent_session_id 确保同 trace 内的 session/name 一致
        visp_core::agent_loop::record_langfuse_trace_fields(
            &spawn_span,
            &self.agent_config,
            parent_session_id,
        );

        // 记录 task_id（Risk R1：前移到 tokio::spawn 之前）
        if let Some(task_id) = _task_id {
            spawn_span.record("visp.subagent.task_id", task_id);
        }

        let loop_handle = tokio::spawn(
            async move {
                run_agent_loop(
                    provider,
                    tool_registry,
                    rule_engine,
                    session_mgr,
                    ctx,
                    &config,
                    msg,
                    agent_tx,
                )
                .await;
            }
            .instrument(spawn_span.clone()),
        );
        self.sub_agent_handles
            .insert(sub_session_id.clone(), loop_handle);

        if let Some(tx) = response_tx {
            self.pending_responses.insert(sub_session_id.clone(), tx);
        }

        tracing::info!(
            parent = parent_session_id,
            child = sub_session_id,
            agent = subagent_type,
            "sub agent spawned"
        );
    }

    /// 处理 Agent 完成
    async fn handle_done(&mut self, session_id: &str) {
        let agent_info = match self.active_agents.get(session_id) {
            Some(a) => {
                let pending_call_id = a.pending_call_id.clone();
                let parent_id = a.parent_session_id.clone();
                let agent_name = a.agent_name.clone();
                tracing::info!(
                    session_id,
                    agent_name = %a.agent_name,
                    parent_id = ?a.parent_session_id,
                    "sub-agent done received"
                );
                (pending_call_id, parent_id, agent_name)
            }
            None => {
                tracing::warn!(session_id, "handle_done: agent not in registry, ignoring");
                return;
            }
        };

        let (pending_call_id, parent_id, agent_name) = agent_info;

        // Remove from registry
        self.active_agents.remove(session_id);
        self.sub_agent_handles.remove(session_id);

        // Finish the session
        let _ = self
            .session_mgr
            .finish_loop(session_id, SessionStatus::Idle);

        // Check pending_responses first (oneshot path)
        if let Some(tx) = self.pending_responses.remove(session_id) {
            let content = self.extract_result(session_id);
            let _ = tx.send(content);
            return;
        }

        // Send result to parent if this is a sub-agent
        if let Some(ref parent_id) = parent_id {
            let content = self.extract_result(session_id);
            let call_id = pending_call_id.unwrap_or_default();

            if let Some(parent) = self.active_agents.get(parent_id) {
                match parent
                    .inbox
                    .try_send(OrchestratorMessage::SubAgentComplete {
                        call_id,
                        content,
                        task_id: String::new(),
                    }) {
                    Ok(()) => {
                        tracing::info!(
                            session_id,
                            parent_id,
                            agent_name,
                            "sub-agent completion forwarded to parent"
                        );
                    }
                    Err(mpsc::error::TrySendError::Full(msg)) => {
                        let inbox = parent.inbox.clone();
                        let log_session_id = session_id.to_string();
                        let log_parent_id = parent_id.clone();
                        let log_agent_name = agent_name.clone();
                        tokio::spawn(async move {
                            let _ = inbox.send(msg).await;
                            tracing::info!(
                                session_id = %log_session_id,
                                parent_id = %log_parent_id,
                                agent_name = %log_agent_name,
                                "sub-agent completion forwarded to parent (after backpressure)"
                            );
                        });
                    }
                    Err(mpsc::error::TrySendError::Closed(_)) => {
                        tracing::warn!(parent_id, "parent agent inbox closed, sub result dropped");
                    }
                }
            } else {
                tracing::warn!(
                    parent_id,
                    "parent agent no longer active, sub result dropped"
                );
            }
        } else {
            // Root agent done — notify CLI
            let _ = self
                .grpc_tx
                .send(AgentEventFrame {
                    event: AgentEvent::Done,
                    session_id: session_id.to_string(),
                    agent_name,
                    parent_session_id: None,
                    parent_session_name: None,
                })
                .await;
        }
    }

    /// W2: 处理 Agent 错误退出（含 panic 转发）
    ///
    /// 与 handle_done 的关键区别：
    /// - handle_done 假设 sub-agent 正常完成，向父发送 SubAgentComplete + 提取最终结果
    /// - handle_agent_error 知道 sub-agent 异常退出，向父发送 SubAgentError 携带错误消息
    ///
    /// 仍执行公共清理：从 active_agents / sub_agent_handles 移除、finish_loop。
    ///
    /// 注：当 code == AgentErrorCode::Cancelled（用户主动取消）时：
    /// - 错误消息统一为 "agent cancelled"（去除 "Agent loop cancelled" 等异种文本）
    /// - 日志降级为 info!（用户取消是预期行为，不该污染 ERROR 日志）
    async fn handle_agent_error(
        &mut self,
        session_id: &str,
        code: AgentErrorCode,
        error_message: String,
    ) {
        let is_cancel = matches!(code, AgentErrorCode::Cancelled);
        // 取消路径下统一错误文本，便于父 agent / 日志侧识别
        let normalized_message = if is_cancel {
            "agent cancelled".to_string()
        } else {
            error_message
        };

        let agent_info = match self.active_agents.get(session_id) {
            Some(a) => {
                let pending_call_id = a.pending_call_id.clone();
                let parent_id = a.parent_session_id.clone();
                let agent_name = a.agent_name.clone();
                if is_cancel {
                    tracing::info!(
                        session_id,
                        agent_name = %a.agent_name,
                        parent_id = ?a.parent_session_id,
                        "agent cancelled by user"
                    );
                } else {
                    tracing::error!(
                        session_id,
                        agent_name = %a.agent_name,
                        parent_id = ?a.parent_session_id,
                        error = %normalized_message,
                        "sub-agent error received"
                    );
                }
                (pending_call_id, parent_id, agent_name)
            }
            None => {
                tracing::warn!(
                    session_id,
                    "handle_agent_error: agent not in registry, ignoring"
                );
                return;
            }
        };

        let (pending_call_id, parent_id, agent_name) = agent_info;

        self.active_agents.remove(session_id);
        self.sub_agent_handles.remove(session_id);
        let _ = self
            .session_mgr
            .finish_loop(session_id, SessionStatus::Error);

        // Check pending_responses first (oneshot path)
        if let Some(tx) = self.pending_responses.remove(session_id) {
            let _ = tx.send(format!("[SubAgent Error] {}", normalized_message));
            return;
        }

        if let Some(ref parent_id) = parent_id {
            let call_id = pending_call_id.unwrap_or_default();
            self.send_sub_agent_error(parent_id, &call_id, &normalized_message)
                .await;
            tracing::info!(
                session_id,
                parent_id,
                agent_name,
                "sub-agent error forwarded to parent as SubAgentError"
            );
        } else if is_cancel {
            tracing::info!(session_id, agent_name, "root agent cancelled by user");
        } else {
            // Root agent error — Error event 已由 run_agent_loop 直接送 CLI；这里仅记录
            tracing::error!(
                session_id,
                agent_name,
                "root agent errored: {normalized_message}"
            );
        }
    }

    /// 提取 Agent 的最终结果（最后一条 assistant 消息内容）
    fn extract_result(&self, session_id: &str) -> String {
        if let Ok(session) = self.session_mgr.get(session_id) {
            for msg in session.history.iter().rev() {
                if msg.role == visp_core::message::Role::Assistant && !msg.content.is_empty() {
                    return msg.content.clone();
                }
            }
        }
        String::new()
    }

    /// 取消 Agent（递归取消所有子孙）
    fn cancel_agent(&self, session_id: &str) {
        if let Some(agent) = self.active_agents.get(session_id) {
            agent.cancel_token.cancel();
        }
        // 同时通过 session_mgr 取消 AgentLoopContext 使用的 token
        // （ActiveAgent.cancel_token 与 AgentLoopContext.cancel_token 是两个不同的实例，
        //  只取消前者对 run_agent_loop 无效）
        self.session_mgr.cancel_agent(session_id);
        for child in self.active_agents.descendants_of(session_id) {
            child.cancel_token.cancel();
            self.session_mgr.cancel_agent(&child.session_id);
        }
    }

    // ── 辅助方法 ─────────────────────────────────────────────

    /// 向父 agent 发送子 agent 错误
    async fn send_sub_agent_error(&mut self, parent_session_id: &str, call_id: &str, error: &str) {
        if let Some(parent) = self.active_agents.get(parent_session_id) {
            let _ = parent.inbox.try_send(OrchestratorMessage::SubAgentError {
                call_id: call_id.to_string(),
                error: error.to_string(),
            });
        }
    }

    /// 解析 provider：agent.model → session.model → default
    fn resolve_model_info(&self, agent: Option<&AgentDefinition>) -> Option<&ModelInfo> {
        let agent = agent?;
        let model_key = agent.model.as_ref()?;
        self.model_infos.get(model_key)
    }

    pub fn resolve_provider(
        &self,
        agent: Option<&AgentDefinition>,
        session_id: &str,
    ) -> Option<Arc<dyn LlmProvider>> {
        // Try agent's model key first
        if let Some(agent) = agent
            && let Some(ref model_key) = agent.model
            && let Some(provider) = self.providers.get(model_key)
        {
            return Some(provider.clone());
        }

        // Try session's model_key (format "{provider}/{name}")
        if let Ok(session) = self.session_mgr.get(session_id)
            && let Some(ref model_key) = session.config.model_key
            && let Some(provider) = self.providers.get(model_key)
        {
            return Some(provider.clone());
        }

        // Try session's model name as direct key (backward compat for legacy configs)
        if let Ok(session) = self.session_mgr.get(session_id)
            && let Some(provider) = self.providers.get(&session.config.model)
        {
            return Some(provider.clone());
        }

        // Fall back to default provider key
        self.providers.get(&self.default_provider_key).cloned()
    }
}

#[cfg(test)]
#[path = "orchestrator_tests.rs"]
mod tests;
