use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use crate::error::AgentErrorCode;
use crate::error::LlmError;
use crate::message::Message;
use crate::message::Role;
use crate::message::ToolCallRequest;
use crate::prompt::PromptBuilder;
use crate::provider::ChatEvent;
use crate::provider::LlmConfig;
use crate::provider::LlmProvider;
use crate::rules::RuleEngine;
use crate::session::SessionManager;
use crate::session::SessionStatus;
use crate::tool::ToolContext;
use crate::tool::ToolResult;
use crate::tool_registry::ToolRegistry;

use futures::StreamExt;
use tokio::sync::mpsc;
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

/// Agent 事件，用于流式通知外部（TUI/WS）
pub enum AgentEvent {
    /// 文本增量
    TextDelta(String),
    /// 工具调用请求
    ToolCallRequest {
        call_id: String,
        tool_name: String,
        arguments: String,
    },
    /// 工具调用结果
    ToolCallResult {
        call_id: String,
        content: String,
        is_error: bool,
    },
    /// 状态更新
    StatusUpdate(String),
    /// 发生错误
    Error {
        code: AgentErrorCode,
        message: String,
    },
    /// 完成
    Done,
    /// 需要用户输入
    UserQuery {
        query_id: String,
        message: String,
        respond: oneshot::Sender<bool>,
    },
}

/// Agent 循环上下文
pub struct AgentLoopContext {
    /// 会话 ID
    pub session_id: String,
    /// 对话历史
    pub history: Vec<Message>,
    /// 工作目录
    pub working_dir: PathBuf,
    /// LLM 配置
    pub config: LlmConfig,
    /// 取消令牌
    pub cancel_token: CancellationToken,
}

/// Agent 执行配置
#[derive(Debug, Clone)]
pub struct AgentConfig {
    /// 最大迭代轮数
    pub max_iterations: u32,
    /// LLM 调用重试次数
    pub llm_retry_attempts: u32,
    /// LLM 重试基础延迟（毫秒）
    pub llm_retry_base_delay_ms: u64,
    /// bash 工具确认模式（执行高危命令前是否需要用户确认）
    pub bash_confirm_mode: bool,
    /// 文件读取/写入的最大字节数
    pub file_max_size_bytes: u64,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            max_iterations: 50,
            llm_retry_attempts: 3,
            llm_retry_base_delay_ms: 1000,
            bash_confirm_mode: true,
            file_max_size_bytes: 1048576,
        }
    }
}

// ── Internal helper ──────────────────────────────────────────────────────────

struct ToolExecResult {
    index: usize,
    call_id: String,
    result: ToolResult,
}

fn llm_error_to_code(err: &LlmError) -> (AgentErrorCode, String) {
    match err {
        LlmError::Network(msg) => (AgentErrorCode::LlmNetwork, msg.clone()),
        LlmError::RateLimit { retry_after_secs } => (
            AgentErrorCode::LlmRateLimit,
            format!("rate limited, retry after {retry_after_secs}s"),
        ),
        LlmError::Auth(msg) => (AgentErrorCode::LlmAuth, msg.clone()),
        LlmError::Api { status, message } => (
            AgentErrorCode::LlmApi,
            format!("status {status}: {message}"),
        ),
        LlmError::Stream(msg) => (AgentErrorCode::LlmStream, msg.clone()),
    }
}

// ── Agent loop ───────────────────────────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
pub async fn run_agent_loop(
    provider: Arc<dyn LlmProvider>,
    tool_registry: Arc<ToolRegistry>,
    rule_engine: Arc<RuleEngine>,
    session_mgr: Arc<SessionManager>,
    mut ctx: AgentLoopContext,
    agent_config: &AgentConfig,
    user_message: Message,
    tx: mpsc::Sender<AgentEvent>,
) {
    // Helper: send event, return false if receiver dropped
    macro_rules! try_send {
        ($event:expr) => {
            if tx.send($event).await.is_err() {
                let _ = session_mgr.finish_loop(&ctx.session_id, SessionStatus::Error);
                return;
            }
        };
    }

    // Early cancellation check before appending
    if ctx.cancel_token.is_cancelled() {
        try_send!(AgentEvent::Error {
            code: AgentErrorCode::Cancelled,
            message: "Agent loop cancelled".into(),
        });
        let _ = session_mgr.finish_loop(&ctx.session_id, SessionStatus::Error);
        return;
    }

    // 1. Append user message to session store and local history
    if let Err(e) = session_mgr.append_message(&ctx.session_id, user_message.clone()) {
        let _ = tx
            .send(AgentEvent::Error {
                code: AgentErrorCode::Internal,
                message: format!("Failed to append user message: {e}"),
            })
            .await;
        let _ = session_mgr.finish_loop(&ctx.session_id, SessionStatus::Error);
        return;
    }
    ctx.history.push(user_message);

    for _ in 0..agent_config.max_iterations {
        // a. Cancellation check
        if ctx.cancel_token.is_cancelled() {
            try_send!(AgentEvent::Error {
                code: AgentErrorCode::Cancelled,
                message: "Agent loop cancelled".into(),
            });
            let _ = session_mgr.finish_loop(&ctx.session_id, SessionStatus::Error);
            return;
        }

        // b. Build prompt
        let session = match session_mgr.get(&ctx.session_id) {
            Ok(s) => s,
            Err(e) => {
                try_send!(AgentEvent::Error {
                    code: AgentErrorCode::Internal,
                    message: format!("Failed to get session: {e}"),
                });
                let _ = session_mgr.finish_loop(&ctx.session_id, SessionStatus::Error);
                return;
            }
        };
        let messages = PromptBuilder::build(
            &session.system_prompt_template,
            &rule_engine.get_active_rules(),
            &ctx.history,
        );

        // c. Get tool definitions
        let tools = tool_registry.definitions();

        // d. Call LLM with retry
        let stream = {
            let mut attempt = 0u32;
            loop {
                match provider.chat_stream(&messages, &tools, &ctx.config).await {
                    Ok(s) => break s,
                    Err(e @ (LlmError::RateLimit { .. } | LlmError::Network(_))) => {
                        if attempt >= agent_config.llm_retry_attempts {
                            let (code, msg) = llm_error_to_code(&e);
                            try_send!(AgentEvent::Error { code, message: msg });
                            let _ = session_mgr.finish_loop(&ctx.session_id, SessionStatus::Error);
                            return;
                        }
                        let delay = agent_config.llm_retry_base_delay_ms * (1u64 << attempt);
                        tokio::time::sleep(Duration::from_millis(delay)).await;
                        attempt += 1;
                    }
                    Err(e) => {
                        let (code, msg) = llm_error_to_code(&e);
                        try_send!(AgentEvent::Error { code, message: msg });
                        let _ = session_mgr.finish_loop(&ctx.session_id, SessionStatus::Error);
                        return;
                    }
                }
            }
        };

        // e. Collect events
        let mut text_buffer = String::new();
        let mut tool_calls: Vec<ToolCallRequest> = Vec::new();

        let mut pin_stream = Box::pin(stream);
        while let Some(event) = pin_stream.next().await {
            match event {
                Ok(ChatEvent::TextDelta(delta)) => {
                    text_buffer.push_str(&delta);
                    try_send!(AgentEvent::TextDelta(delta));
                }
                Ok(ChatEvent::ToolCall {
                    id,
                    name,
                    arguments,
                }) => {
                    tool_calls.push(ToolCallRequest {
                        id,
                        name,
                        arguments,
                    });
                }
                Ok(ChatEvent::Done) => break,
                Err(e) => {
                    let (code, msg) = llm_error_to_code(&e);
                    try_send!(AgentEvent::Error { code, message: msg });
                    let _ = session_mgr.finish_loop(&ctx.session_id, SessionStatus::Error);
                    return;
                }
            }
        }

        // f. Decide: no tool calls → done
        if tool_calls.is_empty() {
            let assistant_msg = Message {
                role: Role::Assistant,
                content: text_buffer,
                tool_call_id: None,
                tool_calls: None,
            };
            ctx.history.push(assistant_msg.clone());
            if let Err(e) = session_mgr.append_message(&ctx.session_id, assistant_msg) {
                try_send!(AgentEvent::Error {
                    code: AgentErrorCode::Internal,
                    message: format!("Failed to append assistant message: {e}"),
                });
                let _ = session_mgr.finish_loop(&ctx.session_id, SessionStatus::Error);
                return;
            }
            try_send!(AgentEvent::Done);
            let _ = session_mgr.finish_loop(&ctx.session_id, SessionStatus::Completed);
            return;
        }

        // Has tool calls: append assistant message with tool_calls
        let assistant_msg = Message {
            role: Role::Assistant,
            content: text_buffer,
            tool_call_id: None,
            tool_calls: Some(tool_calls.clone()),
        };
        ctx.history.push(assistant_msg.clone());
        if let Err(e) = session_mgr.append_message(&ctx.session_id, assistant_msg) {
            try_send!(AgentEvent::Error {
                code: AgentErrorCode::Internal,
                message: format!("Failed to append assistant message: {e}"),
            });
            let _ = session_mgr.finish_loop(&ctx.session_id, SessionStatus::Error);
            return;
        }

        // g. Execute tools in parallel
        let num_tools = tool_calls.len();
        let mut exec_tasks = Vec::with_capacity(num_tools);

        for (i, tc) in tool_calls.iter().enumerate() {
            let tx = tx.clone();
            let cancel = ctx.cancel_token.clone();
            let registry = tool_registry.clone();
            let session_id = ctx.session_id.clone();
            let working_dir = ctx.working_dir.clone();
            let tc = tc.clone();

            exec_tasks.push(tokio::spawn(async move {
                // Cancellation check
                if cancel.is_cancelled() {
                    let _ = tx
                        .send(AgentEvent::ToolCallResult {
                            call_id: tc.id.clone(),
                            content: "Cancelled".into(),
                            is_error: true,
                        })
                        .await;
                    return ToolExecResult {
                        index: i,
                        call_id: tc.id,
                        result: ToolResult::error("Cancelled"),
                    };
                }

                // Send ToolCallRequest
                let _ = tx
                    .send(AgentEvent::ToolCallRequest {
                        call_id: tc.id.clone(),
                        tool_name: tc.name.clone(),
                        arguments: tc.arguments.clone(),
                    })
                    .await;

                // Check if tool requires approval
                let requires_approval = registry
                    .get(&tc.name)
                    .map(|t| t.requires_approval())
                    .unwrap_or(false);

                if requires_approval {
                    let (resp_tx, resp_rx) = oneshot::channel::<bool>();
                    let _ = tx
                        .send(AgentEvent::UserQuery {
                            query_id: tc.id.clone(),
                            message: format!("Allow tool execution: {}?", tc.name),
                            respond: resp_tx,
                        })
                        .await;

                    let approved = resp_rx.await.unwrap_or(false);
                    if !approved {
                        let result = ToolResult::error("User denied");
                        let _ = tx
                            .send(AgentEvent::ToolCallResult {
                                call_id: tc.id.clone(),
                                content: result.content.clone(),
                                is_error: result.is_error,
                            })
                            .await;
                        return ToolExecResult {
                            index: i,
                            call_id: tc.id,
                            result,
                        };
                    }
                }

                // Status update
                let _ = tx
                    .send(AgentEvent::StatusUpdate(format!(
                        "Executing tool: {}",
                        tc.name
                    )))
                    .await;

                // Parse arguments and execute
                let args = serde_json::from_str(&tc.arguments).unwrap_or(serde_json::json!({}));
                let tool_ctx = ToolContext {
                    working_dir: working_dir.clone(),
                    session_id: Some(session_id),
                };

                let result = registry
                    .execute(&tc.name, args, &tool_ctx)
                    .await
                    .unwrap_or_else(|| ToolResult::error("Tool not found in registry"));

                // Send result
                let _ = tx
                    .send(AgentEvent::ToolCallResult {
                        call_id: tc.id.clone(),
                        content: result.content.clone(),
                        is_error: result.is_error,
                    })
                    .await;

                ToolExecResult {
                    index: i,
                    call_id: tc.id,
                    result,
                }
            }));
        }

        // Join all tasks
        let task_results = futures::future::join_all(exec_tasks).await;

        // h. Append tool results to history (in original order)
        let mut sorted_results: Vec<ToolExecResult> =
            task_results.into_iter().filter_map(|r| r.ok()).collect();
        sorted_results.sort_by_key(|r| r.index);

        for tr in sorted_results {
            let tool_msg = Message::tool(tr.result.content, &tr.call_id);
            ctx.history.push(tool_msg.clone());
            if let Err(e) = session_mgr.append_message(&ctx.session_id, tool_msg) {
                try_send!(AgentEvent::Error {
                    code: AgentErrorCode::Internal,
                    message: format!("Failed to append tool result: {e}"),
                });
                let _ = session_mgr.finish_loop(&ctx.session_id, SessionStatus::Error);
                return;
            }
        }
        // i. Continue loop
    }

    // Max iterations reached without completion
    try_send!(AgentEvent::Error {
        code: AgentErrorCode::MaxIterations,
        message: format!(
            "Agent loop reached maximum iterations ({})",
            agent_config.max_iterations
        ),
    });
    let _ = session_mgr.finish_loop(&ctx.session_id, SessionStatus::Error);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::InMemorySessionStore;
    use crate::tool::Tool;
    use std::path::Path;
    use std::sync::Arc as StdArc;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    // ── Mock tool for tests ─────────────────────────────────────────────────

    struct MockAgentTool {
        name: &'static str,
        requires_approval: bool,
        executed: StdArc<AtomicBool>,
    }

    #[async_trait::async_trait]
    impl Tool for MockAgentTool {
        fn name(&self) -> &str {
            self.name
        }
        fn description(&self) -> &str {
            "Mock tool for agent tests"
        }
        fn parameters(&self) -> serde_json::Value {
            serde_json::json!({})
        }
        async fn execute(&self, _args: serde_json::Value, _ctx: &ToolContext) -> ToolResult {
            self.executed.store(true, Ordering::SeqCst);
            ToolResult::success("mock executed")
        }
        fn requires_approval(&self) -> bool {
            self.requires_approval
        }
    }

    fn mock_tool(name: &'static str, approval: bool) -> (Box<dyn Tool>, StdArc<AtomicBool>) {
        let executed = StdArc::new(AtomicBool::new(false));
        let e = executed.clone();
        (
            Box::new(MockAgentTool {
                name,
                requires_approval: approval,
                executed,
            }),
            e,
        )
    }

    // ── Test provider with phased responses ────────────────────────────────

    struct TestProvider {
        phases: Vec<Vec<ChatEvent>>,
        call_count: AtomicUsize,
    }

    impl TestProvider {
        fn new(phases: Vec<Vec<ChatEvent>>) -> Self {
            Self {
                phases,
                call_count: AtomicUsize::new(0),
            }
        }
    }

    #[async_trait::async_trait]
    impl LlmProvider for TestProvider {
        async fn chat_stream(
            &self,
            _messages: &[Message],
            _tools: &[crate::message::ToolDefinition],
            _config: &LlmConfig,
        ) -> Result<
            std::pin::Pin<Box<dyn futures::Stream<Item = Result<ChatEvent, LlmError>> + Send>>,
            LlmError,
        > {
            let idx = self.call_count.fetch_add(1, Ordering::SeqCst);

            let events = self
                .phases
                .get(idx)
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .map(Ok);
            let stream = futures::stream::iter(events);
            Ok(Box::pin(stream))
        }
    }

    // ── Test helpers ───────────────────────────────────────────────────────

    struct TestSetup {
        session_mgr: StdArc<SessionManager>,
        session_id: String,
        ctx: AgentLoopContext,
        rule_engine: StdArc<RuleEngine>,
    }

    fn test_setup() -> TestSetup {
        let session_mgr = StdArc::new(SessionManager::new(InMemorySessionStore::new()));
        let session = session_mgr
            .create(Path::new("/tmp"), LlmConfig::default())
            .unwrap();
        let ctx = session_mgr.start_loop(&session.id).unwrap();
        let rule_engine = StdArc::new(RuleEngine::new(Path::new("/tmp")).unwrap());
        TestSetup {
            session_mgr,
            session_id: session.id,
            ctx,
            rule_engine,
        }
    }

    async fn run_collect(
        provider: StdArc<dyn LlmProvider>,
        tools: Vec<Box<dyn Tool>>,
        setup: TestSetup,
        max_iterations: u32,
        user_msg: Message,
    ) -> (Vec<AgentEvent>, StdArc<SessionManager>, String) {
        let (tx, mut rx) = mpsc::channel(64);

        let mut registry = ToolRegistry::new();
        for tool in tools {
            registry.register(tool).unwrap();
        }
        let tool_registry = StdArc::new(registry);
        let config = AgentConfig {
            max_iterations,
            ..Default::default()
        };

        let session_mgr = setup.session_mgr.clone();
        let sid = setup.session_id.clone();

        tokio::spawn(async move {
            run_agent_loop(
                provider,
                tool_registry,
                setup.rule_engine,
                session_mgr,
                setup.ctx,
                &config,
                user_msg,
                tx,
            )
            .await;
        });

        let mut events = Vec::new();
        while let Some(event) = rx.recv().await {
            events.push(event);
        }

        (events, setup.session_mgr, sid)
    }

    // ── Existing tests ─────────────────────────────────────────────────────

    #[test]
    fn test_agent_config_default() {
        let cfg = AgentConfig::default();
        assert_eq!(cfg.max_iterations, 50);
        assert_eq!(cfg.llm_retry_attempts, 3);
        assert_eq!(cfg.llm_retry_base_delay_ms, 1000);
        assert!(cfg.bash_confirm_mode);
        assert_eq!(cfg.file_max_size_bytes, 1048576);
    }

    #[test]
    fn test_agent_event_text_delta() {
        let evt = AgentEvent::TextDelta("hello".into());
        match evt {
            AgentEvent::TextDelta(content) => assert_eq!(content, "hello"),
            _ => panic!("expected TextDelta"),
        }
    }

    #[test]
    fn test_agent_event_tool_call() {
        let evt = AgentEvent::ToolCallRequest {
            call_id: "call-1".into(),
            tool_name: "bash".into(),
            arguments: "{}".into(),
        };
        match evt {
            AgentEvent::ToolCallRequest {
                call_id,
                tool_name,
                arguments,
            } => {
                assert_eq!(call_id, "call-1");
                assert_eq!(tool_name, "bash");
                assert_eq!(arguments, "{}");
            }
            _ => panic!("expected ToolCallRequest"),
        }
    }

    #[test]
    fn test_agent_event_user_query() {
        let (tx, _rx) = tokio::sync::oneshot::channel::<bool>();
        let evt = AgentEvent::UserQuery {
            query_id: "q1".into(),
            message: "confirm?".into(),
            respond: tx,
        };
        match evt {
            AgentEvent::UserQuery {
                query_id, message, ..
            } => {
                assert_eq!(query_id, "q1");
                assert_eq!(message, "confirm?");
            }
            _ => panic!("expected UserQuery"),
        }
    }

    #[test]
    fn test_agent_loop_context_fields() {
        let cancel = tokio_util::sync::CancellationToken::new();
        let ctx = AgentLoopContext {
            session_id: "sess-1".into(),
            history: vec![],
            working_dir: PathBuf::from("/tmp"),
            config: crate::provider::LlmConfig::default(),
            cancel_token: cancel.clone(),
        };
        assert_eq!(ctx.session_id, "sess-1");
        assert!(ctx.history.is_empty());
        assert_eq!(ctx.working_dir, Path::new("/tmp"));
        assert_eq!(ctx.config.model, "claude-sonnet-4-20250514");
    }

    // ── New agent loop tests ───────────────────────────────────────────────

    /// 1. 简单响应：TextDelta → Done
    #[tokio::test]
    async fn test_simple_response() {
        let provider: StdArc<dyn LlmProvider> = StdArc::new(TestProvider::new(vec![vec![
            ChatEvent::TextDelta("Hello".into()),
            ChatEvent::Done,
        ]]));

        let (events, _sm, _sid) =
            run_collect(provider, vec![], test_setup(), 10, Message::user("Hi")).await;

        let deltas: Vec<&str> = events
            .iter()
            .filter_map(|e| match e {
                AgentEvent::TextDelta(t) => Some(t.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(deltas, vec!["Hello"]);

        assert!(events.iter().any(|e| matches!(e, AgentEvent::Done)));
        assert!(
            events
                .iter()
                .all(|e| !matches!(e, AgentEvent::Error { .. }))
        );
    }

    /// 2. 工具调用：ToolCall → Done
    #[tokio::test]
    async fn test_tool_call() {
        let (tool, _executed) = mock_tool("finder", false);
        let provider: StdArc<dyn LlmProvider> = StdArc::new(TestProvider::new(vec![
            vec![
                ChatEvent::ToolCall {
                    id: "call-1".into(),
                    name: "finder".into(),
                    arguments: "{}".into(),
                },
                ChatEvent::Done,
            ],
            vec![ChatEvent::Done],
        ]));

        let (events, _sm, _sid) = run_collect(
            provider,
            vec![tool],
            test_setup(),
            10,
            Message::user("Find files"),
        )
        .await;

        assert!(
            events
                .iter()
                .any(|e| matches!(e, AgentEvent::ToolCallRequest { .. }))
        );
        assert!(events
            .iter()
            .any(|e| matches!(e, AgentEvent::ToolCallResult { call_id, is_error: false, .. } if call_id == "call-1")));
        assert!(events.iter().any(|e| matches!(e, AgentEvent::Done)));
        assert!(
            events
                .iter()
                .all(|e| !matches!(e, AgentEvent::Error { .. }))
        );
    }

    /// 3. 多工具批量执行：2 个 ToolCall 并行执行
    #[tokio::test]
    async fn test_multi_tool_batch() {
        let (tool_a, _ex_a) = mock_tool("finder", false);
        let (tool_b, _ex_b) = mock_tool("grep", false);
        let provider: StdArc<dyn LlmProvider> = StdArc::new(TestProvider::new(vec![
            vec![
                ChatEvent::ToolCall {
                    id: "call-1".into(),
                    name: "finder".into(),
                    arguments: "{}".into(),
                },
                ChatEvent::ToolCall {
                    id: "call-2".into(),
                    name: "grep".into(),
                    arguments: "{}".into(),
                },
                ChatEvent::Done,
            ],
            vec![ChatEvent::Done],
        ]));

        let (events, _sm, _sid) = run_collect(
            provider,
            vec![tool_a, tool_b],
            test_setup(),
            10,
            Message::user("Find and grep"),
        )
        .await;

        let tool_calls: Vec<&str> = events
            .iter()
            .filter_map(|e| match e {
                AgentEvent::ToolCallRequest { tool_name, .. } => Some(tool_name.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(tool_calls.len(), 2);
        assert!(tool_calls.contains(&"finder"));
        assert!(tool_calls.contains(&"grep"));

        let tool_results: Vec<&str> = events
            .iter()
            .filter_map(|e| match e {
                AgentEvent::ToolCallResult {
                    call_id,
                    is_error: false,
                    ..
                } => Some(call_id.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(tool_results.len(), 2);
        assert!(tool_results.contains(&"call-1"));
        assert!(tool_results.contains(&"call-2"));

        assert!(events.iter().any(|e| matches!(e, AgentEvent::Done)));
        assert!(
            events
                .iter()
                .all(|e| !matches!(e, AgentEvent::Error { .. }))
        );
    }

    /// 4. 最大迭代次数：max_iterations=1，一直返回 ToolCall → Error(MaxIterations)
    #[tokio::test]
    async fn test_max_iterations() {
        let (tool, _executed) = mock_tool("finder", false);
        // Always returns ToolCall
        let provider: StdArc<dyn LlmProvider> = StdArc::new(TestProvider::new(vec![vec![
            ChatEvent::ToolCall {
                id: "call-1".into(),
                name: "finder".into(),
                arguments: "{}".into(),
            },
            ChatEvent::Done,
        ]]));

        let (events, _sm, _sid) = run_collect(
            provider,
            vec![tool],
            test_setup(),
            1, // max_iterations = 1
            Message::user("Find"),
        )
        .await;

        assert!(events.iter().any(|e| matches!(
            e,
            AgentEvent::Error {
                code: AgentErrorCode::MaxIterations,
                ..
            }
        )));
    }

    /// 5. 取消：触发 CancellationToken → Error(Cancelled)
    #[tokio::test]
    async fn test_cancellation() {
        let setup = test_setup();
        setup.ctx.cancel_token.cancel();

        let provider: StdArc<dyn LlmProvider> = StdArc::new(TestProvider::new(vec![vec![
            ChatEvent::TextDelta("Hello".into()),
            ChatEvent::Done,
        ]]));

        let (events, _sm, _sid) =
            run_collect(provider, vec![], setup, 10, Message::user("Hi")).await;

        assert!(events.iter().any(|e| matches!(
            e,
            AgentEvent::Error {
                code: AgentErrorCode::Cancelled,
                ..
            }
        )));
        assert!(!events.iter().any(|e| matches!(e, AgentEvent::Done)));
    }

    /// 6. 用户确认：tool requires_approval=true，回复 true → 工具执行
    #[tokio::test]
    async fn test_user_query() {
        let (tool, executed) = mock_tool("finder", true);
        let provider: StdArc<dyn LlmProvider> = StdArc::new(TestProvider::new(vec![
            vec![
                ChatEvent::ToolCall {
                    id: "call-1".into(),
                    name: "finder".into(),
                    arguments: "{}".into(),
                },
                ChatEvent::Done,
            ],
            vec![ChatEvent::Done],
        ]));

        let (tx, mut rx) = mpsc::channel(64);
        let mut registry = ToolRegistry::new();
        registry.register(tool).unwrap();

        let setup = test_setup();
        let config = AgentConfig {
            max_iterations: 10,
            ..Default::default()
        };

        let sm = setup.session_mgr.clone();

        tokio::spawn(async move {
            run_agent_loop(
                provider,
                StdArc::new(registry),
                setup.rule_engine,
                sm,
                setup.ctx,
                &config,
                Message::user("Find files"),
                tx,
            )
            .await;
        });

        let mut done = false;

        while let Some(event) = rx.recv().await {
            match event {
                AgentEvent::UserQuery { respond, .. } => {
                    // Respond immediately to allow tool task to proceed
                    let _ = respond.send(true);
                }
                AgentEvent::Done => {
                    done = true;
                }
                _ => {}
            }
        }

        assert!(done, "Expected Done event");
        assert!(
            executed.load(Ordering::SeqCst),
            "Tool should have been executed after approval"
        );
    }

    /// 7. 用户拒绝：回复 false → ToolCallResult(is_error=true)
    #[tokio::test]
    async fn test_user_query_denied() {
        let (tool, executed) = mock_tool("finder", true);
        let provider: StdArc<dyn LlmProvider> = StdArc::new(TestProvider::new(vec![
            vec![
                ChatEvent::ToolCall {
                    id: "call-1".into(),
                    name: "finder".into(),
                    arguments: "{}".into(),
                },
                ChatEvent::Done,
            ],
            vec![ChatEvent::Done],
        ]));

        let (tx, mut rx) = mpsc::channel(64);
        let mut registry = ToolRegistry::new();
        registry.register(tool).unwrap();

        let setup = test_setup();
        let config = AgentConfig {
            max_iterations: 10,
            ..Default::default()
        };

        tokio::spawn(async move {
            run_agent_loop(
                provider,
                StdArc::new(registry),
                setup.rule_engine,
                setup.session_mgr,
                setup.ctx,
                &config,
                Message::user("Find files"),
                tx,
            )
            .await;
        });

        let mut error_result: Option<String> = None;

        while let Some(event) = rx.recv().await {
            match event {
                AgentEvent::UserQuery { respond, .. } => {
                    // Deny immediately to allow tool task to proceed
                    let _ = respond.send(false);
                }
                AgentEvent::ToolCallResult {
                    is_error: true,
                    content,
                    ..
                } => {
                    error_result = Some(content);
                }
                _ => {}
            }
        }

        assert_eq!(error_result, Some("User denied".into()));
        assert!(
            !executed.load(Ordering::SeqCst),
            "Tool should NOT have been executed after denial"
        );
    }

    /// 8. mpsc 关闭：drop receiver → agent 不 panic
    #[tokio::test]
    async fn test_mpsc_closed() {
        let provider: StdArc<dyn LlmProvider> = StdArc::new(TestProvider::new(vec![vec![
            ChatEvent::TextDelta("Hello".into()),
            ChatEvent::Done,
        ]]));

        let setup = test_setup();
        let config = AgentConfig::default();
        let (tx, rx) = mpsc::channel(1);
        drop(rx); // drop receiver immediately

        let result = tokio::time::timeout(
            Duration::from_secs(5),
            run_agent_loop(
                provider,
                StdArc::new(ToolRegistry::new()),
                setup.rule_engine,
                setup.session_mgr,
                setup.ctx,
                &config,
                Message::user("Hi"),
                tx,
            ),
        )
        .await;

        assert!(
            result.is_ok(),
            "Agent loop should not hang or panic when receiver is dropped"
        );
    }

    /// 9. 历史记录：结束时 history 包含 user + assistant 消息
    #[tokio::test]
    async fn test_history_appended() {
        let provider: StdArc<dyn LlmProvider> = StdArc::new(TestProvider::new(vec![vec![
            ChatEvent::TextDelta("Hello".into()),
            ChatEvent::Done,
        ]]));

        let setup = test_setup();
        let config = AgentConfig::default();
        let (tx, mut rx) = mpsc::channel(64);

        let registry = ToolRegistry::new();
        let sm = setup.session_mgr.clone();
        let sid = setup.session_id.clone();

        let sm_for_spawn = sm.clone();
        tokio::spawn(async move {
            run_agent_loop(
                provider,
                StdArc::new(registry),
                setup.rule_engine,
                sm_for_spawn,
                setup.ctx,
                &config,
                Message::user("Hi"),
                tx,
            )
            .await;
        });

        // Drain events
        while rx.recv().await.is_some() {}

        // Check session history
        let session = sm.get(&sid).unwrap();
        assert_eq!(session.history.len(), 2, "Expected 2 messages in history");
        assert_eq!(session.history[0].role, Role::User);
        assert_eq!(session.history[0].content, "Hi");
        assert_eq!(session.history[1].role, Role::Assistant);
        assert_eq!(session.history[1].content, "Hello");
    }
}
