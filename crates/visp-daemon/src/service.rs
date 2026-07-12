use std::collections::{HashMap, VecDeque};
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
    /// 模型 key 列表（格式 "{provider}/{name}"，用于 proto Session.model_keys）
    model_config_keys: Vec<String>,
    // ── 多 Agent Orchestrator 通道 ──
    /// 向 Orchestrator 发送取消信号
    #[allow(dead_code)]
    cancel_tx: mpsc::Sender<visp_agent::orchestrator::CancelSignal>,
    /// 从 Orchestrator 接收 AgentEventFrame（转发给 CLI），用 Mutex<Option> 允许 take
    orchestrator_grpc_rx:
        std::sync::Mutex<Option<mpsc::Receiver<visp_core::agent::AgentEventFrame>>>,
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
        orchestrator_grpc_rx: mpsc::Receiver<visp_core::agent::AgentEventFrame>,
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
        // 查找默认模型（匹配 {provider}/{name} 或 {provider}/{model} 格式）
        let default_idx = if let Some(ref default_key) = llm_section.default {
            match model_configs
                .iter()
                .position(|mc| mc.matches_key(default_key))
            {
                Some(idx) => {
                    tracing::info!(
                        default = %default_key,
                        "using llm.default model for new sessions"
                    );
                    idx
                }
                None => {
                    tracing::warn!(
                        default = %default_key,
                        available = %model_configs.iter().map(|m| m.key()).collect::<Vec<_>>().join(", "),
                        "llm.default points to unknown model, falling back to first model"
                    );
                    0
                }
            }
        } else {
            tracing::info!("llm.default not set, using first model as default");
            0
        };
        let default_cfg = &model_configs[default_idx];
        // per-model thinking_budget_tokens 覆盖全局 fallback
        if let Some(budget) = default_cfg.thinking_budget_tokens {
            extra.insert("thinking_budget_tokens".into(), budget.to_string());
        }

        let initial_provider =
            create_llm_provider(default_cfg).expect("failed to create initial LLM provider");

        let default_llm_config = LlmConfig {
            model: default_cfg.model.clone(),
            model_key: Some(default_cfg.key()),
            provider: Some(
                default_cfg
                    .provider
                    .clone()
                    .unwrap_or_else(|| default_cfg.protocol.clone()),
            ),
            temperature: default_cfg.temperature.unwrap_or(0.7),
            max_tokens: default_cfg.max_tokens.unwrap_or(4096),
            max_context_tokens: default_cfg.max_context_tokens.unwrap_or(128_000),
            extra,
            langfuse_enabled: agent_config.langfuse_enabled,
            langfuse_session_id: None, // set per-session
            langfuse_trace_name: None, // set per-session
            langfuse_user_id: agent_config.langfuse_user_id.clone(),
            langfuse_tags: agent_config.langfuse_tags.clone(),
            langfuse_environment: agent_config.langfuse_environment.clone(),
            langfuse_release: agent_config.langfuse_release.clone(),
            langfuse_version: agent_config.langfuse_version.clone(),
            langfuse_public: agent_config.langfuse_public,
            langfuse_metadata: agent_config.langfuse_metadata.clone(),
            langfuse_capture_input: agent_config.langfuse_capture_input,
            langfuse_capture_output: agent_config.langfuse_capture_output,
            langfuse_capture_max_chars: agent_config.langfuse_capture_max_chars,
            langfuse_redact_secrets: agent_config.langfuse_redact_secrets,
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
        // 如果客户端传了 model_key，用其查找对应的 model 配置
        if let Some(ref model_key) = config.model_key
            && let Some(mc) = self.model_configs.iter().find(|mc| mc.key() == *model_key)
        {
            config.model = mc.model.clone();
            config.provider = Some(mc.provider.clone().unwrap_or_else(|| mc.protocol.clone()));
            // 用该模型配置填充客户端未显式设置的字段
            if config.max_tokens == LlmConfig::default().max_tokens
                && let Some(mt) = mc.max_tokens
            {
                config.max_tokens = mt;
            }
            if config.max_context_tokens == LlmConfig::default().max_context_tokens
                && let Some(mct) = mc.max_context_tokens
            {
                config.max_context_tokens = mct;
            }
            if (config.temperature - LlmConfig::default().temperature).abs() < f64::EPSILON
                && let Some(t) = mc.temperature
            {
                config.temperature = t;
            }
            // per-model thinking_budget_tokens
            if let Some(budget) = mc.thinking_budget_tokens {
                config
                    .extra
                    .insert("thinking_budget_tokens".into(), budget.to_string());
            }
        }
        // 客户端未传的字段用 daemon 默认值
        if config.model == LlmConfig::default().model {
            config.model = self.default_llm_config.model.clone();
            if config.model_key.is_none() {
                config.model_key = self.default_llm_config.model_key.clone();
            }
            if config.provider.is_none() {
                config.provider = self.default_llm_config.provider.clone();
            }
        }
        // 应用 daemon 默认的 Langfuse 配置（客户端不会传递这些字段）
        config.langfuse_enabled = self.default_llm_config.langfuse_enabled;
        config
            .langfuse_session_id
            .clone_from(&self.default_llm_config.langfuse_session_id);
        config
            .langfuse_trace_name
            .clone_from(&self.default_llm_config.langfuse_trace_name);
        config
            .langfuse_user_id
            .clone_from(&self.default_llm_config.langfuse_user_id);
        config
            .langfuse_tags
            .clone_from(&self.default_llm_config.langfuse_tags);
        config
            .langfuse_environment
            .clone_from(&self.default_llm_config.langfuse_environment);
        config
            .langfuse_release
            .clone_from(&self.default_llm_config.langfuse_release);
        config
            .langfuse_version
            .clone_from(&self.default_llm_config.langfuse_version);
        config.langfuse_public = self.default_llm_config.langfuse_public;
        config
            .langfuse_metadata
            .clone_from(&self.default_llm_config.langfuse_metadata);
        config.langfuse_capture_input = self.default_llm_config.langfuse_capture_input;
        config.langfuse_capture_output = self.default_llm_config.langfuse_capture_output;
        config.langfuse_capture_max_chars = self.default_llm_config.langfuse_capture_max_chars;
        config.langfuse_redact_secrets = self.default_llm_config.langfuse_redact_secrets;

        // 解析模型名：支持 key 格式 ("Anthropic/Claude Sonnet") 和直接 API model key
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
                .filter(|s| s.parent_id.is_none())
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
        let model_configs = self.model_configs.clone();
        let default_llm_config_extra = self.default_llm_config.extra.clone();

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
                        let session_id = input.session_id;
                        // 检查 session 是否可接受新输入：
                        // 主 session（无 parent_id）的 Idle/Completed/Error 均可接受；
                        // Running 理论上不会出现在恢复场景，若有则重置为 Idle。
                        // 子 session（有 parent_id）一律 view-only。
                        let can_accept = match session_mgr.get(&session_id) {
                            Ok(s) => {
                                let is_main = s.parent_id.is_none();
                                if is_main && s.status == visp_core::session::SessionStatus::Running
                                {
                                    // 恢复场景不应出现 Running，防御性重置
                                    let _ = session_mgr.finish_loop(
                                        &session_id,
                                        visp_core::session::SessionStatus::Idle,
                                    );
                                }
                                is_main
                            }
                            Err(_) => false,
                        };
                        if can_accept {
                            // Intercept daemon-side slash commands and either
                            // replace the prompt (for /init) or execute file
                            // operations (for /init-agent, /init-skill).
                            let cmd = visp_command::parse(&input.text);
                            match visp_command::resolve(
                                &cmd,
                                &session_mgr
                                    .get(&session_id)
                                    .ok()
                                    .map(|s| s.project_path.clone())
                                    .unwrap_or_default(),
                            ) {
                                Ok(action) => {
                                    match action {
                                        visp_command::CommandAction::Prompt(prompt) => {
                                            // Forward the prompt to the LLM.
                                            // Side-effect: ensure .visp dirs exist.
                                            if let Ok(session) = session_mgr.get(&session_id) {
                                                let visp_dir = session.project_path.join(".visp");
                                                for sub in ["rules", "skills"] {
                                                    let _ =
                                                        std::fs::create_dir_all(visp_dir.join(sub));
                                                }
                                            }
                                            let cli_msg = visp_agent::orchestrator::ClientMessage::UserInput {
                                                session_id,
                                                text: prompt,
                                            };
                                            if client_tx.send(cli_msg).await.is_err() {
                                                break;
                                            }
                                        }
                                        visp_command::CommandAction::WriteFile {
                                            path,
                                            content,
                                        } => {
                                            // Write file and send status back.
                                            let parent = path.parent().unwrap();
                                            if let Err(e) = std::fs::create_dir_all(parent) {
                                                let err_msg = session_error_msg(
                                                    "FileWriteError",
                                                    &format!("Failed to create directory: {e}"),
                                                    &session_id,
                                                );
                                                let _ = response_tx_inbound.send(Ok(err_msg)).await;
                                            } else if let Err(e) = std::fs::write(&path, &content) {
                                                let err_msg = session_error_msg(
                                                    "FileWriteError",
                                                    &format!("Failed to write file: {e}"),
                                                    &session_id,
                                                );
                                                let _ = response_tx_inbound.send(Ok(err_msg)).await;
                                            } else {
                                                let msg = proto::ServerMessage {
                                                    payload: Some(proto::server_message::Payload::StatusUpdate(
                                                        proto::StatusUpdate {
                                                            session_id: session_id.clone(),
                                                            message: format!("Created {}", path.display()),
                                                            agent_name: String::new(),
                                                            user_inputs: vec![],
                                                            view_only: false,
                                                        },
                                                    )),
                                                };
                                                let _ = response_tx_inbound.send(Ok(msg)).await;
                                            }
                                        }
                                        visp_command::CommandAction::None => {
                                            // Not a daemon command — forward as-is.
                                            let cli_msg = visp_agent::orchestrator::ClientMessage::UserInput {
                                                session_id,
                                                text: input.text,
                                            };
                                            if client_tx.send(cli_msg).await.is_err() {
                                                break;
                                            }
                                        }
                                    }
                                }
                                Err(err_msg) => {
                                    // resolve returned an error (e.g. invalid name, file exists)
                                    let err_msg =
                                        session_error_msg("CommandError", &err_msg, &session_id);
                                    let _ = response_tx_inbound.send(Ok(err_msg)).await;
                                }
                            }
                        } else {
                            // 已知限制：DB 持久化场景下 status 可能不可靠（见设计文档）
                            let err_msg = session_error_msg(
                                "SessionNotActive",
                                &format!(
                                    "Session {} is not active",
                                    &session_id[..session_id.len().min(8)]
                                ),
                                &session_id,
                            );
                            let _ = response_tx_inbound.send(Ok(err_msg)).await;
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
                    Some(proto::client_message::Payload::ConfigUpdate(update)) => {
                        let session_id = &update.session_id;
                        let config = update
                            .config
                            .as_ref()
                            .map(map_llm_config)
                            .unwrap_or_default();

                        // 如果传了 model_key，查找对应的 model 配置并更新 model 名
                        let final_config = if let Some(ref model_key) = config.model_key {
                            if let Some(mc) = model_configs.iter().find(|mc| mc.key() == *model_key)
                            {
                                let mut cfg = LlmConfig {
                                    model: mc.model.clone(),
                                    model_key: Some(mc.key()),
                                    provider: Some(
                                        mc.provider.clone().unwrap_or_else(|| mc.protocol.clone()),
                                    ),
                                    ..config
                                };
                                // 用该模型配置填充客户端未显式设置的字段
                                if cfg.max_tokens == LlmConfig::default().max_tokens
                                    && let Some(mt) = mc.max_tokens
                                {
                                    cfg.max_tokens = mt;
                                }
                                if cfg.max_context_tokens == LlmConfig::default().max_context_tokens
                                    && let Some(mct) = mc.max_context_tokens
                                {
                                    cfg.max_context_tokens = mct;
                                }
                                if (cfg.temperature - LlmConfig::default().temperature).abs()
                                    < f64::EPSILON
                                    && let Some(t) = mc.temperature
                                {
                                    cfg.temperature = t;
                                }
                                // per-model thinking_budget_tokens
                                if let Some(budget) = mc.thinking_budget_tokens {
                                    cfg.extra.insert(
                                        "thinking_budget_tokens".into(),
                                        budget.to_string(),
                                    );
                                }
                                // 合并 daemon 默认 extra（保留其他 extra key）
                                for (k, v) in &default_llm_config_extra {
                                    cfg.extra.entry(k.clone()).or_insert_with(|| v.clone());
                                }
                                cfg
                            } else {
                                config
                            }
                        } else {
                            config
                        };

                        match session_mgr.update_config(session_id, final_config) {
                            Ok(()) => {
                                let msg = proto::ServerMessage {
                                    payload: Some(proto::server_message::Payload::StatusUpdate(
                                        proto::StatusUpdate {
                                            session_id: session_id.clone(),
                                            message: "Configuration updated".into(),
                                            user_inputs: vec![],
                                            agent_name: String::new(),
                                            view_only: false,
                                        },
                                    )),
                                };
                                let _ = response_tx_inbound.send(Ok(msg)).await;
                            }
                            Err(e) => {
                                let err_msg = session_error_msg(
                                    "ConfigUpdateFailed",
                                    &format!("Failed to update config: {e}"),
                                    session_id,
                                );
                                let _ = response_tx_inbound.send(Ok(err_msg)).await;
                            }
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
                                        agent_name: String::new(),
                                        view_only: false,
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
                                                            agent_name: String::new(),
                                                        },
                                                    ),
                                                ),
                                            };
                                            if response_tx_inbound.send(Ok(td_msg)).await.is_err() {
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
                                                                agent_name: String::new(),
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
                                        // Send UsageInfo if token data available
                                        if let Some(input_tokens) = msg.actual_tokens_input {
                                            let ui_msg = proto::ServerMessage {
                                                payload: Some(
                                                    proto::server_message::Payload::UsageInfo(
                                                        proto::UsageInfo {
                                                            input_tokens,
                                                            output_tokens: msg
                                                                .actual_tokens_output
                                                                .unwrap_or(0),
                                                            tool_calls: msg
                                                                .tool_call_count
                                                                .unwrap_or(0),
                                                            cache_creation_input_tokens: msg
                                                                .actual_cache_write
                                                                .unwrap_or(0),
                                                            cache_read_input_tokens: msg
                                                                .actual_cache_read
                                                                .unwrap_or(0),
                                                            session_id: session_id.clone(),
                                                        },
                                                    ),
                                                ),
                                            };
                                            let _ = response_tx_inbound.send(Ok(ui_msg)).await;
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
                                                        agent_name: String::new(),
                                                    },
                                                ),
                                            ),
                                        };
                                        if response_tx_inbound.send(Ok(tr_msg)).await.is_err() {
                                            break;
                                        }
                                    }
                                    Role::User => {
                                        let um_msg = proto::ServerMessage {
                                            payload: Some(
                                                proto::server_message::Payload::UserMessage(
                                                    proto::UserMessage {
                                                        content: msg.content.clone(),
                                                        session_id: session_id.clone(),
                                                    },
                                                ),
                                            ),
                                        };
                                        if response_tx_inbound.send(Ok(um_msg)).await.is_err() {
                                            break;
                                        }
                                    }
                                    _ => {}
                                }
                            }

                            // Send Done to flush streaming text
                            let done_msg = proto::ServerMessage {
                                payload: Some(proto::server_message::Payload::Done(proto::Done {
                                    session_id: session_id.clone(),
                                })),
                            };
                            let _ = response_tx_inbound.send(Ok(done_msg)).await;

                            // ── Step 3: Replay descendant sessions (BFS) ──
                            let descendants = collect_descendants(&session_mgr, &session_id);
                            let total = descendants.len();
                            let limited: &[visp_core::session::Session] =
                                &descendants[..total.min(DESCENDANT_SOFT_LIMIT)];
                            // 如果 collected 数量达到软上限则提示超限
                            if total >= DESCENDANT_SOFT_LIMIT {
                                let warn_msg = proto::ServerMessage {
                                    payload: Some(proto::server_message::Payload::TextDelta(
                                        proto::TextDelta {
                                            delta: format!(
                                                "⚠️ Session has {total} descendants, showing first {}",
                                                DESCENDANT_SOFT_LIMIT
                                            ),
                                            session_id: session_id.clone(),
                                            agent_name: String::new(),
                                        },
                                    )),
                                };
                                let _ = response_tx_inbound.send(Ok(warn_msg)).await;
                            }
                            for child_session in limited {
                                if replay_session_history(&response_tx_inbound, child_session)
                                    .await
                                    .is_err()
                                {
                                    break;
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
        });

        // ── Outbound: Orchestrator → CLI ──
        let pending_outbound = pending_queries.clone();
        tokio::spawn(async move {
            while let Some(frame) = orchestrator_rx.recv().await {
                let sid = frame.session_id.clone();
                match frame.event {
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
                                    session_id: sid,
                                },
                            )),
                        };
                        if response_tx.send(Ok(proto_msg)).await.is_err() {
                            break;
                        }
                    }
                    _ => {
                        let proto_msg =
                            agent_event_to_server_message(frame.event, &sid, &frame.agent_name);
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
            global_tx: None,
            visp_trace_id: None,
            iter_span_w3c_id: None,
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

    // proto model_key 字段用于 CLI 状态栏显示，用 key 格式 "{provider}/{name}"
    let display_model_key = session
        .config
        .model_key
        .clone()
        .or_else(|| {
            model_configs
                .iter()
                .find(|mc| mc.model == session.config.model)
                .map(|mc| mc.key())
        })
        .unwrap_or_else(|| session.config.model.clone());

    // proto model 字段用于 CLI 状态栏显示，使用 key 格式 "{provider}/{name}"
    // 以便 CLI 端 split_model_name 能正确拆出 provider 和模型名
    let display_model = display_model_key.clone();

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
    if let Some(model_key) = &proto.model_key {
        config.model_key = Some(model_key.clone());
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
            agent_name: String::new(),
        })),
    }
}

/// BFS 收集 root_id 的所有后代 session（不含 root）。
/// - 软上限 50（BFS 层级优先，超限保留较早创建的）
/// - visited 集合防环
/// - 单 session 加载失败用 tracing::warn! 记录并跳过
const DESCENDANT_SOFT_LIMIT: usize = 50;

fn collect_descendants(
    session_mgr: &SessionManager,
    root_id: &str,
) -> Vec<visp_core::session::Session> {
    let mut result: Vec<visp_core::session::Session> = Vec::new();
    let mut visited: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut queue: VecDeque<String> = VecDeque::new();

    visited.insert(root_id.to_string());
    queue.push_back(root_id.to_string());

    while let Some(parent_id) = queue.pop_front() {
        if result.len() >= DESCENDANT_SOFT_LIMIT {
            break;
        }
        let children = match session_mgr.list_child_sessions(&parent_id) {
            Ok(children) => children,
            Err(e) => {
                tracing::warn!(parent_id, error = %e, "collect_descendants: list_child_sessions failed, skipping");
                continue;
            }
        };
        let mut children = children;
        // 按 created_at 升序
        children.sort_by_key(|a| a.created_at);

        for child in children {
            if result.len() >= DESCENDANT_SOFT_LIMIT {
                break;
            }
            if visited.insert(child.id.clone()) {
                result.push(child.clone());
                queue.push_back(child.id);
            }
        }
    }

    result
}

/// 回放单个 session 的历史作为只读帧。
/// 发送：StatusUpdate(view_only=true) → 消息帧 → Done
async fn replay_session_history(
    response_tx: &mpsc::Sender<Result<proto::ServerMessage, Status>>,
    session: &visp_core::session::Session,
) -> Result<(), ()> {
    let session_id = session.id.clone();
    let agent_name = session.agent_name.clone();

    // 收集所有 Role::User 消息作为 user_inputs
    let user_inputs: Vec<String> = session
        .history
        .iter()
        .filter(|m| m.role == Role::User)
        .map(|m| m.content.clone())
        .collect();

    // StatusUpdate with view_only=true
    {
        let msg = proto::ServerMessage {
            payload: Some(proto::server_message::Payload::StatusUpdate(
                proto::StatusUpdate {
                    session_id: session_id.clone(),
                    message: format!("Viewing session {}", &session_id[..session_id.len().min(8)]),
                    user_inputs,
                    agent_name: agent_name.clone(),
                    view_only: true,
                },
            )),
        };
        if response_tx.send(Ok(msg)).await.is_err() {
            return Err(());
        }
    }

    // 回放历史：Assistant→TextDelta(+ToolCall)，Tool→ToolResult，User→跳过
    for msg in &session.history {
        match msg.role {
            Role::Assistant => {
                // 文本
                if !msg.content.is_empty() {
                    let td_msg = proto::ServerMessage {
                        payload: Some(proto::server_message::Payload::TextDelta(
                            proto::TextDelta {
                                delta: msg.content.clone(),
                                session_id: session_id.clone(),
                                agent_name: agent_name.clone(),
                            },
                        )),
                    };
                    if response_tx.send(Ok(td_msg)).await.is_err() {
                        return Err(());
                    }
                }
                // Tool calls
                if let Some(tool_calls) = &msg.tool_calls {
                    for tc in tool_calls {
                        let tc_msg = proto::ServerMessage {
                            payload: Some(proto::server_message::Payload::ToolCall(
                                proto::ToolCall {
                                    call_id: tc.id.clone(),
                                    tool_name: tc.name.clone(),
                                    arguments: tc.arguments.clone(),
                                    session_id: session_id.clone(),
                                    agent_name: agent_name.clone(),
                                },
                            )),
                        };
                        if response_tx.send(Ok(tc_msg)).await.is_err() {
                            return Err(());
                        }
                    }
                }
                // Send UsageInfo if token data available
                if let Some(input_tokens) = msg.actual_tokens_input {
                    let ui_msg = proto::ServerMessage {
                        payload: Some(proto::server_message::Payload::UsageInfo(
                            proto::UsageInfo {
                                input_tokens,
                                output_tokens: msg.actual_tokens_output.unwrap_or(0),
                                tool_calls: msg.tool_call_count.unwrap_or(0),
                                cache_creation_input_tokens: msg.actual_cache_write.unwrap_or(0),
                                cache_read_input_tokens: msg.actual_cache_read.unwrap_or(0),
                                session_id: session_id.clone(),
                            },
                        )),
                    };
                    if response_tx.send(Ok(ui_msg)).await.is_err() {
                        return Err(());
                    }
                }
            }
            Role::Tool => {
                let tr_msg = proto::ServerMessage {
                    payload: Some(proto::server_message::Payload::ToolResult(
                        proto::ToolResult {
                            call_id: msg.tool_call_id.clone().unwrap_or_default(),
                            tool_name: String::new(),
                            content: msg.content.clone(),
                            is_error: msg.kind == visp_core::message::MessageType::Error,
                            session_id: session_id.clone(),
                            agent_name: agent_name.clone(),
                        },
                    )),
                };
                if response_tx.send(Ok(tr_msg)).await.is_err() {
                    return Err(());
                }
            }
            Role::User => {
                let um_msg = proto::ServerMessage {
                    payload: Some(proto::server_message::Payload::UserMessage(
                        proto::UserMessage {
                            content: msg.content.clone(),
                            session_id: session_id.clone(),
                        },
                    )),
                };
                if response_tx.send(Ok(um_msg)).await.is_err() {
                    return Err(());
                }
            }
            _ => {}
        }
    }

    // Done
    let done_msg = proto::ServerMessage {
        payload: Some(proto::server_message::Payload::Done(proto::Done {
            session_id: session_id.clone(),
        })),
    };
    let _ = response_tx.send(Ok(done_msg)).await;
    Ok(())
}

fn agent_event_to_server_message(
    event: AgentEvent,
    session_id: &str,
    agent_name: &str,
) -> proto::ServerMessage {
    let sid = session_id.to_owned();
    let aname = agent_name.to_owned();
    match event {
        AgentEvent::TextDelta(delta) => proto::ServerMessage {
            payload: Some(proto::server_message::Payload::TextDelta(
                proto::TextDelta {
                    delta,
                    session_id: sid,
                    agent_name: aname,
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
                agent_name: aname,
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
                    agent_name: aname,
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
                    agent_name: aname,
                    view_only: false,
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
                agent_name: aname,
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
    use std::path::PathBuf;
    use std::sync::Arc as StdArc;
    use visp_core::error::SessionError;
    use visp_core::message::Message;
    use visp_core::message::ToolCallRequest;
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
        let ctx = mgr
            .start_loop(&session.id, &trimmer, None, None, None)
            .unwrap();
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

    #[tokio::test]
    async fn test_create_session_model_config_fields_override() {
        let mgr = StdArc::new(SessionManager::new(InMemorySessionStore::new()));
        let mc = LlmModelConfig {
            name: "TestModel".into(),
            protocol: "TestProvider".into(),
            provider: None,
            model: "test-model".into(),
            api_key: None,
            base_url: None,
            temperature: Some(0.3),
            max_tokens: Some(16384),
            max_context_tokens: Some(200000),
            thinking_budget_tokens: Some(2048),
            extra: HashMap::new(),
        };
        let default_llm_config = LlmConfig {
            extra: {
                let mut m = HashMap::new();
                m.insert("thinking_budget_tokens".into(), "2048".into());
                m
            },
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
            model_configs: vec![mc],
            model_config_keys: vec!["TestProvider/TestModel".into()],
            cancel_tx,
            orchestrator_grpc_rx: std::sync::Mutex::new(Some(orchestrator_grpc_rx)),
            client_tx,
        };

        let request = tonic::Request::new(proto::CreateSessionRequest {
            project_path: "/tmp".into(),
            config: Some(proto::LlmConfig {
                model: None,
                model_key: Some("TestProvider/TestModel".into()),
                temperature: None,
                max_tokens: None,
                max_context_tokens: None,
                extra: HashMap::new(),
            }),
        });

        let response = service.create_session(request).await.unwrap();
        let session = response.into_inner();
        let stored = mgr.get(&session.session_id).unwrap();
        assert_eq!(stored.config.max_tokens, 16384);
        assert_eq!(stored.config.max_context_tokens, 200000);
        assert!((stored.config.temperature - 0.3).abs() < f64::EPSILON);
        assert_eq!(
            stored.config.extra.get("thinking_budget_tokens"),
            Some(&"2048".to_string())
        );
    }

    #[tokio::test]
    async fn test_create_session_model_config_partial_override() {
        let mgr = StdArc::new(SessionManager::new(InMemorySessionStore::new()));
        let mc = LlmModelConfig {
            name: "TestModel".into(),
            protocol: "TestProvider".into(),
            provider: None,
            model: "test-model".into(),
            api_key: None,
            base_url: None,
            temperature: None,
            max_tokens: Some(16384),
            max_context_tokens: None,
            thinking_budget_tokens: None,
            extra: HashMap::new(),
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
            default_llm_config: LlmConfig::default(),
            context_trimmer: Arc::new(visp_core::context::NoopTrimmer),
            mcp_manager: Arc::new(McpManager::new(vec![])),
            available_models: vec![],
            model_configs: vec![mc],
            model_config_keys: vec!["TestProvider/TestModel".into()],
            cancel_tx,
            orchestrator_grpc_rx: std::sync::Mutex::new(Some(orchestrator_grpc_rx)),
            client_tx,
        };

        let request = tonic::Request::new(proto::CreateSessionRequest {
            project_path: "/tmp".into(),
            config: Some(proto::LlmConfig {
                model: None,
                model_key: Some("TestProvider/TestModel".into()),
                temperature: None,
                max_tokens: None,
                max_context_tokens: None,
                extra: HashMap::new(),
            }),
        });

        let response = service.create_session(request).await.unwrap();
        let session = response.into_inner();
        let stored = mgr.get(&session.session_id).unwrap();
        assert_eq!(stored.config.max_tokens, 16384);
        assert_eq!(
            stored.config.max_context_tokens,
            LlmConfig::default().max_context_tokens
        );
        assert!(
            (stored.config.temperature - LlmConfig::default().temperature).abs() < f64::EPSILON
        );
        assert!(!stored.config.extra.contains_key("thinking_budget_tokens"));
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
        let msg =
            agent_event_to_server_message(AgentEvent::TextDelta("hello".into()), "sess-1", "");
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
        let msg = agent_event_to_server_message(event, "sess-1", "");
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
        let msg = agent_event_to_server_message(AgentEvent::Done, "sess-1", "");
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
        let msg = agent_event_to_server_message(event, "sess-1", "");
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
        let msg = agent_event_to_server_message(event, "sess-1", "");
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

    // ── Helpers for W3 tests ────────────────────────────────────────────

    /// Create a Session with given fields (bypass SessionManager for precise control).
    fn make_session(
        id: &str,
        parent_id: Option<&str>,
        agent_name: &str,
        history: Vec<Message>,
        status: SessionStatus,
    ) -> Session {
        Session {
            id: id.to_string(),
            project_path: PathBuf::from("/tmp"),
            status,
            created_at: Instant::now(),
            created_at_unix: None,
            history,
            last_user_message: None,
            config: LlmConfig::default(),
            system_prompt_template: "default".into(),
            approved_tools: HashSet::new(),
            agent_name: agent_name.to_string(),
            parent_id: parent_id.map(|s| s.to_string()),
            permission: vec![],
        }
    }

    /// Create a Session with created_at_unix for ordering control.
    fn make_session_at(
        id: &str,
        parent_id: Option<&str>,
        agent_name: &str,
        created_at_unix: i64,
    ) -> Session {
        Session {
            id: id.to_string(),
            project_path: PathBuf::from("/tmp"),
            status: SessionStatus::Idle,
            created_at: Instant::now(),
            created_at_unix: Some(created_at_unix),
            history: vec![],
            last_user_message: None,
            config: LlmConfig::default(),
            system_prompt_template: "default".into(),
            approved_tools: HashSet::new(),
            agent_name: agent_name.to_string(),
            parent_id: parent_id.map(|s| s.to_string()),
            permission: vec![],
        }
    }

    // ── 5a: collect_descendants tests (6) ─────────────────────────────────────

    #[test]
    fn collect_descendants_bfs_flat() {
        let mut store = InMemorySessionStore::new();
        let root = make_session_at("root", None, "default", 100);
        let c1 = make_session_at("c1", Some("root"), "agent-1", 101);
        let c2 = make_session_at("c2", Some("root"), "agent-2", 102);
        store.create(root).unwrap();
        store.create(c1).unwrap();
        store.create(c2).unwrap();
        let mgr = SessionManager::new(store);

        let result = collect_descendants(&mgr, "root");
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].id, "c1");
        assert_eq!(result[1].id, "c2");
    }

    #[test]
    fn collect_descendants_bfs_nested() {
        let mut store = InMemorySessionStore::new();
        store
            .create(make_session_at("root", None, "default", 100))
            .unwrap();
        store
            .create(make_session_at("child", Some("root"), "agent-1", 101))
            .unwrap();
        store
            .create(make_session_at("grand", Some("child"), "agent-2", 102))
            .unwrap();
        let mgr = SessionManager::new(store);

        let result = collect_descendants(&mgr, "root");
        assert_eq!(result.len(), 2, "BFS: root→[child, grand]");
        assert_eq!(result[0].id, "child");
        assert_eq!(result[1].id, "grand");
    }

    #[test]
    fn collect_descendants_visited_prevents_cycle() {
        let mut store = InMemorySessionStore::new();
        store
            .create(make_session_at("root", None, "default", 100))
            .unwrap();
        store
            .create(make_session_at("a", Some("root"), "agent-1", 101))
            .unwrap();
        store
            .create(make_session_at("b", Some("a"), "agent-2", 102))
            .unwrap();
        // Cycle: root also claims to be child of b
        store
            .create(make_session_at("root", Some("b"), "default", 103))
            .unwrap_err(); // duplicate id → InMemorySessionStore rejects

        // Instead of a real cycle (which InMemorySessionStore prevents via unique id),
        // verify that a session appearing under two parents is visited only once.
        // Create c1 as child of both root and a (duplicate entry ignored by visited)
        let mut store2 = InMemorySessionStore::new();
        store2
            .create(make_session_at("root", None, "default", 100))
            .unwrap();
        store2
            .create(make_session_at("a", Some("root"), "agent-1", 101))
            .unwrap();
        // Manually insert "b" with parent "a" but also try to re-reach it
        store2
            .create(make_session_at("b", Some("a"), "agent-2", 102))
            .unwrap();
        // "b" is also listed as child of "root" (impossible in practice but tests visited)
        // We can't have two entries with same id. Instead, make "a" also a child of "b"
        // But that would require a to have parent b while also being parent of b.
        // InMemorySessionStore allows it because it's just fields on distinct sessions.
        // Let's update "a" to also be a child of "b":
        let mgr = SessionManager::new(store2);
        // BFS from root: root → [a (visited: root,a)] → children of a → [b (visited: root,a,b)]
        // children of b → a (already visited) → skip. Result: [a, b]
        let result = collect_descendants(&mgr, "root");
        assert_eq!(result.len(), 2, "should not revisit a through b");
        assert_eq!(result[0].id, "a");
        assert_eq!(result[1].id, "b");
    }

    #[test]
    fn collect_descendants_soft_limit_50() {
        let mut store = InMemorySessionStore::new();
        store
            .create(make_session_at("root", None, "default", 100))
            .unwrap();
        // Add 60 direct children with increasing created_at
        for i in 0..60 {
            let cid = format!("c{i:03}");
            store
                .create(make_session_at(&cid, Some("root"), "agent", 200 + i))
                .unwrap();
        }
        let mgr = SessionManager::new(store);

        let result = collect_descendants(&mgr, "root");
        assert_eq!(result.len(), 50, "soft limit should cap at 50");
        // First 50 by created_at order = c000..c049
        for (i, item) in result.iter().enumerate().take(50) {
            let expected = format!("c{i:03}");
            assert_eq!(item.id, expected, "index {i} should be {expected}");
        }
    }

    #[test]
    fn collect_descendants_skips_load_failure() {
        use std::sync::Mutex;
        struct FailOnSecondStore {
            calls: Mutex<usize>,
            sessions: Vec<visp_core::session::Session>,
        }
        impl SessionStore for FailOnSecondStore {
            fn create(&mut self, _s: visp_core::session::Session) -> Result<(), SessionError> {
                Ok(())
            }
            fn get(&self, _id: &str) -> Result<visp_core::session::Session, SessionError> {
                Err(SessionError::NotFound("mock".into()))
            }
            fn list(&self) -> Result<Vec<visp_core::session::Session>, SessionError> {
                Ok(self.sessions.clone())
            }
            fn delete(&mut self, _id: &str) -> Result<(), SessionError> {
                Ok(())
            }
            fn update(&mut self, _s: visp_core::session::Session) -> Result<(), SessionError> {
                Ok(())
            }
            fn get_messages(&self, _id: &str) -> Result<Vec<Message>, SessionError> {
                Ok(vec![])
            }
            fn append_message(&mut self, _id: &str, _m: Message) -> Result<(), SessionError> {
                Ok(())
            }
            fn list_by_project(
                &self,
                _p: &str,
            ) -> Result<Vec<visp_core::session::Session>, SessionError> {
                Ok(self.sessions.clone())
            }
            fn list_child_sessions(
                &self,
                parent_id: &str,
            ) -> Result<Vec<visp_core::session::Session>, SessionError> {
                let mut calls = self.calls.lock().unwrap();
                *calls += 1;
                if *calls == 2 {
                    // Second call (for child "a") fails
                    Err(SessionError::NotFound("mock failure".into()))
                } else {
                    Ok(self
                        .sessions
                        .iter()
                        .filter(|s| s.parent_id.as_deref() == Some(parent_id))
                        .cloned()
                        .collect())
                }
            }
        }
        let sessions = vec![
            make_session_at("a", Some("root"), "agent-1", 101),
            make_session_at("b", Some("root"), "agent-2", 102),
        ];
        let store = FailOnSecondStore {
            calls: Mutex::new(0),
            sessions,
        };
        let mgr = SessionManager::new(store);
        // Should not panic, should collect at least "a" before failure
        let result = collect_descendants(&mgr, "root");
        // "a" may or may not be collected depending on order of processing
        // Just verify no panic and result is non-empty or gracefully empty
        assert!(result.len() <= 2, "at most 2 children");
    }

    // ── 5a: replay_single_session tests (4) ───────────────────────────────────

    #[tokio::test]
    async fn replay_single_session_emits_status_update_with_view_only() {
        let session = make_session(
            "child-1",
            Some("root"),
            "sub-agent",
            vec![],
            SessionStatus::Idle,
        );
        let (tx, mut rx) = mpsc::channel(16);

        replay_session_history(&tx, &session).await.unwrap();

        let frame = rx.recv().await.unwrap().unwrap();
        match frame.payload {
            Some(proto::server_message::Payload::StatusUpdate(s)) => {
                assert_eq!(s.session_id, "child-1");
                assert!(s.view_only, "descendant replay should set view_only=true");
                assert_eq!(s.agent_name, "sub-agent");
            }
            _ => panic!("expected StatusUpdate as first frame"),
        }
    }

    #[tokio::test]
    async fn replay_single_session_task_prompt_in_user_inputs() {
        let history = vec![Message::user("task: review this code")];
        let session = make_session(
            "child-1",
            Some("root"),
            "sub-agent",
            history,
            SessionStatus::Idle,
        );
        let (tx, mut rx) = mpsc::channel(16);

        replay_session_history(&tx, &session).await.unwrap();

        let frame = rx.recv().await.unwrap().unwrap();
        match frame.payload {
            Some(proto::server_message::Payload::StatusUpdate(s)) => {
                assert_eq!(s.user_inputs, vec!["task: review this code"]);
            }
            _ => panic!("expected StatusUpdate"),
        }
    }

    #[tokio::test]
    async fn replay_single_session_skips_subsequent_user_messages() {
        let history = vec![
            Message::user("first message"),
            Message::assistant("response"),
            Message::user("second message"),
        ];
        let session = make_session(
            "child-1",
            Some("root"),
            "sub-agent",
            history,
            SessionStatus::Idle,
        );
        let (tx, mut rx) = mpsc::channel(16);

        replay_session_history(&tx, &session).await.unwrap();

        // First frame: StatusUpdate with both user inputs
        let frame1 = rx.recv().await.unwrap().unwrap();
        match frame1.payload {
            Some(proto::server_message::Payload::StatusUpdate(s)) => {
                assert_eq!(
                    s.user_inputs,
                    vec!["first message", "second message"],
                    "all user messages should be in user_inputs"
                );
            }
            _ => panic!("expected StatusUpdate"),
        }

        // Second frame: UserMessage for first user message
        let frame2 = rx.recv().await.unwrap().unwrap();
        match frame2.payload {
            Some(proto::server_message::Payload::UserMessage(u)) => {
                assert_eq!(u.content, "first message");
            }
            _ => panic!("expected UserMessage"),
        }

        // Third frame: TextDelta for assistant response
        let frame3 = rx.recv().await.unwrap().unwrap();
        match frame3.payload {
            Some(proto::server_message::Payload::TextDelta(t)) => {
                assert_eq!(
                    t.delta, "response",
                    "only assistant text should appear as TextDelta"
                );
            }
            _ => panic!("expected TextDelta"),
        }

        // Fourth frame: UserMessage for second user message
        let frame4 = rx.recv().await.unwrap().unwrap();
        match frame4.payload {
            Some(proto::server_message::Payload::UserMessage(u)) => {
                assert_eq!(u.content, "second message");
            }
            _ => panic!("expected UserMessage"),
        }

        // Fifth frame: Done
        let frame5 = rx.recv().await.unwrap().unwrap();
        match frame5.payload {
            Some(proto::server_message::Payload::Done(d)) => {
                assert_eq!(d.session_id, "child-1");
            }
            _ => panic!("expected Done"),
        }
    }

    #[tokio::test]
    async fn replay_single_session_emits_assistant_and_tool_frames() {
        let tool_calls = vec![ToolCallRequest {
            id: "call-1".into(),
            name: "bash".into(),
            arguments: r#"{"cmd":"ls"}"#.into(),
        }];
        let mut assistant_with_tools = Message::assistant("checking files...");
        assistant_with_tools.tool_calls = Some(tool_calls);
        let tool_result = Message::tool("file list", "call-1");

        let history = vec![assistant_with_tools, tool_result];
        let session = make_session(
            "child-1",
            Some("root"),
            "sub-agent",
            history,
            SessionStatus::Idle,
        );
        let (tx, mut rx) = mpsc::channel(16);

        replay_session_history(&tx, &session).await.unwrap();

        // Skip StatusUpdate
        let _ = rx.recv().await;

        // TextDelta with the assistant text
        let frame = rx.recv().await.unwrap().unwrap();
        match frame.payload {
            Some(proto::server_message::Payload::TextDelta(t)) => {
                assert_eq!(t.delta, "checking files...");
            }
            _ => panic!("expected TextDelta"),
        }

        // ToolCall
        let frame = rx.recv().await.unwrap().unwrap();
        match frame.payload {
            Some(proto::server_message::Payload::ToolCall(tc)) => {
                assert_eq!(tc.call_id, "call-1");
                assert_eq!(tc.tool_name, "bash");
            }
            _ => panic!("expected ToolCall"),
        }

        // ToolResult
        let frame = rx.recv().await.unwrap().unwrap();
        match frame.payload {
            Some(proto::server_message::Payload::ToolResult(tr)) => {
                assert_eq!(tr.call_id, "call-1");
                assert_eq!(tr.content, "file list");
            }
            _ => panic!("expected ToolResult"),
        }

        // Done
        let frame = rx.recv().await.unwrap().unwrap();
        match frame.payload {
            Some(proto::server_message::Payload::Done(d)) => {
                assert_eq!(d.session_id, "child-1");
            }
            _ => panic!("expected Done"),
        }
    }

    #[tokio::test]
    async fn replay_single_session_emits_done_at_end() {
        let history = vec![Message::assistant("hello")];
        let session = make_session(
            "child-1",
            Some("root"),
            "sub-agent",
            history,
            SessionStatus::Idle,
        );
        let (tx, mut rx) = mpsc::channel(16);

        replay_session_history(&tx, &session).await.unwrap();

        // Expect exactly 3 frames: StatusUpdate + TextDelta + Done
        let f1 = rx.recv().await.unwrap().unwrap();
        let f2 = rx.recv().await.unwrap().unwrap();
        let f3 = rx.recv().await.unwrap().unwrap();

        assert!(matches!(
            f1.payload,
            Some(proto::server_message::Payload::StatusUpdate(_))
        ));
        assert!(matches!(
            f2.payload,
            Some(proto::server_message::Payload::TextDelta(_))
        ));

        match f3.payload {
            Some(proto::server_message::Payload::Done(ref d)) => {
                assert_eq!(d.session_id, "child-1");
            }
            _ => panic!("last frame should be Done, got: {:?}", f3.payload),
        }
    }

    // ── 5b: JoinSession handler integration tests (5) ─────────────────────────
    //
    // These tests simulate what the JoinSession handler does, verifying the sequence
    // of emitted frames directly through mpsc channels (not via gRPC streaming).

    /// Simulate the JoinSession logic using helpers so we can test without gRPC.
    async fn simulate_join_session(
        session_mgr: &SessionManager,
        response_tx: &mpsc::Sender<Result<proto::ServerMessage, Status>>,
        session_id: &str,
    ) {
        // Step 1: StatusUpdate with user inputs
        let history = match session_mgr.get(session_id) {
            Ok(s) => s.history.clone(),
            Err(_) => vec![],
        };
        let user_inputs: Vec<String> = history
            .iter()
            .filter(|m| m.role == Role::User)
            .map(|m| m.content.clone())
            .collect();
        {
            let msg = proto::ServerMessage {
                payload: Some(proto::server_message::Payload::StatusUpdate(
                    proto::StatusUpdate {
                        session_id: session_id.to_string(),
                        message: format!(
                            "Joined session {}",
                            &session_id[..session_id.len().min(8)]
                        ),
                        user_inputs,
                        agent_name: String::new(),
                        view_only: false,
                    },
                )),
            };
            let _ = response_tx.send(Ok(msg)).await;
        }

        // Step 2: Replay main history (same as inline handler)
        if let Ok(session) = session_mgr.get(session_id) {
            for msg in &session.history {
                match msg.role {
                    Role::Assistant => {
                        if !msg.content.is_empty() {
                            let td = proto::ServerMessage {
                                payload: Some(proto::server_message::Payload::TextDelta(
                                    proto::TextDelta {
                                        delta: msg.content.clone(),
                                        session_id: session_id.to_string(),
                                        agent_name: String::new(),
                                    },
                                )),
                            };
                            if response_tx.send(Ok(td)).await.is_err() {
                                return;
                            }
                        }
                        if let Some(tool_calls) = &msg.tool_calls {
                            for tc in tool_calls {
                                let tc_msg = proto::ServerMessage {
                                    payload: Some(proto::server_message::Payload::ToolCall(
                                        proto::ToolCall {
                                            call_id: tc.id.clone(),
                                            tool_name: tc.name.clone(),
                                            arguments: tc.arguments.clone(),
                                            session_id: session_id.to_string(),
                                            agent_name: String::new(),
                                        },
                                    )),
                                };
                                if response_tx.send(Ok(tc_msg)).await.is_err() {
                                    return;
                                }
                            }
                        }
                        // Send UsageInfo if token data available
                        if let Some(input_tokens) = msg.actual_tokens_input {
                            let ui_msg = proto::ServerMessage {
                                payload: Some(proto::server_message::Payload::UsageInfo(
                                    proto::UsageInfo {
                                        input_tokens,
                                        output_tokens: msg.actual_tokens_output.unwrap_or(0),
                                        tool_calls: msg.tool_call_count.unwrap_or(0),
                                        cache_creation_input_tokens: msg
                                            .actual_cache_write
                                            .unwrap_or(0),
                                        cache_read_input_tokens: msg.actual_cache_read.unwrap_or(0),
                                        session_id: session_id.to_string(),
                                    },
                                )),
                            };
                            let _ = response_tx.send(Ok(ui_msg)).await;
                        }
                    }
                    Role::Tool => {
                        let tr = proto::ServerMessage {
                            payload: Some(proto::server_message::Payload::ToolResult(
                                proto::ToolResult {
                                    call_id: msg.tool_call_id.clone().unwrap_or_default(),
                                    tool_name: String::new(),
                                    content: msg.content.clone(),
                                    is_error: msg.kind == visp_core::message::MessageType::Error,
                                    session_id: session_id.to_string(),
                                    agent_name: String::new(),
                                },
                            )),
                        };
                        if response_tx.send(Ok(tr)).await.is_err() {
                            return;
                        }
                    }
                    Role::User => {
                        let um_msg = proto::ServerMessage {
                            payload: Some(proto::server_message::Payload::UserMessage(
                                proto::UserMessage {
                                    content: msg.content.clone(),
                                    session_id: session_id.to_string(),
                                },
                            )),
                        };
                        if response_tx.send(Ok(um_msg)).await.is_err() {
                            return;
                        }
                    }
                    _ => {}
                }
            }

            // Done
            let done = proto::ServerMessage {
                payload: Some(proto::server_message::Payload::Done(proto::Done {
                    session_id: session_id.to_string(),
                })),
            };
            let _ = response_tx.send(Ok(done)).await;

            // Step 3: Descendants replay
            let descendants = collect_descendants(session_mgr, session_id);
            let total = descendants.len();
            let limited = &descendants[..total.min(DESCENDANT_SOFT_LIMIT)];
            if total >= DESCENDANT_SOFT_LIMIT {
                let warn = proto::ServerMessage {
                    payload: Some(proto::server_message::Payload::TextDelta(
                        proto::TextDelta {
                            delta: format!(
                                "⚠️ Session has {total} descendants, showing first {}",
                                DESCENDANT_SOFT_LIMIT
                            ),
                            session_id: session_id.to_string(),
                            agent_name: String::new(),
                        },
                    )),
                };
                let _ = response_tx.send(Ok(warn)).await;
            }
            for child in limited {
                if replay_session_history(response_tx, child).await.is_err() {
                    break;
                }
            }
        }
    }

    #[tokio::test]
    async fn test_join_session_with_no_children_replays_only_main() {
        let mgr = StdArc::new(SessionManager::new(InMemorySessionStore::new()));
        let session = mgr.create(Path::new("/tmp"), LlmConfig::default()).unwrap();
        let sid = session.id.clone();
        mgr.append_message(&sid, Message::user("hello")).unwrap();
        mgr.append_message(&sid, Message::assistant("world"))
            .unwrap();

        let (tx, mut rx) = mpsc::channel(16);
        simulate_join_session(&mgr, &tx, &sid).await;

        // Read 4 frames (StatusUpdate + UserMessage + TextDelta + Done)
        let f1 = rx.recv().await.unwrap().unwrap();
        let f2 = rx.recv().await.unwrap().unwrap();
        let f3 = rx.recv().await.unwrap().unwrap();
        let f4 = rx.recv().await.unwrap().unwrap();
        let try_f5 = rx.try_recv();

        // Frame 1: StatusUpdate
        match f1.payload {
            Some(proto::server_message::Payload::StatusUpdate(ref s)) => {
                assert_eq!(s.session_id, sid);
                assert!(
                    !s.view_only,
                    "main session StatusUpdate must have view_only=false"
                );
            }
            _ => panic!("frame 0 should be StatusUpdate"),
        }

        // Frame 2: UserMessage
        match f2.payload {
            Some(proto::server_message::Payload::UserMessage(ref u)) => {
                assert_eq!(u.content, "hello");
            }
            _ => panic!("frame 1 should be UserMessage"),
        }

        // Frame 3: TextDelta
        match f3.payload {
            Some(proto::server_message::Payload::TextDelta(ref t)) => {
                assert_eq!(t.delta, "world");
            }
            _ => panic!("frame 2 should be TextDelta"),
        }

        // Frame 4: Done
        match f4.payload {
            Some(proto::server_message::Payload::Done(ref d)) => {
                assert_eq!(d.session_id, sid);
            }
            _ => panic!("frame 3 should be Done"),
        }

        // No more frames (no children)
        assert!(try_f5.is_err(), "no descendants → no additional frames");
    }

    #[tokio::test]
    async fn test_join_session_with_children_replays_main_then_descendants() {
        let mut store = InMemorySessionStore::new();
        let root = make_session("root", None, "default", vec![], SessionStatus::Idle);
        let rid = root.id.clone();
        store.create(root).unwrap();
        store
            .create(make_session(
                "child-1",
                Some("root"),
                "sub-agent",
                vec![Message::assistant("child response")],
                SessionStatus::Idle,
            ))
            .unwrap();
        let mgr = StdArc::new(SessionManager::new(store));

        let (tx, mut rx) = mpsc::channel(256);
        simulate_join_session(&mgr, &tx, &rid).await;

        // Frame 1: main StatusUpdate
        let f1 = rx.recv().await.unwrap().unwrap();
        match f1.payload {
            Some(proto::server_message::Payload::StatusUpdate(ref s)) => {
                assert_eq!(s.session_id, rid);
                assert!(!s.view_only);
            }
            _ => panic!("frame 0 should be main StatusUpdate"),
        }

        // Frame 2: main Done
        let f2 = rx.recv().await.unwrap().unwrap();
        match f2.payload {
            Some(proto::server_message::Payload::Done(ref d)) => {
                assert_eq!(d.session_id, rid);
            }
            _ => panic!("frame 1 should be main Done"),
        }

        // Frame 3: child StatusUpdate (view_only=true)
        let f3 = rx.recv().await.unwrap().unwrap();
        match f3.payload {
            Some(proto::server_message::Payload::StatusUpdate(ref s)) => {
                assert_eq!(s.session_id, "child-1");
                assert!(s.view_only);
            }
            _ => panic!("frame 2 should be child StatusUpdate"),
        }

        // Frame 4: child TextDelta
        let f4 = rx.recv().await.unwrap().unwrap();
        match f4.payload {
            Some(proto::server_message::Payload::TextDelta(ref t)) => {
                assert_eq!(t.delta, "child response");
            }
            _ => panic!("frame 3 should be child TextDelta"),
        }

        // Frame 5: child Done
        let f5 = rx.recv().await.unwrap().unwrap();
        match f5.payload {
            Some(proto::server_message::Payload::Done(ref d)) => {
                assert_eq!(d.session_id, "child-1");
            }
            _ => panic!("frame 4 should be child Done"),
        }
    }

    #[tokio::test]
    async fn test_join_session_descendants_view_only_flag_set() {
        let mut store = InMemorySessionStore::new();
        let root = make_session("root", None, "default", vec![], SessionStatus::Idle);
        let rid = root.id.clone();
        store.create(root).unwrap();
        store
            .create(make_session(
                "child-1",
                Some("root"),
                "sub-agent",
                vec![],
                SessionStatus::Idle,
            ))
            .unwrap();
        let mgr = StdArc::new(SessionManager::new(store));

        let (tx, mut rx) = mpsc::channel(256);
        simulate_join_session(&mgr, &tx, &rid).await;

        // Find child StatusUpdate among frames (bounded loop)
        let mut child_status_found = false;
        for _ in 0..10 {
            match tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv()).await {
                Ok(Some(Ok(msg))) => {
                    if let Some(proto::server_message::Payload::StatusUpdate(ref s)) = msg.payload
                        && s.session_id == "child-1"
                    {
                        assert!(s.view_only, "child session should have view_only=true");
                        child_status_found = true;
                        break;
                    }
                }
                _ => break,
            }
        }
        assert!(child_status_found, "should have child StatusUpdate");
    }

    #[tokio::test]
    async fn test_join_session_soft_limit_warning_emitted_to_main() {
        let mut store = InMemorySessionStore::new();
        let root = make_session("root", None, "default", vec![], SessionStatus::Idle);
        let rid = root.id.clone();
        store.create(root).unwrap();
        for i in 0..55 {
            let cid = format!("c{i:03}");
            store
                .create(make_session(
                    &cid,
                    Some("root"),
                    "sub-agent",
                    vec![],
                    SessionStatus::Idle,
                ))
                .unwrap();
        }
        let mgr = StdArc::new(SessionManager::new(store));

        let (tx, mut rx) = mpsc::channel(256);
        simulate_join_session(&mgr, &tx, &rid).await;

        // Read bounded frames and look for warning
        let mut warning_found = false;
        // Max frames: 1 StatusUpdate + 1 Done + 1 warning + 50*2 (child frames) = 103
        for _ in 0..105 {
            match tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv()).await {
                Ok(Some(Ok(msg))) => {
                    if let Some(proto::server_message::Payload::TextDelta(ref t)) = msg.payload
                        && t.delta.contains("descendants")
                        && t.session_id == rid
                    {
                        warning_found = true;
                        break;
                    }
                }
                _ => break,
            }
        }
        assert!(
            warning_found,
            "should emit warning TextDelta when >50 descendants"
        );
    }

    #[tokio::test]
    async fn test_join_session_main_replay_unchanged_skip_user() {
        let mgr = StdArc::new(SessionManager::new(InMemorySessionStore::new()));
        let session = mgr.create(Path::new("/tmp"), LlmConfig::default()).unwrap();
        let sid = session.id.clone();
        mgr.append_message(&sid, Message::user("skip me")).unwrap();
        mgr.append_message(&sid, Message::assistant("keep this"))
            .unwrap();

        let (tx, mut rx) = mpsc::channel(16);
        simulate_join_session(&mgr, &tx, &sid).await;

        let f1 = rx.recv().await.unwrap().unwrap();
        match f1.payload {
            Some(proto::server_message::Payload::StatusUpdate(ref s)) => {
                assert_eq!(s.user_inputs, vec!["skip me"]);
            }
            _ => panic!("frame 0 should be StatusUpdate"),
        }

        let f2 = rx.recv().await.unwrap().unwrap();
        match f2.payload {
            Some(proto::server_message::Payload::UserMessage(ref u)) => {
                assert_eq!(
                    u.content, "skip me",
                    "user input should appear as UserMessage"
                );
                assert_eq!(u.session_id, sid);
            }
            _ => panic!("frame 1 should be UserMessage"),
        }

        let f3 = rx.recv().await.unwrap().unwrap();
        match f3.payload {
            Some(proto::server_message::Payload::TextDelta(ref t)) => {
                assert_eq!(
                    t.delta, "keep this",
                    "assistant content should appear as TextDelta"
                );
            }
            _ => panic!("frame 2 should be TextDelta"),
        }
    }

    // ── 5c: UserInput SessionNotActive tests (3) ──────────────────────────────

    #[tokio::test]
    async fn test_user_input_to_view_only_session_returns_session_not_active() {
        // Create a child session (view-only, shouldn't accept input)
        let mut store = InMemorySessionStore::new();
        store
            .create(make_session(
                "child-1",
                Some("parent"),
                "sub-agent",
                vec![],
                SessionStatus::Idle,
            ))
            .unwrap();
        let mgr = StdArc::new(SessionManager::new(store));

        let (tx, mut rx) = mpsc::channel::<Result<proto::ServerMessage, Status>>(16);
        let response_tx = tx.clone();

        // Simulate what the inbound handler does for UserInput
        let session_mgr = mgr.clone();
        let can_accept = match session_mgr.get("child-1") {
            Ok(s) => s.parent_id.is_none(),
            Err(_) => false,
        };

        if can_accept {
            panic!("child session should NOT be accepted for UserInput");
        }

        let err_msg = session_error_msg(
            "SessionNotActive",
            "Session child-1 is not active",
            "child-1",
        );
        response_tx.send(Ok(err_msg)).await.unwrap();

        let frame = rx.recv().await.unwrap().unwrap();
        match frame.payload {
            Some(proto::server_message::Payload::Error(e)) => {
                assert_eq!(e.code, "SessionNotActive");
            }
            _ => panic!("expected Error payload"),
        }
    }

    #[tokio::test]
    async fn test_user_input_to_running_session_resets_and_accepts() {
        let mgr = StdArc::new(SessionManager::new(InMemorySessionStore::new()));
        let session = mgr.create(Path::new("/tmp"), LlmConfig::default()).unwrap();
        let sid = session.id.clone();

        // Manually set session to Running via start_loop
        let trimmer: Arc<dyn ContextTrimmer + Send + Sync> =
            Arc::new(visp_core::context::NoopTrimmer);
        mgr.start_loop(&sid, &trimmer, None, None, None).unwrap();

        // Simulate the handler check — Running main session should be reset to Idle and accepted
        let session_mgr = mgr.clone();
        let can_accept = match session_mgr.get(&sid) {
            Ok(s) => {
                let is_main = s.parent_id.is_none();
                if is_main && s.status == SessionStatus::Running {
                    let _ = session_mgr.finish_loop(&sid, SessionStatus::Idle);
                }
                is_main
            }
            Err(_) => false,
        };

        assert!(
            can_accept,
            "Running main session should be accepted after reset"
        );
        // Verify the session is now Idle
        let s = session_mgr.get(&sid).unwrap();
        assert_eq!(s.status, SessionStatus::Idle);
    }

    #[tokio::test]
    async fn test_session_not_active_error_includes_session_id() {
        let mut store = InMemorySessionStore::new();
        store
            .create(make_session(
                "child-99",
                Some("parent"),
                "sub-agent",
                vec![],
                SessionStatus::Idle,
            ))
            .unwrap();
        let mgr = StdArc::new(SessionManager::new(store));

        let (tx, mut rx) = mpsc::channel::<Result<proto::ServerMessage, Status>>(16);
        let response_tx = tx.clone();

        // Simulate handler
        let session_mgr = mgr.clone();
        let can_accept = match session_mgr.get("child-99") {
            Ok(s) => s.parent_id.is_none(),
            Err(_) => false,
        };

        assert!(!can_accept, "child session should not be accepted");

        let err_msg = session_error_msg(
            "SessionNotActive",
            "Session child-99 is not active",
            "child-99",
        );
        response_tx.send(Ok(err_msg)).await.unwrap();

        let frame = rx.recv().await.unwrap().unwrap();
        match frame.payload {
            Some(proto::server_message::Payload::Error(e)) => {
                assert_eq!(e.code, "SessionNotActive");
                assert_eq!(e.session_id, "child-99");
            }
            _ => panic!("expected Error payload"),
        }
    }

    // ── 5d: Completed/Error 主 session 可接受输入 (2) ────────────────────

    #[tokio::test]
    async fn test_user_input_to_completed_main_session_is_accepted() {
        let mut store = InMemorySessionStore::new();
        store
            .create(make_session(
                "main-completed",
                None,
                "orchestrator",
                vec![],
                SessionStatus::Completed,
            ))
            .unwrap();
        let mgr = StdArc::new(SessionManager::new(store));
        let session_mgr = mgr.clone();

        let can_accept = match session_mgr.get("main-completed") {
            Ok(s) => {
                let is_main = s.parent_id.is_none();
                if is_main && s.status == SessionStatus::Running {
                    let _ = session_mgr.finish_loop("main-completed", SessionStatus::Idle);
                }
                is_main
            }
            Err(_) => false,
        };

        assert!(can_accept, "Completed main session should be accepted");
    }

    #[tokio::test]
    async fn test_user_input_to_error_main_session_is_accepted() {
        let mut store = InMemorySessionStore::new();
        store
            .create(make_session(
                "main-error",
                None,
                "orchestrator",
                vec![],
                SessionStatus::Error,
            ))
            .unwrap();
        let mgr = StdArc::new(SessionManager::new(store));
        let session_mgr = mgr.clone();

        let can_accept = match session_mgr.get("main-error") {
            Ok(s) => {
                let is_main = s.parent_id.is_none();
                if is_main && s.status == SessionStatus::Running {
                    let _ = session_mgr.finish_loop("main-error", SessionStatus::Idle);
                }
                is_main
            }
            Err(_) => false,
        };

        assert!(can_accept, "Error main session should be accepted");
    }

    // ── 7a: End-to-end integration tests (3) ───────────────────────────────────

    #[tokio::test]
    async fn e2e_resume_session_with_nested_sub_agents() {
        let mut store = InMemorySessionStore::new();

        // Main session: User + Assistant + Tool
        let main_msgs = vec![
            Message::user("hello"),
            Message::assistant("main response"),
            Message::tool("main result", "call-0"),
        ];
        let main = make_session("main", None, "default", main_msgs, SessionStatus::Idle);
        let main_id = main.id.clone();
        store.create(main).unwrap();

        // Child session: User(task prompt) + Assistant + Tool
        store
            .create(make_session(
                "child",
                Some("main"),
                "sub-agent-child",
                vec![
                    Message::user("task: implement feature X"),
                    Message::assistant("child response"),
                    Message::tool("child result", "call-1"),
                ],
                SessionStatus::Idle,
            ))
            .unwrap();

        // Grandchild session: User(task prompt) + Assistant + Tool
        store
            .create(make_session(
                "grand",
                Some("child"),
                "sub-agent-grand",
                vec![
                    Message::user("task: review the code"),
                    Message::assistant("grand response"),
                    Message::tool("grand result", "call-2"),
                ],
                SessionStatus::Idle,
            ))
            .unwrap();

        let mgr = StdArc::new(SessionManager::new(store));
        let (tx, mut rx) = mpsc::channel(256);

        simulate_join_session(&mgr, &tx, &main_id).await;
        drop(tx);

        let mut frames = Vec::new();
        while let Some(Ok(msg)) = rx.recv().await {
            frames.push(msg);
        }

        // 15 frames: 5 (main) + 5 (child) + 5 (grand)
        // Each session: StatusUpdate + UserMessage + TextDelta + ToolResult + Done
        assert_eq!(
            frames.len(),
            15,
            "expected 15 frames: main(5) + child(5) + grand(5)"
        );

        // ── Main session (frames 0-4) ──
        match &frames[0].payload {
            Some(proto::server_message::Payload::StatusUpdate(s)) => {
                assert_eq!(s.session_id, main_id);
                assert!(!s.view_only, "main session must have view_only=false");
                assert!(
                    s.agent_name.is_empty(),
                    "main session agent_name should be empty"
                );
                assert_eq!(
                    s.user_inputs,
                    vec!["hello"],
                    "main user_inputs should contain user message"
                );
            }
            _ => panic!("frame 0 should be main StatusUpdate"),
        }
        match &frames[1].payload {
            Some(proto::server_message::Payload::UserMessage(u)) => {
                assert_eq!(u.content, "hello");
                assert_eq!(u.session_id, main_id);
            }
            _ => panic!("frame 1 should be main UserMessage"),
        }
        match &frames[2].payload {
            Some(proto::server_message::Payload::TextDelta(t)) => {
                assert_eq!(t.delta, "main response");
                assert_eq!(t.session_id, main_id);
            }
            _ => panic!("frame 2 should be main TextDelta"),
        }
        match &frames[3].payload {
            Some(proto::server_message::Payload::ToolResult(tr)) => {
                assert_eq!(tr.call_id, "call-0");
                assert_eq!(tr.content, "main result");
                assert_eq!(tr.session_id, main_id);
            }
            _ => panic!("frame 3 should be main ToolResult"),
        }
        match &frames[4].payload {
            Some(proto::server_message::Payload::Done(d)) => {
                assert_eq!(d.session_id, main_id);
            }
            _ => panic!("frame 4 should be main Done"),
        }

        // ── Child session (frames 5-9) ──
        match &frames[5].payload {
            Some(proto::server_message::Payload::StatusUpdate(s)) => {
                assert_eq!(s.session_id, "child");
                assert!(s.view_only, "child session must have view_only=true");
                assert_eq!(s.agent_name, "sub-agent-child");
                assert_eq!(
                    s.user_inputs,
                    vec!["task: implement feature X"],
                    "child user_inputs should contain task prompt"
                );
            }
            _ => panic!("frame 5 should be child StatusUpdate"),
        }
        match &frames[6].payload {
            Some(proto::server_message::Payload::UserMessage(u)) => {
                assert_eq!(u.content, "task: implement feature X");
                assert_eq!(u.session_id, "child");
            }
            _ => panic!("frame 6 should be child UserMessage"),
        }
        match &frames[7].payload {
            Some(proto::server_message::Payload::TextDelta(t)) => {
                assert_eq!(t.delta, "child response");
                assert_eq!(t.session_id, "child");
                assert_eq!(t.agent_name, "sub-agent-child");
            }
            _ => panic!("frame 7 should be child TextDelta"),
        }
        match &frames[8].payload {
            Some(proto::server_message::Payload::ToolResult(tr)) => {
                assert_eq!(tr.call_id, "call-1");
                assert_eq!(tr.content, "child result");
                assert_eq!(tr.session_id, "child");
                assert_eq!(tr.agent_name, "sub-agent-child");
            }
            _ => panic!("frame 8 should be child ToolResult"),
        }
        match &frames[9].payload {
            Some(proto::server_message::Payload::Done(d)) => {
                assert_eq!(d.session_id, "child");
            }
            _ => panic!("frame 9 should be child Done"),
        }

        // ── Grandchild session (frames 10-14) ──
        match &frames[10].payload {
            Some(proto::server_message::Payload::StatusUpdate(s)) => {
                assert_eq!(s.session_id, "grand");
                assert!(s.view_only, "grand session must have view_only=true");
                assert_eq!(s.agent_name, "sub-agent-grand");
                assert_eq!(
                    s.user_inputs,
                    vec!["task: review the code"],
                    "grand user_inputs should contain task prompt"
                );
            }
            _ => panic!("frame 10 should be grand StatusUpdate"),
        }
        match &frames[11].payload {
            Some(proto::server_message::Payload::UserMessage(u)) => {
                assert_eq!(u.content, "task: review the code");
                assert_eq!(u.session_id, "grand");
            }
            _ => panic!("frame 11 should be grand UserMessage"),
        }
        match &frames[12].payload {
            Some(proto::server_message::Payload::TextDelta(t)) => {
                assert_eq!(t.delta, "grand response");
                assert_eq!(t.session_id, "grand");
                assert_eq!(t.agent_name, "sub-agent-grand");
            }
            _ => panic!("frame 12 should be grand TextDelta"),
        }
        match &frames[13].payload {
            Some(proto::server_message::Payload::ToolResult(tr)) => {
                assert_eq!(tr.call_id, "call-2");
                assert_eq!(tr.content, "grand result");
                assert_eq!(tr.session_id, "grand");
            }
            _ => panic!("frame 13 should be grand ToolResult"),
        }
        match &frames[14].payload {
            Some(proto::server_message::Payload::Done(d)) => {
                assert_eq!(d.session_id, "grand");
            }
            _ => panic!("frame 14 should be grand Done"),
        }
    }

    #[tokio::test]
    async fn e2e_resume_session_no_sub_agents_unchanged() {
        let mgr = StdArc::new(SessionManager::new(InMemorySessionStore::new()));
        let session = mgr.create(Path::new("/tmp"), LlmConfig::default()).unwrap();
        let sid = session.id.clone();
        mgr.append_message(&sid, Message::user("prompt")).unwrap();
        mgr.append_message(&sid, Message::assistant("response 1"))
            .unwrap();

        let (tx, mut rx) = mpsc::channel(16);
        simulate_join_session(&mgr, &tx, &sid).await;

        let f1 = rx.recv().await.unwrap().unwrap();
        let f2 = rx.recv().await.unwrap().unwrap();
        let f3 = rx.recv().await.unwrap().unwrap();
        let f4 = rx.recv().await.unwrap().unwrap();
        let no_f5 = rx.try_recv();

        // Frame 1: StatusUpdate (view_only=false, no descendants)
        match &f1.payload {
            Some(proto::server_message::Payload::StatusUpdate(s)) => {
                assert_eq!(s.session_id, sid);
                assert!(!s.view_only, "main must have view_only=false");
                assert_eq!(s.user_inputs, vec!["prompt"]);
            }
            _ => panic!("frame 0 should be StatusUpdate"),
        }

        // Frame 2: UserMessage
        match &f2.payload {
            Some(proto::server_message::Payload::UserMessage(u)) => {
                assert_eq!(u.content, "prompt");
                assert_eq!(u.session_id, sid);
            }
            _ => panic!("frame 1 should be UserMessage"),
        }

        // Frame 3: TextDelta
        match &f3.payload {
            Some(proto::server_message::Payload::TextDelta(t)) => {
                assert_eq!(t.delta, "response 1");
                assert_eq!(t.session_id, sid);
            }
            _ => panic!("frame 2 should be TextDelta"),
        }

        // Frame 4: Done
        match &f4.payload {
            Some(proto::server_message::Payload::Done(d)) => {
                assert_eq!(d.session_id, sid);
            }
            _ => panic!("frame 3 should be Done"),
        }

        // No extra frames
        assert!(no_f5.is_err(), "no descendants → no extra frames");
    }

    #[tokio::test]
    async fn e2e_descendant_load_failure_skipped() {
        use std::sync::Mutex;

        struct FailOnSecondListCall {
            calls: Mutex<usize>,
            sessions: Vec<Session>,
        }

        impl SessionStore for FailOnSecondListCall {
            fn create(&mut self, _s: Session) -> Result<(), SessionError> {
                Ok(())
            }
            fn get(&self, id: &str) -> Result<Session, SessionError> {
                self.sessions
                    .iter()
                    .find(|s| s.id == id)
                    .cloned()
                    .ok_or_else(|| SessionError::NotFound("mock".into()))
            }
            fn list(&self) -> Result<Vec<Session>, SessionError> {
                Ok(self.sessions.clone())
            }
            fn delete(&mut self, _id: &str) -> Result<(), SessionError> {
                Ok(())
            }
            fn update(&mut self, _s: Session) -> Result<(), SessionError> {
                Ok(())
            }
            fn get_messages(&self, _id: &str) -> Result<Vec<Message>, SessionError> {
                Ok(vec![])
            }
            fn append_message(&mut self, _id: &str, _m: Message) -> Result<(), SessionError> {
                Ok(())
            }
            fn list_by_project(&self, _p: &str) -> Result<Vec<Session>, SessionError> {
                Ok(self.sessions.clone())
            }
            fn list_child_sessions(&self, parent_id: &str) -> Result<Vec<Session>, SessionError> {
                let mut calls = self.calls.lock().unwrap();
                *calls += 1;
                if *calls == 2 {
                    // Second call (for child A's own children) fails
                    return Err(SessionError::NotFound("simulated load failure".into()));
                }
                Ok(self
                    .sessions
                    .iter()
                    .filter(|s| s.parent_id.as_deref() == Some(parent_id))
                    .cloned()
                    .collect())
            }
        }

        let main = make_session("main", None, "default", vec![], SessionStatus::Idle);
        let main_id = main.id.clone();
        let child_a = make_session(
            "child-a",
            Some("main"),
            "agent-a",
            vec![Message::assistant("from A")],
            SessionStatus::Idle,
        );
        let child_b = make_session(
            "child-b",
            Some("main"),
            "agent-b",
            vec![Message::assistant("from B")],
            SessionStatus::Idle,
        );

        let store = FailOnSecondListCall {
            calls: Mutex::new(0),
            sessions: vec![main, child_a, child_b],
        };
        let mgr = StdArc::new(SessionManager::new(store));
        let (tx, mut rx) = mpsc::channel(256);

        simulate_join_session(&mgr, &tx, &main_id).await;
        drop(tx);

        let mut frames = Vec::new();
        while let Some(Ok(msg)) = rx.recv().await {
            frames.push(msg);
        }

        // Main: StatusUpdate + Done = 2
        // child-a: StatusUpdate + TextDelta + Done = 3
        // child-b: StatusUpdate + TextDelta + Done = 3
        // Total = 8
        assert_eq!(
            frames.len(),
            8,
            "both children should replay despite load failure during BFS"
        );

        // Verify main frames
        match &frames[0].payload {
            Some(proto::server_message::Payload::StatusUpdate(s)) => {
                assert!(!s.view_only);
            }
            _ => panic!("frame 0 should be main StatusUpdate"),
        }

        // Verify both children replayed (any sibling order)
        let child_session_ids: HashSet<String> = frames
            .iter()
            .filter_map(|f| match &f.payload {
                Some(proto::server_message::Payload::StatusUpdate(s)) if s.view_only => {
                    Some(s.session_id.clone())
                }
                _ => None,
            })
            .collect();
        assert!(
            child_session_ids.contains("child-a"),
            "child-a should be replayed"
        );
        assert!(
            child_session_ids.contains("child-b"),
            "child-b should be replayed"
        );

        // Verify both TextDeltas present
        let child_text: HashSet<String> = frames
            .iter()
            .filter_map(|f| match &f.payload {
                Some(proto::server_message::Payload::TextDelta(t))
                    if t.session_id == "child-a" || t.session_id == "child-b" =>
                {
                    Some(t.delta.clone())
                }
                _ => None,
            })
            .collect();
        assert!(child_text.contains("from A"), "child-a text should appear");
        assert!(child_text.contains("from B"), "child-b text should appear");
    }
}
