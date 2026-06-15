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

use visp_core::agent::{
    AgentConfig, AgentEvent, AgentMessage, Envelope, OrchestratorMessage, UserQueryResult,
};
use visp_core::agent_definition::{merge_permissions, AgentDefinition};
use visp_core::agent_registry::AgentRegistry;
use visp_core::context::ContextTrimmer;
use visp_core::error::SessionError;
use visp_core::message::Message;
use visp_core::provider::LlmProvider;
use visp_core::rules::RuleEngine;
use visp_core::session::{SessionManager, SessionStatus, SubSessionParams};
use visp_core::tool_registry::ToolRegistry;
use visp_core::agent::run_agent_loop;

use crate::active_agent::{ActiveAgent, ActiveAgentRegistry};

/// 取消信号
pub struct CancelSignal;

/// CLI → 服务器的消息
#[derive(Debug, Clone)]
pub enum ClientMessage {
    UserInput { session_id: String, text: String },
    UserQueryResponse { query_id: String, selected_index: i32, text: String },
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
    grpc_tx: mpsc::Sender<AgentEvent>,

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
        grpc_tx: mpsc::Sender<AgentEvent>,
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
            AgentMessage::TextDelta(content) => {
                let _ = self.grpc_tx.send(AgentEvent::TextDelta(content)).await;
            }
            AgentMessage::ThinkingBlock(_) => {
                // Thinking blocks are not forwarded to CLI in V1
            }
            AgentMessage::UsageInfo { input_tokens, output_tokens, tool_calls, cache_creation_input_tokens, cache_read_input_tokens } => {
                let _ = self.grpc_tx.send(AgentEvent::UsageInfo { input_tokens, output_tokens, tool_calls, cache_creation_input_tokens, cache_read_input_tokens }).await;
            }
            AgentMessage::StatusUpdate(content) => {
                let _ = self.grpc_tx.send(AgentEvent::StatusUpdate(content)).await;
            }
            AgentMessage::Error { code, message } => {
                let _ = self.grpc_tx.send(AgentEvent::Error { code, message }).await;
                self.handle_done(&session_id).await;
            }
            AgentMessage::ToolCallRequest { call_id, tool_name, arguments } => {
                let _ = self.grpc_tx.send(AgentEvent::ToolCallRequest { call_id, tool_name, arguments }).await;
            }
            AgentMessage::ToolCallResult { call_id, tool_name, content, is_error } => {
                let _ = self.grpc_tx.send(AgentEvent::ToolCallResult { call_id, tool_name, content, is_error }).await;
            }
            AgentMessage::UserQuery { query_id, message, options, allow_other, respond } => {
                self.pending_queries.insert(query_id.clone(), (session_id.clone(), respond));
                let _ = self.grpc_tx.send(AgentEvent::UserQuery {
                    query_id,
                    message,
                    options,
                    allow_other,
                    respond: oneshot::channel().0, // placeholder, real one stored in pending_queries
                }).await;
            }
            AgentMessage::SpawnRequest { call_id, subagent_type, description, task_id } => {
                self.spawn_sub_agent(&envelope.session_id, &call_id, &subagent_type, &description, task_id.as_deref()).await;
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
                if let Ok(session) = self.session_mgr.get(&session_id) && session.status == SessionStatus::Idle {
                    self.start_main_agent(&session_id, &text).await;
                }
            }
            ClientMessage::UserQueryResponse { query_id, selected_index, text } => {
                if let Some((_session_id, respond)) = self.pending_queries.remove(&query_id) {
                    let _ = respond.send(UserQueryResult { selected_index, text });
                }
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
        let tx = self.grpc_tx.clone();

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
                tx,
            ).await;
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
            tracing::warn!(parent_session_id, depth, max = self.agent_config.max_depth, "max depth exceeded");
            self.send_sub_agent_error(parent_session_id, call_id, "Max depth exceeded").await;
            return;
        }

        // 2. Look up agent definition
        let agent_def = match self.agent_registry.get(subagent_type) {
            Some(a) => a.clone(),
            None => {
                tracing::error!(subagent_type, "subagent definition not found");
                self.send_sub_agent_error(parent_session_id, call_id, &format!("Unknown subagent type: {subagent_type}")).await;
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
        let parent_agent_permission = parent_agent_def.map(|a| a.permission.as_slice()).unwrap_or(&[]);
        let merged_rules = merge_permissions(
            &parent_session.permission,
            parent_agent_permission,
            &agent_def.permission,
        );

        // 5. Generate session ID
        let sub_session_id = format!(
            "{parent_session_id}/{subagent_type}/{}",
            uuid::Uuid::new_v4().to_string().split('-').next().unwrap_or("0000")
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
                self.send_sub_agent_error(parent_session_id, call_id, "Failed to create sub session").await;
                return;
            }
        };
        let sub_session_id = sub_session.id.clone();

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
                self.send_sub_agent_error(parent_session_id, call_id, "Failed to start sub agent loop").await;
                return;
            }
        };

        // 9. Resolve provider
        let provider = match self.resolve_provider(Some(&agent_def), &sub_session_id) {
            Some(p) => p,
            None => {
                tracing::error!(subagent_type, "no provider available");
                self.active_agents.remove(&sub_session_id);
                self.send_sub_agent_error(parent_session_id, call_id, "No provider available").await;
                return;
            }
        };

        // 10. Send initial user message with the task description
        let msg = Message::user(description);

        let provider = provider.clone();
        let tool_registry = self.tool_registry.clone();
        let rule_engine = self.rule_engine.clone();
        let session_mgr = self.session_mgr.clone();
        let config = self.agent_config.clone();
        let tx = self.grpc_tx.clone();

        tokio::spawn(async move {
            run_agent_loop(
                provider,
                tool_registry,
                rule_engine,
                session_mgr,
                ctx,
                &config,
                msg,
                tx,
            ).await;
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
        let agent = match self.active_agents.get(session_id) {
            Some(a) => {
                let pending_call_id = a.pending_call_id.clone();
                let parent_id = a.parent_session_id.clone();
                (pending_call_id, parent_id)
            }
            None => {
                tracing::warn!(session_id, "handle_done: agent not in registry, ignoring");
                return;
            }
        };

        let (pending_call_id, parent_id) = agent;

        // Remove from registry
        self.active_agents.remove(session_id);

        // Finish the session
        let _ = self.session_mgr.finish_loop(session_id, SessionStatus::Idle);

        // Send result to parent if this is a sub-agent
        if let Some(ref parent_id) = parent_id {
            let content = self.extract_result(session_id);
            let call_id = pending_call_id.unwrap_or_default();

            if let Some(parent) = self.active_agents.get(parent_id) {
                match parent.inbox.try_send(OrchestratorMessage::SubAgentComplete {
                    call_id,
                    content,
                    task_id: String::new(),
                }) {
                    Ok(()) => {}
                    Err(mpsc::error::TrySendError::Full(msg)) => {
                        let inbox = parent.inbox.clone();
                        tokio::spawn(async move {
                            let _ = inbox.send(msg).await;
                        });
                    }
                    Err(mpsc::error::TrySendError::Closed(_)) => {
                        tracing::warn!(parent_id, "parent agent inbox closed, sub result dropped");
                    }
                }
            } else {
                tracing::warn!(parent_id, "parent agent no longer active, sub result dropped");
            }
        } else {
            // Root agent done — notify CLI
            let _ = self.grpc_tx.send(AgentEvent::Done).await;
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
        for child in self.active_agents.descendants_of(session_id) {
            child.cancel_token.cancel();
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
        if let Some(agent) = agent && let Some(ref model_key) = agent.model && let Some(provider) = self.providers.get(model_key) {
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
    use visp_core::context::{ContextTrimmer, NoopTrimmer};
    use visp_core::session::InMemorySessionStore;
    use std::path::PathBuf;

    fn make_orchestrator() -> (Orchestrator, mpsc::Sender<Envelope>, mpsc::Sender<ClientMessage>, mpsc::Receiver<AgentEvent>) {
        let (_cancel_tx, cancel_rx) = mpsc::channel(16);
        let (global_tx, global_rx) = mpsc::channel(256);
        let (grpc_tx, grpc_rx) = mpsc::channel(256);
        let (client_tx, client_rx) = mpsc::channel(64);

        let global_tx_for_orch = global_tx.clone();
        let global_tx_for_test = global_tx;

        let store: Box<dyn visp_core::session::SessionStore> = Box::new(InMemorySessionStore::new());
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
    async fn test_handle_text_delta() {
        let (mut orch, global_tx, _client_tx, mut grpc_rx) = make_orchestrator();

        // Send TextDelta via global_tx
        global_tx.send(Envelope {
            session_id: "s-1".to_string(),
            message: AgentMessage::TextDelta("hello".to_string()),
        }).await.unwrap();

        // Process it
        if let Some(envelope) = orch.global_rx.try_recv().ok() {
            orch.handle_agent_message(envelope).await;
        }

        // Check it was forwarded to grpc_tx
        if let Ok(msg) = grpc_rx.try_recv() {
            match msg {
                AgentEvent::TextDelta(content) => assert_eq!(content, "hello"),
                _ => panic!("expected TextDelta"),
            }
        }
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
        }).await;

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
}
