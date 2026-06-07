mod config;
mod server;
mod service;

use std::sync::Arc;

use vbw_core::{
    agent::AgentConfig,
    rules::RuleEngine,
    session::{InMemorySessionStore, SessionManager},
    tool_registry::ToolRegistry,
};
use vbw_llm::anthropic::AnthropicProvider;
use vbw_tools::{
    bash::Bash,
    file::{EditFile, ReadFile, WriteFile},
    search::{Glob, Grep},
};

use crate::config::DaemonConfig;
use crate::service::CoderDaemonService;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 1. Init tracing
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    // 2. Load config
    let config_path = std::env::args().nth(1).map(std::path::PathBuf::from);
    let config: DaemonConfig =
        config::load_config(config_path.as_deref()).map_err(|e| format!("config: {e}"))?;

    tracing::info!(listen_addr = %config.daemon.listen_addr, "starting vbw-daemon");

    // 3. Create LLM provider
    let api_key = config
        .llm
        .api_key
        .clone()
        .or_else(|| std::env::var("ANTHROPIC_API_KEY").ok())
        .ok_or_else(|| {
            "ANTHROPIC_API_KEY not set (configure llm.api_key or set env)".to_string()
        })?;
    let provider: Arc<dyn vbw_core::provider::LlmProvider> =
        if let Some(ref base_url) = config.llm.base_url {
            Arc::new(AnthropicProvider::with_base_url(api_key, base_url.clone()))
        } else {
            Arc::new(AnthropicProvider::new(api_key))
        };

    // 4. Create tool registry
    let mut tool_registry = ToolRegistry::new();
    tool_registry
        .register(Box::new(ReadFile))
        .map_err(|e| format!("register read_file: {e}"))?;
    tool_registry
        .register(Box::new(WriteFile))
        .map_err(|e| format!("register write_file: {e}"))?;
    tool_registry
        .register(Box::new(EditFile))
        .map_err(|e| format!("register edit_file: {e}"))?;
    tool_registry
        .register(Box::new(Bash))
        .map_err(|e| format!("register bash: {e}"))?;
    tool_registry
        .register(Box::new(Grep))
        .map_err(|e| format!("register grep: {e}"))?;
    tool_registry
        .register(Box::new(Glob))
        .map_err(|e| format!("register glob: {e}"))?;
    let tool_registry = Arc::new(tool_registry);

    // 5. Create rule engine
    let cwd = std::env::current_dir()?;
    let rule_engine = Arc::new(RuleEngine::new(&cwd)?);

    // 6. Create session manager
    let session_mgr = Arc::new(SessionManager::new(InMemorySessionStore::new()));

    // 7. Agent config
    let agent_config = AgentConfig {
        max_iterations: config.agent.max_iterations,
        llm_retry_attempts: config.agent.llm_retry_attempts,
        llm_retry_base_delay_ms: config.agent.llm_retry_base_delay_ms,
        bash_confirm_mode: config.agent.bash_confirm_mode,
        file_max_size_bytes: config.agent.file_max_size_bytes,
    };

    // 8. Assemble service
    let service = CoderDaemonService::new(
        provider,
        tool_registry,
        rule_engine,
        session_mgr,
        agent_config,
        config.llm,
    );

    // 9. Start gRPC server
    let addr = config.daemon.listen_addr.clone();
    let server_handle = tokio::spawn(async move {
        if let Err(e) = server::start_server(&addr, service).await {
            tracing::error!("grpc server error: {e}");
        }
    });

    // 10. Wait for Ctrl+C
    tokio::signal::ctrl_c().await?;
    tracing::info!("shutdown signal received, stopping server");

    server_handle.abort();

    Ok(())
}
