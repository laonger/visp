use std::collections::HashMap;
use std::path::Path;
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use futures::StreamExt;
use tokio::sync::{mpsc, oneshot, Mutex, RwLock};
use tonic::{Request, Response, Status, Streaming};

use vbw_codegraph::CodeGraph;
use vbw_core::{
    agent::{run_agent_loop, AgentConfig, AgentEvent},
    message::Message,
    provider::{LlmConfig, LlmProvider},
    rules::RuleEngine,
    session::{SessionManager, SessionStatus},
    tool::ToolContext,
    tool_registry::ToolRegistry,
};
use vbw_proto::vibewisp::{coder_daemon_server::CoderDaemon, self as proto};

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
}

impl CoderDaemonService {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        provider: Arc<dyn LlmProvider>,
        tool_registry: Arc<ToolRegistry>,
        rule_engine: Arc<RuleEngine>,
        session_mgr: Arc<SessionManager>,
        agent_config: AgentConfig,
    ) -> Self {
        Self {
            provider,
            tool_registry,
            rule_engine,
            session_mgr,
            agent_config,
            start_time: Instant::now(),
            codegraphs: Arc::new(RwLock::new(HashMap::new())),
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

        let cg = CodeGraph::open(Path::new(project_path))
            .map_err(|e| Status::internal(format!("codegraph open: {e}")))?;
        let cg = Arc::new(cg);

        // Background index build
        let bg = cg.clone();
        let pp = project_path.to_owned();
        tokio::spawn(async move {
            let config = vbw_codegraph::index::CodeGraphConfig::default();
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
        let config = map_to_llm_config(&req.config);
        let session = self
            .session_mgr
            .create(Path::new(&req.project_path), config)
            .map_err(|e| Status::internal(e.to_string()))?;
        Ok(Response::new(session_to_proto(&session)))
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
            sessions: sessions.iter().map(session_to_proto).collect(),
        }))
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

        tokio::spawn(async move {
            let pending_queries: Arc<Mutex<HashMap<String, oneshot::Sender<bool>>>> =
                Arc::new(Mutex::new(HashMap::new()));

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
                        let ctx = match session_mgr.start_loop(&session_id) {
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

                        // Append user message
                        let user_msg = Message::user(&text);
                        if let Err(e) = session_mgr.append_message(&session_id, user_msg.clone()) {
                            let _ = session_mgr.finish_loop(&session_id, SessionStatus::Error);
                            let _ = tx
                                .send(Ok(session_error_msg(
                                    "InternalError",
                                    &e.to_string(),
                                    &session_id,
                                )))
                                .await;
                            continue;
                        }

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

                        tokio::spawn(async move {
                            run_agent_loop(p, tr, re, sm, ctx, &ac, user_msg, agent_tx).await;
                        });

                        tokio::spawn(async move {
                            while let Some(event) = agent_rx.recv().await {
                                let msg = match event {
                                    AgentEvent::UserQuery {
                                        query_id,
                                        message,
                                        respond,
                                    } => {
                                        pq.lock().await.insert(query_id.clone(), respond);
                                        proto::ServerMessage {
                                            payload: Some(
                                                proto::server_message::Payload::UserQuery(
                                                    proto::UserQuery {
                                                        query_id,
                                                        message,
                                                        session_id: sid.clone(),
                                                    },
                                                ),
                                            ),
                                        }
                                    }
                                    _ => agent_event_to_server_message(event, &sid),
                                };
                                if tx_events.send(Ok(msg)).await.is_err() {
                                    break;
                                }
                            }
                        });
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
                        if let Some(model) = update.model {
                            config.model = model;
                        }
                        if let Some(temp) = update.temperature {
                            config.temperature = temp;
                        }
                        if let Some(max_tokens) = update.max_tokens {
                            config.max_tokens = max_tokens;
                        }
                        config.extra.extend(update.extra);

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
                        if let Some(sender) = sender {
                            let _ = sender.send(resp.approved);
                        }
                    }
                    None => {}
                }
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

        let args = serde_json::json!({ "path": path });
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
        let limit = if req.limit <= 0 { 20 } else { req.limit as usize };

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
        let mut details = cg
            .get_details(&symbol_name)
            .map_err(Status::internal)?;

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
        Ok(Response::new(()))
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn session_to_proto(session: &vbw_core::session::Session) -> proto::Session {
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
        created_at: Some(prost_types::Timestamp {
            seconds: created_secs,
            nanos: 0,
        }),
    }
}

fn map_to_llm_config(config: &HashMap<String, String>) -> LlmConfig {
    let mut llm = LlmConfig::default();
    for (key, value) in config {
        match key.as_str() {
            "model" => llm.model = value.clone(),
            "temperature" => {
                if let Ok(v) = value.parse::<f64>() {
                    llm.temperature = v;
                }
            }
            "max_tokens" => {
                if let Ok(v) = value.parse::<u32>() {
                    llm.max_tokens = v;
                }
            }
            _ => {
                llm.extra.insert(key.clone(), value.clone());
            }
        }
    }
    llm
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
            payload: Some(proto::server_message::Payload::TextDelta(proto::TextDelta {
                delta,
                session_id: sid,
            })),
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
            content,
            is_error,
        } => proto::ServerMessage {
            payload: Some(proto::server_message::Payload::ToolResult(proto::ToolResult {
                call_id,
                content,
                is_error,
                session_id: sid,
            })),
        },
        AgentEvent::StatusUpdate(message) => proto::ServerMessage {
            payload: Some(proto::server_message::Payload::StatusUpdate(proto::StatusUpdate {
                message,
                session_id: sid,
            })),
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
    use vbw_core::session::Session;

    #[test]
    fn test_map_to_llm_config_empty() {
        let config = HashMap::new();
        let llm = map_to_llm_config(&config);
        assert_eq!(llm.model, "claude-sonnet-4-20250514");
        assert!((llm.temperature - 0.7).abs() < f64::EPSILON);
        assert_eq!(llm.max_tokens, 4096);
    }

    #[test]
    fn test_map_to_llm_config_full() {
        let mut config = HashMap::new();
        config.insert("model".into(), "gpt-4".into());
        config.insert("temperature".into(), "0.5".into());
        config.insert("max_tokens".into(), "2048".into());
        config.insert("custom_key".into(), "custom_val".into());

        let llm = map_to_llm_config(&config);
        assert_eq!(llm.model, "gpt-4");
        assert!((llm.temperature - 0.5).abs() < f64::EPSILON);
        assert_eq!(llm.max_tokens, 2048);
        assert_eq!(llm.extra.get("custom_key").unwrap(), "custom_val");
    }

    #[test]
    fn test_map_to_llm_config_invalid_values() {
        let mut config = HashMap::new();
        config.insert("temperature".into(), "not-a-number".into());
        config.insert("max_tokens".into(), "not-a-number".into());

        let llm = map_to_llm_config(&config);
        assert!((llm.temperature - 0.7).abs() < f64::EPSILON);
        assert_eq!(llm.max_tokens, 4096);
    }

    #[test]
    fn test_session_to_proto_idle() {
        let session = Session {
            id: "test-1".into(),
            project_path: "/tmp".into(),
            status: SessionStatus::Idle,
            created_at: Instant::now(),
            history: vec![],
            config: LlmConfig::default(),
            system_prompt_template: "default".into(),
        };

        let proto = session_to_proto(&session);
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
                history: vec![],
                config: LlmConfig::default(),
                system_prompt_template: "".into(),
            }
        };

        assert_eq!(
            session_to_proto(&base(SessionStatus::Idle)).status,
            proto::SessionStatus::Idle as i32
        );
        assert_eq!(
            session_to_proto(&base(SessionStatus::Running)).status,
            proto::SessionStatus::Running as i32
        );
        assert_eq!(
            session_to_proto(&base(SessionStatus::Completed)).status,
            proto::SessionStatus::Completed as i32
        );
        assert_eq!(
            session_to_proto(&base(SessionStatus::Error)).status,
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
            code: vbw_core::error::AgentErrorCode::MaxIterations,
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
        let (tx, _rx) = oneshot::channel::<bool>();
        let event = AgentEvent::UserQuery {
            query_id: "q-1".into(),
            message: "confirm?".into(),
            respond: tx,
        };
        let msg = agent_event_to_server_message(event, "sess-1");
        assert!(msg.payload.is_none());
    }
}
