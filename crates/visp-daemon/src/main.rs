#[allow(dead_code)]
mod agent_loader;
mod command;
mod config;
mod server;
mod service;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;

use visp_core::{
    agent::AgentConfig,
    context::ContextTrimmer,
    provider::LlmProvider,
    rules::RuleEngine,
    session::{InMemorySessionStore, SessionManager, SessionStore},
    tool_registry::ToolRegistry,
};

use visp_agent::orchestrator::{CancelSignal, Orchestrator};
use visp_mcp::manager::McpManager;
use visp_tools::{
    bash::Bash,
    codegraph::{
        CodeGraphContext, CodeGraphGetDetails, CodeGraphImpact, CodeGraphRebuild, CodeGraphSearch,
        CodeGraphTrace,
    },
    fetch::WebFetch,
    file::{EditFile, ReadFile, WriteFile},
    search::{Glob, Grep},
    task::TaskTool,
};

use crate::config::{DaemonConfig, LlmModelConfig};
use crate::service::CoderDaemonService;

/// Create an LLM provider from a model config.
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

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 1. Init tracing
    tracing_subscriber::fmt()
        .with_ansi(false)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    // 2. Load config
    let config_path = std::env::args().nth(1).map(std::path::PathBuf::from);
    let config: DaemonConfig =
        config::load_config(config_path.as_deref()).map_err(|e| format!("config: {e}"))?;

    tracing::info!(listen_addr = %config.daemon.listen_addr, "starting visp-daemon");

    // 3. Build model configs
    let model_configs = config.llm.effective_models();
    let model_keys: Vec<String> = model_configs.iter().map(|mc| mc.key()).collect();
    let default_protocol = model_configs
        .first()
        .map(|mc| mc.protocol.as_str())
        .unwrap_or("anthropic");
    let available_models = config.llm.available_models();

    tracing::info!(
        protocol = %default_protocol,
        models = %model_keys.join(", "),
        "LLM configured"
    );

    // 4. Create tool registry
    let tool_registry = ToolRegistry::new();

    // ── 常用工具 ──
    tool_registry
        .register(Arc::new(Bash::from_toml(config.tool.get("bash"))))
        .map_err(|e| format!("register bash: {e}"))?;
    tool_registry
        .register(Arc::new(ReadFile::from_toml(config.tool.get("read_file"))))
        .map_err(|e| format!("register read_file: {e}"))?;
    tool_registry
        .register(Arc::new(WriteFile::from_toml(
            config.tool.get("write_file"),
        )))
        .map_err(|e| format!("register write_file: {e}"))?;
    tool_registry
        .register(Arc::new(EditFile::from_toml(config.tool.get("edit_file"))))
        .map_err(|e| format!("register edit_file: {e}"))?;
    tool_registry
        .register(Arc::new(Grep::from_toml(config.tool.get("grep"))))
        .map_err(|e| format!("register grep: {e}"))?;
    tool_registry
        .register(Arc::new(Glob::from_toml(config.tool.get("glob"))))
        .map_err(|e| format!("register glob: {e}"))?;

    // ── 网络工具 ──
    tool_registry
        .register(Arc::new(WebFetch::from_toml(config.tool.get("webfetch"))))
        .map_err(|e| format!("register fetch_web: {e}"))?;

    // ── 代码分析工具 ──
    tool_registry
        .register(Arc::new(CodeGraphSearch::from_toml(
            config.tool.get("codegraph_search"),
        )))
        .map_err(|e| format!("register codegraph_search: {e}"))?;
    tool_registry
        .register(Arc::new(CodeGraphGetDetails::from_toml(
            config.tool.get("codegraph_get_details"),
        )))
        .map_err(|e| format!("register codegraph_get_details: {e}"))?;
    tool_registry
        .register(Arc::new(CodeGraphRebuild))
        .map_err(|e| format!("register codegraph_rebuild: {e}"))?;
    tool_registry
        .register(Arc::new(CodeGraphContext::from_toml(
            config.tool.get("codegraph_context"),
        )))
        .map_err(|e| format!("register codegraph_context: {e}"))?;
    tool_registry
        .register(Arc::new(CodeGraphTrace::from_toml(
            config.tool.get("codegraph_trace"),
        )))
        .map_err(|e| format!("register codegraph_trace: {e}"))?;
    tool_registry
        .register(Arc::new(CodeGraphImpact::from_toml(
            config.tool.get("codegraph_impact"),
        )))
        .map_err(|e| format!("register codegraph_impact: {e}"))?;

    // ── 子 Agent 委派工具 ──
    tool_registry
        .register(Arc::new(TaskTool))
        .map_err(|e| format!("register task: {e}"))?;

    let tool_registry = Arc::new(tool_registry);

    // ── 锁定核心工具（MCP 工具不能覆盖这些名称）──
    tool_registry.seal_core_tools();

    // 5. Initialize MCP Manager
    let mcp_manager = Arc::new(McpManager::new(config.mcp.servers.clone()));
    {
        let tr = tool_registry.clone();
        // 跟踪每个服务器已注册的工具名，用于重连时更新
        let mcp_tool_names: Arc<Mutex<HashMap<String, Vec<String>>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let on_ready: visp_mcp::manager::OnToolsReady = Arc::new(move |server_name, tools| {
            let tool_names: Vec<String> = tools.iter().map(|t| t.name().to_string()).collect();

            // 获取已有的该服务器工具名列表（insert 返回旧值）
            let old_tool_names = {
                let mut map = mcp_tool_names.lock().unwrap();
                map.insert(server_name.to_string(), tool_names.clone())
                    .unwrap_or_default()
            };

            // 移除旧工具（第一次注册时 old_tool_names 为空列表，跳过）
            for old_name in &old_tool_names {
                if !tool_names.contains(old_name) {
                    // 工具名已不存在于新列表中，移除
                    let _ = tr.remove(old_name);
                }
            }

            // 注册/更新新工具
            for (i, tool) in tools.into_iter().enumerate() {
                let tool_name = &tool_names[i];
                let tool = Arc::from(tool); // Box → Arc
                if old_tool_names.contains(tool_name) {
                    // 重连场景：更新现有工具
                    if let Err(e) = tr.update(tool_name, tool) {
                        tracing::warn!("failed to update MCP tool '{tool_name}': {e}");
                    }
                } else {
                    // 首次注册场景
                    if let Err(e) = tr.register_mcp(tool) {
                        tracing::warn!("failed to register MCP tool '{tool_name}': {e}");
                    }
                }
            }
        });
        mcp_manager.start_all(on_ready).await;
    }

    // 6. Create rule engine
    let cwd = std::env::current_dir()?;
    let rule_engine = Arc::new(RuleEngine::new(&cwd)?);

    // 6.5. Create context trimmer
    let context_trimmer: Arc<dyn ContextTrimmer + Send + Sync> =
        Arc::new(visp_context::DefaultContextTrimmer::default());

    // 7. Create session manager
    let store: Box<dyn SessionStore> = match config.storage.driver.as_str() {
        "memory" => Box::new(InMemorySessionStore::new()),
        "sqlite" => {
            let sqlite_store = visp_db::SqliteSessionStore::open(&config.storage.path)
                .map_err(|e| format!("failed to open sqlite store: {e}"))?;
            Box::new(sqlite_store)
        }
        other => {
            return Err(
                format!("unknown storage driver: {other} (expected 'sqlite' or 'memory')").into(),
            );
        }
    };
    let session_mgr = Arc::new(SessionManager::new(store));

    // 8. Agent config
    let agent_config = AgentConfig {
        soft_limit: config.agent.soft_limit,
        hard_limit: 200,
        doom_loop_threshold: config.agent.doom_loop_threshold,
        llm_retry_attempts: config.agent.llm_retry_attempts,
        llm_retry_base_delay_ms: config.agent.llm_retry_base_delay_ms,
        bash_confirm_mode: config.agent.bash_confirm_mode,
        file_max_size_bytes: config.agent.file_max_size_bytes,
        max_depth: config.agent.max_depth,
    };

    // 8.5. Create provider HashMap
    let mut providers: HashMap<String, Arc<dyn LlmProvider>> = HashMap::new();
    let default_model_key = model_configs
        .first()
        .map(|mc| mc.key())
        .unwrap_or_else(|| "default".to_string());
    for mc in &model_configs {
        match create_llm_provider(mc) {
            Ok(provider) => {
                providers.insert(mc.key(), provider);
            }
            Err(e) => {
                tracing::warn!(model_key = %mc.key(), error = %e, "failed to create provider");
            }
        }
    }

    // 8.6. Load agent definitions
    let agent_registry = Arc::new(agent_loader::load_agents(&cwd));

    // 8.7. Create orchestration channels
    let (global_tx, global_rx) = mpsc::channel(256);
    let (cancel_tx, cancel_rx) = mpsc::channel::<CancelSignal>(16);
    let (orchestrator_grpc_tx, orchestrator_grpc_rx) = mpsc::channel(256);
    let (client_tx, client_rx) = mpsc::channel(64);

    // 8.8. Create and start Orchestrator
    let mut orchestrator = Orchestrator::new(
        cancel_rx,
        global_rx,
        global_tx.clone(),
        client_rx,
        orchestrator_grpc_tx.clone(),
        session_mgr.clone(),
        agent_registry.clone(),
        tool_registry.clone(),
        rule_engine.clone(),
        agent_config.clone(),
        context_trimmer.clone(),
        providers,
        default_model_key,
    );
    tokio::spawn(async move {
        orchestrator.run().await;
    });

    // 9. Assemble service
    let mcp_shutdown = mcp_manager.clone();
    let service = CoderDaemonService::new(
        model_configs,
        tool_registry,
        rule_engine,
        session_mgr,
        agent_config,
        config.llm,
        context_trimmer,
        mcp_manager,
        available_models,
        cancel_tx,
        orchestrator_grpc_rx,
        client_tx,
    );

    // 10. Start gRPC server
    let addr = config.daemon.listen_addr.clone();
    let server_handle = tokio::spawn(async move {
        if let Err(e) = server::start_server(&addr, service).await {
            tracing::error!("grpc server error: {e}");
        }
    });

    // 11. Wait for Ctrl+C
    tokio::signal::ctrl_c().await?;
    tracing::info!("shutdown signal received, stopping server");

    // Gracefully shut down MCP connections before aborting the server
    mcp_shutdown.shutdown_all().await;

    server_handle.abort();

    Ok(())
}
