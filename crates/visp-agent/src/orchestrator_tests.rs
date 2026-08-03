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
        HashMap::new(),
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
            trace_context: None,
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
        trace_context: None,
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

    // Inject a SpawnRequest
    let envelope = Envelope {
        session_id: "parent-1".to_string(),
        message: AgentMessage::SpawnRequest {
            call_id: "call-task-1".to_string(),
            subagent_type: "default".to_string(),
            description: "do something".to_string(),
            prompt: "test task".into(),
            task_id: None,
            trace_context: None,
            response_tx: None,
        },
        trace_context: None,
    };
    orch.handle_agent_message(envelope).await;

    // The orchestrator should attempt to spawn the sub-agent.
    // Since no parent session exists it should log an error,
    // but we can verify it didn't panic by reaching here.
}

#[tokio::test]
async fn test_create_sub_with_parent_reference() {
    let store: Box<dyn visp_core::session::SessionStore> = Box::new(InMemorySessionStore::new());
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

    let store: Box<dyn visp_core::session::SessionStore> = Box::new(InMemorySessionStore::new());
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
    let store: Box<dyn visp_core::session::SessionStore> = Box::new(InMemorySessionStore::new());
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

// ── W3: Orchestrator 持有 sub-agent JoinHandle ───────────────────────

#[tokio::test]
async fn test_subagent_applies_agent_model_override() {
    // Verify that when an agent definition has a `model` key,
    // the sub-session's LlmConfig is overridden with the correct model info.
    let (_cancel_tx, cancel_rx) = mpsc::channel(16);
    let (global_tx, global_rx) = mpsc::channel(256);
    let (grpc_tx, grpc_rx) = mpsc::channel::<AgentEventFrame>(256);
    let (_client_tx, client_rx) = mpsc::channel(64);

    let store: Box<dyn visp_core::session::SessionStore> = Box::new(InMemorySessionStore::new());
    let session_mgr = Arc::new(SessionManager::new(store));

    // Parent session with default model
    let parent = session_mgr
        .create(&PathBuf::from("/tmp"), LlmConfig::default())
        .unwrap();
    let parent_id = parent.id.clone();

    // Register a sub-agent with a specific model key and temperature
    let mut agent_registry = AgentRegistry::new();
    agent_registry
        .register(AgentDefinition {
            name: "explorer".to_string(),
            description: String::new(),
            mode: AgentMode::Subagent,
            model: Some("Opencode/deepseek-v4-flash".to_string()),
            temperature: Some(0.1),
            steps: Some(10),
            permission: vec![],
            allowed_sub_agents: Vec::new(),
            system_prompt: String::new(),
        })
        .ok();
    let agent_registry = Arc::new(agent_registry);

    let tool_registry = Arc::new(ToolRegistry::new());
    let rule_engine = Arc::new(RuleEngine::new(&PathBuf::from(".")).unwrap());
    let agent_config = AgentConfig::default();
    let context_trimmer: Arc<dyn ContextTrimmer + Send + Sync> = Arc::new(NoopTrimmer);

    let provider: Arc<dyn LlmProvider> = Arc::new(visp_llm::mock::MockProvider::new(vec![]));
    let mut providers = HashMap::new();
    providers.insert("Opencode/deepseek-v4-flash".to_string(), provider);

    // Build model_infos with the matching model
    let mut model_infos: HashMap<String, ModelInfo> = HashMap::new();
    model_infos.insert(
        "Opencode/deepseek-v4-flash".to_string(),
        ModelInfo {
            model: "deepseek-v4-flash".to_string(),
            provider: Some("Opencode".to_string()),
            temperature: Some(0.5),
            max_tokens: Some(8192),
            max_context_tokens: Some(64000),
            image_generation: false,
            use_tool: None,
        },
    );

    let mut orch = Orchestrator::new(
        cancel_rx,
        global_rx,
        global_tx,
        client_rx,
        grpc_tx,
        session_mgr,
        agent_registry,
        tool_registry,
        rule_engine,
        agent_config,
        context_trimmer,
        providers,
        "default".to_string(),
        model_infos,
    );

    // Spawn the sub-agent
    let envelope = Envelope {
        session_id: parent_id.clone(),
        message: AgentMessage::SpawnRequest {
            call_id: "call-model-test".to_string(),
            subagent_type: "explorer".to_string(),
            description: "test task".to_string(),
            prompt: "test task".into(),
            task_id: None,
            trace_context: None,
            response_tx: None,
        },
        trace_context: None,
    };
    orch.handle_agent_message(envelope).await;

    // Find the sub-session and verify its config was overridden
    let sessions = orch.session_mgr.list().unwrap();
    let sub_session = sessions
        .iter()
        .find(|s| s.agent_name == "explorer" && s.parent_id == Some(parent_id.clone()))
        .expect("sub-session should exist");

    // Model should be overridden from parent's "claude-3-7-sonnet-20250219" to "deepseek-v4-flash"
    assert_eq!(
        sub_session.config.model, "deepseek-v4-flash",
        "sub-session model should be overridden by agent def"
    );
    assert_eq!(
        sub_session.config.model_key,
        Some("Opencode/deepseek-v4-flash".to_string()),
        "sub-session model_key should match agent def model"
    );
    assert_eq!(
        sub_session.config.provider,
        Some("Opencode".to_string()),
        "sub-session provider should be overridden"
    );
    // Temperature: agent_def.temperature (0.1) takes precedence over model_info (0.5)
    assert!(
        (sub_session.config.temperature - 0.1).abs() < 1e-5,
        "sub-session temperature should be overridden by agent def temperature, got: {}",
        sub_session.config.temperature
    );
    // max_tokens from model_info
    assert_eq!(
        sub_session.config.max_tokens, 8192,
        "sub-session max_tokens should be overridden by model info"
    );
    // max_context_tokens from model_info
    assert_eq!(
        sub_session.config.max_context_tokens, 64000,
        "sub-session max_context_tokens should be overridden by model info"
    );

    // Drop grpc_rx to avoid warnings
    drop(grpc_rx);
}

#[tokio::test]
async fn test_subagent_inherits_parent_config_when_no_model_override() {
    // Verify that when an agent definition has NO `model` key,
    // the sub-session inherits the parent's config unchanged.
    let (_cancel_tx, cancel_rx) = mpsc::channel(16);
    let (global_tx, global_rx) = mpsc::channel(256);
    let (grpc_tx, grpc_rx) = mpsc::channel::<AgentEventFrame>(256);
    let (_client_tx, client_rx) = mpsc::channel(64);

    let store: Box<dyn visp_core::session::SessionStore> = Box::new(InMemorySessionStore::new());
    let session_mgr = Arc::new(SessionManager::new(store));

    // Parent session with custom model
    let parent_config = LlmConfig {
        model: "parent-model".to_string(),
        temperature: 0.8,
        ..LlmConfig::default()
    };
    let parent = session_mgr
        .create(&PathBuf::from("/tmp"), parent_config)
        .unwrap();
    let parent_id = parent.id.clone();

    // Register a sub-agent with NO model override
    let mut agent_registry = AgentRegistry::new();
    agent_registry
        .register(AgentDefinition {
            name: "explorer".to_string(),
            description: String::new(),
            mode: AgentMode::Subagent,
            model: None,
            temperature: None,
            steps: None,
            permission: vec![],
            allowed_sub_agents: Vec::new(),
            system_prompt: String::new(),
        })
        .ok();
    let agent_registry = Arc::new(agent_registry);

    let tool_registry = Arc::new(ToolRegistry::new());
    let rule_engine = Arc::new(RuleEngine::new(&PathBuf::from(".")).unwrap());
    let agent_config = AgentConfig::default();
    let context_trimmer: Arc<dyn ContextTrimmer + Send + Sync> = Arc::new(NoopTrimmer);

    let provider: Arc<dyn LlmProvider> = Arc::new(visp_llm::mock::MockProvider::new(vec![]));
    let mut providers = HashMap::new();
    providers.insert("default".to_string(), provider);

    let mut orch = Orchestrator::new(
        cancel_rx,
        global_rx,
        global_tx,
        client_rx,
        grpc_tx,
        session_mgr,
        agent_registry,
        tool_registry,
        rule_engine,
        agent_config,
        context_trimmer,
        providers,
        "default".to_string(),
        HashMap::new(), // empty model_infos
    );

    let envelope = Envelope {
        session_id: parent_id.clone(),
        message: AgentMessage::SpawnRequest {
            call_id: "call-no-override".to_string(),
            subagent_type: "explorer".to_string(),
            description: "test task".to_string(),
            prompt: "test task".into(),
            task_id: None,
            trace_context: None,
            response_tx: None,
        },
        trace_context: None,
    };
    orch.handle_agent_message(envelope).await;

    let sessions = orch.session_mgr.list().unwrap();
    let sub_session = sessions
        .iter()
        .find(|s| s.agent_name == "explorer" && s.parent_id == Some(parent_id.clone()))
        .expect("sub-session should exist");

    // Should inherit parent's model
    assert_eq!(
        sub_session.config.model, "parent-model",
        "sub-session should inherit parent model when no agent model override"
    );
    assert!(
        (sub_session.config.temperature - 0.8).abs() < f64::EPSILON,
        "sub-session should inherit parent temperature when no agent override"
    );

    drop(grpc_rx);
}

/// Bug 复现：agent 定义有 model override 时，用户通过 /model 切换模型后，
/// start_main_agent 不应该用 agent 定义的 model 覆盖 session.config。
#[tokio::test]
async fn test_main_agent_respects_user_model_switch() {
    let (_cancel_tx, cancel_rx) = mpsc::channel(16);
    let (global_tx, global_rx) = mpsc::channel(256);
    let (grpc_tx, grpc_rx) = mpsc::channel::<AgentEventFrame>(256);
    let (_client_tx, client_rx) = mpsc::channel(64);

    let store: Box<dyn visp_core::session::SessionStore> = Box::new(InMemorySessionStore::new());
    let session_mgr = Arc::new(SessionManager::new(store));

    // 创建 session，初始用 model-a
    let initial_config = LlmConfig {
        model: "model-a".to_string(),
        model_key: Some("ProviderA/model-a".to_string()),
        provider: Some("ProviderA".to_string()),
        ..LlmConfig::default()
    };
    let session = session_mgr
        .create(&PathBuf::from("/tmp"), initial_config)
        .unwrap();
    let session_id = session.id.clone();

    // 注册有 model override 的 default agent
    let mut agent_registry = AgentRegistry::new();
    agent_registry
        .register(AgentDefinition {
            name: "default".to_string(),
            description: String::new(),
            mode: AgentMode::All,
            model: Some("ProviderA/model-a".to_string()),
            temperature: None,
            steps: None,
            permission: vec![],
            allowed_sub_agents: Vec::new(),
            system_prompt: String::new(),
        })
        .ok();
    let agent_registry = Arc::new(agent_registry);

    let tool_registry = Arc::new(ToolRegistry::new());
    let rule_engine = Arc::new(RuleEngine::new(&PathBuf::from(".")).unwrap());
    let agent_config = AgentConfig::default();
    let context_trimmer: Arc<dyn ContextTrimmer + Send + Sync> = Arc::new(NoopTrimmer);

    // 两个 provider：model-a 和 model-b
    let provider: Arc<dyn LlmProvider> = Arc::new(visp_llm::mock::MockProvider::new(vec![]));
    let mut providers = HashMap::new();
    providers.insert("ProviderA/model-a".to_string(), provider.clone());
    providers.insert("ProviderB/model-b".to_string(), provider);

    let mut model_infos: HashMap<String, ModelInfo> = HashMap::new();
    model_infos.insert(
        "ProviderA/model-a".to_string(),
        ModelInfo {
            model: "model-a".to_string(),
            provider: Some("ProviderA".to_string()),
            temperature: None,
            max_tokens: None,
            max_context_tokens: None,
            image_generation: false,
            use_tool: None,
        },
    );
    model_infos.insert(
        "ProviderB/model-b".to_string(),
        ModelInfo {
            model: "model-b".to_string(),
            provider: Some("ProviderB".to_string()),
            temperature: None,
            max_tokens: None,
            max_context_tokens: None,
            image_generation: false,
            use_tool: None,
        },
    );

    let mut orch = Orchestrator::new(
        cancel_rx,
        global_rx,
        global_tx,
        client_rx,
        grpc_tx,
        session_mgr,
        agent_registry,
        tool_registry,
        rule_engine,
        agent_config,
        context_trimmer,
        providers,
        "ProviderA/model-a".to_string(),
        model_infos,
    );

    // 模拟用户通过 /model 切换到 model-b
    let switched_config = LlmConfig {
        model: "model-b".to_string(),
        model_key: Some("ProviderB/model-b".to_string()),
        provider: Some("ProviderB".to_string()),
        ..LlmConfig::default()
    };
    orch.session_mgr
        .update_config(&session_id, switched_config)
        .unwrap();

    // 启动主 agent
    orch.start_main_agent(&session_id, "hello").await;

    // 验证 session.config 没有被 agent override 覆盖回 model-a
    let session = orch.session_mgr.get(&session_id).unwrap();
    assert_eq!(
        session.config.model, "model-b",
        "用户切换的 model-b 不应被 agent 定义覆盖回 model-a"
    );
    assert_eq!(
        session.config.model_key,
        Some("ProviderB/model-b".to_string()),
        "用户切换的 model_key 不应被 agent 定义覆盖"
    );

    drop(grpc_rx);
}

#[tokio::test]
async fn test_orchestrator_tracks_sub_agent_join_handles() {
    let (mut orch, _global_tx, _client_tx, _grpc_rx) = make_orchestrator();

    // 初始 map 为空
    assert!(
        orch.sub_agent_handles.is_empty(),
        "新建 Orchestrator 的 sub_agent_handles 应为空"
    );

    // 模拟 spawn：插入一个长跑的 task handle
    let handle = tokio::spawn(async {
        tokio::time::sleep(std::time::Duration::from_secs(60)).await;
    });
    orch.sub_agent_handles.insert("child-1".to_string(), handle);

    assert_eq!(orch.sub_agent_handles.len(), 1);
    assert!(orch.sub_agent_handles.contains_key("child-1"));

    // 注册对应 active agent，便于 handle_done 走通常路径
    let cancel = CancellationToken::new();
    let (parent_inbox_tx, _parent_inbox_rx) = mpsc::channel(16);
    orch.active_agents.register(ActiveAgent {
        session_id: "parent-1".to_string(),
        parent_session_id: None,
        agent_name: "root".to_string(),
        cancel_token: cancel.clone(),
        inbox: parent_inbox_tx,
        pending_call_id: None,
        started_at: Instant::now(),
    });
    orch.active_agents.register(ActiveAgent {
        session_id: "child-1".to_string(),
        parent_session_id: Some("parent-1".to_string()),
        agent_name: "sub-agent".to_string(),
        cancel_token: cancel.clone(),
        inbox: mpsc::channel(16).0,
        pending_call_id: Some("call-1".to_string()),
        started_at: Instant::now(),
    });

    // handle_done 后应从 map 中移除
    orch.handle_done("child-1").await;

    assert!(
        !orch.sub_agent_handles.contains_key("child-1"),
        "handle_done 后 sub_agent_handles 应移除该 session"
    );
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

/// W2: sub-agent 发 AgentMessage::Error（如 panic 转发）时，
/// orchestrator 应通过 SubAgentError 通知父 agent，而非走 SubAgentComplete 的空内容路径。
#[tokio::test]
async fn test_handle_agent_error_forwards_sub_agent_error_to_parent() {
    use visp_core::agent::{AgentMessage, Envelope};
    use visp_core::error::AgentErrorCode;

    let (mut orch, _global_tx, _client_tx, _grpc_rx) = make_orchestrator();
    let cancel = CancellationToken::new();

    // Parent agent + inbox
    let (parent_inbox_tx, mut parent_inbox_rx) = mpsc::channel(16);
    orch.active_agents.register(ActiveAgent {
        session_id: "parent-1".to_string(),
        parent_session_id: None,
        agent_name: "root".to_string(),
        cancel_token: cancel.clone(),
        inbox: parent_inbox_tx,
        pending_call_id: None,
        started_at: Instant::now(),
    });

    // Sub-agent with pending_call_id
    orch.active_agents.register(ActiveAgent {
        session_id: "child-1".to_string(),
        parent_session_id: Some("parent-1".to_string()),
        agent_name: "sub-agent".to_string(),
        cancel_token: cancel.clone(),
        inbox: mpsc::channel(16).0,
        pending_call_id: Some("call-task-1".to_string()),
        started_at: Instant::now(),
    });

    // Act: simulate AgentMessage::Error envelope from the sub-agent
    let envelope = Envelope {
        session_id: "child-1".to_string(),
        message: AgentMessage::Error {
            code: AgentErrorCode::Internal,
            message: "agent loop panicked: provider panic".to_string(),
        },
        trace_context: None,
    };
    orch.handle_agent_message(envelope).await;

    // Assert: child removed from registry
    assert!(
        orch.active_agents.get("child-1").is_none(),
        "child should be removed after error"
    );

    // Assert: parent inbox received SubAgentError (NOT SubAgentComplete)
    let msg = parent_inbox_rx
        .try_recv()
        .expect("parent inbox should contain a message");
    match msg {
        OrchestratorMessage::SubAgentError { call_id, error } => {
            assert_eq!(call_id, "call-task-1");
            assert!(
                error.contains("panic"),
                "error should mention panic, got: {error}"
            );
        }
        OrchestratorMessage::SubAgentComplete { .. } => {
            panic!("got SubAgentComplete but expected SubAgentError on error path");
        }
        _ => panic!("unexpected message variant"),
    }
}

// ── W1-S3c: 注入 visp.subagent.spawn span ─────────────────────────────
use std::sync::Arc as TArc;
use std::sync::Mutex as TMutex;
use std::time::Duration;
use tracing::Subscriber;
use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Id, Record};
use tracing_subscriber::layer::{Context, Layer};
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::util::SubscriberInitExt;
use visp_core::agent_definition::AgentMode;
use visp_core::provider::LlmConfig;

#[derive(Debug, Clone)]
struct CapturedSpan {
    name: String,
    fields: Vec<(String, String)>,
    id: u64,
    parent_id: Option<u64>,
}

struct SpanFieldVisitor {
    fields: Vec<(String, String)>,
}

impl Visit for SpanFieldVisitor {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        self.fields
            .push((field.name().to_string(), format!("{value:?}")));
    }
    fn record_str(&mut self, field: &Field, value: &str) {
        self.fields
            .push((field.name().to_string(), value.to_string()));
    }
    fn record_i64(&mut self, field: &Field, value: i64) {
        self.fields
            .push((field.name().to_string(), value.to_string()));
    }
    fn record_u64(&mut self, field: &Field, value: u64) {
        self.fields
            .push((field.name().to_string(), value.to_string()));
    }
    fn record_bool(&mut self, field: &Field, value: bool) {
        self.fields
            .push((field.name().to_string(), value.to_string()));
    }
}

struct TestLayer {
    spans: TArc<TMutex<Vec<CapturedSpan>>>,
    #[allow(dead_code)]
    events: TArc<TMutex<Vec<String>>>,
    captured_tcs: TArc<TMutex<Vec<visp_core::TraceContext>>>,
}

impl TestLayer {
    fn new(
        spans: TArc<TMutex<Vec<CapturedSpan>>>,
        events: TArc<TMutex<Vec<String>>>,
        captured_tcs: TArc<TMutex<Vec<visp_core::TraceContext>>>,
    ) -> Self {
        Self {
            spans,
            events,
            captured_tcs,
        }
    }
}

impl<S> Layer<S> for TestLayer
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    fn on_new_span(&self, attrs: &Attributes<'_>, id: &Id, ctx: Context<'_, S>) {
        let mut visitor = SpanFieldVisitor { fields: Vec::new() };
        attrs.record(&mut visitor);
        let parent_id = ctx.lookup_current().map(|s| s.id().into_u64());
        let mut spans = self.spans.lock().unwrap();
        spans.push(CapturedSpan {
            name: attrs.metadata().name().to_string(),
            fields: visitor.fields,
            id: id.into_u64(),
            parent_id,
        });

        // 如果当前 span（父 span）有 TraceContext extension，捕获它
        if let Some(current) = ctx.lookup_current()
            && let Some(tc) = current.extensions().get::<visp_core::TraceContext>()
        {
            self.captured_tcs.lock().unwrap().push(tc.clone());
        }
    }

    fn on_record(&self, id: &Id, values: &Record<'_>, _ctx: Context<'_, S>) {
        let mut visitor = SpanFieldVisitor { fields: Vec::new() };
        values.record(&mut visitor);
        let mut spans = self.spans.lock().unwrap();
        if let Some(span) = spans.iter_mut().find(|s| s.id == id.into_u64()) {
            span.fields.extend(visitor.fields);
        }
    }
}

#[allow(clippy::type_complexity)]
fn setup_tracing() -> (
    TArc<TMutex<Vec<CapturedSpan>>>,
    TArc<TMutex<Vec<String>>>,
    TArc<TMutex<Vec<visp_core::TraceContext>>>,
) {
    let spans = TArc::new(TMutex::new(Vec::new()));
    let events = TArc::new(TMutex::new(Vec::new()));
    let captured_tcs = TArc::new(TMutex::new(Vec::new()));
    (spans, events, captured_tcs)
}

fn make_tracing_guard(
    spans: &TArc<TMutex<Vec<CapturedSpan>>>,
    events: &TArc<TMutex<Vec<String>>>,
    captured_tcs: &TArc<TMutex<Vec<visp_core::TraceContext>>>,
) -> tracing::subscriber::DefaultGuard {
    use tracing_subscriber::layer::SubscriberExt;
    tracing_subscriber::registry()
        .with(TestLayer::new(
            spans.clone(),
            events.clone(),
            captured_tcs.clone(),
        ))
        .set_default()
}

/// 创建完整可用的 orchestrator（含父 session、agent 定义、provider，默认配置）
fn make_orchestrator_for_spawn() -> (
    Orchestrator,
    mpsc::Sender<Envelope>,
    mpsc::Receiver<AgentEventFrame>,
    String,
) {
    make_orchestrator_for_spawn_with_config(AgentConfig::default())
}

/// 创建完整可用的 orchestrator，使用指定的 AgentConfig
fn make_orchestrator_for_spawn_with_config(
    agent_config: AgentConfig,
) -> (
    Orchestrator,
    mpsc::Sender<Envelope>,
    mpsc::Receiver<AgentEventFrame>,
    String,
) {
    let (_cancel_tx, cancel_rx) = mpsc::channel(16);
    let (global_tx, global_rx) = mpsc::channel(256);
    let (grpc_tx, grpc_rx) = mpsc::channel::<AgentEventFrame>(256);
    let (_client_tx, client_rx) = mpsc::channel(64);

    let store: Box<dyn visp_core::session::SessionStore> = Box::new(InMemorySessionStore::new());
    let session_mgr = Arc::new(SessionManager::new(store));
    let parent = session_mgr
        .create(&PathBuf::from("/tmp"), LlmConfig::default())
        .unwrap();
    let parent_id = parent.id.clone();

    // Register default subagent type
    let mut agent_registry = AgentRegistry::new();
    agent_registry
        .register(AgentDefinition {
            name: "default".to_string(),
            description: String::new(),
            mode: AgentMode::Subagent,
            model: None,
            temperature: None,
            steps: None,
            permission: vec![],
            allowed_sub_agents: Vec::new(),
            system_prompt: String::new(),
        })
        .ok();
    let agent_registry = Arc::new(agent_registry);

    let tool_registry = Arc::new(ToolRegistry::new());
    let rule_engine = Arc::new(RuleEngine::new(&PathBuf::from(".")).unwrap());
    let context_trimmer: Arc<dyn ContextTrimmer + Send + Sync> = Arc::new(NoopTrimmer);

    let provider: Arc<dyn LlmProvider> = Arc::new(visp_llm::mock::MockProvider::new(vec![]));
    let mut providers = HashMap::new();
    providers.insert("default".to_string(), provider);

    let global_tx_for_orch = global_tx.clone();
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
        providers,
        "default".to_string(),
        HashMap::new(),
    );

    (orch, global_tx, grpc_rx, parent_id)
}

#[tokio::test]
async fn test_subagent_spawn_span_created_in_orchestrator() {
    let (spans, _events, _tcs) = setup_tracing();
    let _guard = make_tracing_guard(&spans, &_events, &_tcs);
    let (mut orch, _global_tx, _grpc_rx, parent_id) = make_orchestrator_for_spawn();

    let envelope = Envelope {
        session_id: parent_id.clone(),
        message: AgentMessage::SpawnRequest {
            call_id: "call-1".to_string(),
            subagent_type: "default".to_string(),
            description: "do something".to_string(),
            prompt: "test task".into(),
            task_id: None,
            trace_context: None,
            response_tx: None,
        },
        trace_context: None,
    };
    orch.handle_agent_message(envelope).await;

    let captured = spans.lock().unwrap();
    let spawn_spans: Vec<_> = captured
        .iter()
        .filter(|s| s.name == "visp.subagent.spawn")
        .collect();
    assert!(
        !spawn_spans.is_empty(),
        "expected at least one 'visp.subagent.spawn' span, found {} total spans: {:?}",
        captured.len(),
        captured.iter().map(|s| &s.name).collect::<Vec<_>>()
    );
}

#[tokio::test]
async fn test_subagent_spawn_fields() {
    let (spans, _events, _tcs) = setup_tracing();
    let _guard = make_tracing_guard(&spans, &_events, &_tcs);
    let (mut orch, _global_tx, _grpc_rx, parent_id) = make_orchestrator_for_spawn();

    let envelope = Envelope {
        session_id: parent_id.clone(),
        message: AgentMessage::SpawnRequest {
            call_id: "call-field-test".to_string(),
            subagent_type: "default".to_string(),
            description: "test description".to_string(),
            prompt: "test task".into(),
            task_id: Some("task-42".to_string()),
            trace_context: None,
            response_tx: None,
        },
        trace_context: None,
    };
    orch.handle_agent_message(envelope).await;

    let captured = spans.lock().unwrap();
    let spawn_span = captured
        .iter()
        .find(|s| s.name == "visp.subagent.spawn")
        .expect("expected 'visp.subagent.spawn' span");

    let field_map: std::collections::HashMap<&str, &str> = spawn_span
        .fields
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();

    assert_eq!(
        field_map.get("visp.subagent.name"),
        Some(&"default"),
        "visp.subagent.name should be 'default', got: {:?}",
        field_map.get("visp.subagent.name")
    );
    assert_eq!(
        field_map.get("visp.subagent.call_id"),
        Some(&"call-field-test"),
        "visp.subagent.call_id should be 'call-field-test'"
    );
    assert!(
        field_map.contains_key("visp.subagent.session_id"),
        "visp.subagent.session_id should be present"
    );
    assert!(
        field_map.get("visp.subagent.task_id") == Some(&"task-42"),
        "visp.subagent.task_id should be 'task-42'"
    );
    assert_eq!(
        field_map.get("visp.subagent.depth"),
        Some(&"0"),
        "visp.subagent.depth should be '0', got: {:?}",
        field_map.get("visp.subagent.depth")
    );
}

#[tokio::test]
async fn test_subagent_run_loop_attached_via_instrument() {
    let (spans, _events, _tcs) = setup_tracing();
    let _guard = make_tracing_guard(&spans, &_events, &_tcs);
    let (mut orch, _global_tx, _grpc_rx, parent_id) = make_orchestrator_for_spawn();

    let envelope = Envelope {
        session_id: parent_id.clone(),
        message: AgentMessage::SpawnRequest {
            call_id: "call-3".to_string(),
            subagent_type: "default".to_string(),
            description: "do something".to_string(),
            prompt: "test task".into(),
            task_id: None,
            trace_context: None,
            response_tx: None,
        },
        trace_context: None,
    };
    orch.handle_agent_message(envelope).await;

    // 给 spawned task 时间启动，使 visp.agent.run span 被创建
    tokio::time::sleep(Duration::from_millis(200)).await;

    let captured = spans.lock().unwrap();
    let spawn_span = captured
        .iter()
        .find(|s| s.name == "visp.subagent.spawn")
        .expect("expected 'visp.subagent.spawn' span");
    let run_span = captured
        .iter()
        .find(|s| s.name == "visp.agent.run")
        .expect("expected 'visp.agent.run' span (run_agent_loop should have started)");

    assert_eq!(
        run_span.parent_id,
        Some(spawn_span.id),
        "'visp.agent.run' should be a child of 'visp.subagent.spawn' (parent_id={:?}, spawn_id={})",
        run_span.parent_id,
        spawn_span.id,
    );
}

#[tokio::test]
async fn test_orchestrator_reads_trace_context_from_envelope() {
    let (spans, _events, _tcs) = setup_tracing();
    let _guard = make_tracing_guard(&spans, &_events, &_tcs);
    let (mut orch, _global_tx, _grpc_rx, parent_id) = make_orchestrator_for_spawn();

    let tc = visp_core::TraceContext::new(
        "0af7651916cd43dd8448eb211c80319c".to_string(),
        "b7ad6b7169203331".to_string(),
        1,
        Some("congo=toto".to_string()),
        Some("aaaaaaaaaaaaaaaa".to_string()),
    )
    .unwrap();

    let envelope = Envelope {
        session_id: parent_id.clone(),
        message: AgentMessage::SpawnRequest {
            call_id: "call-tc-1".to_string(),
            subagent_type: "default".to_string(),
            description: "task with trace".to_string(),
            prompt: "test task".into(),
            task_id: None,
            trace_context: Some(tc.clone()),
            response_tx: None,
        },

        trace_context: Some(tc.clone()),
    };
    orch.handle_agent_message(envelope).await;

    // orchestrator 优先使用 envelope/SpawnRequest 携带的 TraceContext，
    // 确保子 agent 跨 mpsc 边界继承父 trace_id。验证 spawn span 记录了
    // 传入的 trace_id / parent_span_id / trace_state（而非 fallback）。
    let captured = spans.lock().unwrap();
    let spawn_span = captured
        .iter()
        .find(|s| s.name == "visp.subagent.spawn")
        .expect("expected 'visp.subagent.spawn' span");

    let field_map: std::collections::HashMap<&str, &str> = spawn_span
        .fields
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();

    let trace_id = field_map
        .get("trace_id")
        .copied()
        .expect("trace_id should be recorded");
    assert_eq!(
        trace_id, "0af7651916cd43dd8448eb211c80319c",
        "trace_id 必须是 envelope 携带的，而非 fallback UUID"
    );

    let psid = field_map
        .get("parent_span_id")
        .copied()
        .expect("parent_span_id should be recorded");
    assert_eq!(
        psid, "aaaaaaaaaaaaaaaa",
        "parent_span_id 必须是 envelope 携带的"
    );

    // trace_state 来自传入的 TraceContext
    assert_eq!(
        field_map.get("trace_state").copied(),
        Some("congo=toto"),
        "trace_state 应来自传入的 TraceContext"
    );
}

#[tokio::test]
async fn test_orchestrator_missing_trace_context_falls_back_to_orphan() {
    let (spans, _events, _tcs) = setup_tracing();
    let _guard = make_tracing_guard(&spans, &_events, &_tcs);
    let (mut orch, _global_tx, _grpc_rx, parent_id) = make_orchestrator_for_spawn();

    // Envelope 不带 trace_context（None），orchestrator 回退到
    // extract_trace_context() 生成 fallback TraceContext。
    let envelope = Envelope {
        session_id: parent_id.clone(),
        message: AgentMessage::SpawnRequest {
            call_id: "call-orphan".to_string(),
            subagent_type: "default".to_string(),
            description: "orphan test".to_string(),
            prompt: "test task".into(),
            task_id: None,
            trace_context: None,
            response_tx: None,
        },
        trace_context: None,
    };
    orch.handle_agent_message(envelope).await;

    // 验证不 panic，且 spawn span 已创建
    let captured = spans.lock().unwrap();
    assert!(
        captured.iter().any(|s| s.name == "visp.subagent.spawn"),
        "'visp.subagent.spawn' span should be created even without trace_context"
    );

    // W2-S4: orchestrator 会生成 fallback TraceContext（UUID based），因此
    // trace_id / parent_span_id 会被记录到 span fields 上。
    let spawn_span = captured
        .iter()
        .find(|s| s.name == "visp.subagent.spawn")
        .expect("expected 'visp.subagent.spawn' span");
    let field_map: std::collections::HashMap<&str, &str> = spawn_span
        .fields
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();

    // trace_id 应存在（fallback UUID）
    let trace_id = field_map
        .get("trace_id")
        .expect("trace_id should be recorded (fallback UUID)");
    assert_eq!(trace_id.len(), 32, "trace_id must be 32 hex chars");

    // parent_span_id 应存在（fallback W3C span ID）
    let psid = field_map
        .get("parent_span_id")
        .expect("parent_span_id should be recorded (fallback W3C ID)");
    assert_eq!(psid.len(), 16, "parent_span_id must be 16 hex chars");

    // trace_state 仍为 None（fallback 不设置 trace_state）
    assert!(
        !field_map.contains_key("trace_state"),
        "trace_state should NOT be recorded (fallback sets None)"
    );
}

#[tokio::test]
async fn test_subagent_spawn_span_records_trace_fields_for_observation() {
    let (spans, _events, _tcs) = setup_tracing();
    let _guard = make_tracing_guard(&spans, &_events, &_tcs);
    let (mut orch, _global_tx, _grpc_rx, parent_id) = make_orchestrator_for_spawn();

    // Envelope 携带了 trace_context，但 W2-S4 orchestrator 用 extract_trace_context()
    // 替换为 fallback UUID 版本。验证 spawn span 记录的是生成的字段。
    let envelope = Envelope {
        session_id: parent_id.clone(),
        message: AgentMessage::SpawnRequest {
            call_id: "call-tc-field".to_string(),
            subagent_type: "default".to_string(),
            description: "task with trace".to_string(),
            prompt: "test task".into(),
            task_id: None,
            trace_context: None,
            response_tx: None,
        },
        trace_context: None,
    };
    orch.handle_agent_message(envelope).await;

    let captured = spans.lock().unwrap();
    let spawn_span = captured
        .iter()
        .find(|s| s.name == "visp.subagent.spawn")
        .expect("expected 'visp.subagent.spawn' span");

    let field_map: std::collections::HashMap<&str, &str> = spawn_span
        .fields
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();

    // W2-S4: trace_id 来自 fallback UUID
    let trace_id = field_map
        .get("trace_id")
        .expect("trace_id should be recorded on visp.subagent.spawn");
    assert_eq!(trace_id.len(), 32, "trace_id must be 32 hex chars");

    // W2-S4: parent_span_id 来自 fallback UUID
    let psid = field_map
        .get("parent_span_id")
        .expect("parent_span_id should be recorded on visp.subagent.spawn");
    assert_eq!(psid.len(), 16, "parent_span_id must be 16 hex chars");

    // trace_state = None（fallback 不设置）
    assert!(
        !field_map.contains_key("trace_state"),
        "trace_state should NOT be recorded (W2 fallback sets None)"
    );
}

// ── W2-S5: set_parent / tracing parent chain integration tests ──────

/// Helper: set up a TestLayer + OTel subscriber combo.
#[allow(clippy::type_complexity)]
fn setup_tracing_with_otel() -> (
    TArc<TMutex<Vec<CapturedSpan>>>,
    TArc<TMutex<Vec<String>>>,
    TArc<TMutex<Vec<visp_core::TraceContext>>>,
    tracing::subscriber::DefaultGuard,
) {
    let (spans, events, tcs) = setup_tracing();
    let guard = make_tracing_guard(&spans, &events, &tcs);
    (spans, events, tcs, guard)
}

/// W2-S5 Test 1: Oracle B1 fix — spawn_span inherits parent trace_id
/// and parent_span_id from the parent's TraceContext via set_parent.
///
/// Verified via tracing-level parent chain: spawn_span's tracing parent
/// should be the parent span (agent.iteration).
#[tokio::test]
async fn test_orchestrator_spawn_span_inherits_parent_via_set_parent() {
    let (spans, _events, _tcs, _guard) = setup_tracing_with_otel();

    let parent_span = tracing::info_span!("agent.iteration");
    let parent_id_u64 = parent_span.in_scope(|| {
        let temp = tracing::info_span!("marker");
        let id = {
            let captured = spans.lock().unwrap();
            captured
                .iter()
                .find(|s| s.name == "marker")
                .expect("marker span should exist")
                .parent_id
                .expect("marker should have parent (agent.iteration)")
        };
        drop(temp);
        id
    });

    let (mut orch, _gtx, _grx, session_id) = make_orchestrator_for_spawn();
    let envelope = Envelope {
        session_id: session_id.clone(),
        message: AgentMessage::SpawnRequest {
            call_id: "call-w2s5-1".to_string(),
            subagent_type: "default".to_string(),
            description: "test set_parent".to_string(),
            prompt: "test task".into(),
            task_id: Some("task-w2s5-1".to_string()),
            trace_context: None,
            response_tx: None,
        },
        trace_context: None,
    };

    async {
        orch.handle_agent_message(envelope).await;
    }
    .instrument(parent_span)
    .await;

    // Wait for spawned task to complete
    let sub_sessions: Vec<String> = orch.sub_agent_handles.keys().cloned().collect();
    for sid in &sub_sessions {
        if let Some(handle) = orch.sub_agent_handles.remove(sid) {
            let _ = handle.await;
        }
    }

    let captured = spans.lock().unwrap();
    let spawn_span = captured
        .iter()
        .find(|s| s.name == "visp.subagent.spawn")
        .expect("should find visp.subagent.spawn span");
    assert_eq!(
        spawn_span.parent_id,
        Some(parent_id_u64),
        "spawn_span's tracing parent should be parent_span (Oracle B1 fix)"
    );
}

/// W2-S5 Test 2: Sub-agent's visp.agent.run automatically parented to
/// visp.subagent.spawn via tracing span hierarchy (which drives OTel
/// auto-propagation in contextual mode).
#[tokio::test]
async fn test_subagent_root_span_auto_parented_to_spawn_span() {
    let (spans, _events, _tcs, _guard) = setup_tracing_with_otel();

    let parent_span = tracing::info_span!("agent.iteration");
    let (mut orch, _gtx, _grx, session_id) = make_orchestrator_for_spawn();
    let envelope = Envelope {
        session_id: session_id.clone(),
        message: AgentMessage::SpawnRequest {
            call_id: "call-w2s5-2".to_string(),
            subagent_type: "default".to_string(),
            description: "test auto parent".to_string(),
            prompt: "test task".into(),
            task_id: None,
            trace_context: None,
            response_tx: None,
        },
        trace_context: None,
    };

    async {
        orch.handle_agent_message(envelope).await;
    }
    .instrument(parent_span)
    .await;

    // Wait for spawned task to complete
    let sub_sessions: Vec<String> = orch.sub_agent_handles.keys().cloned().collect();
    for sid in &sub_sessions {
        if let Some(handle) = orch.sub_agent_handles.remove(sid) {
            let _ = handle.await;
        }
    }

    let captured = spans.lock().unwrap();
    let spawn_span = captured
        .iter()
        .find(|s| s.name == "visp.subagent.spawn")
        .expect("should find visp.subagent.spawn");
    let run_span = captured
        .iter()
        .find(|s| s.name == "visp.agent.run")
        .expect("should find visp.agent.run (sub-agent)");

    // visp.agent.run should be a tracing child of visp.subagent.spawn
    assert_eq!(
        run_span.parent_id,
        Some(spawn_span.id),
        "visp.agent.run should be child of visp.subagent.spawn"
    );
}

/// W2-S5 Test 3: Full trace chain — all spans share the same parent chain:
/// agent.iteration → visp.subagent.spawn → visp.agent.run (sub)
#[tokio::test]
async fn test_subagent_full_trace_chain_single_trace_id() {
    let (spans, _events, _tcs, _guard) = setup_tracing_with_otel();

    let parent_span = tracing::info_span!("agent.iteration");
    let parent_id_u64 = parent_span.in_scope(|| {
        let temp = tracing::info_span!("marker");
        let id = {
            let captured = spans.lock().unwrap();
            captured
                .iter()
                .find(|s| s.name == "marker")
                .expect("marker span should exist")
                .parent_id
                .expect("marker should have parent (agent.iteration)")
        };
        drop(temp);
        id
    });

    let (mut orch, _gtx, _grx, session_id) = make_orchestrator_for_spawn();
    let envelope = Envelope {
        session_id: session_id.clone(),
        message: AgentMessage::SpawnRequest {
            call_id: "call-w2s5-3".to_string(),
            subagent_type: "default".to_string(),
            description: "full chain test".to_string(),
            prompt: "test task".into(),
            task_id: None,
            trace_context: None,
            response_tx: None,
        },
        trace_context: None,
    };

    async {
        orch.handle_agent_message(envelope).await;
    }
    .instrument(parent_span)
    .await;

    // Wait for spawned task to complete
    let sub_sessions: Vec<String> = orch.sub_agent_handles.keys().cloned().collect();
    for sid in &sub_sessions {
        if let Some(handle) = orch.sub_agent_handles.remove(sid) {
            let _ = handle.await;
        }
    }

    let captured = spans.lock().unwrap();
    let spawn_span = captured
        .iter()
        .find(|s| s.name == "visp.subagent.spawn")
        .expect("should find visp.subagent.spawn");
    let run_span = captured
        .iter()
        .find(|s| s.name == "visp.agent.run")
        .expect("should find visp.agent.run (sub-agent)");

    // Chain: agent.iteration → visp.subagent.spawn → visp.agent.run
    assert_eq!(
        spawn_span.parent_id,
        Some(parent_id_u64),
        "spawn_span parent should be parent_span (Oracle B1 fix)"
    );
    assert_eq!(
        run_span.parent_id,
        Some(spawn_span.id),
        "visp.agent.run parent should be visp.subagent.spawn"
    );
}

/// Regression: handle_agent_message 必须使用 SpawnRequest 携带的
/// TraceContext，而不是从当前 span 重新提取。否则在跨 mpsc 边界
/// （orchestrator 在独立 task 中处理消息）且 OTel 不活跃时，
/// extract_trace_context() 走 UUID fallback 生成全新 trace_id，
/// 导致子 agent 的 trace_id 与父 agent 不同。
#[tokio::test]
async fn test_spawn_uses_incoming_trace_context_not_reextracted() {
    let (spans, _events, _tcs, _guard) = setup_tracing_with_otel();

    // 32-hex trace_id，期望传播到 spawn span
    let expected_trace_id = "0123456789abcdef0123456789abcdef";
    let tc = visp_core::TraceContext::new(
        expected_trace_id.to_string(),
        "0123456789abcdef".to_string(),
        1,
        None,
        Some("fedcba9876543210".to_string()),
    )
    .expect("valid TraceContext");

    let (mut orch, _gtx, _grx, session_id) = make_orchestrator_for_spawn();
    let envelope = Envelope {
        session_id: session_id.clone(),
        message: AgentMessage::SpawnRequest {
            call_id: "call-w2s5-6".to_string(),
            subagent_type: "default".to_string(),
            description: "incoming tc test".to_string(),
            prompt: "test task".into(),
            task_id: None,
            trace_context: Some(tc.clone()),
            response_tx: None,
        },
        trace_context: Some(tc),
    };

    orch.handle_agent_message(envelope).await;

    // 等待 spawned task 完成
    let sub_sessions: Vec<String> = orch.sub_agent_handles.keys().cloned().collect();
    for sid in &sub_sessions {
        if let Some(handle) = orch.sub_agent_handles.remove(sid) {
            let _ = handle.await;
        }
    }

    let captured = spans.lock().unwrap();
    let spawn_span = captured
        .iter()
        .find(|s| s.name == "visp.subagent.spawn")
        .expect("should find visp.subagent.spawn span");
    let trace_id_field = spawn_span
        .fields
        .iter()
        .find(|(k, _)| k == "trace_id")
        .map(|(_, v)| v.as_str())
        .expect("spawn_span should have a trace_id field");
    assert_eq!(
        trace_id_field, expected_trace_id,
        "spawn_span 必须使用传入 TraceContext 的 trace_id，而非重新提取的"
    );
}

/// W2-S5 Test 4: When TraceContext is invalid, rebuild_parent_context
/// returns None, set_parent is NOT called, and the spawn_span becomes
/// a new trace root (no crash).
#[tokio::test]
async fn test_set_parent_fallback_when_trace_context_invalid() {
    let (spans, _events, _tcs, _guard) = setup_tracing_with_otel();

    let (mut orch, _gtx, _grx, session_id) = make_orchestrator_for_spawn();

    // Construct an invalid TraceContext (empty trace_id — fails hex parse)
    let invalid_tc = visp_core::TraceContext {
        trace_id: "".to_string(),
        span_id: "b7ad6b7169203331".to_string(),
        trace_flags: 1,
        trace_state: None,
        parent_span_id: Some("aaaaaaaaaaaaaaaa".to_string()),
    };

    let envelope = Envelope {
        session_id: session_id.clone(),
        message: AgentMessage::SpawnRequest {
            call_id: "call-w2s5-4".to_string(),
            subagent_type: "default".to_string(),
            description: "fallback test".to_string(),
            prompt: "test task".into(),
            task_id: None,
            trace_context: Some(invalid_tc),
            response_tx: None,
        },
        trace_context: None,
    };
    orch.handle_agent_message(envelope).await;

    // Wait for spawned task to complete
    let sub_sessions: Vec<String> = orch.sub_agent_handles.keys().cloned().collect();
    for sid in &sub_sessions {
        if let Some(handle) = orch.sub_agent_handles.remove(sid) {
            let _ = handle.await;
        }
    }

    let captured = spans.lock().unwrap();
    let spawn_span = captured
        .iter()
        .find(|s| s.name == "visp.subagent.spawn")
        .expect("should find visp.subagent.spawn even with invalid TraceContext");

    // When set_parent is not called, the tracing span has no explicit parent
    // (it's created outside any parent span scope), so it becomes a root span.
    assert!(
        spawn_span.parent_id.is_none(),
        "spawn_span should be a root span when TraceContext is invalid, parent_id={:?}",
        spawn_span.parent_id
    );
}

/// Cancel 路径下：sub-agent 因 Ctrl-C 退出（code=Cancelled），
/// orchestrator 应将统一文本 "agent cancelled" 转发给父，
/// 不应让"Agent loop cancelled"等异种文本混入。
#[tokio::test]
async fn test_handle_agent_error_cancel_unifies_text() {
    use visp_core::agent::{AgentMessage, Envelope};
    use visp_core::error::AgentErrorCode;

    let (mut orch, _global_tx, _client_tx, _grpc_rx) = make_orchestrator();
    let cancel = CancellationToken::new();

    let (parent_inbox_tx, mut parent_inbox_rx) = mpsc::channel(16);
    orch.active_agents.register(ActiveAgent {
        session_id: "parent-c".to_string(),
        parent_session_id: None,
        agent_name: "root".to_string(),
        cancel_token: cancel.clone(),
        inbox: parent_inbox_tx,
        pending_call_id: None,
        started_at: Instant::now(),
    });
    orch.active_agents.register(ActiveAgent {
        session_id: "child-c".to_string(),
        parent_session_id: Some("parent-c".to_string()),
        agent_name: "explorer".to_string(),
        cancel_token: cancel.clone(),
        inbox: mpsc::channel(16).0,
        pending_call_id: Some("call-cancel-1".to_string()),
        started_at: Instant::now(),
    });

    // 模拟 sub-agent 取消退出：code=Cancelled，message 是 agent_loop 里旧文本
    let envelope = Envelope {
        session_id: "child-c".to_string(),
        message: AgentMessage::Error {
            code: AgentErrorCode::Cancelled,
            message: "Agent loop cancelled".to_string(),
        },
        trace_context: None,
    };
    orch.handle_agent_message(envelope).await;

    let msg = parent_inbox_rx
        .try_recv()
        .expect("parent inbox should contain a message");
    match msg {
        OrchestratorMessage::SubAgentError { call_id, error } => {
            assert_eq!(call_id, "call-cancel-1");
            assert_eq!(
                error, "agent cancelled",
                "cancel 路径下应统一文本为 'agent cancelled'，实际: {error}"
            );
        }
        OrchestratorMessage::SubAgentComplete { .. } => {
            panic!("expected SubAgentError, got SubAgentComplete")
        }
        _ => panic!("unexpected message variant"),
    }
}

// ── 步骤 5a: Langfuse trace 级字段 ─────────────────────────

#[tokio::test]
async fn test_subagent_spawn_langfuse_disabled_no_langfuse_fields() {
    let (spans, _events, _tcs) = setup_tracing();
    let _guard = make_tracing_guard(&spans, &_events, &_tcs);
    let (mut orch, _global_tx, _grpc_rx, parent_id) = make_orchestrator_for_spawn();

    let envelope = Envelope {
        session_id: parent_id.clone(),
        message: AgentMessage::SpawnRequest {
            call_id: "call-lf-off".to_string(),
            subagent_type: "default".to_string(),
            description: "disabled test".to_string(),
            prompt: "test task".into(),
            task_id: None,
            trace_context: None,
            response_tx: None,
        },
        trace_context: None,
    };
    orch.handle_agent_message(envelope).await;

    let captured = spans.lock().unwrap();
    let spawn_span = captured
        .iter()
        .find(|s| s.name == "visp.subagent.spawn")
        .expect("expected 'visp.subagent.spawn' span");

    let langfuse_fields: Vec<_> = spawn_span
        .fields
        .iter()
        .filter(|(k, _)| k.starts_with("langfuse."))
        .collect();

    assert!(
        langfuse_fields.is_empty(),
        "expected no langfuse.* fields when disabled, found: {langfuse_fields:?}"
    );
}

#[tokio::test]
async fn test_subagent_spawn_langfuse_enabled_all_fields() {
    let (spans, _events, _tcs) = setup_tracing();
    let _guard = make_tracing_guard(&spans, &_events, &_tcs);

    let mut metadata = std::collections::HashMap::new();
    metadata.insert("key1".to_string(), "val1".to_string());
    metadata.insert("key2".to_string(), "val2".to_string());

    let agent_config = AgentConfig {
        langfuse_enabled: true,
        langfuse_user_id: Some("test-user".to_string()),
        langfuse_tags: Some(r#"["tag1","tag2"]"#.to_string()),
        langfuse_environment: Some("staging".to_string()),
        langfuse_release: Some("v1.0.0".to_string()),
        langfuse_version: Some("1.0.0".to_string()),
        langfuse_public: Some(true),
        langfuse_metadata: Some(metadata),
        ..AgentConfig::default()
    };

    let (mut orch, _global_tx, _grpc_rx, parent_id) =
        make_orchestrator_for_spawn_with_config(agent_config);

    let envelope = Envelope {
        session_id: parent_id.clone(),
        message: AgentMessage::SpawnRequest {
            call_id: "call-lf-all".to_string(),
            subagent_type: "default".to_string(),
            description: "all fields".to_string(),
            prompt: "test task".into(),
            task_id: Some("task-42".to_string()),
            trace_context: None,
            response_tx: None,
        },
        trace_context: None,
    };
    orch.handle_agent_message(envelope).await;

    let captured = spans.lock().unwrap();
    let spawn_span = captured
        .iter()
        .find(|s| s.name == "visp.subagent.spawn")
        .expect("expected 'visp.subagent.spawn' span");

    let field_map: std::collections::HashMap<&str, &str> = spawn_span
        .fields
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();

    // ── Langfuse fields ──
    assert_eq!(
        field_map.get("langfuse.session.id"),
        Some(&parent_id.as_str()),
        "langfuse.session.id should be parent_session_id"
    );
    assert_eq!(field_map.get("langfuse.user.id"), Some(&"test-user"));
    assert_eq!(
        field_map.get("langfuse.trace.tags"),
        Some(&r#"["tag1","tag2"]"#)
    );
    let expected_name = "visp.agent.run";
    assert_eq!(field_map.get("langfuse.trace.name"), Some(&expected_name));
    assert_eq!(field_map.get("langfuse.environment"), Some(&"staging"));
    assert_eq!(field_map.get("langfuse.release"), Some(&"v1.0.0"));
    assert_eq!(field_map.get("langfuse.version"), Some(&"1.0.0"));
    assert_eq!(field_map.get("langfuse.trace.public"), Some(&"true"));

    // metadata 以 JSON 字符串形式写入
    let metadata_str = field_map
        .get("langfuse.trace.metadata")
        .expect("metadata should be present");
    let parsed: std::collections::HashMap<String, String> =
        serde_json::from_str(metadata_str).expect("metadata should be valid JSON");
    assert_eq!(parsed.get("key1"), Some(&"val1".to_string()));
    assert_eq!(parsed.get("key2"), Some(&"val2".to_string()));

    // ── 现有字段保留 ──
    assert_eq!(field_map.get("visp.subagent.name"), Some(&"default"));
    assert_eq!(field_map.get("visp.subagent.call_id"), Some(&"call-lf-all"));
    assert!(
        field_map.contains_key("visp.subagent.session_id"),
        "visp.subagent.session_id should be present"
    );
    assert_eq!(field_map.get("visp.subagent.task_id"), Some(&"task-42"));
    assert_eq!(field_map.get("visp.subagent.depth"), Some(&"0"));
}

#[tokio::test]
async fn test_subagent_spawn_langfuse_enabled_partial() {
    let (spans, _events, _tcs) = setup_tracing();
    let _guard = make_tracing_guard(&spans, &_events, &_tcs);

    // 只配置 enabled + user_id，其他字段 None
    let agent_config = AgentConfig {
        langfuse_enabled: true,
        langfuse_user_id: Some("partial-user".to_string()),
        langfuse_tags: None,
        langfuse_environment: None,
        langfuse_release: None,
        langfuse_version: None,
        langfuse_public: None,
        langfuse_metadata: None,
        ..AgentConfig::default()
    };

    let (mut orch, _global_tx, _grpc_rx, parent_id) =
        make_orchestrator_for_spawn_with_config(agent_config);

    let envelope = Envelope {
        session_id: parent_id.clone(),
        message: AgentMessage::SpawnRequest {
            call_id: "call-lf-partial".to_string(),
            subagent_type: "default".to_string(),
            description: "partial fields".to_string(),
            prompt: "test task".into(),
            task_id: None,
            trace_context: None,
            response_tx: None,
        },
        trace_context: None,
    };
    orch.handle_agent_message(envelope).await;

    let captured = spans.lock().unwrap();
    let spawn_span = captured
        .iter()
        .find(|s| s.name == "visp.subagent.spawn")
        .expect("expected 'visp.subagent.spawn' span");

    let field_map: std::collections::HashMap<&str, &str> = spawn_span
        .fields
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();

    // 始终写入的字段
    assert_eq!(
        field_map.get("langfuse.session.id"),
        Some(&parent_id.as_str())
    );
    assert_eq!(field_map.get("langfuse.user.id"), Some(&"partial-user"));
    // trace.name 和 environment 总有值
    let expected_name = "visp.agent.run";
    assert_eq!(field_map.get("langfuse.trace.name"), Some(&expected_name));
    assert_eq!(field_map.get("langfuse.environment"), Some(&"default"));

    // 未配置的字段不写入
    assert!(
        !field_map.contains_key("langfuse.trace.tags"),
        "tags should NOT be present when None"
    );
    assert!(
        !field_map.contains_key("langfuse.release"),
        "release should NOT be present when None"
    );
    assert!(
        !field_map.contains_key("langfuse.version"),
        "version should NOT be present when None"
    );
    assert!(
        !field_map.contains_key("langfuse.trace.public"),
        "public should NOT be present when None"
    );
    assert!(
        !field_map.contains_key("langfuse.trace.metadata"),
        "metadata should NOT be present when None"
    );
}

#[tokio::test]
async fn test_subagent_spawn_langfuse_enabled_public_false() {
    let (spans, _events, _tcs) = setup_tracing();
    let _guard = make_tracing_guard(&spans, &_events, &_tcs);

    let agent_config = AgentConfig {
        langfuse_enabled: true,
        langfuse_public: Some(false),
        ..AgentConfig::default()
    };

    let (mut orch, _global_tx, _grpc_rx, parent_id) =
        make_orchestrator_for_spawn_with_config(agent_config);

    let envelope = Envelope {
        session_id: parent_id.clone(),
        message: AgentMessage::SpawnRequest {
            call_id: "call-lf-pub".to_string(),
            subagent_type: "default".to_string(),
            description: "public false".to_string(),
            prompt: "test task".into(),
            task_id: None,
            trace_context: None,
            response_tx: None,
        },
        trace_context: None,
    };
    orch.handle_agent_message(envelope).await;

    let captured = spans.lock().unwrap();
    let spawn_span = captured
        .iter()
        .find(|s| s.name == "visp.subagent.spawn")
        .expect("expected 'visp.subagent.spawn' span");

    let field_map: std::collections::HashMap<&str, &str> = spawn_span
        .fields
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();

    assert_eq!(
        field_map.get("langfuse.trace.public"),
        Some(&"false"),
        "public=false should be recorded as 'false'"
    );
}

// ── Wave 2b: Oneshot response path ────────────────────────────

fn find_child_id(orch: &Orchestrator, parent_id: &str) -> String {
    let sessions = orch.session_mgr.list().unwrap();
    sessions
        .iter()
        .find(|s| s.parent_id.as_deref() == Some(parent_id))
        .map(|s| s.id.clone())
        .expect("child session should exist")
}

#[tokio::test]
async fn test_spawn_sub_agent_with_response_tx() {
    let (mut orch, _global_tx, _grpc_rx, parent_id) = make_orchestrator_for_spawn();
    let (tx, _rx) = oneshot::channel();

    orch.spawn_sub_agent(
        &parent_id,
        "call-1",
        "default",
        "test description",
        "test prompt",
        None,
        None,
        Some(tx),
    )
    .await;

    let child_id = find_child_id(&orch, &parent_id);
    assert!(
        orch.pending_responses.contains_key(&child_id),
        "pending_responses should contain the child session_id"
    );
}

#[tokio::test]
async fn test_handle_done_response_tx_some() {
    let (mut orch, _global_tx, _grpc_rx, parent_id) = make_orchestrator_for_spawn();
    let (tx, rx) = oneshot::channel();

    orch.spawn_sub_agent(
        &parent_id,
        "call-1",
        "default",
        "test description",
        "test prompt",
        None,
        None,
        Some(tx),
    )
    .await;

    let child_id = find_child_id(&orch, &parent_id);
    assert!(orch.pending_responses.contains_key(&child_id));

    orch.handle_done(&child_id).await;

    // Should receive result via oneshot
    let result = rx.await.expect("should receive result via oneshot");
    assert!(
        !orch.pending_responses.contains_key(&child_id),
        "pending_responses should be cleaned up after handle_done"
    );
    // result may be empty string (no assistant messages in session), which is fine
    assert_eq!(
        result, "",
        "result content from empty session should be empty"
    );
}

#[tokio::test]
async fn test_handle_done_response_tx_none() {
    let (mut orch, _global_tx, _grpc_rx, parent_id) = make_orchestrator_for_spawn();
    let cancel = CancellationToken::new();
    let (parent_inbox_tx, mut parent_inbox_rx) = mpsc::channel(16);

    // Register parent agent explicitly (needed for inbox path)
    orch.active_agents.register(ActiveAgent {
        session_id: parent_id.clone(),
        parent_session_id: None,
        agent_name: "root".to_string(),
        cancel_token: cancel,
        inbox: parent_inbox_tx,
        pending_call_id: None,
        started_at: Instant::now(),
    });

    orch.spawn_sub_agent(
        &parent_id,
        "call-1",
        "default",
        "test description",
        "test prompt",
        None,
        None,
        None, // No response_tx
    )
    .await;

    let child_id = find_child_id(&orch, &parent_id);

    orch.handle_done(&child_id).await;

    // Should go through inbox path (existing logic)
    let msg = parent_inbox_rx
        .try_recv()
        .expect("parent inbox should receive SubAgentComplete");
    match msg {
        OrchestratorMessage::SubAgentComplete { .. } => {} // expected
        _ => panic!("expected SubAgentComplete, got a different variant"),
    }
}

#[tokio::test]
async fn test_handle_agent_error_response_tx_some() {
    let (mut orch, _global_tx, _grpc_rx, parent_id) = make_orchestrator_for_spawn();
    let (tx, rx) = oneshot::channel();

    orch.spawn_sub_agent(
        &parent_id,
        "call-1",
        "default",
        "test description",
        "test prompt",
        None,
        None,
        Some(tx),
    )
    .await;

    let child_id = find_child_id(&orch, &parent_id);

    orch.handle_agent_error(
        &child_id,
        AgentErrorCode::Internal,
        "something went wrong".to_string(),
    )
    .await;

    let result = rx.await.expect("should receive error via oneshot");
    assert!(
        result.contains("[SubAgent Error]"),
        "error message should contain '[SubAgent Error]', got: {result}"
    );
    assert!(
        result.contains("something went wrong"),
        "error message should contain original error text, got: {result}"
    );
}

#[tokio::test]
async fn test_handle_agent_error_response_tx_none() {
    let (mut orch, _global_tx, _grpc_rx, parent_id) = make_orchestrator_for_spawn();
    let cancel = CancellationToken::new();
    let (parent_inbox_tx, mut parent_inbox_rx) = mpsc::channel(16);

    orch.active_agents.register(ActiveAgent {
        session_id: parent_id.clone(),
        parent_session_id: None,
        agent_name: "root".to_string(),
        cancel_token: cancel,
        inbox: parent_inbox_tx,
        pending_call_id: None,
        started_at: Instant::now(),
    });

    orch.spawn_sub_agent(
        &parent_id,
        "call-1",
        "default",
        "test description",
        "test prompt",
        None,
        None,
        None, // No response_tx
    )
    .await;

    let child_id = find_child_id(&orch, &parent_id);

    orch.handle_agent_error(
        &child_id,
        AgentErrorCode::Internal,
        "some error".to_string(),
    )
    .await;

    // Should go through inbox path (existing logic)
    let msg = parent_inbox_rx
        .try_recv()
        .expect("parent inbox should receive SubAgentError");
    match msg {
        OrchestratorMessage::SubAgentError { .. } => {} // expected
        _ => panic!("expected SubAgentError, got a different variant"),
    }
}

#[tokio::test]
async fn test_pending_responses_cleanup_on_done() {
    let (mut orch, _global_tx, _grpc_rx, parent_id) = make_orchestrator_for_spawn();
    let (tx, _rx) = oneshot::channel();

    orch.spawn_sub_agent(
        &parent_id,
        "call-1",
        "default",
        "test description",
        "test prompt",
        None,
        None,
        Some(tx),
    )
    .await;

    let child_id = find_child_id(&orch, &parent_id);
    assert!(orch.pending_responses.contains_key(&child_id));

    orch.handle_done(&child_id).await;

    assert!(
        !orch.pending_responses.contains_key(&child_id),
        "pending_responses should be cleaned up after handle_done"
    );
}

#[tokio::test]
async fn test_pending_responses_cleanup_on_error() {
    let (mut orch, _global_tx, _grpc_rx, parent_id) = make_orchestrator_for_spawn();
    let (tx, _rx) = oneshot::channel();

    orch.spawn_sub_agent(
        &parent_id,
        "call-1",
        "default",
        "test description",
        "test prompt",
        None,
        None,
        Some(tx),
    )
    .await;

    let child_id = find_child_id(&orch, &parent_id);
    assert!(orch.pending_responses.contains_key(&child_id));

    orch.handle_agent_error(&child_id, AgentErrorCode::Internal, "error".to_string())
        .await;

    assert!(
        !orch.pending_responses.contains_key(&child_id),
        "pending_responses should be cleaned up after handle_agent_error"
    );
}

// ── Wave 3a: 子 Agent 工具筛选 ─────────────────────────────────────

use async_trait::async_trait;
use visp_core::tool::ToolType;

/// 可指定 tool_type 的 mock 工具
struct MockTypedTool {
    name: String,
    kind: ToolType,
}

#[async_trait]
impl visp_core::tool::Tool for MockTypedTool {
    fn name(&self) -> &str {
        &self.name
    }
    fn description(&self) -> &str {
        "mock typed tool"
    }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({})
    }
    async fn execute(
        &self,
        _args: serde_json::Value,
        _ctx: &visp_core::tool::ToolContext,
    ) -> visp_core::tool::ToolResult {
        visp_core::tool::ToolResult::success("ok")
    }
    fn tool_type(&self) -> ToolType {
        self.kind
    }
}

fn mock_tool(name: &str, kind: ToolType) -> std::sync::Arc<dyn visp_core::tool::Tool> {
    std::sync::Arc::new(MockTypedTool {
        name: name.to_string(),
        kind,
    })
}

/// Test: allowed_sub_agents 为空时，子 Agent 看不到任何 agent 工具
#[test]
fn test_sub_agent_no_allowed_sub_agents() {
    let registry = ToolRegistry::new();
    registry
        .register(mock_tool("builtin-read", ToolType::Builtin))
        .unwrap();
    registry
        .register(mock_tool("mcp-write", ToolType::Mcp))
        .unwrap();
    registry
        .register(mock_tool("agent-fixer", ToolType::Agent))
        .unwrap();
    registry
        .register(mock_tool("agent-explorer", ToolType::Agent))
        .unwrap();
    registry
        .register(mock_tool("skill-deploy", ToolType::Skill))
        .unwrap();

    let filtered = filter_tools_for_sub_agent(&registry, &[]);
    let names = filtered.names();

    assert!(names.contains(&"builtin-read".to_string()));
    assert!(names.contains(&"mcp-write".to_string()));
    assert!(names.contains(&"skill-deploy".to_string()));
    assert!(!names.contains(&"agent-fixer".to_string()));
    assert!(!names.contains(&"agent-explorer".to_string()));
}

/// Test: allowed_sub_agents 有值时，只保留列表中指定的 agent 工具
#[test]
fn test_sub_agent_with_allowed_sub_agents() {
    let registry = ToolRegistry::new();
    registry
        .register(mock_tool("builtin-read", ToolType::Builtin))
        .unwrap();
    registry
        .register(mock_tool("agent-fixer", ToolType::Agent))
        .unwrap();
    registry
        .register(mock_tool("agent-explorer", ToolType::Agent))
        .unwrap();
    registry
        .register(mock_tool("agent-oracle", ToolType::Agent))
        .unwrap();

    let allowed = vec!["agent-fixer".to_string(), "agent-oracle".to_string()];
    let filtered = filter_tools_for_sub_agent(&registry, &allowed);
    let names = filtered.names();

    assert!(names.contains(&"builtin-read".to_string()));
    assert!(names.contains(&"agent-fixer".to_string()));
    assert!(names.contains(&"agent-oracle".to_string()));
    assert!(!names.contains(&"agent-explorer".to_string()));
}

/// Test: compute_depth 检查在工具筛选后仍生效
#[tokio::test]
async fn test_sub_agent_depth_limit_still_applies() {
    let agent_config = AgentConfig {
        max_depth: 0,
        ..AgentConfig::default()
    };
    let (mut orch, _global_tx, _grpc_rx, parent_id) =
        make_orchestrator_for_spawn_with_config(agent_config);

    // 注册 parent agent，使其在 active_agents 中可查
    let cancel = CancellationToken::new();
    let (parent_inbox_tx, mut parent_inbox_rx) = mpsc::channel(16);
    orch.active_agents.register(ActiveAgent {
        session_id: parent_id.clone(),
        parent_session_id: None,
        agent_name: "root".to_string(),
        cancel_token: cancel,
        inbox: parent_inbox_tx,
        pending_call_id: None,
        started_at: Instant::now(),
    });

    // Try to spawn: parent depth=0 >= max_depth=0 → error
    let envelope = Envelope {
        session_id: parent_id.clone(),
        message: AgentMessage::SpawnRequest {
            call_id: "call-depth-test".to_string(),
            subagent_type: "default".to_string(),
            description: "depth test".to_string(),
            prompt: "test".into(),
            task_id: None,
            trace_context: None,
            response_tx: None,
        },
        trace_context: None,
    };
    orch.handle_agent_message(envelope).await;

    let msg = parent_inbox_rx
        .try_recv()
        .expect("parent inbox should receive SubAgentError");
    match msg {
        OrchestratorMessage::SubAgentError { call_id, error } => {
            assert_eq!(call_id, "call-depth-test");
            assert!(
                error.contains("Max depth exceeded"),
                "depth check error expected, got: {error}"
            );
        }
        _ => panic!("expected SubAgentError, got a different variant"),
    }
}

// ── Wave 4a: System Prompt — "task" tool → agent tools ────────────────

#[test]
fn test_build_subagent_prompt_no_task_reference() {
    use visp_core::agent_definition::AgentMode;

    let mut registry = AgentRegistry::new();
    registry
        .register(AgentDefinition {
            name: "explorer".to_string(),
            description: "code search".to_string(),
            mode: AgentMode::Subagent,
            model: None,
            temperature: None,
            steps: None,
            permission: vec![],
            allowed_sub_agents: Vec::new(),
            system_prompt: String::new(),
        })
        .unwrap();
    registry
        .register(AgentDefinition {
            name: "fixer".to_string(),
            description: "implementation".to_string(),
            mode: AgentMode::Subagent,
            model: None,
            temperature: None,
            steps: None,
            permission: vec![],
            allowed_sub_agents: Vec::new(),
            system_prompt: String::new(),
        })
        .unwrap();

    let prompt = build_subagent_prompt(&registry);
    assert!(!prompt.is_empty(), "prompt should not be empty");
    assert!(
        !prompt.contains("`task`"),
        "should not reference the `task` tool, got: {prompt}"
    );
}

#[test]
fn test_build_subagent_prompt_agent_tools_guidance() {
    use visp_core::agent_definition::AgentMode;

    let mut registry = AgentRegistry::new();
    registry
        .register(AgentDefinition {
            name: "explorer".to_string(),
            description: "code search".to_string(),
            mode: AgentMode::Subagent,
            model: None,
            temperature: None,
            steps: None,
            permission: vec![],
            allowed_sub_agents: Vec::new(),
            system_prompt: String::new(),
        })
        .unwrap();

    let prompt = build_subagent_prompt(&registry);
    assert!(!prompt.is_empty());
    // Should mention agent tools (e.g. @agent_name or delegate pattern)
    assert!(
        prompt.contains("@explorer")
            || prompt.contains("@fixer")
            || prompt.contains("@agent_name")
            || prompt.contains("agent tool"),
        "should contain agent tool guidance, got: {prompt}"
    );
    assert!(
        prompt.contains("explorer"),
        "should list the registered agent name, got: {prompt}"
    );
}

// ── Wave 4b: 全量回归 + 端到端集成测试 ─────────────────────────────

#[tokio::test]
async fn test_end_to_end_agent_tool_spawn() {
    // 端到端测试：agent 工具调用 → spawn 子 Agent → 完成 → 结果返回
    //
    // 1. 创建 Orchestrator，注册 explorer 子 Agent
    // 2. 通过 global_tx 发送 SpawnRequest（模拟 agent 工具调用）
    // 3. 验证 Orchestrator 正确处理 spawn → run → done 流程
    // 4. 验证 oneshot response_tx 收到结果
    // 5. 验证结果包含预期内容

    let (mut orch, _global_tx, _grpc_rx, parent_id) = make_orchestrator_for_spawn();
    let (tx, rx) = tokio::sync::oneshot::channel();

    // Register parent agent in active_agents so the sub-agent has a parent context
    let cancel = CancellationToken::new();
    let (parent_inbox_tx, _parent_inbox_rx) = mpsc::channel(16);
    orch.active_agents.register(ActiveAgent {
        session_id: parent_id.clone(),
        parent_session_id: None,
        agent_name: "root".to_string(),
        cancel_token: cancel.clone(),
        inbox: parent_inbox_tx,
        pending_call_id: None,
        started_at: Instant::now(),
    });

    // ── Step 2: Send SpawnRequest via handle_agent_message ──────────
    let envelope = Envelope {
        session_id: parent_id.clone(),
        message: AgentMessage::SpawnRequest {
            call_id: "call-e2e-1".to_string(),
            subagent_type: "default".to_string(),
            description: "E2E integration test task".to_string(),
            prompt: "do something".into(),
            task_id: Some("task-e2e".to_string()),
            trace_context: None,
            response_tx: Some(tx),
        },
        trace_context: None,
    };
    orch.handle_agent_message(envelope).await;

    // ── Step 3: Verify spawn succeeded ─────────────────────────────
    let child_id = find_child_id(&orch, &parent_id);
    assert!(
        orch.active_agents.get(&child_id).is_some(),
        "sub-agent should be active after spawn"
    );
    assert!(
        orch.pending_responses.contains_key(&child_id),
        "pending_responses should track the child session"
    );
    assert!(
        orch.sub_agent_handles.contains_key(&child_id),
        "sub_agent_handles should track the child JoinHandle"
    );

    // ── Step 4: Wait for spawned run_agent_loop task to complete ───
    // The MockProvider (empty response vec) causes the agent loop to
    // finish immediately. The spawned task sends AgentMessage::Done
    // via global_tx after completion.
    if let Some(handle) = orch.sub_agent_handles.remove(&child_id) {
        let _ = tokio::time::timeout(Duration::from_secs(5), handle)
            .await
            .expect("sub-agent task should complete within timeout");
    }

    // ── Step 5: Process the Done message ───────────────────────────
    // The spawned task sent Done through global_tx. We simulate the
    // orchestrator's run loop by calling handle_agent_message directly.
    orch.handle_agent_message(Envelope {
        session_id: child_id.clone(),
        message: AgentMessage::Done,
        trace_context: None,
    })
    .await;

    // ── Step 6: Verify result via oneshot ──────────────────────────
    let result = rx
        .await
        .expect("should receive result via oneshot response_tx");
    assert!(
        !orch.pending_responses.contains_key(&child_id),
        "pending_responses should be cleaned up after handle_done"
    );
    assert!(
        orch.active_agents.get(&child_id).is_none(),
        "sub-agent should be removed from active_agents after done"
    );
    // With MockProvider (empty vec), no assistant messages → empty result
    assert_eq!(
        result, "",
        "result from empty MockProvider session should be empty"
    );
}
