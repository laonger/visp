use std::collections::HashMap;
use std::path::Path;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::{Mutex, RwLock as StdRwLock};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use futures::StreamExt;
use tokio::sync::{RwLock, mpsc, oneshot};
use tonic::{Request, Response, Status, Streaming};

use visp_codegraph::CodeGraph;
use visp_core::{
    agent::{AgentConfig, AgentEvent, UserQueryResult},
    context::ContextTrimmer,
    message::{MessageType, Role},
    provider::{LlmConfig, LlmProvider},
    rules::RuleEngine,
    session::{SessionManager, SessionStatus},
    tool::ToolContext,
    tool_registry::ToolRegistry,
};
use visp_mcp::manager::McpManager;
use visp_proto::visp::{self as proto, coder_daemon_server::CoderDaemon};

use crate::config::LlmModelConfig;
use crate::config::LlmSection;

type ResponseStream =
    Pin<Box<dyn futures::Stream<Item = Result<proto::ServerMessage, tonic::Status>> + Send>>;
type CodeGraphMap = Arc<RwLock<HashMap<String, Arc<CodeGraph>>>>;

fn create_llm_provider(config: &LlmModelConfig) -> Result<Arc<dyn LlmProvider>, String> {
    match config.protocol.as_str() {
        "openai" => {
            let api_key = config
                .api_key
                .clone()
                .or_else(|| std::env::var("OPENAI_API_KEY").ok())
                .ok_or_else(|| {
                    "OPENAI_API_KEY not set (configure api_key or set env)".to_string()
                })?;
            if let Some(ref base_url) = config.base_url {
                Ok(Arc::new(visp_llm::openai::OpenAiProvider::with_base_url(
                    api_key,
                    base_url.clone(),
                )))
            } else {
                Ok(Arc::new(visp_llm::openai::OpenAiProvider::new(api_key)))
            }
        }
        _ => {
            let api_key = config
                .api_key
                .clone()
                .or_else(|| std::env::var("ANTHROPIC_API_KEY").ok())
                .ok_or_else(|| {
                    "ANTHROPIC_API_KEY not set (configure api_key or set env)".to_string()
                })?;
            if let Some(ref base_url) = config.base_url {
                Ok(Arc::new(
                    visp_llm::anthropic::AnthropicProvider::with_base_url(
                        api_key,
                        base_url.clone(),
                    ),
                ))
            } else {
                Ok(Arc::new(visp_llm::anthropic::AnthropicProvider::new(
                    api_key,
                )))
            }
        }
    }
}

pub struct CoderDaemonService {
    #[allow(dead_code)]
    provider: Arc<StdRwLock<Arc<dyn LlmProvider>>>,
    tool_registry: Arc<ToolRegistry>,
    #[allow(dead_code)]
    rule_engine: Arc<RuleEngine>,
    session_mgr: Arc<SessionManager>,
    #[allow(dead_code)]
    agent_config: AgentConfig,
    start_time: Instant,
    /// Phase 5: lazy-loaded CodeGraph instances per project path
    codegraphs: CodeGraphMap,
    /// 默认 LLM 配置（来自 daemon.toml），create_session 时与客户端配置合并
    default_llm_config: LlmConfig,
    /// 上下文裁剪器
    #[allow(dead_code)]
    context_trimmer: Arc<dyn ContextTrimmer + Send + Sync>,
    /// MCP 服务器管理器
    mcp_manager: Arc<McpManager>,
    /// 模型显示标签列表（格式 "{name}({provider})"，用于 proto Session.available_models）
    available_models: Vec<String>,
    /// 完整模型配置列表
    model_configs: Vec<LlmModelConfig>,
    /// 模型 key 列表（格式 "{provider}.{name}"，用于 proto Session.model_keys）
    model_config_keys: Vec<String>,
    // ── 多 Agent Orchestrator 通道 ──
    /// 向 Orchestrator 发送取消信号
    #[allow(dead_code)]
    cancel_tx: mpsc::Sender<visp_agent::orchestrator::CancelSignal>,
    /// 从 Orchestrator 接收 AgentEvent（转发给 CLI），用 Mutex<Option> 允许 take
    orchestrator_grpc_rx: std::sync::Mutex<Option<mpsc::Receiver<visp_core::agent::AgentEvent>>>,
    /// 向 Orchestrator 发送 ClientMessage（CLI 输入）
    client_tx: mpsc::Sender<visp_agent::orchestrator::ClientMessage>,
}

impl CoderDaemonService {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        model_configs: Vec<LlmModelConfig>,
        tool_registry: Arc<ToolRegistry>,
        #[allow(dead_code)] rule_engine: Arc<RuleEngine>,
        session_mgr: Arc<SessionManager>,
        #[allow(dead_code)] agent_config: AgentConfig,
        llm_section: LlmSection,
        #[allow(dead_code)] context_trimmer: Arc<dyn ContextTrimmer + Send + Sync>,
        mcp_manager: Arc<McpManager>,
        available_models: Vec<String>,
        cancel_tx: mpsc::Sender<visp_agent::orchestrator::CancelSignal>,
        orchestrator_grpc_rx: mpsc::Receiver<visp_core::agent::AgentEvent>,
        client_tx: mpsc::Sender<visp_agent::orchestrator::ClientMessage>,
    ) -> Self {
        let mut extra = std::collections::HashMap::new();
        if let Some(budget) = llm_section.thinking_budget_tokens {
            extra.insert("thinking_budget_tokens".into(), budget.to_string());
        }
        // 合并 [llm.extra] 中的自定义参数
        for (k, v) in llm_section.extra.iter() {
            extra.insert(k.clone(), v.clone());
        }
        // 查找默认模型
        let default_idx = llm_section
            .default
            .as_ref()
            .and_then(|key| model_configs.iter().position(|mc| mc.key() == *key))
            .unwrap_or(0);
        let default_cfg = &model_configs[default_idx];

        let initial_provider =
            create_llm_provider(default_cfg).expect("failed to create initial LLM provider");

        let default_llm_config = LlmConfig {
            model: default_cfg.model.clone(),
            temperature: default_cfg.temperature.unwrap_or(0.7),
            max_tokens: default_cfg.max_tokens.unwrap_or(4096),
            max_context_tokens: default_cfg.max_context_tokens.unwrap_or(128_000),
            extra,
        };
        let model_config_keys: Vec<String> = model_configs.iter().map(|mc| mc.key()).collect();
        Self {
            provider: Arc::new(StdRwLock::new(initial_provider)),
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
            model_configs,
            model_config_keys,
            cancel_tx,
            orchestrator_grpc_rx: std::sync::Mutex::new(Some(orchestrator_grpc_rx)),
            client_tx,
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
        // 解析模型名：支持 key 格式 ("Anthropic.Claude Sonnet") 和直接 API model key
        if let Some(mc) = self
            .model_configs
            .iter()
            .find(|mc| mc.key() == config.model)
        {
            config.model = mc.model.clone();
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
            &self.model_config_keys,
            &self.model_configs,
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
                .map(|s| {
                    session_to_proto(
                        s,
                        &self.available_models,
                        &self.model_config_keys,
                        &self.model_configs,
                    )
                })
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
                &self.model_config_keys,
                &self.model_configs,
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
                &self.model_config_keys,
                &self.model_configs,
            ))),
            _ => Err(Status::invalid_argument("Ambiguous session prefix")),
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

        // Take the orchestrator receiver (one per connection)
        let mut orchestrator_rx = self
            .orchestrator_grpc_rx
            .lock()
            .unwrap()
            .take()
            .ok_or_else(|| Status::internal("orchestrator receiver already taken"))?;

        // Clone channels for the two forwarding tasks
        let client_tx = self.client_tx.clone();
        let response_tx = tx.clone();
        let session_mgr = self.session_mgr.clone();

        // Shared pending user queries: maps query_id → respond sender
        // Used to route UserResponse from CLI back to the agent loop that's waiting
        // for it. This is necessary because the orchestrator cannot store the respond
        // sender (it's not clonable and the event bypasses global_tx).
        let pending_queries: Arc<Mutex<HashMap<String, oneshot::Sender<UserQueryResult>>>> =
            Arc::new(Mutex::new(HashMap::new()));

        // ── Inbound: CLI → Orchestrator / Pending Queries ──
        let pending_inbound = pending_queries.clone();
        let response_tx_inbound = response_tx.clone();
        tokio::spawn(async move {
            while let Some(msg_result) = in_stream.next().await {
                let msg = match msg_result {
                    Ok(m) => m,
                    Err(_) => break,
                };
                match msg.payload {
                    Some(proto::client_message::Payload::UserInput(input)) => {
                        let cli_msg = visp_agent::orchestrator::ClientMessage::UserInput {
                            session_id: input.session_id,
                            text: input.text,
                        };
                        if client_tx.send(cli_msg).await.is_err() {
                            break;
                        }
                    }
                    Some(proto::client_message::Payload::UserResponse(resp)) => {
                        let query_id = resp.query_id;
                        let text = resp.text;
                        let selected_index = resp.selected_index;

                        // Try daemon-level pending queries first (direct route to agent loop)
                        let responded = {
                            let mut map = pending_inbound.lock().unwrap();
                            if let Some(respond) = map.remove(&query_id) {
                                let _ = respond.send(UserQueryResult {
                                    selected_index,
                                    text: text.clone(),
                                });
                                true
                            } else {
                                false
                            }
                        };
                        if !responded {
                            // Fall back to orchestrator
                            let cli_msg =
                                visp_agent::orchestrator::ClientMessage::UserQueryResponse {
                                    query_id,
                                    selected_index,
                                    text,
                                };
                            if client_tx.send(cli_msg).await.is_err() {
                                break;
                            }
                        }
                    }
                    Some(proto::client_message::Payload::Cancel(cancel)) => {
                        let cli_msg = visp_agent::orchestrator::ClientMessage::Cancel {
                            session_id: cancel.session_id,
                        };
                        if client_tx.send(cli_msg).await.is_err() {
                            break;
                        }
                    }
                    Some(proto::client_message::Payload::JoinSession(join)) => {
                        let session_id = join.session_id;
                        let user_inputs: Vec<String> = match session_mgr.get(&session_id) {
                            Ok(session) => session
                                .history
                                .iter()
                                .filter(|m| m.role == Role::User)
                                .map(|m| m.content.clone())
                                .collect(),
                            Err(_) => vec![],
                        };

                        // ── Step 1: Send StatusUpdate with user inputs ──
                        {
                            let msg = proto::ServerMessage {
                                payload: Some(proto::server_message::Payload::StatusUpdate(
                                    proto::StatusUpdate {
                                        session_id: session_id.clone(),
                                        message: format!(
                                            "Joined session {}",
                                            &session_id[..session_id.len().min(8)]
                                        ),
                                        user_inputs,
                                    },
                                )),
                            };
                            let _ = response_tx_inbound.send(Ok(msg)).await;
                        }

                        // ── Step 2: Replay full conversation history ──
                        if let Ok(session) = session_mgr.get(&session_id) {
                            for msg in &session.history {
                                match msg.role {
                                    Role::Assistant => {
                                        // Send text content
                                        if !msg.content.is_empty() {
                                            let td_msg = proto::ServerMessage {
                                                payload: Some(
                                                    proto::server_message::Payload::TextDelta(
                                                        proto::TextDelta {
                                                            delta: msg.content.clone(),
                                                            session_id: session_id.clone(),
                                                        },
                                                    ),
                                                ),
                                            };
                                            if response_tx_inbound
                                                .send(Ok(td_msg))
                                                .await
                                                .is_err()
                                            {
                                                break;
                                            }
                                        }
                                        // Send tool calls
                                        if let Some(tool_calls) = &msg.tool_calls {
                                            for tc in tool_calls {
                                                let tc_msg = proto::ServerMessage {
                                                    payload: Some(
                                                        proto::server_message::Payload::ToolCall(
                                                            proto::ToolCall {
                                                                call_id: tc.id.clone(),
                                                                tool_name: tc.name.clone(),
                                                                arguments: tc.arguments.clone(),
                                                                session_id: session_id.clone(),
                                                            },
                                                        ),
                                                    ),
                                                };
                                                if response_tx_inbound
                                                    .send(Ok(tc_msg))
                                                    .await
                                                    .is_err()
                                                {
                                                    break;
                                                }
                                            }
                                        }
                                    }
                                    Role::Tool => {
                                        let tr_msg = proto::ServerMessage {
                                            payload: Some(
                                                proto::server_message::Payload::ToolResult(
                                                    proto::ToolResult {
                                                        call_id: msg
                                                            .tool_call_id
                                                            .clone()
                                                            .unwrap_or_default(),
                                                        tool_name: String::new(),
                                                        content: msg.content.clone(),
                                                        is_error: msg.kind == MessageType::Error,
                                                        session_id: session_id.clone(),
                                                    },
                                                ),
                                            ),
                                        };
                                        if response_tx_inbound
                                            .send(Ok(tr_msg))
                                            .await
                                            .is_err()
                                        {
                                            break;
                                        }
                                    }
                                    _ => {}
                                }
                            }

                            // Send Done to flush streaming text
                            let done_msg = proto::ServerMessage {
                                payload: Some(proto::server_message::Payload::Done(
                                    proto::Done {
                                        session_id: session_id.clone(),
                                    },
                                )),
                            };
                            let _ = response_tx_inbound.send(Ok(done_msg)).await;
                        }
                    }
                    _ => {}
                }
            }
        });

        // ── Outbound: Orchestrator → CLI ──
        let pending_outbound = pending_queries.clone();
        tokio::spawn(async move {
            while let Some(event) = orchestrator_rx.recv().await {
                match event {
                    AgentEvent::UserQuery {
                        query_id,
                        message,
                        options,
                        allow_other,
                        respond,
                    } => {
                        // Store the respond sender so the inbound task can route
                        // UserResponse back directly to the waiting agent loop.
                        pending_outbound
                            .lock()
                            .unwrap()
                            .insert(query_id.clone(), respond);
                        let proto_msg = proto::ServerMessage {
                            payload: Some(proto::server_message::Payload::UserQuery(
                                proto::UserQuery {
                                    query_id,
                                    message,
                                    options,
                                    allow_other,
                                    session_id: String::new(),
                                },
                            )),
                        };
                        if response_tx.send(Ok(proto_msg)).await.is_err() {
                            break;
                        }
                    }
                    _ => {
                        let proto_msg = agent_event_to_server_message(event, "");
                        if let Some(payload) = proto_msg.payload
                            && response_tx
                                .send(Ok(proto::ServerMessage {
                                    payload: Some(payload),
                                }))
                                .await
                                .is_err()
                        {
                            break;
                        }
                    }
                }
            }
        });

        let stream = tokio_stream::wrappers::ReceiverStream::new(rx);
        Ok(Response::new(Box::pin(stream) as Self::ChatStream))
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
            permission_rules: None,
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
    model_keys: &[String],
    model_configs: &[LlmModelConfig],
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

    // proto model_key 字段用于 CLI 状态栏显示，用 key 格式 "{provider}.{name}"
    let display_model_key = model_configs
        .iter()
        .find(|mc| mc.model == session.config.model)
        .map(|mc| mc.key())
        .unwrap_or_else(|| session.config.model.clone());

    // proto model 字段用于 CLI 状态栏显示，使用 key 格式 "{provider}.{model_name}"
    // 以便 CLI 端 split_model_name 能正确拆出 provider 和模型名
    let display_model = model_configs
        .iter()
        .find(|mc| mc.model == session.config.model)
        .map(|mc| mc.key())
        .unwrap_or_else(|| session.config.model.clone());

    proto::Session {
        session_id: session.id.clone(),
        status: status.into(),
        project_path: session.project_path.to_string_lossy().to_string(),
        model: display_model,
        last_user_message: session.last_user_message.clone().unwrap_or_default(),
        created_at: Some(prost_types::Timestamp {
            seconds: created_secs,
            nanos: 0,
        }),
        available_models: available_models.to_vec(),
        model_keys: model_keys.to_vec(),
        model_key: display_model_key,
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

#[cfg_attr(not(test), allow(dead_code))]
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
        AgentEvent::UserQuery {
            query_id,
            message,
            options,
            allow_other,
            respond: _,
        } => proto::ServerMessage {
            payload: Some(proto::server_message::Payload::UserQuery(
                proto::UserQuery {
                    query_id: query_id.clone(),
                    message: message.clone(),
                    options: options.clone(),
                    allow_other,
                    session_id: sid,
                },
            )),
        },
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
        let (cancel_tx, _cancel_rx) = mpsc::channel(16);
        let (_grpc_tx, orchestrator_grpc_rx) = mpsc::channel(256);
        let (client_tx, _client_rx) = mpsc::channel(64);
        CoderDaemonService {
            provider: Arc::new(StdRwLock::new(
                Arc::new(MockProvider::new(vec![])) as Arc<dyn LlmProvider>
            )),
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
            model_configs: vec![],
            model_config_keys: vec![],
            cancel_tx,
            orchestrator_grpc_rx: std::sync::Mutex::new(Some(orchestrator_grpc_rx)),
            client_tx,
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
                agent_name: "default".into(),
                parent_id: None,
                permission: vec![],
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
                agent_name: "default".into(),
                parent_id: None,
                permission: vec![],
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
                agent_name: "default".into(),
                parent_id: None,
                permission: vec![],
            })
            .unwrap();
        let mgr = StdArc::new(SessionManager::new(store));
        let service = make_service(mgr);

        let request = tonic::Request::new(proto::GetSessionRequest {
            session_id: "common".into(),
        });
        let err = service.get_session(request).await.unwrap_err();
        assert_eq!(err.code(), tonic::Code::InvalidArgument);
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
            model_key: None,
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
            model_key: None,
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
            model_key: None,
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
        let (cancel_tx, _cancel_rx) = mpsc::channel(16);
        let (_grpc_tx, orchestrator_grpc_rx) = mpsc::channel(256);
        let (client_tx, _client_rx) = mpsc::channel(64);
        let service = CoderDaemonService {
            provider: Arc::new(StdRwLock::new(
                Arc::new(MockProvider::new(vec![])) as Arc<dyn LlmProvider>
            )),
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
            model_configs: vec![],
            model_config_keys: vec![],
            cancel_tx,
            orchestrator_grpc_rx: std::sync::Mutex::new(Some(orchestrator_grpc_rx)),
            client_tx,
        };

        let request = tonic::Request::new(proto::CreateSessionRequest {
            project_path: "/tmp".into(),
            config: Some(proto::LlmConfig {
                model: None,
                model_key: None,
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
        let (cancel_tx, _cancel_rx) = mpsc::channel(16);
        let (_grpc_tx, orchestrator_grpc_rx) = mpsc::channel(256);
        let (client_tx, _client_rx) = mpsc::channel(64);
        let service = CoderDaemonService {
            provider: Arc::new(StdRwLock::new(
                Arc::new(MockProvider::new(vec![])) as Arc<dyn LlmProvider>
            )),
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
            model_configs: vec![],
            model_config_keys: vec![],
            cancel_tx,
            orchestrator_grpc_rx: std::sync::Mutex::new(Some(orchestrator_grpc_rx)),
            client_tx,
        };

        let request = tonic::Request::new(proto::CreateSessionRequest {
            project_path: "/tmp".into(),
            config: Some(proto::LlmConfig {
                model: None,
                model_key: None,
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
            agent_name: "default".into(),
            parent_id: None,
            permission: vec![],
        };

        let proto = session_to_proto(&session, &[], &[], &[]);
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
                agent_name: "default".into(),
                parent_id: None,
                permission: vec![],
            }
        };

        assert_eq!(
            session_to_proto(&base(SessionStatus::Idle), &[], &[], &[]).status,
            proto::SessionStatus::Idle as i32
        );
        assert_eq!(
            session_to_proto(&base(SessionStatus::Running), &[], &[], &[]).status,
            proto::SessionStatus::Running as i32
        );
        assert_eq!(
            session_to_proto(&base(SessionStatus::Completed), &[], &[], &[]).status,
            proto::SessionStatus::Completed as i32
        );
        assert_eq!(
            session_to_proto(&base(SessionStatus::Error), &[], &[], &[]).status,
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
    fn test_agent_event_to_server_message_user_query() {
        let (tx, _rx) = tokio::sync::oneshot::channel();
        let event = AgentEvent::UserQuery {
            query_id: "q-1".into(),
            message: "confirm?".into(),
            options: vec!["yes".into(), "no".into()],
            allow_other: true,
            respond: tx,
        };
        let msg = agent_event_to_server_message(event, "sess-1");
        match msg.payload {
            Some(proto::server_message::Payload::UserQuery(query)) => {
                assert_eq!(query.query_id, "q-1");
                assert_eq!(query.message, "confirm?");
                assert_eq!(query.options, vec!["yes", "no"]);
                assert!(query.allow_other);
                assert_eq!(query.session_id, "sess-1");
            }
            _ => panic!("expected UserQuery payload"),
        }
    }
}
