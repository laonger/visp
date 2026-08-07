mod config;
mod observability;
mod server;
mod service;

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;

use visp_core::{
    agent::{AgentConfig, AgentTool, Envelope},
    agent_registry::AgentRegistry,
    context::ContextTrimmer,
    provider::LlmProvider,
    rules::RuleEngine,
    session::{InMemorySessionStore, SessionManager, SessionStatus, SessionStore},
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
};

use crate::config::{DaemonConfig, LlmModelConfig};
use crate::service::CoderDaemonService;

/// Register agent tools from the AgentRegistry into the ToolRegistry.
/// In single-agent mode (global_tx is None), agent tools are skipped.
fn register_agent_tools(
    tool_registry: &ToolRegistry,
    agent_registry: &AgentRegistry,
    global_tx: Option<&mpsc::Sender<Envelope>>,
) -> Result<(), String> {
    if global_tx.is_none() {
        return Ok(());
    }
    for agent_def in agent_registry.list_subagents() {
        let tool = Arc::new(AgentTool::new(
            agent_def.name.clone(),
            agent_def.description.clone(),
        ));
        tool_registry
            .register(tool)
            .map_err(|e| format!("register agent tool '{}': {e}", agent_def.name))?;
    }
    Ok(())
}

/// Create an LLM provider from a model config.
fn create_llm_provider(config: &LlmModelConfig) -> Result<Arc<dyn LlmProvider>, String> {
    match config.protocol.as_str() {
        "openai" => {
            let api_key = config.api_key.clone().ok_or_else(|| {
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
            let api_key = config.api_key.clone().ok_or_else(|| {
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
    // 1. Load config
    let config_path = std::env::args().nth(1).map(std::path::PathBuf::from);
    let config: DaemonConfig =
        visp_config::load_config(config_path.as_deref()).map_err(|e| format!("config: {e}"))?;

    // 2. Init observability (tracing subscriber stack)
    //    Guard lives for the lifetime of main; on drop it unwinds the subscriber.
    let _observability_guard =
        crate::observability::init::init_observability(&config.observability);

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

    // ── 技能工具（description 内嵌可用技能列表）──
    let cwd = std::env::current_dir()?;
    tool_registry
        .register(Arc::new(visp_tools::skill::SkillTool::new(&cwd)))
        .map_err(|e| format!("register skill: {e}"))?;

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

    // 7.1. Reset orphaned Running sessions to Idle.
    // When daemon restarts, sessions that were Running in the previous
    // instance have no actual agent loop but retain Running status in DB.
    // Resetting them to Idle ensures they can accept new input.
    if let Ok(sessions) = session_mgr.list() {
        for session in &sessions {
            if session.status == SessionStatus::Running {
                tracing::info!(
                    session_id = %session.id,
                    "resetting orphaned Running session to Idle"
                );
                let _ = session_mgr.finish_loop(&session.id, SessionStatus::Idle);
            }
        }
    }

    // 7.2. Delete empty sessions (no messages).
    // Sessions created but never used (e.g. from aborted /new commands)
    // clutter the session list. Clean them up on startup.
    if let Ok(sessions) = session_mgr.list() {
        for session in &sessions {
            if let Ok(messages) = session_mgr.get_messages(&session.id)
                && messages.is_empty()
            {
                tracing::info!(
                    session_id = %session.id,
                    "deleting empty session (no messages)"
                );
                let _ = session_mgr.delete(&session.id);
            }
        }
    }

    // 8. Agent config
    let langfuse = &config.observability.langfuse;
    let langfuse_tags = if langfuse.tags.is_empty() {
        None
    } else {
        Some(serde_json::to_string(&langfuse.tags).unwrap_or_default())
    };
    let langfuse_metadata = langfuse.metadata.as_ref().map(|meta| {
        meta.iter()
            .map(|(k, v)| {
                let val = match v {
                    toml::Value::String(s) => s.clone(),
                    toml::Value::Integer(i) => i.to_string(),
                    toml::Value::Float(f) => f.to_string(),
                    toml::Value::Boolean(b) => b.to_string(),
                    _ => serde_json::to_string(v).unwrap_or_default(),
                };
                (k.clone(), val)
            })
            .collect()
    });
    let agent_config = AgentConfig {
        soft_limit: config.agent.soft_limit,
        hard_limit: 200,
        doom_loop_threshold: config.agent.doom_loop_threshold,
        llm_retry_attempts: config.agent.llm_retry_attempts,
        llm_retry_base_delay_ms: config.agent.llm_retry_base_delay_ms,
        bash_confirm_mode: config.agent.bash_confirm_mode,
        file_max_size_bytes: config.agent.file_max_size_bytes,
        max_depth: config.agent.max_depth,
        langfuse_enabled: langfuse.enabled,
        langfuse_user_id: langfuse.user_id.clone(),
        langfuse_tags,
        langfuse_environment: langfuse.environment.clone(),
        langfuse_release: langfuse.release.clone(),
        langfuse_version: langfuse.version.clone(),
        langfuse_public: langfuse.public,
        langfuse_metadata,
        langfuse_capture_input: langfuse.capture.input,
        langfuse_capture_output: langfuse.capture.output,
        langfuse_capture_max_chars: langfuse.capture.max_chars,
        langfuse_redact_secrets: langfuse.capture.redact_secrets,
    };

    // 8.5. Create provider HashMap
    let mut providers: HashMap<String, Arc<dyn LlmProvider>> = HashMap::new();
    for mc in &model_configs {
        match create_llm_provider(mc) {
            Ok(provider) => {
                let key = mc.key();
                providers.insert(key, provider.clone());
                // 额外注册 {provider}/{model} 别名，使 agent 配置文件中的 model 字段
                // 可以使用更直观的格式，例如 "Opencode/deepseek-v4-flash"
                let provider_name = mc.provider.as_deref().unwrap_or(&mc.protocol);
                let model_alias = format!("{provider_name}/{}", mc.model);
                if model_alias != mc.key() {
                    providers.insert(model_alias, provider);
                }
            }
            Err(e) => {
                tracing::warn!(model_key = %mc.key(), error = %e, "failed to create provider");
            }
        }
    }

    // 8.6. Load agent definitions
    let mut builtin_overrides: Vec<visp_agent::agent_loader::BuiltinAgentOverride> = config
        .agent
        .builtin
        .iter()
        .map(|c| visp_agent::agent_loader::BuiltinAgentOverride {
            name: c.name.clone(),
            model: c.model.clone(),
            temperature: c.temperature,
            steps: c.steps,
        })
        .collect();

    // Wire llm.image_generation_model / llm.vision_model to builtin agents
    if let Some(ref key) = config.llm.image_generation_model {
        builtin_overrides.push(visp_agent::agent_loader::BuiltinAgentOverride {
            name: "painter".to_string(),
            model: Some(key.clone()),
            temperature: None,
            steps: None,
        });
    }
    if let Some(ref key) = config.llm.vision_model {
        builtin_overrides.push(visp_agent::agent_loader::BuiltinAgentOverride {
            name: "vision".to_string(),
            model: Some(key.clone()),
            temperature: None,
            steps: None,
        });
    }

    // Agent directories: global config dir (lower priority) → project dir (higher priority)
    let mut agent_dirs: Vec<std::path::PathBuf> = Vec::new();
    if let Some(global_agents_dir) = visp_config::path::agents_dir_global()
        && global_agents_dir.exists()
    {
        agent_dirs.push(global_agents_dir);
    }
    let project_agents_dir = visp_config::path::agents_dir_project(&cwd);
    if project_agents_dir.exists() {
        agent_dirs.push(project_agents_dir);
    }
    let agent_dir_refs: Vec<&Path> = agent_dirs.iter().map(|p| p.as_path()).collect();
    let agent_registry = Arc::new(visp_agent::agent_loader::load_agents(
        &agent_dir_refs,
        &builtin_overrides,
    ));

    // 8.7. Create orchestration channels
    let (global_tx, global_rx) = mpsc::channel(256);
    let (cancel_tx, cancel_rx) = mpsc::channel::<CancelSignal>(16);
    let (orchestrator_grpc_tx, orchestrator_grpc_rx) =
        mpsc::channel::<visp_core::agent::AgentEventFrame>(256);
    let (client_tx, client_rx) = mpsc::channel(64);

    // 8.7.5. Register agent tools from AgentRegistry (skip in single-agent mode)
    register_agent_tools(&tool_registry, &agent_registry, Some(&global_tx))
        .map_err(|e| format!("register agent tools: {e}"))?;

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
        Arc::new(config.clone()),
        providers,
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
        Arc::new(config.clone()),
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

#[cfg(test)]
mod tests {
    use super::*;
    use visp_core::agent_definition::{AgentDefinition, AgentMode};

    fn make_agent(name: &str, mode: AgentMode) -> AgentDefinition {
        AgentDefinition {
            name: name.to_string(),
            description: format!("Agent {name}"),
            mode,
            model: None,
            temperature: None,
            steps: None,
            permission: vec![],
            allowed_sub_agents: Vec::new(),
            system_prompt: String::new(),
        }
    }

    fn make_registry(subagent_names: &[&str]) -> AgentRegistry {
        let mut registry = AgentRegistry::new();
        for name in subagent_names {
            registry
                .register(make_agent(name, AgentMode::Subagent))
                .unwrap();
        }
        // Also register a primary agent to verify it's not included
        registry
            .register(make_agent("primary", AgentMode::Primary))
            .unwrap();
        registry
    }

    #[test]
    fn test_agent_tools_registered_from_registry() {
        let registry = make_registry(&["fixer", "explorer"]);
        let tool_registry = ToolRegistry::new();
        let (tx, _rx) = mpsc::channel::<Envelope>(16);

        let result = register_agent_tools(&tool_registry, &registry, Some(&tx));
        assert!(result.is_ok());

        let defs = tool_registry.definitions();
        let names: Vec<&str> = defs.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(names.len(), 2);
        assert!(names.contains(&"fixer"));
        assert!(names.contains(&"explorer"));
        // Primary agent should NOT be registered
        assert!(!names.contains(&"primary"));
    }

    #[test]
    fn test_agent_tools_not_registered_in_single_agent_mode() {
        let registry = make_registry(&["fixer", "explorer"]);
        let tool_registry = ToolRegistry::new();

        // global_tx is None → single agent mode
        let result = register_agent_tools(&tool_registry, &registry, None);
        assert!(result.is_ok());

        let defs = tool_registry.definitions();
        assert!(
            defs.is_empty(),
            "No agent tools should be registered in single-agent mode"
        );
    }

    #[test]
    fn test_agent_tool_names_match_agent_definition() {
        let registry = make_registry(&["fixer", "explorer", "designer"]);
        let tool_registry = ToolRegistry::new();
        let (tx, _rx) = mpsc::channel::<Envelope>(16);

        let result = register_agent_tools(&tool_registry, &registry, Some(&tx));
        assert!(result.is_ok());

        let defs = tool_registry.definitions();
        for def in &defs {
            // Each registered tool name should match an agent definition name
            let agent = registry.get(&def.name);
            assert!(
                agent.is_some(),
                "Tool '{}' should have a matching AgentDefinition",
                def.name
            );
            assert_eq!(
                agent.unwrap().name,
                def.name,
                "Tool name should match AgentDefinition.name"
            );
        }
        // Verify tool_type for all registered tools
        for def in &defs {
            let tool = tool_registry.get(&def.name).unwrap();
            assert_eq!(
                tool.tool_type(),
                visp_core::tool::ToolType::Agent,
                "Agent tool '{}' should have ToolType::Agent",
                def.name
            );
        }
    }
}
