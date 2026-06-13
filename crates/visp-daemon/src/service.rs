use std::collections::HashMap;
use std::path::Path;
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use futures::StreamExt;
use tokio::sync::{Mutex, RwLock, mpsc, oneshot};
use tonic::{Request, Response, Status, Streaming};

use visp_codegraph::CodeGraph;
use visp_core::{
    agent::{AgentConfig, AgentEvent, UserQueryResult, run_agent_loop},
    context::ContextTrimmer,
    message::{Message, Role},
    provider::{LlmConfig, LlmProvider},
    rules::RuleEngine,
    session::{SessionManager, SessionStatus},
    tool::ToolContext,
    tool_registry::ToolRegistry,
};
use visp_mcp::manager::McpManager;
use visp_proto::visp::{self as proto, coder_daemon_server::CoderDaemon};

use crate::config::LlmSection;

type ResponseStream =
    Pin<Box<dyn futures::Stream<Item = Result<proto::ServerMessage, tonic::Status>> + Send>>;
type CodeGraphMap = Arc<RwLock<HashMap<String, Arc<CodeGraph>>>>;

pub struct CoderDaemonService {
    provider: Arc<dyn LlmProvider>,
    tool_registry: Arc<ToolRegistry>,
    rule_engine: Arc<RuleEngine>,
    session_mgr: Arc<SessionManager>,
    agent_config: AgentConfig,
    start_time: Instant,
    /// Phase 5: lazy-loaded CodeGraph instances per project path
    codegraphs: CodeGraphMap,
    /// 默认 LLM 配置（来自 daemon.toml），create_session 时与客户端配置合并
    default_llm_config: LlmConfig,
    /// 上下文裁剪器
    context_trimmer: Arc<dyn ContextTrimmer + Send + Sync>,
    /// MCP 服务器管理器
    mcp_manager: Arc<McpManager>,
    /// 可用的模型名称列表
    available_models: Vec<String>,
}

impl CoderDaemonService {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        provider: Arc<dyn LlmProvider>,
        tool_registry: Arc<ToolRegistry>,
        rule_engine: Arc<RuleEngine>,
        session_mgr: Arc<SessionManager>,
        agent_config: AgentConfig,
        llm_section: LlmSection,
        context_trimmer: Arc<dyn ContextTrimmer + Send + Sync>,
        mcp_manager: Arc<McpManager>,
        available_models: Vec<String>,
    ) -> Self {
        let mut extra = std::collections::HashMap::new();
        if let Some(budget) = llm_section.thinking_budget_tokens {
            extra.insert("thinking_budget_tokens".into(), budget.to_string());
        }
        // 合并 [llm.extra] 中的自定义参数
        for (k, v) in llm_section.extra.iter() {
            extra.insert(k.clone(), v.clone());
        }
        let default_llm_config = {
            let mut model = llm_section.model.clone();
            // 当 provider 为 openai 且 model 是默认的 Claude 时，自动切换为 GPT
            if llm_section.protocol == "openai" && model == *crate::config::default_model() {
                model = "gpt-4o".to_string();
                tracing::info!("protocol=openai, overriding default model to gpt-4o");
            }
            LlmConfig {
                model,
                temperature: llm_section.temperature,
                max_tokens: llm_section.max_tokens,
                max_context_tokens: llm_section.max_context_tokens,
                extra,
            }
        };
        Self {
            provider,
            tool_registry,
            rule_engine,
            session_mgr,
            agent_config,
            start_time: Instant::now(),
            codegraphs: Arc::new(RwLock::new(HashMap::new())),
            default_llm_config,
            context_trimmer,
            mcp_manager,
            available_models,
        }
    }

    /// Phase 5: lazy-load a CodeGraph for a project path.
    /// Triggers background build_full on first access.
    async fn get_codegraph(&self, project_path: &str) -> Result<Arc<CodeGraph>, Status> {
        let map = self.codegraphs.read().await;
        if let Some(cg) = map.get(project_path) {
            return Ok(cg.clone());
        }
        drop(map);

        let mut cg = CodeGraph::open(Path::new(project_path))
            .map_err(|e| Status::internal(format!("codegraph open: {e}")))?;

        // Start file watcher for incremental indexing
        if let Err(e) = cg
            .start_watching(
                Path::new(project_path),
                visp_codegraph::index::CodeGraphConfig::default(),
            )
            .await
        {
            tracing::warn!("codegraph watcher start failed for {project_path}: {e}");
        }

        let cg = Arc::new(cg);

        // Background full index build (incremental updates will come via watcher)
        let bg = cg.clone();
        let pp = project_path.to_owned();
        let config = visp_codegraph::index::CodeGraphConfig::default();
        tokio::spawn(async move {
            if let Err(e) = bg.build_full(Path::new(&pp), &config).await {
                tracing::warn!("codegraph build_full failed for {pp}: {e}");
            }
        });

        let mut map = self.codegraphs.write().await;
        map.insert(project_path.to_owned(), cg.clone());
        Ok(cg)
    }
}

// ── trait implementation ──────────────────────────────────────────────────────

#[tonic::async_trait]
impl CoderDaemon for CoderDaemonService {
    type ChatStream = ResponseStream;

    async fn create_session(
        &self,
        request: Request<proto::CreateSessionRequest>,
    ) -> Result<Response<proto::Session>, Status> {
        let req = request.into_inner();
        // 从客户端配置开始，然后用 daemon 默认值覆盖未设置的字段
        let mut config = req.config.as_ref().map(map_llm_config).unwrap_or_default();
        if config.extra.is_empty() {
            config.extra = self.default_llm_config.extra.clone();
        }
        // 客户端未传的字段用 daemon 默认值
        if config.model == LlmConfig::default().model {
            config.model = self.default_llm_config.model.clone();
        }
        if (config.temperature - LlmConfig::default().temperature).abs() < f64::EPSILON {
            config.temperature = self.default_llm_config.temperature;
        }
        if config.max_tokens == LlmConfig::default().max_tokens {
            config.max_tokens = self.default_llm_config.max_tokens;
        }
        if config.max_context_tokens == LlmConfig::default().max_context_tokens {
            config.max_context_tokens = self.default_llm_config.max_context_tokens;
        }
        let session = self
            .session_mgr
            .create(Path::new(&req.project_path), config)
            .map_err(|e| Status::internal(e.to_string()))?;
        Ok(Response::new(session_to_proto(
            &session,
            &self.available_models,
        )))
    }

    async fn list_sessions(
        &self,
        _request: Request<()>,
    ) -> Result<Response<proto::ListSessionsResponse>, Status> {
        let sessions = self
            .session_mgr
            .list()
            .map_err(|e| Status::internal(e.to_string()))?;
        Ok(Response::new(proto::ListSessionsResponse {
            sessions: sessions
                .iter()
                .map(|s| session_to_proto(s, &self.available_models))
                .collect(),
        }))
    }

    async fn get_session(
        &self,
        request: Request<proto::GetSessionRequest>,
    ) -> Result<Response<proto::Session>, Status> {
        let session_id = request.into_inner().session_id;

        // Step 1: Exact match
        if let Ok(session) = self.session_mgr.get(&session_id) {
            return Ok(Response::new(session_to_proto(
                &session,
                &self.available_models,
            )));
        }

        // Step 2: Prefix matching
        let sessions = self
            .session_mgr
            .list()
            .map_err(|e| Status::internal(e.to_string()))?;

        let matched: Vec<_> = sessions
            .iter()
            .filter(|s| s.id.starts_with(&session_id))
            .collect();

        match matched.len() {
            0 => Err(Status::not_found("Session not found")),
            1 => Ok(Response::new(session_to_proto(
                matched[0],
                &self.available_models,
            ))),
            _ => Err(Status::not_found("Session not found")),
        }
    }

    async fn delete_session(
        &self,
        request: Request<proto::DeleteSessionRequest>,
    ) -> Result<Response<()>, Status> {
        let session_id = request.into_inner().session_id;
        self.session_mgr
            .delete(&session_id)
            .map_err(|e| Status::internal(e.to_string()))?;
        Ok(Response::new(()))
    }

    async fn chat(
        &self,
        request: Request<Streaming<proto::ClientMessage>>,
    ) -> Result<Response<Self::ChatStream>, Status> {
        let mut in_stream = request.into_inner();
        let (tx, rx) = mpsc::channel::<Result<proto::ServerMessage, Status>>(128);

        let provider = self.provider.clone();
        let tool_registry = self.tool_registry.clone();
        let rule_engine = self.rule_engine.clone();
        let session_mgr = self.session_mgr.clone();
        let agent_config = self.agent_config.clone();
        let context_trimmer = self.context_trimmer.clone();

        tokio::spawn(async move {
            let pending_queries: Arc<
                Mutex<HashMap<String, (String, oneshot::Sender<UserQueryResult>)>>,
            > = Arc::new(Mutex::new(HashMap::new()));
            let mut running_sessions: Vec<String> = Vec::new();

            while let Some(msg_result) = in_stream.next().await {
                let msg = match msg_result {
                    Ok(m) => m,
                    Err(e) => {
                        let _ = tx.send(Err(Status::internal(e.to_string()))).await;
                        break;
                    }
                };

                match msg.payload {
                    Some(proto::client_message::Payload::UserInput(input)) => {
                        let session_id = input.session_id;
                        let text = input.text;
                        tracing::info!(session_id = %session_id, text = %text, "[DAEMON] received UserInput");

                        // Validate session exists and is Idle
                        let session = match session_mgr.get(&session_id) {
                            Ok(s) => s,
                            Err(e) => {
                                let _ = tx
                                    .send(Ok(session_error_msg(
                                        "SessionNotFound",
                                        &e.to_string(),
                                        &session_id,
                                    )))
                                    .await;
                                continue;
                            }
                        };

                        if session.status != SessionStatus::Idle {
                            let _ = tx
                                .send(Ok(session_error_msg(
                                    "SessionBusy",
                                    "Session is not idle",
                                    &session_id,
                                )))
                                .await;
                            continue;
                        }

                        // Start agent loop
                        let text = if text.trim().starts_with("/init") {
                            match crate::command::init::prepare(&session.project_path, &text).await
                            {
                                Ok((init_msg, statuses)) => {
                                    for s in &statuses {
                                        let _ = tx
                                            .send(Ok(proto::ServerMessage {
                                                payload: Some(
                                                    proto::server_message::Payload::StatusUpdate(
                                                        proto::StatusUpdate {
                                                            message: s.clone(),
                                                            session_id: session_id.clone(),
                                                            user_inputs: vec![],
                                                        },
                                                    ),
                                                ),
                                            }))
                                            .await;
                                    }
                                    init_msg.content
                                }
                                Err(e) => {
                                    let _ = tx
                                        .send(Ok(session_error_msg("InitError", &e, &session_id)))
                                        .await;
                                    continue;
                                }
                            }
                        } else {
                            text
                        };

                        let ctx = match session_mgr.start_loop(&session_id, &context_trimmer) {
                            Ok(c) => c,
                            Err(e) => {
                                let _ = tx
                                    .send(Ok(session_error_msg(
                                        "InternalError",
                                        &e.to_string(),
                                        &session_id,
                                    )))
                                    .await;
                                continue;
                            }
                        };

                        // User message will be appended by run_agent_loop
                        let user_msg = Message::user(&text);

                        // Clone Arc refs for the inner spawn
                        let p = provider.clone();
                        let tr = tool_registry.clone();
                        let re = rule_engine.clone();
                        let sm = session_mgr.clone();
                        let ac = agent_config.clone();

                        let (agent_tx, mut agent_rx) = mpsc::channel(64);
                        let tx_events = tx.clone();
                        let sid = session_id.clone();
                        let pq = pending_queries.clone();

                        running_sessions.push(session_id.clone());

                        let sid2 = sid.clone();
                        tokio::spawn(async move {
                            tracing::info!(session_id = %sid, "[DAEMON] agent loop started");
                            run_agent_loop(p, tr, re, sm, ctx, &ac, user_msg, agent_tx).await;
                            tracing::info!(session_id = %sid, "[DAEMON] agent loop finished");
                        });

                        tokio::spawn(async move {
                            while let Some(event) = agent_rx.recv().await {
                                let is_done = matches!(event, AgentEvent::Done);
                                if is_done {
                                    tracing::info!(session_id = %sid2, "[DAEMON] forwarding Done to client");
                                }
                                if matches!(&event, AgentEvent::TextDelta(d) if d.is_empty()) {
                                    tracing::warn!(session_id = %sid2, "[DAEMON] forwarding empty TextDelta");
                                }
                                let msg = match event {
                                    AgentEvent::UserQuery {
                                        query_id,
                                        message,
                                        options,
                                        allow_other,
                                        respond,
                                    } => {
                                        pq.lock()
                                            .await
                                            .insert(query_id.clone(), (sid2.clone(), respond));
                                        proto::ServerMessage {
                                            payload: Some(
                                                proto::server_message::Payload::UserQuery(
                                                    proto::UserQuery {
                                                        query_id,
                                                        message,
                                                        session_id: sid2.clone(),
                                                        options,
                                                        allow_other,
                                                    },
                                                ),
                                            ),
                                        }
                                    }
                                    _ => agent_event_to_server_message(event, &sid2),
                                };
                                if tx_events.send(Ok(msg)).await.is_err() {
                                    break;
                                }
                            }
                        });
                    }
                    Some(proto::client_message::Payload::JoinSession(join)) => {
                        let session_id = join.session_id;
                        if let Ok(session) = session_mgr.get(&session_id) {
                            if session.history.is_empty() {
                                // no-op: new session, no history to show
                            } else {
                                let mut history_lines = Vec::new();
                                history_lines.push(format!(
                                    "═══ Resumed session ({} previous messages) ═══",
                                    session.history.len()
                                ));
                                for msg in &session.history {
                                    let role_label = match msg.role {
                                        Role::User => "User",
                                        Role::Assistant => "Assistant",
                                        Role::Tool => "  Tool",
                                        _ => continue,
                                    };
                                    let preview: String = msg.content.chars().take(120).collect();
                                    if preview.len() < msg.content.len() {
                                        history_lines.push(format!("{role_label}: {preview}..."));
                                    } else {
                                        history_lines.push(format!("{role_label}: {preview}"));
                                    }
                                }
                                history_lines.push("═══ End of history ═══".into());
                                // 收集用户输入，供 CLI 填充 input_history（↑↓ 翻找历史提问）
                                let user_inputs: Vec<String> = session
                                    .history
                                    .iter()
                                    .filter(|m| m.role == Role::User)
                                    .map(|m| m.content.clone())
                                    .collect();
                                let _ = tx
                                    .send(Ok(proto::ServerMessage {
                                        payload: Some(
                                            proto::server_message::Payload::StatusUpdate(
                                                proto::StatusUpdate {
                                                    message: history_lines.join("\n"),
                                                    session_id: session_id.clone(),
                                                    user_inputs,
                                                },
                                            ),
                                        ),
                                    }))
                                    .await;
                            }
                        }
                    }
                    Some(proto::client_message::Payload::ConfigUpdate(update)) => {
                        let session_id = update.session_id;

                        let session = match session_mgr.get(&session_id) {
                            Ok(s) => s,
                            Err(e) => {
                                let _ = tx
                                    .send(Ok(session_error_msg(
                                        "SessionNotFound",
                                        &e.to_string(),
                                        &session_id,
                                    )))
                                    .await;
                                continue;
                            }
                        };

                        if session.status != SessionStatus::Idle {
                            let _ = tx
                                .send(Ok(session_error_msg(
                                    "SessionBusy",
                                    "Cannot update config while session is running",
                                    &session_id,
                                )))
                                .await;
                            continue;
                        }

                        let mut config = session.config.clone();
                        if let Some(update_config) = &update.config {
                            if let Some(model) = &update_config.model {
                                config.model = model.clone();
                            }
                            if let Some(temp) = update_config.temperature {
                                config.temperature = temp;
                            }
                            if let Some(tokens) = update_config.max_tokens {
                                config.max_tokens = tokens;
                            }
                            if !update_config.extra.is_empty() {
                                config.extra.extend(update_config.extra.clone());
                            }
                        }

                        if let Err(e) = session_mgr.update_config(&session_id, config) {
                            let _ = tx
                                .send(Ok(session_error_msg(
                                    "InternalError",
                                    &e.to_string(),
                                    &session_id,
                                )))
                                .await;
                        }
                    }
                    Some(proto::client_message::Payload::UserResponse(resp)) => {
                        let sender = pending_queries.lock().await.remove(&resp.query_id);
                        if let Some((_sid, sender)) = sender {
                            let _ = sender.send(UserQueryResult {
                                selected_index: resp.selected_index,
                                text: resp.text,
                            });
                        }
                    }
                    Some(proto::client_message::Payload::Cancel(cancel)) => {
                        let sid = &cancel.session_id;
                        match session_mgr.get(sid) {
                            Ok(s) if s.status == SessionStatus::Running => {
                                session_mgr.cancel_agent(sid);
                                running_sessions.retain(|id| id != sid);
                                // 清理该会话的所有 pending queries，双重保险
                                let mut pq = pending_queries.lock().await;
                                pq.retain(|_, (sess_id, _)| sess_id != sid);
                            }
                            _ => {
                                // 不存在或非 Running 状态 → 静默忽略
                            }
                        }
                    }
                    Some(proto::client_message::Payload::Ack(ack)) => {
                        tracing::info!(request_id = %ack.request_id, "[DAEMON] received client Ack");
                        // 后续可据此清理 request 级状态
                    }
                    None => {}
                }
            }

            // 客户端断开 → 取消此连接上所有运行中的 agent loop
            for sid in &running_sessions {
                session_mgr.cancel_agent(sid);
            }
            if !running_sessions.is_empty() {
                tracing::info!(
                    "[DAEMON] client disconnected, cancelled {} sessions",
                    running_sessions.len()
                );
            }
        });

        let out_stream = futures::stream::unfold(rx, |mut rx| async move {
            rx.recv().await.map(|item| (item, rx))
        });

        Ok(Response::new(Box::pin(out_stream)))
    }

    async fn read_file(
        &self,
        request: Request<proto::ReadFileRequest>,
    ) -> Result<Response<proto::ReadFileResponse>, Status> {
        let req = request.into_inner();
        let path = req.path.clone();

        let working_dir = self
            .session_mgr
            .get(&req.session_id)
            .map(|s| s.project_path.clone())
            .unwrap_or_else(|_| std::path::PathBuf::from("."));

        let ctx = ToolContext {
            working_dir,
            session_id: Some(req.session_id),
        };

        let mut args = serde_json::json!({ "path": path });
        if let Some(start_line) = req.start_line {
            args["start_line"] = serde_json::json!(start_line);
        }
        if let Some(end_line) = req.end_line {
            args["end_line"] = serde_json::json!(end_line);
        }

        let result = self
            .tool_registry
            .execute("read_file", args, &ctx)
            .await
            .ok_or_else(|| Status::internal("Tool 'read_file' not found"))?;

        if result.is_error {
            return Err(Status::internal(result.content));
        }

        Ok(Response::new(proto::ReadFileResponse {
            content: result.content,
            path: req.path,
        }))
    }

    async fn search_symbols(
        &self,
        request: Request<proto::SearchSymbolsRequest>,
    ) -> Result<Response<proto::SearchSymbolsResponse>, Status> {
        let req = request.into_inner();
        let project_path = req.project_path;
        let query = req.query;
        let limit = if req.limit <= 0 {
            20
        } else {
            req.limit as usize
        };

        let cg = self.get_codegraph(&project_path).await?;
        let symbols = cg.search(&query, limit).map_err(Status::internal)?;

        Ok(Response::new(proto::SearchSymbolsResponse {
            symbols: symbols
                .into_iter()
                .map(|s| proto::SymbolInfo {
                    name: s.name,
                    kind: s.kind,
                    file_path: s.file_path,
                    line: s.line,
                    column: s.column,
                    signature: s.signature.unwrap_or_default(),
                })
                .collect(),
        }))
    }

    async fn get_symbol_details(
        &self,
        request: Request<proto::GetSymbolDetailsRequest>,
    ) -> Result<Response<proto::SymbolDetails>, Status> {
        let req = request.into_inner();
        let project_path = req.project_path;
        let symbol_name = req.symbol_name;

        let cg = self.get_codegraph(&project_path).await?;
        let mut details = cg.get_details(&symbol_name).map_err(Status::internal)?;

        let d = details
            .drain(..)
            .next()
            .ok_or_else(|| Status::not_found(format!("Symbol '{symbol_name}' not found")))?;

        Ok(Response::new(proto::SymbolDetails {
            name: d.name,
            kind: d.kind,
            file_path: d.file_path,
            line: d.line,
            column: d.column,
            signature: d.signature.unwrap_or_default(),
            docstring: d.docstring.unwrap_or_default(),
            source: d.source,
            callers: d.callers,
            callees: d.callees,
        }))
    }

    async fn health_check(
        &self,
        _request: Request<()>,
    ) -> Result<Response<proto::HealthStatus>, Status> {
        let uptime = self.start_time.elapsed().as_secs();
        Ok(Response::new(proto::HealthStatus {
            alive: true,
            version: env!("CARGO_PKG_VERSION").to_owned(),
            uptime_seconds: uptime,
        }))
    }

    async fn shutdown(
        &self,
        _request: Request<proto::ShutdownRequest>,
    ) -> Result<Response<()>, Status> {
        tracing::info!("shutdown requested, stopping MCP servers");
        self.mcp_manager.shutdown_all().await;
        Ok(Response::new(()))
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn session_to_proto(
    session: &visp_core::session::Session,
    available_models: &[String],
) -> proto::Session {
    let status = match session.status {
        SessionStatus::Idle => proto::SessionStatus::Idle,
        SessionStatus::Running => proto::SessionStatus::Running,
        SessionStatus::Completed => proto::SessionStatus::Completed,
        SessionStatus::Error => proto::SessionStatus::Error,
    };

    let elapsed = session.created_at.elapsed();
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let created_secs = now.as_secs() as i64 - elapsed.as_secs() as i64;

    proto::Session {
        session_id: session.id.clone(),
        status: status.into(),
        project_path: session.project_path.to_string_lossy().to_string(),
        model: session.config.model.clone(),
        last_user_message: session.last_user_message.clone().unwrap_or_default(),
        created_at: Some(prost_types::Timestamp {
            seconds: created_secs,
            nanos: 0,
        }),
        available_models: available_models.to_vec(),
    }
}

fn map_llm_config(proto: &proto::LlmConfig) -> LlmConfig {
    let mut config = LlmConfig::default();
    if let Some(model) = &proto.model {
        config.model = model.clone();
    }
    if let Some(temperature) = proto.temperature {
        config.temperature = temperature;
    }
    if let Some(max_tokens) = proto.max_tokens {
        config.max_tokens = max_tokens;
    }
    if let Some(max_context_tokens) = proto.max_context_tokens {
        config.max_context_tokens = max_context_tokens;
    }
    config.extra = proto.extra.clone();
    config
}

fn session_error_msg(code: &str, message: &str, session_id: &str) -> proto::ServerMessage {
    proto::ServerMessage {
        payload: Some(proto::server_message::Payload::Error(proto::Error {
            code: code.to_owned(),
            message: message.to_owned(),
            session_id: session_id.to_owned(),
        })),
    }
}

fn agent_event_to_server_message(event: AgentEvent, session_id: &str) -> proto::ServerMessage {
    let sid = session_id.to_owned();
    match event {
        AgentEvent::TextDelta(delta) => proto::ServerMessage {
            payload: Some(proto::server_message::Payload::TextDelta(
                proto::TextDelta {
                    delta,
                    session_id: sid,
                },
            )),
        },
        AgentEvent::ToolCallRequest {
            call_id,
            tool_name,
            arguments,
        } => proto::ServerMessage {
            payload: Some(proto::server_message::Payload::ToolCall(proto::ToolCall {
                call_id,
                tool_name,
                arguments,
                session_id: sid,
            })),
        },
        AgentEvent::ToolCallResult {
            call_id,
            tool_name,
            content,
            is_error,
        } => proto::ServerMessage {
            payload: Some(proto::server_message::Payload::ToolResult(
                proto::ToolResult {
                    call_id,
                    content,
                    is_error,
                    tool_name,
                    session_id: sid,
                },
            )),
        },
        AgentEvent::UsageInfo {
            input_tokens,
            output_tokens,
            tool_calls,
            cache_creation_input_tokens,
            cache_read_input_tokens,
        } => proto::ServerMessage {
            payload: Some(proto::server_message::Payload::UsageInfo(
                proto::UsageInfo {
                    input_tokens,
                    output_tokens,
                    tool_calls,
                    cache_creation_input_tokens,
                    cache_read_input_tokens,
                    session_id: sid,
                },
            )),
        },
        AgentEvent::ThinkingBlock(block) => {
            let thinking = block.get("thinking").and_then(|v| v.as_str()).unwrap_or("");
            let signature = block
                .get("signature")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            proto::ServerMessage {
                payload: Some(proto::server_message::Payload::ThinkingBlock(
                    proto::ThinkingBlock {
                        thinking: thinking.to_string(),
                        signature: signature.to_string(),
                        session_id: sid,
                    },
                )),
            }
        }
        AgentEvent::StatusUpdate(message) => proto::ServerMessage {
            payload: Some(proto::server_message::Payload::StatusUpdate(
                proto::StatusUpdate {
                    message,
                    session_id: sid,
                    user_inputs: vec![],
                },
            )),
        },
        AgentEvent::UserQuery { .. } => {
            // Handled separately in the chat handler (sender extraction)
            proto::ServerMessage { payload: None }
        }
        AgentEvent::Error { code, message } => proto::ServerMessage {
            payload: Some(proto::server_message::Payload::Error(proto::Error {
                code: code.to_string(),
                message,
                session_id: sid,
            })),
        },
        AgentEvent::Done => proto::ServerMessage {
            payload: Some(proto::server_message::Payload::Done(proto::Done {
                session_id: sid,
            })),
        },
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;
    use std::sync::Arc as StdArc;
    use visp_core::session::InMemorySessionStore;
    use visp_core::session::Session;
    use visp_core::session::SessionStatus as CoreStatus;
    use visp_core::session::SessionStore;
    use visp_llm::mock::MockProvider;

    // ── Helpers ─────────────────────────────────────────────────────────────

    fn make_service(mgr: StdArc<SessionManager>) -> CoderDaemonService {
        CoderDaemonService {
            provider: Arc::new(MockProvider::new(vec![])),
            tool_registry: Arc::new(ToolRegistry::new()),
            rule_engine: Arc::new(RuleEngine::new(Path::new("/tmp")).unwrap()),
            session_mgr: mgr,
            agent_config: AgentConfig::default(),
            start_time: Instant::now(),
            codegraphs: Arc::new(RwLock::new(HashMap::new())),
            default_llm_config: LlmConfig::default(),
            context_trimmer: Arc::new(visp_core::context::NoopTrimmer),
            mcp_manager: Arc::new(McpManager::new(vec![])),
            available_models: vec![],
        }
    }

    // ── GetSession tests ────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_get_session_exact_match() {
        let mgr = StdArc::new(SessionManager::new(InMemorySessionStore::new()));
        let session = mgr.create(Path::new("/tmp"), LlmConfig::default()).unwrap();
        let session_id = session.id.clone();
        let service = make_service(mgr);

        let request = tonic::Request::new(proto::GetSessionRequest {
            session_id: session_id.clone(),
        });
        let response = service.get_session(request).await.unwrap();
        let result = response.into_inner();
        assert_eq!(result.session_id, session_id);
        assert_eq!(result.project_path, "/tmp");
    }

    #[tokio::test]
    async fn test_get_session_prefix_unique() {
        let mut store = InMemorySessionStore::new();
        store
            .create(Session {
                id: "unique-abcdef".into(),
                project_path: Path::new("/tmp").to_path_buf(),
                status: SessionStatus::Idle,
                created_at: Instant::now(),
                created_at_unix: None,
                history: vec![],
                last_user_message: None,
                config: LlmConfig::default(),
                system_prompt_template: "default".into(),
                approved_tools: HashSet::new(),
            })
            .unwrap();
        let mgr = StdArc::new(SessionManager::new(store));
        let service = make_service(mgr);

        let request = tonic::Request::new(proto::GetSessionRequest {
            session_id: "unique".into(),
        });
        let response = service.get_session(request).await.unwrap();
        let result = response.into_inner();
        assert_eq!(result.session_id, "unique-abcdef");
    }

    #[tokio::test]
    async fn test_get_session_prefix_zero() {
        let mgr = StdArc::new(SessionManager::new(InMemorySessionStore::new()));
        mgr.create(Path::new("/tmp"), LlmConfig::default()).unwrap();
        let service = make_service(mgr);

        let request = tonic::Request::new(proto::GetSessionRequest {
            session_id: "nonexistent-prefix-".into(),
        });
        let err = service.get_session(request).await.unwrap_err();
        assert_eq!(err.code(), tonic::Code::NotFound);
    }

    #[tokio::test]
    async fn test_get_session_prefix_multiple() {
        let mut store = InMemorySessionStore::new();
        store
            .create(Session {
                id: "common-prefix-a".into(),
                project_path: Path::new("/tmp/a").to_path_buf(),
                status: SessionStatus::Idle,
                created_at: Instant::now(),
                created_at_unix: None,
                history: vec![],
                last_user_message: None,
                config: LlmConfig::default(),
                system_prompt_template: "default".into(),
                approved_tools: HashSet::new(),
            })
            .unwrap();
        store
            .create(Session {
                id: "common-prefix-b".into(),
                project_path: Path::new("/tmp/b").to_path_buf(),
                status: SessionStatus::Idle,
                created_at: Instant::now(),
                created_at_unix: None,
                history: vec![],
                last_user_message: None,
                config: LlmConfig::default(),
                system_prompt_template: "default".into(),
                approved_tools: HashSet::new(),
            })
            .unwrap();
        let mgr = StdArc::new(SessionManager::new(store));
        let service = make_service(mgr);

        let request = tonic::Request::new(proto::GetSessionRequest {
            session_id: "common".into(),
        });
        let err = service.get_session(request).await.unwrap_err();
        assert_eq!(err.code(), tonic::Code::NotFound);
    }

    #[tokio::test]
    async fn test_get_session_not_found() {
        let mgr = StdArc::new(SessionManager::new(InMemorySessionStore::new()));
        mgr.create(Path::new("/tmp"), LlmConfig::default()).unwrap();
        let service = make_service(mgr);

        let request = tonic::Request::new(proto::GetSessionRequest {
            session_id: "i-do-not-exist".into(),
        });
        let err = service.get_session(request).await.unwrap_err();
        assert_eq!(err.code(), tonic::Code::NotFound);
    }

    #[tokio::test]
    async fn test_get_session_error_propagation() {
        let mgr = StdArc::new(SessionManager::new(InMemorySessionStore::new()));
        let service = make_service(mgr);

        let request = tonic::Request::new(proto::GetSessionRequest {
            session_id: "missing".into(),
        });
        let err = service.get_session(request).await.unwrap_err();
        assert_eq!(err.code(), tonic::Code::NotFound);
        assert_eq!(err.message(), "Session not found");
    }

    // ── Cancel tests ────────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_cancel_during_agent_loop() {
        let mgr = StdArc::new(SessionManager::new(InMemorySessionStore::new()));
        let session = mgr.create(Path::new("/tmp"), LlmConfig::default()).unwrap();
        let trimmer: Arc<dyn ContextTrimmer + Send + Sync> =
            Arc::new(visp_core::context::NoopTrimmer);
        let ctx = mgr.start_loop(&session.id, &trimmer).unwrap();
        let token = ctx.cancel_token.clone();
        assert!(
            !token.is_cancelled(),
            "token should not be cancelled initially"
        );

        // Simulate Cancel handler: retrieve session, check Running, cancel agent
        let s = mgr.get(&session.id).unwrap();
        assert_eq!(s.status, CoreStatus::Running);
        mgr.cancel_agent(&session.id);

        assert!(
            token.is_cancelled(),
            "token should be cancelled after cancel_agent"
        );
    }

    #[tokio::test]
    async fn test_cancel_idle_session() {
        let mgr = StdArc::new(SessionManager::new(InMemorySessionStore::new()));
        let session = mgr.create(Path::new("/tmp"), LlmConfig::default()).unwrap();

        // Simulate Cancel handler: retrieve session, check status
        let s = mgr.get(&session.id).unwrap();
        assert_eq!(s.status, CoreStatus::Idle);

        // Even if cancel_agent were called on idle session, it should be no-op
        mgr.cancel_agent(&session.id);

        let s = mgr.get(&session.id).unwrap();
        assert_eq!(
            s.status,
            CoreStatus::Idle,
            "idle session should remain idle after cancel"
        );
    }

    #[test]
    fn test_map_llm_config_empty() {
        let config = proto::LlmConfig {
            model: None,
            temperature: None,
            max_tokens: None,
            max_context_tokens: None,
            extra: HashMap::new(),
        };
        let llm = map_llm_config(&config);
        assert_eq!(llm.model, "claude-3-7-sonnet-20250219");
        assert!((llm.temperature - 0.7).abs() < f64::EPSILON);
        assert_eq!(llm.max_tokens, 4096);
    }

    #[test]
    fn test_map_llm_config_full() {
        let mut extra = HashMap::new();
        extra.insert("custom_key".into(), "custom_val".into());
        let config = proto::LlmConfig {
            model: Some("gpt-4".into()),
            temperature: Some(0.5),
            max_tokens: Some(2048),
            max_context_tokens: Some(64000),
            extra,
        };

        let llm = map_llm_config(&config);
        assert_eq!(llm.model, "gpt-4");
        assert!((llm.temperature - 0.5).abs() < f64::EPSILON);
        assert_eq!(llm.max_tokens, 2048);
        assert_eq!(llm.max_context_tokens, 64_000);
        assert_eq!(llm.extra.get("custom_key").unwrap(), "custom_val");
    }

    #[test]
    fn test_map_llm_config_partial() {
        let config = proto::LlmConfig {
            model: Some("gpt-4".into()),
            temperature: None,
            max_tokens: None,
            max_context_tokens: None,
            extra: HashMap::new(),
        };

        let llm = map_llm_config(&config);
        assert_eq!(llm.model, "gpt-4");
        assert!((llm.temperature - 0.7).abs() < f64::EPSILON);
        assert_eq!(llm.max_tokens, 4096);
    }

    #[tokio::test]
    async fn test_create_session_max_context_tokens_default() {
        let mgr = StdArc::new(SessionManager::new(InMemorySessionStore::new()));
        let default_llm_config = LlmConfig {
            max_context_tokens: 200_000,
            ..LlmConfig::default()
        };
        let service = CoderDaemonService {
            provider: Arc::new(MockProvider::new(vec![])),
            tool_registry: Arc::new(ToolRegistry::new()),
            rule_engine: Arc::new(RuleEngine::new(Path::new("/tmp")).unwrap()),
            session_mgr: mgr.clone(),
            agent_config: AgentConfig::default(),
            start_time: Instant::now(),
            codegraphs: Arc::new(RwLock::new(HashMap::new())),
            default_llm_config,
            context_trimmer: Arc::new(visp_core::context::NoopTrimmer),
            mcp_manager: Arc::new(McpManager::new(vec![])),
            available_models: vec![],
        };

        let request = tonic::Request::new(proto::CreateSessionRequest {
            project_path: "/tmp".into(),
            config: Some(proto::LlmConfig {
                model: None,
                temperature: None,
                max_tokens: None,
                max_context_tokens: None,
                extra: HashMap::new(),
            }),
        });

        let response = service.create_session(request).await.unwrap();
        let session = response.into_inner();
        let stored = mgr.get(&session.session_id).unwrap();
        assert_eq!(stored.config.max_context_tokens, 200_000);
    }

    #[tokio::test]
    async fn test_create_session_max_context_tokens_override() {
        let mgr = StdArc::new(SessionManager::new(InMemorySessionStore::new()));
        let default_llm_config = LlmConfig {
            max_context_tokens: 200_000,
            ..LlmConfig::default()
        };
        let service = CoderDaemonService {
            provider: Arc::new(MockProvider::new(vec![])),
            tool_registry: Arc::new(ToolRegistry::new()),
            rule_engine: Arc::new(RuleEngine::new(Path::new("/tmp")).unwrap()),
            session_mgr: mgr.clone(),
            agent_config: AgentConfig::default(),
            start_time: Instant::now(),
            codegraphs: Arc::new(RwLock::new(HashMap::new())),
            default_llm_config,
            context_trimmer: Arc::new(visp_core::context::NoopTrimmer),
            mcp_manager: Arc::new(McpManager::new(vec![])),
            available_models: vec![],
        };

        let request = tonic::Request::new(proto::CreateSessionRequest {
            project_path: "/tmp".into(),
            config: Some(proto::LlmConfig {
                model: None,
                temperature: None,
                max_tokens: None,
                max_context_tokens: Some(32000),
                extra: HashMap::new(),
            }),
        });

        let response = service.create_session(request).await.unwrap();
        let session = response.into_inner();
        let stored = mgr.get(&session.session_id).unwrap();
        assert_eq!(stored.config.max_context_tokens, 32_000);
    }

    #[test]
    fn test_session_to_proto_idle() {
        let session = Session {
            id: "test-1".into(),
            project_path: "/tmp".into(),
            status: SessionStatus::Idle,
            created_at: Instant::now(),
            created_at_unix: None,
            history: vec![],
            last_user_message: None,
            config: LlmConfig::default(),
            system_prompt_template: "default".into(),
            approved_tools: HashSet::new(),
        };

        let proto = session_to_proto(&session, &[]);
        assert_eq!(proto.session_id, "test-1");
        assert_eq!(proto.status, proto::SessionStatus::Idle as i32);
        assert_eq!(proto.project_path, "/tmp");
        assert!(proto.created_at.is_some());
    }

    #[test]
    fn test_session_to_proto_status_mapping() {
        let base = |status: SessionStatus| -> Session {
            Session {
                id: "s".into(),
                project_path: "/p".into(),
                status,
                created_at: Instant::now(),
                created_at_unix: None,
                history: vec![],
                last_user_message: None,
                config: LlmConfig::default(),
                system_prompt_template: "".into(),
                approved_tools: HashSet::new(),
            }
        };

        assert_eq!(
            session_to_proto(&base(SessionStatus::Idle), &[]).status,
            proto::SessionStatus::Idle as i32
        );
        assert_eq!(
            session_to_proto(&base(SessionStatus::Running), &[]).status,
            proto::SessionStatus::Running as i32
        );
        assert_eq!(
            session_to_proto(&base(SessionStatus::Completed), &[]).status,
            proto::SessionStatus::Completed as i32
        );
        assert_eq!(
            session_to_proto(&base(SessionStatus::Error), &[]).status,
            proto::SessionStatus::Error as i32
        );
    }

    #[test]
    fn test_session_error_msg_contains_fields() {
        let msg = session_error_msg("SessionNotFound", "test error", "sess-1");
        match msg.payload {
            Some(proto::server_message::Payload::Error(e)) => {
                assert_eq!(e.code, "SessionNotFound");
                assert_eq!(e.message, "test error");
                assert_eq!(e.session_id, "sess-1");
            }
            _ => panic!("expected Error payload"),
        }
    }

    #[test]
    fn test_agent_event_to_server_message_text_delta() {
        let msg = agent_event_to_server_message(AgentEvent::TextDelta("hello".into()), "sess-1");
        match msg.payload {
            Some(proto::server_message::Payload::TextDelta(t)) => {
                assert_eq!(t.delta, "hello");
                assert_eq!(t.session_id, "sess-1");
            }
            _ => panic!("expected TextDelta"),
        }
    }

    #[test]
    fn test_agent_event_to_server_message_tool_call() {
        let event = AgentEvent::ToolCallRequest {
            call_id: "call-1".into(),
            tool_name: "bash".into(),
            arguments: r#"{"cmd":"ls"}"#.into(),
        };
        let msg = agent_event_to_server_message(event, "sess-1");
        match msg.payload {
            Some(proto::server_message::Payload::ToolCall(t)) => {
                assert_eq!(t.call_id, "call-1");
                assert_eq!(t.tool_name, "bash");
                assert_eq!(t.arguments, r#"{"cmd":"ls"}"#);
                assert_eq!(t.session_id, "sess-1");
            }
            _ => panic!("expected ToolCall"),
        }
    }

    #[test]
    fn test_agent_event_to_server_message_done() {
        let msg = agent_event_to_server_message(AgentEvent::Done, "sess-1");
        match msg.payload {
            Some(proto::server_message::Payload::Done(d)) => {
                assert_eq!(d.session_id, "sess-1");
            }
            _ => panic!("expected Done"),
        }
    }

    #[test]
    fn test_agent_event_to_server_message_error() {
        let event = AgentEvent::Error {
            code: visp_core::error::AgentErrorCode::MaxIterations,
            message: "max reached".into(),
        };
        let msg = agent_event_to_server_message(event, "sess-1");
        match msg.payload {
            Some(proto::server_message::Payload::Error(e)) => {
                assert_eq!(e.code, "Maximum iterations reached");
                assert_eq!(e.message, "max reached");
                assert_eq!(e.session_id, "sess-1");
            }
            _ => panic!("expected Error"),
        }
    }

    #[test]
    fn test_agent_event_to_server_message_user_query_skipped() {
        let (tx, _rx) = oneshot::channel::<UserQueryResult>();
        let event = AgentEvent::UserQuery {
            query_id: "q-1".into(),
            message: "confirm?".into(),
            options: vec![],
            allow_other: false,
            respond: tx,
        };
        let msg = agent_event_to_server_message(event, "sess-1");
        assert!(msg.payload.is_none());
    }
}
