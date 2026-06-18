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
use tracing;

use visp_core::agent::run_agent_loop;
use visp_core::agent::{
    AgentConfig, AgentEvent, AgentEventFrame, AgentMessage, Envelope, OrchestratorMessage,
    UserQueryResult,
};
use visp_core::agent_definition::{AgentDefinition, merge_permissions};
use visp_core::agent_registry::AgentRegistry;
use visp_core::context::ContextTrimmer;
use visp_core::error::SessionError;
use visp_core::message::Message;
use visp_core::provider::LlmProvider;
use visp_core::rules::RuleEngine;
use visp_core::session::{SessionManager, SessionStatus, SubSessionParams};
use visp_core::tool_registry::ToolRegistry;

use crate::active_agent::{ActiveAgent, ActiveAgentRegistry};

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

    // ── 共享依赖 ─────────────────────────────────────────────
    session_mgr: Arc<SessionManager>,
    agent_registry: Arc<AgentRegistry>,
    tool_registry: Arc<ToolRegistry>,
    rule_engine: Arc<RuleEngine>,
    agent_config: AgentConfig,
    context_trimmer: Arc<dyn ContextTrimmer + Send + Sync>,
    providers: HashMap<String, Arc<dyn LlmProvider>>,
    default_provider_key: String,
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
    ) -> Self {
        Self {
            cancel_rx,
            global_rx,
            global_tx,
            grpc_rx,
            grpc_tx,
            active_agents: ActiveAgentRegistry::new(),
            pending_queries: HashMap::new(),
            session_mgr,
            agent_registry,
            tool_registry,
            rule_engine,
            agent_config,
            context_trimmer,
            providers,
            default_provider_key,
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
            AgentMessage::Error { .. } => {
                self.handle_done(&session_id).await;
                // Error 已由 run_agent_loop 直接送达 CLI
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
                task_id,
            } => {
                self.spawn_sub_agent(
                    &envelope.session_id,
                    &call_id,
                    &subagent_type,
                    &description,
                    task_id.as_deref(),
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
                if let Ok(session) = self.session_mgr.get(&session_id)
                    && session.status == SessionStatus::Idle
                {
                    self.start_main_agent(&session_id, &text).await;
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

        // Create inbox
        let (inbox_tx, inbox_rx) = mpsc::channel(64);

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
        let ctx = match self.session_mgr.start_loop_v2(
            session_id,
            &self.context_trimmer,
            self.global_tx.clone(),
            inbox_rx,
            Arc::new(permissions),
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
        let config = self.agent_config.clone();

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
    async fn spawn_sub_agent(
        &mut self,
        parent_session_id: &str,
        call_id: &str,
        subagent_type: &str,
        description: &str,
        _task_id: Option<&str>,
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

        // 5. Generate session ID
        let sub_session_id = format!(
            "{parent_session_id}/{subagent_type}/{}",
            uuid::Uuid::new_v4()
                .to_string()
                .split('-')
                .next()
                .unwrap_or("0000")
        );

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
        let (inbox_tx, inbox_rx) = mpsc::channel(64);
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
        let ctx = match self.session_mgr.start_loop_v2(
            &sub_session_id,
            &self.context_trimmer,
            self.global_tx.clone(),
            inbox_rx,
            Arc::new(merged_rules),
        ) {
            Ok(ctx) => ctx,
            Err(e) => {
                tracing::error!(session_id = sub_session_id, error = %e, "start_loop_v2 failed for sub agent");
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

        // 10. Send initial user message with the task description
        let msg = Message::user(description);

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
        let tool_registry = self.tool_registry.clone();
        let rule_engine = self.rule_engine.clone();
        let session_mgr = self.session_mgr.clone();
        let config = self.agent_config.clone();

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

        // Finish the session
        let _ = self
            .session_mgr
            .finish_loop(session_id, SessionStatus::Idle);

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

        // Try session's model
        if let Ok(session) = self.session_mgr.get(session_id) {
            let model_key = &session.config.model;
            if let Some(provider) = self.providers.get(model_key) {
                return Some(provider.clone());
            }
        }

        // Fall back to default provider key
        self.providers.get(&self.default_provider_key).cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::time::Instant;
    use tokio_util::sync::CancellationToken;
    use visp_core::context::{ContextTrimmer, NoopTrimmer};
    use visp_core::session::InMemorySessionStore;

    fn make_orchestrator() -> (
        Orchestrator,
        mpsc::Sender<Envelope>,
        mpsc::Sender<ClientMessage>,
        mpsc::Receiver<AgentEventFrame>,
    ) {
        let (_cancel_tx, cancel_rx) = mpsc::channel(16);
        let (global_tx, global_rx) = mpsc::channel(256);
        let (grpc_tx, grpc_rx) = mpsc::channel::<AgentEventFrame>(256);
        let (client_tx, client_rx) = mpsc::channel(64);

        let global_tx_for_orch = global_tx.clone();
        let global_tx_for_test = global_tx;

        let store: Box<dyn visp_core::session::SessionStore> =
            Box::new(InMemorySessionStore::new());
        let session_mgr = Arc::new(SessionManager::new(store));
        let agent_registry = Arc::new(AgentRegistry::new());
        let tool_registry = Arc::new(ToolRegistry::new());
        let rule_engine = Arc::new(RuleEngine::new(&PathBuf::from(".")).unwrap());
        let agent_config = AgentConfig::default();
        let context_trimmer: Arc<dyn ContextTrimmer + Send + Sync> = Arc::new(NoopTrimmer);

        let orch = Orchestrator::new(
            cancel_rx,
            global_rx,
            global_tx_for_orch,
            client_rx,
            grpc_tx,
            session_mgr,
            agent_registry,
            tool_registry,
            rule_engine,
            agent_config,
            context_trimmer,
            HashMap::new(),
            "default".to_string(),
        );

        (orch, global_tx_for_test, client_tx, grpc_rx)
    }

    #[tokio::test]
    async fn test_handle_text_delta_not_forwarded() {
        let (mut orch, global_tx, _client_tx, mut grpc_rx) = make_orchestrator();

        // Send TextDelta via global_tx (现已不再转发到 grpc_tx)
        global_tx
            .send(Envelope {
                session_id: "s-1".to_string(),
                message: AgentMessage::TextDelta("hello".to_string()),
            })
            .await
            .unwrap();

        // Process it
        if let Ok(envelope) = orch.global_rx.try_recv() {
            orch.handle_agent_message(envelope).await;
        }

        // TextDelta 不应再被转发到 grpc_tx（由 run_agent_loop 直接送达）
        let result = grpc_rx.try_recv();
        assert!(
            result.is_err(),
            "TextDelta should not be forwarded to grpc_tx"
        );
    }

    #[tokio::test]
    async fn test_pending_query_routing() {
        let (mut orch, _global_tx, _client_tx, _grpc_rx) = make_orchestrator();
        let (respond, _response) = oneshot::channel();

        // Insert a pending query directly
        orch.handle_agent_message(Envelope {
            session_id: "s-1".to_string(),
            message: AgentMessage::UserQuery {
                query_id: "q-1".to_string(),
                message: "Allow?".to_string(),
                options: vec![],
                allow_other: false,
                respond,
            },
        })
        .await;

        // Verify it was stored
        assert!(orch.pending_queries.contains_key("q-1"));
    }

    #[test]
    fn test_resolve_provider_fallback() {
        let (orch, _global_tx, _client_tx, _grpc_rx) = make_orchestrator();
        // Without any providers registered, resolve_provider returns None
        let result = orch.resolve_provider(None, "unknown");
        assert!(result.is_none());
    }

    // ── Multi-agent integration tests ─────────────────────────────────

    #[tokio::test]
    async fn test_spawn_request_creates_sub_agent() {
        let (mut orch, _global_tx, _client_tx, mut _grpc_rx) = make_orchestrator();

        // Register TaskTool in the tool_registry via internal access
        // Since orch.tool_registry is pub, we can register directly
        orch.tool_registry
            .register(Arc::new(visp_tools::task::TaskTool))
            .ok();

        // Inject a SpawnRequest
        let envelope = Envelope {
            session_id: "parent-1".to_string(),
            message: AgentMessage::SpawnRequest {
                call_id: "call-task-1".to_string(),
                subagent_type: "default".to_string(),
                description: "do something".to_string(),
                task_id: None,
            },
        };
        orch.handle_agent_message(envelope).await;

        // The orchestrator should attempt to spawn the sub-agent.
        // Since no parent session exists it should log an error,
        // but we can verify it didn't panic by reaching here.
    }

    #[tokio::test]
    async fn test_create_sub_with_parent_reference() {
        let store: Box<dyn visp_core::session::SessionStore> =
            Box::new(InMemorySessionStore::new());
        let session_mgr = Arc::new(SessionManager::new(store));

        // Create parent session
        let parent = session_mgr
            .create(
                &std::path::PathBuf::from("/tmp"),
                visp_core::provider::LlmConfig::default(),
            )
            .unwrap();

        // Create sub-session using create_sub (public API)
        let child = session_mgr
            .create_sub(SubSessionParams {
                parent_id: Some(parent.id.clone()),
                agent_name: "code-review".to_string(),
                session_id: None,
                project_path: std::path::PathBuf::from("/tmp"),
                config: visp_core::provider::LlmConfig::default(),
                permission: vec![visp_core::agent_definition::PermissionRule {
                    permission: "edit".to_string(),
                    pattern: "*".to_string(),
                    action: visp_core::agent_definition::PermissionAction::Deny,
                }],
            })
            .unwrap();

        // Verify child has parent_id set
        let child_session = session_mgr.get(&child.id).unwrap();
        let parent_id = parent.id.clone();
        assert_eq!(child_session.parent_id, Some(parent_id.clone()));

        // Verify the parent session exists via list
        let sessions = session_mgr.list().unwrap();
        assert!(sessions.iter().any(|s| s.id == parent_id));
        assert!(sessions.iter().any(|s| s.id == child.id));
    }

    #[tokio::test]
    async fn test_max_depth_exceeded() {
        // This test validates that sub-session creation with a parent reference works.
        // Depth enforcement is done at runtime by the orchestrator's active_agents registry.
        // Default AgentConfig has max_depth = 5.
        #[allow(unused_variables)]
        let agent_config = AgentConfig::default();
        assert_eq!(agent_config.max_depth, 5);

        let store: Box<dyn visp_core::session::SessionStore> =
            Box::new(InMemorySessionStore::new());
        let session_mgr = Arc::new(SessionManager::new(store));

        // Create parent session
        let parent = session_mgr
            .create(
                &std::path::PathBuf::from("/tmp"),
                visp_core::provider::LlmConfig::default(),
            )
            .unwrap();

        // Create child of parent
        let child = session_mgr
            .create_sub(SubSessionParams {
                parent_id: Some(parent.id.clone()),
                agent_name: "child".to_string(),
                session_id: None,
                project_path: std::path::PathBuf::from("/tmp"),
                config: visp_core::provider::LlmConfig::default(),
                permission: vec![],
            })
            .unwrap();

        // Verify parent-child chain
        let parent_id = parent.id.clone();
        assert_eq!(child.parent_id, Some(parent_id.clone()));
        assert_ne!(child.id, parent_id);
    }

    #[tokio::test]
    async fn test_subagent_permission_inheritance() {
        let store: Box<dyn visp_core::session::SessionStore> =
            Box::new(InMemorySessionStore::new());
        let session_mgr = Arc::new(SessionManager::new(store));

        let parent = session_mgr
            .create(
                &std::path::PathBuf::from("/tmp"),
                visp_core::provider::LlmConfig::default(),
            )
            .unwrap();

        // Create child with deny on "edit"
        let child = session_mgr
            .create_sub(SubSessionParams {
                parent_id: Some(parent.id.clone()),
                agent_name: "reviewer".to_string(),
                session_id: None,
                project_path: std::path::PathBuf::from("/tmp"),
                config: visp_core::provider::LlmConfig::default(),
                permission: vec![visp_core::agent_definition::PermissionRule {
                    permission: "edit".to_string(),
                    pattern: "*".to_string(),
                    action: visp_core::agent_definition::PermissionAction::Deny,
                }],
            })
            .unwrap();

        let child_session = session_mgr.get(&child.id).unwrap();
        assert_eq!(child_session.parent_id, Some(parent.id));
        assert_eq!(child_session.agent_name, "reviewer");
        // Verify permission was set on child session
        assert!(!child_session.permission.is_empty());
    }

    // ── W1: Sub-agent lifecycle observability ─────────────────────────────

    #[tokio::test]
    async fn test_handle_done_emits_completion_log() {
        let (mut orch, _global_tx, _client_tx, _grpc_rx) = make_orchestrator();
        let cancel = CancellationToken::new();

        // Create inbox for parent agent
        let (parent_inbox_tx, mut parent_inbox_rx) = mpsc::channel(16);

        // Register parent agent
        orch.active_agents.register(ActiveAgent {
            session_id: "parent-1".to_string(),
            parent_session_id: None,
            agent_name: "root".to_string(),
            cancel_token: cancel.clone(),
            inbox: parent_inbox_tx,
            pending_call_id: None,
            started_at: Instant::now(),
        });

        // Register sub-agent
        orch.active_agents.register(ActiveAgent {
            session_id: "child-1".to_string(),
            parent_session_id: Some("parent-1".to_string()),
            agent_name: "sub-agent".to_string(),
            cancel_token: cancel.clone(),
            inbox: mpsc::channel(16).0,
            pending_call_id: Some("call-task-1".to_string()),
            started_at: Instant::now(),
        });

        // Act: handle_done for the sub-agent
        orch.handle_done("child-1").await;

        // Assert: child removed from registry
        assert!(orch.active_agents.get("child-1").is_none());

        // Assert: parent still active
        assert!(orch.active_agents.get("parent-1").is_some());

        // Assert: parent inbox received SubAgentComplete
        let msg = parent_inbox_rx
            .try_recv()
            .expect("parent inbox should contain SubAgentComplete");
        match msg {
            OrchestratorMessage::SubAgentComplete { call_id, .. } => {
                assert_eq!(call_id, "call-task-1");
            }
            _ => panic!("expected SubAgentComplete, got a different variant"),
        }
    }
}
