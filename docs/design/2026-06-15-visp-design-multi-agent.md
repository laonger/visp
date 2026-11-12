# visp 多 Agent 支持设计

## 1. 目标

为 visp 添加多 Agent 能力，使 Agent 循环能够：

- 通过内建 + 文件扩展的方式定义多个 Agent
- 主 Agent 和子 Agent 统一由 AgentRegistry 管理
- 通过 `task` 工具在运行时启动子 Agent 完成子任务
- 子 Agent 拥有独立的系统提示词、模型配置和权限规则
- 支持子 Agent 上下文复用（`task_id` 恢复）
- 权限系统在 ToolRegistry 层执行检查
- 所有 Agent 通过全局事件总线与 Orchestrator 通信
- Orchestrator 统一管理所有 Agent 的生命周期和消息路由

**一句话总结**：参考 OpenCode 的多 Agent 架构，结合 visp 现有设计（规则系统、ToolRegistry、SessionManager），构建一套统一的多 Agent 体系。

## 2. 背景

### 2.1 当前状态

- 所有 crates 均已重命名为 `visp-*`
- Agent 循环为单 Agent 模式：`run_agent_loop()` 接收用户消息 → LLM 调用 → 工具执行 → 循环
- 工具注册在 `visp-daemon/src/main.rs` 中静态完成
- `Tool` trait 和 `ToolRegistry` 设计良好
- Session 是扁平的，无父子层级
- 规则系统已支持 `.visp/rules/` + `~/.config/visp/rules/`
- 不支持 Agent 概念，无权限检查

### 2.2 关键术语

| 术语 | 说明 |
|------|------|
| Agent | 一个具备身份、系统提示词、模型配置、权限规则的 AI 角色 |
| AgentDefinition | Agent 的静态定义，来自内建或文件 |
| AgentRegistry | 所有 Agent 定义的注册中心 |
| 主 Agent | Session 当前使用的 Agent，用户直接交互的对象 |
| 子 Agent | 通过 `task` 工具临时启动的 Agent，完成特定子任务 |
| 子 Session | 子 Agent 运行时创建的独立 Session，通过 parent_id 关联父 Session |
| Permission Rule | 权限规则三元组 `(permission, pattern, action)` |
| **事件总线** | 全局 `mpsc` 通道，所有 Agent 共用一条 `global_tx` 发消息 |
| **Orchestrator** | Daemon 中的消息循环，收事件总线 + CLI 消息，统一处理 |
| **ActiveAgent** | 正在运行的 Agent 实例注册表，记录 session_id、父子关系、inbox 通道 |
| **inbox** | 每个 Agent 独享的 `mpsc::Sender`，Orchestrator 用来给该 Agent 发消息 |
| **AgentMessage** | Agent → Orchestrator，统一消息类型 |
| **OrchestratorMessage** | Orchestrator → Agent，统一消息类型 |

## 3. 架构总览

```
┌──────────────────────────────────────────────────────────────────┐
│                        visp-daemon (组装层)                        │
│                                                                   │
│  ┌──────────────┐    ┌──────────────────┐                        │
│  │  daemon.toml  │    │  .visp/agents/   │                        │
│  │  [[llm.models]│    │  *.md            │                        │
│  └──────┬───────┘    └───────┬──────────┘                       │
│         │                    │                                    │
│         ▼                    ▼                                    │
│  ┌──────────────────────────────────────────────────────┐        │
│  │                   AgentRegistry                        │        │
│  │  ┌──────────────────────────────────────────────┐    │        │
│  │  │  "default"     (primary)    系统提示词+权限    │    │        │
│  │  │  "code-review" (subagent)  系统提示词+权限    │    │        │
│  │  │  "architect"   (subagent)  系统提示词+权限    │    │        │
│  │  └──────────────────────────────────────────────┘    │        │
│  └──────────────────────────────────────────────────────┘        │
│                                                                   │
│  ┌──────────────────────────────────────────────────────────┐    │
│  │  visp-agent (运行时编排) ─── Orchestrator                   │    │
│  │                                                           │    │
│  │          global_rx (事件总线收)  ←── 各 Agent 共用        │    │
│  │          grpc_rx (CLI 消息)      ←── 用户输入             │    │
│  │                                                           │    │
│  │          active_agents:                                    │    │
│  │          ┌────────────┬──────────┬──────────────────┐    │    │
│  │          │ session_id │ parent   │ inbox (tx→agent) │    │    │
│  │          ├────────────┼──────────┼──────────────────┤    │    │
│  │          │ abc        │ None     │ tx_1             │    │    │
│  │          │ def        │ abc      │ tx_2             │    │    │
│  │          └────────────┴──────────┴──────────────────┘    │    │
│  └──────────────────────────────────────────────────────────┘    │
│                                   │                               │
│  ┌────────────────────────────────┴──────────────────────────┐  │
│  │              Agent Loop (每个 session 一个)                │  │
│  │                                                           │  │
│  │  AgentLoopContext {                                        │  │
│  │    global_tx: Sender<AgentMessage>,   // 共用事件总线     │  │
│  │    inbox_rx: Receiver<OrchestratorMessage>, // 独享收      │  │
│  │  }                                                         │  │
│  └──────────────────────────────────────────────────────────┘  │
└──────────────────────────────────────────────────────────────────┘
```

### 3.1 模块变更

| Crate | 变更 | 类型 |
|-------|------|------|
| **visp-core** | 新增 `AgentDefinition`、`PermissionRule`、`AgentRegistry`；`Session` 加 `agent_name`、`parent_id`；`Tool` trait 加权限检查；新增 `AgentMessage` / `OrchestratorMessage` 枚举；`AgentLoopContext` 加 `global_tx` / `inbox_rx`；Agent loop 工具执行改为 `select!` 支持异步结果 | 扩展 |
| **visp-tools** | 新增 `task` 工具（仅注册定义，执行由 agent loop 拦截） | 新建 |
| **visp-agent** | **新增 crate**：`Orchestrator` 主循环、`ActiveAgentRegistry` 管理、事件总线、子 agent 创建/取消、消息转发 | **新建** |
| **visp-daemon** | 加载 `.visp/agents/*.md` 填充 AgentRegistry；创建 `Orchestrator` 并启动；`service.rs` 简化，运行时逻辑交给 `visp-agent` | 调整 |
| **visp-db** | Session 表加 `agent_name`、`parent_id` 列；新增 migration V3 | 扩展 |

## 4. Agent 定义

### 4.1 定义方式

两种来源合并（同名文件覆盖内建）：

```
1. 内建 Agent    → daemon 硬编码，开箱即用
2. .visp/agents/ → Markdown + YAML frontmatter，项目级扩展
```

### 4.2 内建 Agent

| Agent | mode | 说明 | 可用工具 |
|-------|------|------|---------|
| `default` | primary | 通用编程助手（向后兼容） | 全部 allow |
| `code-review` | subagent | 只读代码审查 | 仅 read/grep/glob/search 等只读工具 |

### 4.3 文件格式

```markdown
---
name: code-review
description: 审查代码变更
mode: subagent
model: "Anthropic.Claude Sonnet"   # 可选，不填则用 session 默认模型
temperature: 0.3                   # 可选
steps: 30                          # 可选，默认 50
permission:                        # 可选，不填则全部 allow
  - permission: edit
    pattern: "*"
    action: deny
  - permission: write
    pattern: "*"
    action: deny
  - permission: bash
    pattern: "*"
    action: deny
  - permission: read
    pattern: "*"
    action: allow
---
你是一个代码审查专家。仔细审查代码变更，关注：
- 逻辑正确性
- 安全隐患
- 代码风格

只读不写，不修改任何文件。
```

### 4.4 文件加载规则

```
.visp/agents/
├── code-review.md     → name = "code-review"
├── architect.md       → name = "architect"（新增）
└── default.md         → 覆盖内建 default
```

- 同级目录：`agents/*.md`
- 多级目录暂不支持

## 5. 核心数据结构

### 5.1 AgentDefinition

```rust
/// Agent 模式
#[derive(Debug, Clone, PartialEq)]
pub enum AgentMode {
    Primary,   // 主 agent，可作为默认 agent
    Subagent,  // 子 agent，只能通过 task 工具调用
    All,       // 两者皆可
}

/// 权限规则
#[derive(Debug, Clone)]
pub struct PermissionRule {
    pub permission: String,  // 工具名（如 "edit", "bash", "read", "*"）
    pub pattern: String,     // 参数路径 glob（如 "*", "/src/**"）
    pub action: PermissionAction,
}

#[derive(Debug, Clone, PartialEq)]
pub enum PermissionAction {
    Allow,
    Deny,
}

/// Agent 的静态定义
#[derive(Debug, Clone)]
pub struct AgentDefinition {
    pub name: String,
    pub description: String,
    pub mode: AgentMode,
    pub model: Option<String>,        // 可选，不填用 session 默认
    pub temperature: Option<f32>,     // 可选
    pub steps: Option<u32>,           // 可选
    pub permission: Vec<PermissionRule>,
    pub system_prompt: String,
}
```

### 5.2 AgentRegistry

```rust
/// Agent 定义注册中心
pub struct AgentRegistry {
    agents: HashMap<String, AgentDefinition>,
}

impl AgentRegistry {
    pub fn register(&mut self, agent: AgentDefinition);
    pub fn get(&self, name: &str) -> Option<&AgentDefinition>;
    pub fn default(&self) -> Option<&AgentDefinition>;
    pub fn list(&self) -> Vec<&AgentDefinition>;
    pub fn list_subagents(&self) -> Vec<&AgentDefinition>;
}
```

### 5.3 AgentConfig 扩展

AgentConfig 是 agent loop 的运行时配置，对应 `daemon.toml` 的 `[agent]` 节。新增 `max_depth` 作为全局嵌套深度上限：

```rust
pub struct AgentConfig {
    pub soft_limit: u32,
    pub hard_limit: u32,
    pub doom_loop_threshold: u32,
    pub max_depth: u32,              // ← 新增，默认 5
    pub llm_retry_attempts: u32,
    pub llm_retry_base_delay_ms: u64,
    pub bash_confirm_mode: bool,
    pub file_max_size_bytes: u64,
}
```

```toml
# daemon.toml
[agent]
max_depth = 5   # 子 agent 嵌套深度上限，超出则拒绝 spawn
```

spawn_sub_agent 时检查当前链深度，超了就给父 agent 返回错误：

```rust
fn spawn_sub_agent(&mut self, parent_session_id: &str, call_id: String, ...) {
    let current_depth = self.compute_depth(parent_session_id);
    if current_depth >= self.agent_config.max_depth {
        let _ = parent.inbox.try_send(OrchestratorMessage::SubAgentError {
            call_id,
            error: format!("max depth ({}) exceeded", self.agent_config.max_depth),
        });
        return;
    }
    // ...
}

/// 递归计算一个 agent 在调用链中的深度
fn compute_depth(&self, session_id: &str) -> u32 {
    let mut depth = 0;
    let mut current = session_id;
    while let Some(agent) = self.active_agents.get(current) {
        if let Some(ref parent) = agent.parent_session_id {
            depth += 1;
            current = parent;
        } else {
            break;
        }
    }
    depth
}
```

### 5.4 Session 扩展

```rust
pub struct Session {
    // ... 现有字段
    pub agent_name: String,          // 新增，默认 "default"
    pub parent_id: Option<String>,   // 新增，子 session 标识父 session
    pub permission: Vec<PermissionRule>,  // 新增，运行时权限规则集
}
```

### 5.5 消息类型（新增）

```rust
/// Agent → Orchestrator：所有从 agent 发出的消息
/// 所有 agent 共用全局事件总线发送
pub enum AgentMessage {
    // ── 要转发给用户看的事件 ──
    TextDelta(String),
    ThinkingBlock(serde_json::Value),
    UsageInfo {
        input_tokens: u32,
        output_tokens: u32,
        tool_calls: u32,
        cache_creation_input_tokens: u32,
        cache_read_input_tokens: u32,
    },
    StatusUpdate(String),
    Error { code: AgentErrorCode, message: String },
    ToolCallRequest { call_id: String, tool_name: String, arguments: String },
    ToolCallResult { call_id: String, tool_name: String, content: String, is_error: bool },

    // ── 需要 Orchestrator 代为处理 ──
    UserQuery {
        query_id: String,
        message: String,
        options: Vec<String>,
        allow_other: bool,
        respond: oneshot::Sender<UserQueryResult>,  // 回复通过此通道返回
    },
    SpawnRequest {
        call_id: String,
        subagent_type: String,
        description: String,
        task_id: Option<String>,
        // 注意：没有 respond！
        // 子 agent 结果通过 inbox_rx 以 OrchestratorMessage 返回
    },

    // ── 生命周期 ──
    Done,
}

/// Orchestrator → Agent：发给指定 agent 的消息
/// 通过该 agent 独享的 inbox 通道发送
pub enum OrchestratorMessage {
    /// 子 agent 完成了
    SubAgentComplete {
        call_id: String,
        content: String,
        task_id: String,
    },
    /// 子 agent 启动失败或被拒绝（如超出 max_depth）
    SubAgentError {
        call_id: String,
        error: String,
    },
    /// 被取消了
    Cancelled,
}
```

### 5.6 AgentLoopContext 扩展

```rust
pub struct AgentLoopContext {
    pub session_id: String,
    pub history: Vec<Message>,
    pub working_dir: PathBuf,
    pub config: LlmConfig,
    pub cancel_token: CancellationToken,
    pub context_trimmer: Arc<dyn ContextTrimmer>,

    // ── 通信通道（新增） ──
    pub global_tx: mpsc::Sender<Envelope>,  // 全局事件总线，发送 AgentMessage
    pub inbox_rx: mpsc::Receiver<OrchestratorMessage>,  // Orchestrator 发来的消息
}

/// 事件总线上的信封，标记消息来源
pub struct Envelope {
    pub session_id: String,
    pub message: AgentMessage,
}
```

### 5.7 ToolContext 扩展

```rust
pub struct ToolContext {
    pub working_dir: PathBuf,
    pub session_id: Option<String>,
    pub permission_rules: Arc<Vec<PermissionRule>>,  // 新增
}
```

## 6. 运行时通信架构（核心新增）

这是多 Agent 的通信枢纽，也是本次设计最大的新增部分。

### 6.1 通道全景

```
                           全局事件总线
  ┌────────────────────────────────────────────────────────────┐
  │                     global_tx                                │
  │  (所有 Agent 共用这个 Sender 发消息)                         │
  └────────────────────────────────────────────────────────────┘
        ▲                 ▲                  ▲
        │                 │                  │
  Agent Loop 1      Agent Loop 2       Agent Loop 3
  (主 agent)         (子 agent)          (子 agent)
        │                 │                  │
        ▼                 ▼                  ▼
  ┌────────────────────────────────────────────────────────────┐
  │                    global_rx                                 │
  │  (Orchestrator 从这一条通道收所有 Agent 的消息)              │
  └────────────────────────────────────────────────────────────┘
        │
        ▼
  ┌──────────────────────────────────────────────────────────────┐
  │                    Orchestrator                                │
  │                                                               │
  │  tokio::select! { biased;                                     │
  │    🥇 cancel_rx     → cancel_agent()                          │
  │    🥈 global_rx     → handle_agent_message()                  │
  │    🥉 grpc_rx       → handle_client_message()                 │
  │  }                                                            │
  │                                                               │
  │  handle_agent_message(envelope) {                             │
  │    match envelope.message {                                   │
  │      TextDelta    → grpc_tx.send(forward(envelope))           │
  │      UserQuery    → pending_queries.insert(...)               │
  │                   → grpc_tx.send(UserQuery { ... })           │
  │      SpawnRequest → spawn_sub_agent(...)                     │
  │      Done         → handle_done(envelope.session_id)          │
  │      ...                                                     │
  │    }                                                          │
  │  }                                                            │
  └──────────────────────────────────────────────────────────────┘
        │
        ├──→ CLI (gRPC)：转发事件给用户
        │
        ├──→ cancel_tx (来自 CLI 的 Ctrl-C，独立通道，最高优先级)
        │
        └──→ 各 Agent 的 inbox：发 OrchestratorMessage
                  │
        ┌─────────┼─────────┐
        ▼         ▼         ▼
   Agent 1     Agent 2    Agent 3
   inbox_rx    inbox_rx   inbox_rx
```

### 6.2 Orchestrator 主循环

```rust
// orchestrator.rs (in visp-agent)

/// 取消信号（来自 Ctrl-C）
pub struct CancelSignal {
    pub session_id: String,
}

pub struct Orchestrator {
    // 🥇 取消通道（最高优先级，独立通道不被阻塞）
    cancel_rx: mpsc::Receiver<CancelSignal>,

    // 🥈 全局事件总线（收端）
    global_rx: mpsc::Receiver<Envelope>,

    // 🥉 CLI 普通消息
    grpc_rx: mpsc::Receiver<ClientMessage>,

    // 🎯 转发事件到 CLI（handle_agent_message 中用于向用户推送事件）
    grpc_tx: mpsc::Sender<ServerMessage>,

    // 活跃 Agent 注册表
    active_agents: ActiveAgentRegistry,

    // 待响应用户查询
    pending_queries: HashMap<String, (String, oneshot::Sender<UserQueryResult>)>,

    // ── 依赖（由 daemon 组装时注入） ──
    session_mgr: Arc<SessionManager>,
    agent_registry: Arc<AgentRegistry>,
    tool_registry: Arc<ToolRegistry>,
    rule_engine: Arc<RuleEngine>,
    agent_config: AgentConfig,

    /// LLM Provider 缓存：key = "provider.name"（即 model key）
    /// daemon 启动时预创建所有配置的 provider，传入 Orchestrator
    /// spawn agent 时根据 agent 的 model 字段查表
    providers: HashMap<String, Arc<dyn LlmProvider>>,
    /// 默认 provider key（当 agent 未指定 model 时使用）
    default_provider_key: String,
}

impl Orchestrator {
    pub async fn run(&mut self) {
        loop {
            tokio::select! {
                // 🥇 Ctrl-C 取消，永远优先处理
                biased;

                Some(signal) = self.cancel_rx.recv() => {
                    self.cancel_agent(&signal.session_id);
                }

                // 🥈 所有 Agent 的消息通过一条 global_rx 到达
                Some(envelope) = self.global_rx.recv() => {
                    self.handle_agent_message(envelope).await;
                }

                // 🥉 CLI 普通消息
                Some(msg) = self.grpc_rx.recv() => {
                    self.handle_client_message(msg).await;
                }
            }
        }
    }
}
```

### 6.3 handle_agent_message

```rust
async fn handle_agent_message(&mut self, envelope: Envelope) {
    let session_id = &envelope.session_id;

    match envelope.message {
        // ── 事件转发到 CLI（所有 agent 的事件用户都能看到） ──
        AgentMessage::TextDelta(content) => {
            let _ = grpc_tx.send(ServerMessage::TextDelta {
                session_id: session_id.clone(),
                delta: content,
            }).await;
        }
        AgentMessage::ToolCallRequest { call_id, tool_name, arguments } => {
            let _ = grpc_tx.send(ServerMessage::ToolCall { session_id, call_id, tool_name, arguments }).await;
        }
        AgentMessage::ToolCallResult { call_id, tool_name, content, is_error } => {
            let _ = grpc_tx.send(ServerMessage::ToolResult { session_id, call_id, tool_name, content, is_error }).await;
        }
        AgentMessage::StatusUpdate(msg) => {
            let _ = grpc_tx.send(ServerMessage::StatusUpdate { session_id, message: msg }).await;
        }
        AgentMessage::UsageInfo { .. } | AgentMessage::ThinkingBlock(_) => {
            let _ = grpc_tx.send(ServerMessage::from(envelope)).await;
        }

        // ── 需要用户交互 ──
        AgentMessage::UserQuery { query_id, message, options, allow_other, respond } => {
            // 存 respond，用户回复后通过它回传
            self.pending_queries.insert(query_id.clone(), (session_id.clone(), respond));
            let _ = grpc_tx.send(ServerMessage::UserQuery {
                session_id: session_id.clone(),
                query_id,
                message,
                options,
                allow_other,
            }).await;
        }

        // ── 子 Agent 请求 ──
        AgentMessage::SpawnRequest { call_id, subagent_type, description, task_id } => {
            self.spawn_sub_agent(session_id, call_id, subagent_type, description, task_id);
        }

        // ── 生命周期 ──
        AgentMessage::Done => {
            self.handle_done(session_id);
        }

        AgentMessage::Error { code, message } => {
            tracing::error!(session_id = %session_id, code = ?code, "agent error: {message}");
            let _ = grpc_tx.send(ServerMessage::AgentError { session_id, code, message }).await;
            self.handle_done(session_id);
        }
    }
}
```

### 6.4 handle_client_message

```rust
async fn handle_client_message(&mut self, msg: ClientMessage) {
    match msg {
        ClientMessage::UserInput { session_id, text } => {
            // 启动（或恢复）主 Agent
            if let Ok(session) = self.session_mgr.get(&session_id) {
                if session.status == Idle {
                    self.start_main_agent(session_id, text);
                }
            }
        }

        ClientMessage::UserQueryResponse { query_id, selected_index, text } => {
            // 找到存储的 respond，发回 Agent
            if let Some((_, respond)) = self.pending_queries.remove(&query_id) {
                let _ = respond.send(UserQueryResult { selected_index, text });
            }
        }

        // ⚠️ Cancel 不在 grpc_rx 处理！
        //    Ctrl-C 信号走独立的 cancel_rx 通道（见 6.2），
        //    保证不会被其他消息阻塞。
    }
}
```

### 6.5 ActiveAgent 注册表

```rust
/// 一个正在运行的 Agent 实例
struct ActiveAgent {
    session_id: String,
    parent_session_id: Option<String>,
    agent_name: String,
    cancel_token: CancellationToken,
    /// Orchestrator 通过这个通道发消息给该 Agent
    inbox: mpsc::Sender<OrchestratorMessage>,
    /// 子 Agent 完成时记录的 call_id（Task 工具调用 ID）
    pending_call_id: Option<String>,
    started_at: Instant,
}

/// 活跃 Agent 注册表
struct ActiveAgentRegistry {
    agents: HashMap<String, ActiveAgent>,  // key = session_id
}

impl ActiveAgentRegistry {
    fn register(&mut self, agent: ActiveAgent);
    fn remove(&mut self, session_id: &str) -> Option<ActiveAgent>;
    fn get(&self, session_id: &str) -> Option<&ActiveAgent>;
    fn get_mut(&mut self, session_id: &str) -> Option<&mut ActiveAgent>;

    /// 查找某个 Agent 的所有直接子 Agent
    fn children_of(&self, parent_id: &str) -> Vec<&ActiveAgent>;

    /// 递归查找所有子孙 Agent
    fn descendants_of(&self, parent_id: &str) -> Vec<&ActiveAgent>;
}
```

### 6.6 启动主 Agent

```rust
fn start_main_agent(&mut self, session_id: String, text: String) {
    // 1. 查 Session 的 agent 定义
    let session = self.session_mgr.get(&session_id).unwrap();
    let agent_def = self.agent_registry.get(&session.agent_name)
        .expect("unknown agent for session");

    // 2. 创建 inbox 通道
    let (inbox_tx, inbox_rx) = mpsc::channel(64);

    // 3. 注册 ActiveAgent（无 parent）
    let cancel_token = CancellationToken::new();
    self.active_agents.register(ActiveAgent {
        session_id: session_id.clone(),
        parent_session_id: None,
        agent_name: session.agent_name.clone(),
        cancel_token: cancel_token.clone(),
        inbox: inbox_tx,
        pending_call_id: None,
        started_at: Instant::now(),
    });

    // 4. 查找 Provider
    //    agent 定义中可指定 model，未指定则用 session 默认模型
    let model_key = agent_def.model
        .as_ref()
        .or_else(|| session.config.model.as_ref())
        .unwrap_or(&self.default_provider_key);
    let provider = self.providers.get(model_key)
        .expect("provider not found");

    // 5. 构建用户消息
    let user_message = Message::user(text);

    // 6. 启动 Agent loop
    let ctx = AgentLoopContext {
        session_id,
        global_tx: self.global_tx.clone(),
        inbox_rx,
        cancel_token,
        // ... 其他字段
    };
    tokio::spawn(run_agent_loop(
        provider.clone(),
        self.tool_registry.clone(),
        self.rule_engine.clone(),
        self.session_mgr.clone(),
        ctx,
        self.agent_config.clone(),
        user_message,
    ));
}
```

### 6.7 启动子 Agent

```rust
fn spawn_sub_agent(
    &mut self,
    parent_session_id: &str,
    call_id: String,
    subagent_type: String,
    description: String,
    task_id: Option<String>,
) {
    // 1. 查 Agent 定义
    let agent_def = self.agent_registry.get(&subagent_type)
        .expect("unknown agent type");

    // 2. 创建子 Session
    let parent_session = self.session_mgr.get(parent_session_id).unwrap();
    let parent_agent_def = self.agent_registry.get(&parent_session.agent_name).ok();
    let parent_agent_permission = parent_agent_def
        .map(|a| a.permission.as_slice())
        .unwrap_or(&[]);

    let sub_session = self.session_mgr.create(SubSessionParams {
        parent_id: Some(parent_session_id.to_string()),
        agent_name: subagent_type.clone(),
        permission: merge_permissions(
            parent_session.permission,
            parent_agent_permission,
            agent_def.permission,
        ),
        // 复用或新建
        session_id: task_id.clone(),
    });

    // 3. 创建 inbox 通道（Orchestrator → 子 Agent）
    let (inbox_tx, inbox_rx) = mpsc::channel(64);

    // 4. 注册 ActiveAgent
    let cancel_token = CancellationToken::new();
    self.active_agents.register(ActiveAgent {
        session_id: sub_session.id.clone(),
        parent_session_id: Some(parent_session_id.to_string()),
        agent_name: subagent_type,
        cancel_token: cancel_token.clone(),
        inbox: inbox_tx,
        pending_call_id: Some(call_id),
        started_at: Instant::now(),
    });

    // 5. 查找 Provider
    //    子 Agent 可指定独立模型，未指定则继承父 session 的模型
    let model_key = agent_def.model
        .as_ref()
        .or_else(|| parent_session.config.model.as_ref())
        .unwrap_or(&self.default_provider_key);
    let provider = self.providers.get(model_key)
        .expect("provider not found");

    // 6. 构建子 Agent 的初始消息
    let task_message = Message::user(format!("请执行以下任务：\n{}", description));

    // 7. 启动子 Agent loop
    let ctx = AgentLoopContext {
        session_id: sub_session.id.clone(),
        global_tx: self.global_tx.clone(),  // 共用事件总线
        inbox_rx,                           // 专属收件箱
        cancel_token,
        // ... 其他字段
    };
    tokio::spawn(run_agent_loop(
        provider.clone(),
        self.tool_registry.clone(),
        self.rule_engine.clone(),
        self.session_mgr.clone(),
        ctx,
        self.agent_config.clone(),
        task_message,
    ));
}
```

### 6.8 处理 Done（方案 A：try_send 不阻塞 Orchestrator）

```rust
fn handle_done(&mut self, session_id: &str) {
    if let Some(agent) = self.active_agents.remove(session_id) {
        if let Some(parent_id) = agent.parent_session_id {
            // 子 Agent 完成，通知父 Agent
            let content = self.extract_result(session_id);
            let call_id = agent.pending_call_id.unwrap_or_default();

            if let Some(parent) = self.active_agents.get(&parent_id) {
                let msg = OrchestratorMessage::SubAgentComplete {
                    call_id,
                    content,
                    task_id: session_id.to_string(),
                };

                // 用 try_send，绝不阻塞 Orchestrator 循环
                // 若 inbox 满了，spawn 后台任务等待发送
                // 若 parent 已关闭（通道关闭），丢弃
                match parent.inbox.try_send(msg) {
                    Ok(()) => {}
                    Err(TrySendError::Full(msg)) => {
                        let inbox = parent.inbox.clone();
                        tokio::spawn(async move {
                            if inbox.send(msg).await.is_err() {
                                tracing::warn!("parent agent inbox closed, dropping SubAgentComplete");
                            }
                        });
                    }
                    Err(TrySendError::Closed(_)) => {
                        tracing::warn!("parent agent inbox closed, dropping SubAgentComplete");
                    }
                }
            }
        } else {
            // 主 Agent 完成
            self.session_mgr.finish_loop(session_id, SessionStatus::Idle);
        }
    }
}
```

### 6.9 extract_result 辅助方法

```rust
/// 从子 Session 的最后一条 assistant 消息中提取结果文本
fn extract_result(&self, session_id: &str) -> String {
    let messages = self.session_mgr
        .get_messages(session_id)
        .unwrap_or_default();
    // 取最后一条 assistant 消息的内容
    messages.iter().rev().find_map(|m| {
        if m.role == Role::Assistant && m.text.is_some() {
            m.text.clone()
        } else {
            None
        }
    }).unwrap_or_default()
}
```

### 6.10 级联取消（通过 cancel_rx 触发）

```rust
/// 取消一个 Agent 及其所有子孙 Agent
/// 由 cancel_rx 通道触发（🥇 最高优先级）
fn cancel_agent(&mut self, session_id: &str) {
    tracing::info!(session_id = %session_id, "cancelling agent");

    // 1. 取消自身
    if let Some(agent) = self.active_agents.get(session_id) {
        agent.cancel_token.cancel();
    }

    // 2. 递归取消所有子孙 Agent
    let descendants: Vec<String> = self.active_agents
        .descendants_of(session_id)
        .iter()
        .map(|a| a.session_id.clone())
        .collect();

    for child_id in descendants {
        if let Some(child) = self.active_agents.get(&child_id) {
            child.cancel_token.cancel();
        }
        // 子 Agent 的 Done 事件会通过事件总线自然触发 handle_done
        // → 从注册表移除 → 清理
    }

    // 3. 级联 cancel 是异步的，不等待所有子 Agent 结束
    //    它们各自的 Done 会通过 global_rx 到达，由 handle_done 逐一清理
}
```

## 7. 权限检查机制

### 7.1 规则匹配

权限检查在 `ToolRegistry::execute()` 中进行：

```
PermissionRule 匹配逻辑：
  1. 遍历 permission_rules
  2. 找到第一个 (permission == 工具名) && (pattern 匹配参数路径) 的规则
  3. 匹配到 Allow → 放行
  4. 匹配到 Deny  → 拒绝
  5. 无匹配规则 → 默认 Allow（向后兼容）
```

### 7.2 子 Agent 权限继承

子 Agent 的权限 = 父 Session deny 规则 + 父 Agent deny 规则 + 自身 deny 规则 + 自身 allow 规则

```rust
fn merge_permissions(
    parent_session_permission: &[PermissionRule],
    parent_agent_permission: &[PermissionRule],
    subagent_permission: &[PermissionRule],
) -> Vec<PermissionRule> {
    let mut result = Vec::new();

    // 1. 继承父 Session 的 deny 规则
    result.extend(parent_session_permission.iter()
        .filter(|r| r.action == Deny).cloned());

    // 2. 继承父 Agent 的 deny 规则（如 Plan Mode 的 edit deny）
    result.extend(parent_agent_permission.iter()
        .filter(|r| r.action == Deny).cloned());

    // 3. 子 Agent 自身规则
    result.extend_from_slice(subagent_permission);

    // ⚠️ 安全兜底：如果最终规则集里没有兜底的 deny-all 规则，
    //    则自动追加 `*: deny`。防止子 Agent 因忘记配 permission
    //    而获得超出预期的权限。
    //    这条规则放在最后，所以子 Agent 的显式 allow 规则仍可覆盖它。
    if !has_deny_all(&result) {
        result.push(PermissionRule {
            permission: "*".into(),
            pattern: "*".into(),
            action: PermissionAction::Deny,
        });
    }

    result
}

/// 检查规则集中是否存在兜底的 deny-all 规则
fn has_deny_all(rules: &[PermissionRule]) -> bool {
    rules.iter().any(|r| {
        r.permission == "*" && r.pattern == "*" && r.action == PermissionAction::Deny
    })
}
```

对应的匹配逻辑调整：遍历规则时，**精确匹配优先于通配匹配**，即先匹配 `(permission == "edit")`，如果没命中再匹配 `(permission == "*")`。这样兜底 `*: deny` 不会覆盖更具体的 allow 规则。

```rust
fn check_permission(
    &self,
    name: &str,
    args: &serde_json::Value,
    rules: &[PermissionRule],
) -> PermissionDecision {
    // 第一轮：精确匹配
    for rule in rules {
        if rule.permission != name { continue; }
        if !pattern_matches(&rule.pattern, args) { continue; }
        return match rule.action {
            Allow => PermissionDecision::Allowed,
            Deny => PermissionDecision::Denied(rule.permission.clone()),
        };
    }

    // 第二轮：通配匹配 (permission == "*")
    for rule in rules {
        if rule.permission != "*" { continue; }
        if !pattern_matches(&rule.pattern, args) { continue; }
        return match rule.action {
            Allow => PermissionDecision::Allowed,
            Deny => PermissionDecision::Denied(rule.permission.clone()),
        };
    }

    PermissionDecision::Allowed  // 无匹配 → 默认允许（向后兼容主 Agent）
}
```

## 8. Tool 执行改造：支持异步结果

Agent loop 的工具执行需要从 `join_all` 改为 `select!`，以支持 task 工具的异步结果。

### 8.1 当前流程（单 Agent）

```
LLM 返回 tool_calls
  → 对每个 tool_call：审批 → spawn 执行任务
  → join_all 等所有任务完成
  → 收集 ToolResult → 写入历史 → 下一轮 LLM 调用
```

### 8.2 新流程（支持 Task 工具）

```
LLM 返回 tool_calls
  │
  ├── Phase 1: 分派工具
  │   ├── 普通工具 → spawn 执行任务
  │   └── task 工具 → 通过 global_tx 发 SpawnRequest，标记为 pending
  │
  ├── Phase 2: 循环收集结果
  │   tokio::select! {
  │     join_all(普通工具)    → 收集结果
  │     inbox_rx.recv()      → 收到 SubAgentComplete → 构造 ToolResult
  │     cancel_token         → 取消
  │   }
  │   └── 直到所有工具（含 pending）都拿到结果
  │
  └── Phase 3: 排序 → 写入历史 → 下一轮 LLM 调用
```

### 8.3 核心代码逻辑

```rust
// 在 run_agent_loop 的工具执行阶段

// Phase 1: 分派
let mut exec_tasks = Vec::new();
let mut pending_tasks: Vec<String> = Vec::new();  // 存放 task 工具的 call_id

for (i, tc) in tool_calls.iter().enumerate() {
    // 先发 ToolCallRequest（CLI 显示）
    let _ = ctx.global_tx.send(Envelope {
        session_id: ctx.session_id.clone(),
        message: AgentMessage::ToolCallRequest {
            call_id: tc.id.clone(),
            tool_name: tc.name.clone(),
            arguments: tc.arguments.clone(),
        },
    }).await;

    // 检查审批...
    if requires_approval && !already_approved {
        // 发 UserQuery 等用户确认
        // ...
    }

    // task 是唯一需要 agent loop 拦截执行的工具
    // 它不走 registry.execute()，而是通过事件总线发 SpawnRequest
    // 结果通过 inbox_rx 异步返回
    if tc.name == "task" {
        let args: TaskArgs = serde_json::from_str(&tc.arguments)?;
        ctx.global_tx.send(Envelope {
            session_id: ctx.session_id.clone(),
            message: AgentMessage::SpawnRequest {
                call_id: tc.id.clone(),
                subagent_type: args.subagent_type,
                description: args.description,
                task_id: args.task_id,
            },
        }).await;
        pending_tasks.push(tc.id.clone());
    } else {
        // 普通工具：spawn 执行
        exec_tasks.push(tokio::spawn({
            // 执行工具...
            ToolExecResult { index: i, call_id: tc.id.clone(), result }
        }));
    }
}

// Phase 2: 收集结果
let mut results: Vec<ToolExecResult> = Vec::new();
let mut regular_done = false;

loop {
    tokio::select! {
        // 普通工具完成
        batch = futures::future::join_all(
            std::mem::take(&mut exec_tasks)
        ) => {
            for r in batch {
                if let Ok(tr) = r { results.push(tr); }
            }
            regular_done = true;
            if pending_tasks.is_empty() { break; }
        }

        // 子 Agent 结果到达
        Some(msg) = ctx.inbox_rx.recv() => {
            match msg {
                OrchestratorMessage::SubAgentComplete {
                    call_id, content, task_id
                } => {
                    let idx = tool_calls.iter()
                        .position(|tc| tc.id == call_id).unwrap();
                    results.push(ToolExecResult {
                        index: idx,
                        call_id,
                        result: ToolResult::success(content),
                    });
                    pending_tasks.retain(|id| id != &call_id);
                    if pending_tasks.is_empty() && regular_done { break; }
                }
                OrchestratorMessage::SubAgentError { call_id, error } => {
                    // 子 Agent 启动被拒（如超出 max_depth），
                    // 当作普通错误结果，父 Agent 继续
                    let idx = tool_calls.iter()
                        .position(|tc| tc.id == call_id).unwrap();
                    results.push(ToolExecResult {
                        index: idx,
                        call_id,
                        result: ToolResult::error(error),
                    });
                    pending_tasks.retain(|id| id != &call_id);
                    if pending_tasks.is_empty() && regular_done { break; }
                }
                OrchestratorMessage::Cancelled => {
                    // Agent 收到取消信号，立即退出
                    // 此时 inbox 中可能还有未读的 SubAgentComplete，
                    // 但 agent loop return 后 inbox 被 drop，
                    // Orchestrator 的 try_send 会收到 Closed 并丢弃。
                    return;
                }
            }
        }

        _ = ctx.cancel_token.cancelled() => {
            return; // 被取消
        }
    }
}

// Phase 3: 排序 + 写入历史
results.sort_by_key(|r| r.index);
for tr in results {
    let tool_msg = Message::tool(tr.result.content, &tr.call_id);
    ctx.history.push(tool_msg.clone());
    sm.append_message(&ctx.session_id, tool_msg);
}
```

### 8.4 task 工具注册

task 工具在 ToolRegistry 中**仅注册定义**（名称、描述、参数 schema），让 LLM 知道可以调用它。执行时由 agent loop 拦截，不走 `registry.execute()`。

```rust
// visp-tools/src/task.rs
pub struct TaskTool;

impl Tool for TaskTool {
    fn name(&self) -> &str { "task" }

    fn description(&self) -> &str {
        "启动一个子 Agent 处理复杂任务。当任务适合某个专门的 Agent 时使用。"
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "subagent_type": {
                    "type": "string",
                    "description": "子 Agent 类型，从可用 subagent 列表中选择"
                },
                "description": {
                    "type": "string",
                    "description": "任务的详细描述。必须有清晰的上下文和目标"
                },
                "task_id": {
                    "type": "string",
                    "description": "可选的 task_id，复用已有子 session 的上下文"
                }
            },
            "required": ["subagent_type", "description"]
        })
    }

    fn execute(&self, _arguments: serde_json::Value, _context: &ToolContext) -> ToolResult {
        // ❌ 不在此处执行
        // task 工具的调用由 agent loop 在工具执行阶段拦截，
        // 通过 global_tx 发 SpawnRequest，结果通过 inbox_rx 异步返回
        unreachable!("task tool execution is handled by agent loop")
    }
}
```

## 9. Task 工具的数据流全景

以下跟踪一次 `task` 调用的完整生命周期：

```
父 Agent Loop                     Orchestrator                   子 Agent Loop
──── ──────────                   ────────────                   ─────────────

1. LLM 返回 tool_call: task(...)
   │
2. 发 SpawnRequest ──────────────>│
   (通过 global_tx)                │
                                   │ 3. 创建子 Session
                                   │ 4. 注册 ActiveAgent
                                   │ 5. 创建 inbox 通道
                                   │
                                   │ 6. 启动 ───────────────────>
                                   │    (给子 Agent global_tx + inbox_rx)
                                   │                              │
                                   │                             7. 子 Agent 执行
                                   │                              (工具通过 global_tx 转发)
                                   │                              │
                子 Agent 的事件也通过 global_tx 到 Orchestrator，转发到 CLI
                用户能看到子 Agent 的 TextDelta、ToolCallRequest 等
                                   │                              │
                                   │                             8. 完成
                                   │                     Done ───>
                                   │                              │
                                   │ 9. 查 active_agents
                                   │    找到父 Agent 的 inbox
                                   │
                                   │ 10. inbox.send(
                                   │     SubAgentComplete {      │
                                   │       call_id,              │
                                   │       content,              │
                                   │       task_id               │
                                   │     })                      │
                                   │         │
11. inbox_rx.recv() <─────────────┘
    │
12. 匹配 call_id 到对应的 tool_call
13. 构造 ToolExecResult
14. 写入历史
15. 继续 LLM 迭代
```

## 10. Session 持久化

### 10.1 子 Session 状态管理

- 子 Session 的 `status` 独立于父 Session
- 子 Session 运行中 → 父 Session 仍为 `Running`
- 子 Session `Completed` → 父 Agent 拿到结果，父 Session 继续
- 父 Session `Idle` + 子 Session `Running` → 异常，Orchestrator 应清理

### 10.2 DB Schema 变更

```sql
-- V3 migration
ALTER TABLE session ADD COLUMN agent_name TEXT NOT NULL DEFAULT 'default';
ALTER TABLE session ADD COLUMN parent_id TEXT;
ALTER TABLE session ADD COLUMN permission_json TEXT;
```

### 10.3 迁移策略

```rust
// impl Migrator for V3
fn run(tx: &Transaction) -> Result<()> {
    if !has_column(tx, "session", "agent_name") {
        tx.execute("ALTER TABLE session ADD COLUMN agent_name TEXT NOT NULL DEFAULT 'default'")?;
    }
    if !has_column(tx, "session", "parent_id") {
        tx.execute("ALTER TABLE session ADD COLUMN parent_id TEXT")?;
    }
    if !has_column(tx, "session", "permission_json") {
        tx.execute("ALTER TABLE session ADD COLUMN permission_json TEXT")?;
    }
    Ok(())
}
```

## 11. CLI 相关

### 11.1 Session 列表显示

```
/list 输出扩展:
  # 主 session
  a1b2c3d4  Idle      "优化数据库查询" [default]

  # 子 session 缩进显示
  e5f6g7h8  Completed "审查 src/main.rs" [code-review] ← a1b2c3d4
```

### 11.2 子 Agent 事件显示

用户可见子 Agent 的事件（通过事件总线转发）：
- `TextDelta`：子 Agent 的输出增量
- `ToolCallRequest` / `ToolCallResult`：子 Agent 调到用的工具
- `UserQuery`：子 Agent 向用户提问或请求审批
- `StatusUpdate`：状态更新

CLI 中通过 session_id 前缀 + 颜色区分事件来源（V1 方案，后续版本再精细化 UX）：

```
[abc] 正在审查 src/main.rs...
[abc] ── Tool: grep("fn main") ──
[abc] 发现 3 处匹配
[abc] ✓ 审查完成

[def] (子 Agent: code-review) ─────────────────────
[def] 审查报告:
[def] 1. 第 42 行：未处理的错误返回值
[def] 2. 第 78 行：潜在的内存泄漏
```

### 11.3 命令（暂定，后续可加）

| 命令 | 行为 | 优先级 |
|------|------|--------|
| `/agent` | 无参 → 列出可用 agents（交互式选择） | TODO |
| `/agent <name>` | 切换当前 session 的 agent | TODO |

## 12. 目录结构变更（新增文件）

```
crates/
├── visp-core/
│   ├── src/
│   │   ├── agent_definition.rs    ← 新增：AgentDefinition, PermissionRule
│   │   ├── agent_registry.rs      ← 新增：AgentRegistry
│   │   ├── agent.rs               ← 扩展：AgentMessage, OrchestratorMessage, Envelope
│   │   │                              AgentLoopContext 加 global_tx / inbox_rx
│   │   │                              工具执行改为 select! 流程
│   │   ├── session.rs             ← 扩展：agent_name, parent_id, permission
│   │   ├── tool.rs                ← 扩展：ToolContext.permission_rules
│   │   └── tool_registry.rs       ← 扩展：权限检查逻辑
├── visp-tools/
│   ├── src/
│   │   ├── task.rs                ← 新增：Task 工具定义（仅注册，不执行）
│   │   └── mod.rs                 ← 注册 task
├── visp-agent/                    ← 新增 crate：多 Agent 运行时
│   ├── Cargo.toml
│   ├── src/
│   │   ├── lib.rs
│   │   ├── orchestrator.rs        ← Orchestrator 主循环
│   │   ├── active_agent.rs        ← ActiveAgent / ActiveAgentRegistry
│   │   └── event_bus.rs           ← 事件总线管理
├── visp-daemon/
│   ├── src/
│   │   ├── main.rs                ← 扩展：加载 .visp/agents/，创建 Orchestrator
│   │   └── service.rs             ← 简化：只组装，运行时交给 Orchestrator
└── visp-db/
    └── src/
        └── schema.rs              ← 扩展：V3 migration
```

### 新增 crates 依赖

**visp-core 新增依赖**：
- 无新增依赖（消息类型和通道只用 tokio mpsc/oneshot，已是 workspace 依赖）

> **IO 边界说明**：`visp-core` 新增的 `mpsc::Sender<Envelope>` 和 `mpsc::Receiver<OrchestratorMessage>` 属于**任务间通信**原语，不是文件/网络/进程 IO。`visp-core` 只消费调用者传入的 Sender/Receiver，不创建通道，不违反无 IO 约束。这与现有代码中 `run_agent_loop` 接受 `mpsc::Sender<AgentEvent>` 参数的逻辑一致。

**visp-tools 新增依赖**：
- `visp-core`：已有
- `tokio`：workspace 已有

**visp-agent（新增 crate）依赖**：
- `visp-core`（类型、AgentLoopContext、SessionManager）
- `visp-llm`（创建 provider）
- `visp-tools`（执行工具）
- `visp-db`（session 持久化）
- `tokio`（workspace 已有）
- `tonic`（gRPC 转发到 CLI）
- `futures`（workspace 已有）

**visp-daemon 新增依赖**：
- `visp-agent`：Orchestrator
- `visp-core`：已有
- 文件 IO（扫描 `.visp/agents/`）：已有 `glob` 或 `walkdir`

AgentRegistry 的文件加载放在 daemon 层，core 层只定义类型和 trait。

## 13. 向后兼容

- 未配置 `.visp/agents/` 的项目 → 仅使用内建 agent，行为等同于现在
- 未配置权限规则的 agent → 默认全部 allow，行为等同于现在
- `Session.parent_id = None` + `session.agent_name = "default"` → 等同于现在的单 Agent 模式
- 无子 Agent 运行时，事件总线上只有主 Agent 的消息，Orchestrator 行为等同于现在的转发逻辑
- 现有 `TaskTool.execute()` 不走 `unreachable!` 路径（因为 agent loop 拦截了），但为了安全，可以作为 fallback 实现阻塞调用（走旧的 oneshot 方式），以防外部工具框架直接调用

## 14. TODO

以下功能在本设计中明确但不在第一期实现：

1. **动态生成 Agent**：用户通过自然语言描述需求，LLM 动态生成 AgentDefinition 并注入注册中心
2. **后台模式**：`task(background: true)` 异步执行子 Agent，主 Agent 继续其他工作，完成后通知 — 当前架构支持，但第一期只做前台模式
3. **CLI 命令**：`/agent` 切换当前 Agent
4. **测试验证嵌套子 Agent**：当前设计已天然支持子 Agent 再启动子 Agent（递归 task），不属于新功能。需要端到端测试验证正确性，尤其是权限继承链和 cancel 级联。
5. **子 Agent 恢复**：通过 `task_id` 恢复已完成的子 Session 上下文
6. **Orchestrator 并行化**（源自 #1）：将 `handle_agent_message` 拆分为快速路由 + 耗时处理两段，耗时的 spawn 出去执行，避免单个消息处理阻塞其他消息。当前 `try_send` + spawn 重试已解决最危险的问题，一期无需改动。
7. **子 Agent 超时**：为 `AgentDefinition` 增加 `timeout: Option<Duration>`，子 agent 超时后通过 `OrchestratorMessage::SubAgentTimeout` 通知父 agent。需要新增消息变体并处理父 agent 的恢复逻辑，有一定复杂度和决策点，留到 V2。
