# 多 Agent 工作计划

> 基于 `docs/design/visp-design-multi-agent.md`，TDD 五步循环驱动。
>
> **每步循环**：🔴 编写测试 → 🟢 实现 → 🧪 执行测试 → ♻️ 重构 → 👁️ 代码审核 → 📦 提交

**影响范围**：visp-core（修改）、visp-tools（扩展）、visp-agent（新建）、visp-daemon（修改）、visp-db（扩展）

**新增 crate**：`visp-agent`（多 Agent 运行时）

---

## 步骤 1：类型定义（Wave 0）

> Wave 0 无业务逻辑，纯数据结构和接口定义。无测试（编译器保证）。

### 1a：AgentDefinition + PermissionRule + AgentMode

**文件**：新建 `crates/visp-core/src/agent_definition.rs`

#### 🟢 绿 — 实现

```rust
#[derive(Debug, Clone, PartialEq)]
pub enum AgentMode { Primary, Subagent, All }

#[derive(Debug, Clone)]
pub struct PermissionRule {
    pub permission: String,
    pub pattern: String,
    pub action: PermissionAction,
}

#[derive(Debug, Clone, PartialEq)]
pub enum PermissionAction { Allow, Deny }

#[derive(Debug, Clone)]
pub struct AgentDefinition {
    pub name: String,
    pub description: String,
    pub mode: AgentMode,
    pub model: Option<String>,
    pub temperature: Option<f32>,
    pub steps: Option<u32>,
    pub permission: Vec<PermissionRule>,
    pub system_prompt: String,
}
```

#### ♻️ 重构

- `crates/visp-core/src/lib.rs` 追加 `pub mod agent_definition;`

#### 🧪 测试 → 类型检查

```bash
cargo build -p visp-core && cargo clippy -p visp-core -- -D warnings
```

#### 👁️ 代码审核

- `AgentMode` 三个变体命名正确（`Primary` / `Subagent` / `All`）
- `PermissionRule` 字段类型合理（`String` + `String` + `PermissionAction`）
- `PermissionAction` 无额外变体
- `AgentDefinition` 所有字段覆盖设计文档 5.1 节

#### 📦 提交

```bash
git add crates/visp-core/src/agent_definition.rs crates/visp-core/src/lib.rs
git commit -m "feat(core): add AgentDefinition, PermissionRule, AgentMode types"
```

---

### 1b：AgentRegistry

**文件**：新建 `crates/visp-core/src/agent_registry.rs`

#### 🟢 绿 — 实现

```rust
pub struct AgentRegistry {
    agents: HashMap<String, AgentDefinition>,
}

impl AgentRegistry {
    pub fn new() -> Self;
    pub fn register(&mut self, agent: AgentDefinition) -> Result<(), CoreError>;
    pub fn get(&self, name: &str) -> Option<&AgentDefinition>;
    pub fn default(&self) -> Option<&AgentDefinition>;
    pub fn list(&self) -> Vec<&AgentDefinition>;
    pub fn list_subagents(&self) -> Vec<&AgentDefinition>;
}
```

- `default()`：优先取 `mode != Subagent` 且名称含"default"的 agent；无则第一个 `mode != Subagent`；全 `Subagent` 时返回 `None`
- `register()` 同名返回 `Err`

#### ♻️ 重构

- `crates/visp-core/src/lib.rs` 追加 `pub mod agent_registry;`

#### 🧪 测试 → 类型检查

```bash
cargo build -p visp-core && cargo clippy -p visp-core -- -D warnings
```

#### 👁️ 代码审核

- `register` 返回 `Result` 而非 `panic`
- `default` 退化至 `None` 而非 `unwrap`
- 方法签名合理

#### 📦 提交

```bash
git add crates/visp-core/src/agent_registry.rs crates/visp-core/src/lib.rs
git commit -m "feat(core): add AgentRegistry"
```

---

### 1c：Session 扩展 + create_sub

**文件**：`crates/visp-core/src/session.rs`

#### 🟢 绿 — 实现

`Session` 结构体追加三个字段（设默认值保证向后兼容）：

```rust
pub struct Session {
    // ... 现有
    pub agent_name: String,
    pub parent_id: Option<String>,
    pub permission: Vec<PermissionRule>,
}
```

`SessionManager` 新增：

```rust
pub struct SubSessionParams {
    pub parent_id: Option<String>,
    pub agent_name: String,
    pub permission: Vec<PermissionRule>,
    pub session_id: Option<String>,
    pub project_path: PathBuf,
    pub config: LlmConfig,
}

impl SessionManager {
    pub fn create_sub(&self, params: SubSessionParams) -> Result<Session, SessionError>;
}
```

`create_sub` 逻辑：
- `session_id = Some(id)` 复用已存在 session（`get` 后直接返回）
- `session_id = None` 生成新 UUID
- 新 session 的 `status = Idle`，`history = []`

#### ♻️ 重构

- 现有 `Session` 构造调用处补上三个新字段（编译器会指引）
- `Session::default()` / 测试 mock 补新字段

#### 🧪 测试 → 类型检查

```bash
cargo build -p visp-core && cargo clippy -p visp-core -- -D warnings
```

#### 👁️ 代码审核

- 新字段有合理的默认值（`"default"`, `None`, `vec![]`）
- `create_sub` 复用逻辑没有副作用（不修改原始 session）
- 不影响现有 session 创建路径

#### 📦 提交

```bash
git add crates/visp-core/src/session.rs
git commit -m "feat(core): extend Session with agent_name, parent_id, permission; add create_sub"
```

---

### 1d：AgentConfig + max_depth

**文件**：`crates/visp-core/src/agent.rs`

#### 🟢 绿 — 实现

```rust
pub struct AgentConfig {
    // ... 现有
    pub max_depth: u32,  // 默认 5
}
```

更新 `Default` impl 设置 `max_depth: 5`。

#### 🧪 测试 → 类型检查

```bash
cargo build -p visp-core && cargo clippy -p visp-core -- -D warnings
```

#### 👁️ 代码审核

- 默认值 5 合理
- 兼容现有 `daemon.toml` 配置

#### 📦 提交

```bash
git add crates/visp-core/src/agent.rs
git commit -m "feat(core): add max_depth to AgentConfig"
```

---

### 1e：AgentLoopContext + ToolContext 扩展

**文件**：`crates/visp-core/src/agent.rs` + `crates/visp-core/src/tool.rs`

#### 🟢 绿 — 实现

`AgentLoopContext` 追加：

```rust
pub struct AgentLoopContext {
    // ... 现有
    pub global_tx: mpsc::Sender<Envelope>,
    pub inbox_rx: mpsc::Receiver<OrchestratorMessage>,
    pub permission_rules: Arc<Vec<PermissionRule>>,
}
```

`ToolContext` 追加：

```rust
pub struct ToolContext {
    // ... 现有
    pub permission_rules: Arc<Vec<PermissionRule>>,
}
```

#### ♻️ 重构

- 所有现有 `AgentLoopContext` 构造处补上新字段（影响 `SessionManager::start_loop`）
- 所有现有 `ToolContext` 构造处补上新字段

#### 🧪 测试 → 类型检查

```bash
cargo build && cargo clippy -- -D warnings
```

#### 👁️ 代码审核

- `global_tx` / `inbox_rx` 使用 `Option` 还是必填？设计上多 Agent 模式必填，单 Agent 模式填 `None`。用 `Option` 包裹以向后兼容

#### 📦 提交

```bash
git add crates/visp-core/src/agent.rs crates/visp-core/src/tool.rs
git commit -m "feat(core): extend AgentLoopContext and ToolContext with multi-agent fields"
```

---

### 1f：消息类型

**文件**：`crates/visp-core/src/message.rs`（或 `agent.rs`）

#### 🟢 绿 — 实现

```rust
pub enum AgentMessage {
    TextDelta(String),
    ThinkingBlock(serde_json::Value),
    UsageInfo { input_tokens: u32, output_tokens: u32, tool_calls: u32, cache_creation_input_tokens: u32, cache_read_input_tokens: u32 },
    StatusUpdate(String),
    Error { code: AgentErrorCode, message: String },
    ToolCallRequest { call_id: String, tool_name: String, arguments: String },
    ToolCallResult { call_id: String, tool_name: String, content: String, is_error: bool },
    UserQuery { query_id: String, message: String, options: Vec<String>, allow_other: bool, respond: oneshot::Sender<UserQueryResult> },
    SpawnRequest { call_id: String, subagent_type: String, description: String, task_id: Option<String> },
    Done,
}

pub enum OrchestratorMessage {
    SubAgentComplete { call_id: String, content: String, task_id: String },
    SubAgentError { call_id: String, error: String },
    Cancelled,
}

pub struct Envelope {
    pub session_id: String,
    pub message: AgentMessage,
}
```

#### 🧪 测试 → 类型检查

```bash
cargo build -p visp-core && cargo clippy -p visp-core -- -D warnings
```

#### 👁️ 代码审核

- `oneshot::Sender` 可跨线程 `Send`？`UserQueryResult` 必须 `Send + 'static`
- `Envelope` 不含 `!Send` 字段

#### 📦 提交

```bash
git add crates/visp-core/src/message.rs
git commit -m "feat(core): add AgentMessage, OrchestratorMessage, Envelope types"
```

---

## 步骤 2：核心逻辑（Wave 1）

### 2a：merge_permissions + check_permission

**文件**：`crates/visp-core/src/agent_definition.rs`

#### 🔴 红 — 测试

| # | 测试用例 |
|---|---------|
| 1 | `merge_permissions` 双方空列表 → 仅含兜底 `*: deny` |
| 2 | `merge_permissions` 继承父 session 的 deny |
| 3 | `merge_permissions` 继承父 agent 的 deny |
| 4 | `merge_permissions` 子 agent 的 allow 覆盖兜底 deny-all |
| 5 | `merge_permissions` 已有 `*: deny` 时不再追加 |
| 6 | `merge_permissions` 显式 `*: deny` 保留 |
| 7 | `check_permission` 精确匹配 Allow |
| 8 | `check_permission` 精确匹配 Deny |
| 9 | `check_permission` 通配匹配（`permission="*"`） |
| 10 | `check_permission` 精确匹配优先于通配 |
| 11 | `check_permission` 无匹配 → 默认 Allow |

#### 🟢 绿 — 实现

```rust
pub fn merge_permissions(
    parent_session_permission: &[PermissionRule],
    parent_agent_permission: &[PermissionRule],
    subagent_permission: &[PermissionRule],
) -> Vec<PermissionRule>;

pub fn check_permission(
    name: &str,
    args: &serde_json::Value,
    rules: &[PermissionRule],
) -> PermissionDecision;

pub enum PermissionDecision {
    Allowed,
    Denied(String),
}
```

`merge_permissions`：父 session deny → 父 agent deny → 子 agent 规则 → 检查兜底 `*: deny`

`check_permission`：两轮匹配（精确→通配），无匹配默认 Allow

#### 🧪 测试 → 执行

```bash
cargo test -p visp-core -- agent_definition && cargo clippy -p visp-core -- -D warnings
```

#### ♻️ 重构

- 提取 `has_deny_all` 辅助函数
- 检查精度匹配和通配匹配的重复逻辑提取

#### 👁️ 代码审核

- `PermissionDecision::Denied` 携带原因字符串
- 兜底 `*: deny` 放在最后不影响子 agent 显式 allow
- `check_permission` 的 pattern 匹配用 glob？当前用前缀匹配即可（V1 简化）

#### 📦 提交

```bash
git add crates/visp-core/src/agent_definition.rs
git commit -m "feat(core): add merge_permissions and check_permission"
```

---

### 2b：AgentRegistry 测试

**文件**：`crates/visp-core/src/agent_registry.rs`

#### 🔴 红 — 测试

| # | 测试用例 |
|---|---------|
| 1 | `register` 后 `get` 返回 agent |
| 2 | `register` 同名返回 `Err(CoreError::Tool(...))` |
| 3 | `get` 不存在的名返回 `None` |
| 4 | `default` 取名为 "default" 的 primary agent |
| 5 | `default` 无"default"名时取第一个 primary |
| 6 | `default` 全 subagent 时返回 `None` |
| 7 | `list` 返回所有注册的 agent |
| 8 | `list_subagents` 只返回 `mode == Subagent` 的 agent |

#### 🟢 绿 — 实现

`AgentRegistry` 所有方法完整实现。

#### 🧪 测试 → 执行

```bash
cargo test -p visp-core -- agent_registry && cargo clippy -p visp-core -- -D warnings
```

#### ♻️ 重构

- `default()` 的查找逻辑提取为 `find_first_primary` 辅助

#### 👁️ 代码审核

- `register` 错误类型用 `CoreError` 还是 `AgentError`？当前无 `AgentError`，用 `CoreError::Tool` 暂代或新增 `CoreError::Agent`
- `default()` 退化到 `None` 的安全处理

#### 📦 提交

```bash
git add crates/visp-core/src/agent_registry.rs
git commit -m "feat(core): implement AgentRegistry with tests"
```

---

### 2c：create_sub 测试

**文件**：`crates/visp-core/src/session.rs`

#### 🔴 红 — 测试

| # | 测试用例 |
|---|---------|
| 1 | `create_sub` 正常创建，返回 session 字段正确 |
| 2 | `create_sub` 返回的 session 含正确 `parent_id` |
| 3 | `create_sub` 返回的 session 含正确 `agent_name` |
| 4 | `create_sub` 返回的 session 含正确 `permission` |
| 5 | `create_sub` 传入 `session_id` 时复用已有 session |
| 6 | `create_sub` 不传 `session_id` 时生成新 UUID |

#### 🟢 绿 — 实现

`SessionManager::create_sub` 完整实现。

#### 🧪 测试 → 执行

```bash
cargo test -p visp-core -- session && cargo clippy -p visp-core -- -D warnings
```

#### ♻️ 重构

- `create` 和 `create_sub` 的共有逻辑提取为 `new_session` 内部方法

#### 👁️ 代码审核

- 复用 session 时不做权限合并（只返回已有 session）
- system_prompt 构造是否包含 agent 名称

#### 📦 提交

```bash
git add crates/visp-core/src/session.rs
git commit -m "feat(core): implement SessionManager::create_sub"
```

---

### 2d：AgentConfig max_depth 测试

**文件**：`crates/visp-core/src/agent.rs`

#### 🔴 红 — 测试

| # | 测试用例 |
|---|---------|
| 1 | `AgentConfig::default()` 的 `max_depth` 为 5 |
| 2 | 构造时设置 `max_depth = 3` 后读取为 3 |

#### 🟢 绿 — 实现

`AgentConfig` 加字段 + 更新 `Default` impl + `Deserialize` 兼容（若需要）。

#### 🧪 测试 → 执行

```bash
cargo test -p visp-core -- agent::tests && cargo clippy -p visp-core -- -D warnings
```

#### ♻️ 重构

- 无

#### 👁️ 代码审核

- 默认值 5
- 不影响现有 `daemon.toml` 解析

#### 📦 提交

```bash
git add crates/visp-core/src/agent.rs
git commit -m "feat(core): add max_depth with default 5"
```

---

## 步骤 3：工具执行改造（Wave 2）

### 3a：AgentMessage 发送

**文件**：`crates/visp-core/src/agent.rs`

#### 🔴 红 — 测试

| # | 测试用例 |
|---|---------|
| 1 | `run_agent_loop` 中 TextDelta 通过 `global_tx` 的 `Envelope` 发送 |
| 2 | ToolCallRequest 通过 `global_tx` 发送 |
| 3 | Done 通过 `global_tx` 发送 |

#### 🟢 绿 — 实现

在 `run_agent_loop` 中，将所有 `tx.send(AgentEvent::Xxx)` 替换为：

```rust
let _ = ctx.global_tx.send(Envelope {
    session_id: ctx.session_id.clone(),
    message: AgentMessage::Xxx { ... },
}).await;
```

保留 `tx` 参数作为向后兼容路径（当 `global_tx` 不存在时 fallback）。

#### 🧪 测试 → 执行

```bash
cargo test -p visp-core -- agent && cargo clippy -p visp-core -- -D warnings
```

#### ♻️ 重构

- 引入 `try_send!` 宏的变体统一处理两种发送路径

#### 👁️ 代码审核

- 单 Agent 模式和原来行为一致（`global_tx` 用 `Option` 包裹）
- 所有 `AgentEvent` 发送点全覆盖

#### 📦 提交

```bash
git add crates/visp-core/src/agent.rs
git commit -m "feat(core): send AgentMessage through global_tx in run_agent_loop"
```

---

### 3b：Tool Execution select! 改造

**文件**：`crates/visp-core/src/agent.rs`

#### 🔴 红 — 测试

| # | 测试用例 |
|---|---------|
| 1 | Phase 1：tool 名为 "task" 时发 `SpawnRequest` 而非 execute |
| 2 | Phase 1：普通工具走 spawn |
| 3 | Phase 2：收到 `SubAgentComplete` → 收集结果 |
| 4 | Phase 2：收到 `SubAgentError` → 收集错误结果 |
| 5 | Phase 2：收到 `Cancelled` → 退出 agent loop |
| 6 | Phase 2：所有 pending 完成 + regular done → break |

#### 🟢 绿 — 实现

在 `run_agent_loop` 的工具执行阶段：

```rust
// Phase 1: 分派
for tc in tool_calls {
    if tc.name == "task" {
        // 发 SpawnRequest 到 global_tx
        // 记录 pending
    } else {
        // spawn 执行
    }
}

// Phase 2: select! 收集
loop {
    tokio::select! {
        batch = join_all(exec_tasks) => { regular_done = true; }
        Some(msg) = inbox_rx.recv() => { match msg { ... } }
        _ = cancel_token.cancelled() => { return; }
    }
}
```

通过 `ctx.inbox_rx` 和 `ctx.global_tx` 是否为 `Some` 判断走新路径还是旧路径（向后兼容）。

#### 🧪 测试 → 执行

```bash
cargo test -p visp-core -- agent && cargo clippy -p visp-core -- -D warnings
```

#### ♻️ 重构

- 提取 `dispatch_tool_calls` 和 `collect_tool_results` 两个内部函数
- 新旧路径共享部分逻辑

#### 👁️ 代码审核

- 向后兼容路径：`global_tx` / `inbox_rx` 为 `None` 时仍走旧 `join_all`
- `TaskArgs` 解析错误处理（serde_json::from_str 失败 → 返回错误结果）
- 并发 task 与普通工具混合场景正确

#### 📦 提交

```bash
git add crates/visp-core/src/agent.rs
git commit -m "feat(core): implement select!-based tool execution for multi-agent"
```

---

### 3c：start_loop_v2

**文件**：`crates/visp-core/src/session.rs`

#### 🟢 绿 — 实现

```rust
impl SessionManager {
    pub fn start_loop_v2(
        &self,
        id: &str,
        context_trimmer: &Arc<dyn ContextTrimmer + Send + Sync>,
        global_tx: mpsc::Sender<Envelope>,
        inbox_rx: mpsc::Receiver<OrchestratorMessage>,
        permission_rules: Arc<Vec<PermissionRule>>,
    ) -> Result<AgentLoopContext, SessionError>;
}
```

与 `start_loop` 区别：接受额外三个参数并存入 `AgentLoopContext`。

#### 🧪 测试 → 类型检查

```bash
cargo build -p visp-core && cargo clippy -p visp-core -- -D warnings
```

#### 👁️ 代码审核

- 与 `start_loop` 的代码重复度（可提取共有逻辑）

#### 📦 提交

```bash
git add crates/visp-core/src/session.rs
git commit -m "feat(core): add start_loop_v2 for multi-agent context"
```

---

## 步骤 4：Task 工具定义（Wave 3）

### 4a：TaskTool

**文件**：新建 `crates/visp-tools/src/task.rs`

#### 🔴 红 — 测试

| # | 测试用例 |
|---|---------|
| 1 | `name()` 返回 `"task"` |
| 2 | `parameters()` 含 `subagent_type` 字段，类型 string，required |
| 3 | `parameters()` 含 `description` 字段，类型 string，required |
| 4 | `parameters()` 含 `task_id` 字段，类型 string，optional |
| 5 | `execute()` 调用时 panic 或返回错误 |

#### 🟢 绿 — 实现

```rust
pub struct TaskTool;

impl Tool for TaskTool {
    fn name(&self) -> &str { "task" }
    fn description(&self) -> &str { "启动一个子 Agent 处理复杂任务。当任务适合某个专门的 Agent 时使用。" }
    fn parameters(&self) -> serde_json::Value { ... }
    fn execute(&self, _args, _ctx) -> ToolResult {
        unreachable!("task tool execution is handled by agent loop")
    }
    fn category(&self) -> &str { "agent" }
}
```

#### ♻️ 重构

- `crates/visp-tools/src/mod.rs` 追加 `pub mod task;`

#### 🧪 测试 → 执行

```bash
cargo test -p visp-tools -- task && cargo clippy -p visp-tools -- -D warnings
```

#### 👁️ 代码审核

- JSON Schema 的 `required` 数组正确
- `category()` 返回新值 `"agent"`（CLI 可能据此过滤显示）

#### 📦 提交

```bash
git add crates/visp-tools/src/task.rs crates/visp-tools/src/mod.rs
git commit -m "feat(tools): add TaskTool stub"
```

---

## 步骤 5：运行时 Orchestrator（Wave 4）

### 5a：visp-agent crate 骨架

**文件**：新建 `crates/visp-agent/`

#### 🟢 绿 — 实现

```bash
cargo new crates/visp-agent --lib
```

```toml
# crates/visp-agent/Cargo.toml
[package]
name = "visp-agent"
version.workspace = true
edition.workspace = true

[dependencies]
visp-core = { path = "../visp-core" }
visp-llm = { path = "../visp-llm" }
visp-tools = { path = "../visp-tools" }
tokio.workspace = true
futures.workspace = true
tracing.workspace = true
```

文件结构：

```
crates/visp-agent/src/
├── lib.rs
├── active_agent.rs
├── orchestrator.rs
└── event_bus.rs
```

`lib.rs` 导出所有模块。

#### ♻️ 重构

- 根 `Cargo.toml` 的 `members` 追加 `"crates/visp-agent"`

#### 🧪 测试 → 类型检查

```bash
cargo build -p visp-agent && cargo clippy -p visp-agent -- -D warnings
```

#### 👁️ 代码审核

- workspace `members` 追加正确

#### 📦 提交

```bash
git add crates/visp-agent/ Cargo.toml Cargo.lock
git commit -m "feat(agent): create visp-agent crate skeleton"
```

---

### 5b：ActiveAgent + ActiveAgentRegistry

**文件**：`crates/visp-agent/src/active_agent.rs`

#### 🔴 红 — 测试

| # | 测试用例 |
|---|---------|
| 1 | `register` 后 `get` 返回 agent |
| 2 | `register` 同 session_id 覆盖旧值 |
| 3 | `remove` 后 `get` 返回 None |
| 4 | `children_of` 返回直接子 agent |
| 5 | `children_of` 无子时返回空列表 |
| 6 | `descendants_of` 递归查找两代（A→B→C） |
| 7 | `descendants_of` 无后代时返回空列表 |
| 8 | `compute_depth` 根 agent（parent_id=None）返回 0 |
| 9 | `compute_depth` 子 agent 返回 1 |
| 10 | `compute_depth` 孙 agent 返回 2 |

#### 🟢 绿 — 实现

```rust
pub struct ActiveAgent {
    pub session_id: String,
    pub parent_session_id: Option<String>,
    pub agent_name: String,
    pub cancel_token: CancellationToken,
    pub inbox: mpsc::Sender<OrchestratorMessage>,
    pub pending_call_id: Option<String>,
    pub started_at: Instant,
}

pub struct ActiveAgentRegistry {
    agents: HashMap<String, ActiveAgent>,
}

impl ActiveAgentRegistry {
    pub fn new() -> Self;
    pub fn register(&mut self, agent: ActiveAgent);
    pub fn remove(&mut self, session_id: &str) -> Option<ActiveAgent>;
    pub fn get(&self, session_id: &str) -> Option<&ActiveAgent>;
    pub fn get_mut(&mut self, session_id: &str) -> Option<&mut ActiveAgent>;
    pub fn children_of(&self, parent_id: &str) -> Vec<&ActiveAgent>;
    pub fn descendants_of(&self, parent_id: &str) -> Vec<&ActiveAgent>;
    pub fn compute_depth(&self, session_id: &str) -> u32;
}
```

#### 🧪 测试 → 执行

```bash
cargo test -p visp-agent -- active_agent && cargo clippy -p visp-agent -- -D warnings
```

#### ♻️ 重构

- `descendants_of` 用 BFS 而非递归（栈溢出防护）？列表规模小，递归可行

#### 👁️ 代码审核

- `compute_depth` 在 session_id 不存在于 registry 时返回 0 而非 panic
- `children_of` / `descendants_of` 返回 `Vec<&ActiveAgent>` 不拥有所有权

#### 📦 提交

```bash
git add crates/visp-agent/src/active_agent.rs crates/visp-agent/src/lib.rs
git commit -m "feat(agent): add ActiveAgent and ActiveAgentRegistry"
```

---

### 5c：Orchestrator — handle_agent_message

**文件**：`crates/visp-agent/src/orchestrator.rs`

#### 🔴 红 — 测试

| # | 测试用例 |
|---|---------|
| 1 | TextDelta → `grpc_tx.send(ServerMessage::TextDelta { ... })` |
| 2 | ToolCallRequest → `grpc_tx.send(ServerMessage::ToolCall { ... })` |
| 3 | ToolCallResult → `grpc_tx.send(ServerMessage::ToolResult { ... })` |
| 4 | StatusUpdate → `grpc_tx.send(ServerMessage::StatusUpdate { ... })` |
| 5 | UserQuery → `pending_queries` 插入 + `grpc_tx.send(UserQuery { ... })` |
| 6 | SpawnRequest → 调用 `self.spawn_sub_agent(...)` |
| 7 | Done → 调用 `self.handle_done(session_id)` |
| 8 | Error → `grpc_tx.send(AgentError)` + `handle_done` |
| 9 | 子 Agent Done → `parent.inbox.try_send(SubAgentComplete { ... })` |
| 10 | parent inbox 满 → spawn 后台任务等待发送 |
| 11 | parent inbox 关闭 → 丢弃并 log warning |

#### 🟢 绿 — 实现

实现 `async fn handle_agent_message(&mut self, envelope: Envelope)`，match 所有变体，转发对应消息到 `self.grpc_tx`，调用对应内部方法。

#### 🧪 测试 → 执行

```bash
cargo test -p visp-agent -- orchestrator && cargo clippy -p visp-agent -- -D warnings
```

#### ♻️ 重构

- `grpc_tx.send(...)` 的重复 `.await` 提取为 `send_to_cli` 辅助方法

#### 👁️ 代码审核

- 所有 `send` 使用 `let _ = ...` 忽略结果（CLI 断开时丢弃）
- `handle_done` 只移除一次
- `SpawnRequest` 没有对应 agent 时如何处理？`step 5f` 中 `agent_registry.get` 会 `expect`，应改为 `if let Some` + `SubAgentError`

#### 📦 提交

```bash
git add crates/visp-agent/src/orchestrator.rs
git commit -m "feat(agent): implement handle_agent_message"
```

---

### 5d：Orchestrator — handle_client_message

**文件**：`crates/visp-agent/src/orchestrator.rs`

#### 🔴 红 — 测试

| # | 测试用例 |
|---|---------|
| 1 | `UserInput` 时 session `Idle` → 调用 `start_main_agent` |
| 2 | `UserInput` 时 session `Running` → 忽略 |
| 3 | `UserQueryResponse` 匹配 `query_id` → `respond.send(...)` |
| 4 | `UserQueryResponse` 不匹配 `query_id` → 忽略 |

#### 🟢 绿 — 实现

```rust
async fn handle_client_message(&mut self, msg: ClientMessage) {
    match msg {
        ClientMessage::UserInput { session_id, text } => {
            if let Ok(session) = self.session_mgr.get(&session_id) {
                if session.status == SessionStatus::Idle {
                    self.start_main_agent(session_id, text);
                }
            }
        }
        ClientMessage::UserQueryResponse { query_id, selected_index, text } => {
            if let Some((_, respond)) = self.pending_queries.remove(&query_id) {
                let _ = respond.send(UserQueryResult { selected_index, text });
            }
        }
    }
}
```

#### 🧪 测试 → 执行

```bash
cargo test -p visp-agent -- orchestrator && cargo clippy -p visp-agent -- -D warnings
```

#### 👁️ 代码审核

- `ClientMessage` 类型定义来自何处？（visp-proto 或 visp-core 的自定义类型）
- `pending_queries.remove` 拿到 respond 后 `send` 不会阻塞（oneshot 的 send 是同步非阻塞）

#### 📦 提交

```bash
git add crates/visp-agent/src/orchestrator.rs
git commit -m "feat(agent): implement handle_client_message"
```

---

### 5e：Orchestrator — start_main_agent

**文件**：`crates/visp-agent/src/orchestrator.rs`

#### 🔴 红 — 测试

| # | 测试用例 |
|---|---------|
| 1 | 注册 ActiveAgent，`parent_session_id = None` |
| 2 | 使用 session 的 `agent_name` 查找 agent 定义 |
| 3 | 根据 agent model / session model / 默认 key 选择 provider |
| 4 | `tokio::spawn` 被调用 |

#### 🟢 绿 — 实现

伪代码见设计文档 6.6 节。关键点：
- 创建 inbox 通道
- 注册 ActiveAgent（无 parent）
- 查找 provider（agent.model → session.config.model → self.default_provider_key）
- 构建 AgentLoopContext
- tokio::spawn(run_agent_loop(...))

#### 🧪 测试 → 执行

```bash
cargo test -p visp-agent -- orchestrator && cargo clippy -p visp-agent -- -D warnings
```

#### ♻️ 重构

- provider 查找逻辑提取为 `resolve_provider(&self, agent: &AgentDefinition, session: &Session) -> Arc<dyn LlmProvider>`

#### 👁️ 代码审核

- `agent_registry.get(&session.agent_name)` 失败时 panic？应 log error + return
- provider 不存在时 panic？应 log error + return
- provider key 格式统一（`"provider.name"`）

#### 📦 提交

```bash
git add crates/visp-agent/src/orchestrator.rs
git commit -m "feat(agent): implement start_main_agent"
```

---

### 5f：Orchestrator — spawn_sub_agent

**文件**：`crates/visp-agent/src/orchestrator.rs`

#### 🔴 红 — 测试

| # | 测试用例 |
|---|---------|
| 1 | 创建子 session（调用 `session_mgr.create_sub`） |
| 2 | 权限继承（调用 `merge_permissions`） |
| 3 | 注册 ActiveAgent（`parent_session_id` 正确） |
| 4 | `task_id` 传入时复用 session |
| 5 | `compute_depth` ≥ `max_depth` → 发 `SubAgentError`，不 spawn |
| 6 | 根据 subagent model / parent model / 默认 key 选择 provider |
| 7 | `tokio::spawn` 被调用 |

#### 🟢 绿 — 实现

伪代码见设计文档 6.7 节。关键流程：
1. 深度检查（超过 max_depth 发 SubAgentError 并 return）
2. 查 Agent 定义
3. 合并权限（merge_permissions）
4. 创建子 Session（create_sub）
5. 创建 inbox + 注册 ActiveAgent
6. 查找 provider
7. 构建 AgentLoopContext
8. tokio::spawn(run_agent_loop(...))

#### 🧪 测试 → 执行

```bash
cargo test -p visp-agent -- orchestrator && cargo clippy -p visp-agent -- -D warnings
```

#### ♻️ 重构

- `start_main_agent` 和 `spawn_sub_agent` 的共有部分提取为 `spawn_agent_loop` 内部方法

#### 👁️ 代码审核

- 深度检查在子 agent 创建 session 之前（避免创建无用 session）
- 未知 `subagent_type` 用 SubAgentError 而非 panic
- `create_sub` 传权限规则正确

#### 📦 提交

```bash
git add crates/visp-agent/src/orchestrator.rs
git commit -m "feat(agent): implement spawn_sub_agent"
```

---

### 5g：Orchestrator — handle_done + extract_result

**文件**：`crates/visp-agent/src/orchestrator.rs`

#### 🔴 红 — 测试

| # | 测试用例 |
|---|---------|
| 1 | 主 Agent Done → `session_mgr.finish_loop` 调用 |
| 2 | 子 Agent Done → `parent.inbox.try_send(SubAgentComplete { ... })` |
| 3 | 子 Agent Done 但 parent 已不存在 → 丢弃并 log warning |
| 4 | `extract_result` 从 session 消息中返回最后一条 assistant 的 text |
| 5 | `extract_result` 无 assistant 消息时返回空字符串 |

#### 🟢 绿 — 实现

```rust
fn handle_done(&mut self, session_id: &str) {
    if let Some(agent) = self.active_agents.remove(session_id) {
        if let Some(parent_id) = agent.parent_session_id {
            let content = self.extract_result(session_id);
            let call_id = agent.pending_call_id.unwrap_or_default();
            if let Some(parent) = self.active_agents.get(&parent_id) {
                match parent.inbox.try_send(OrchestratorMessage::SubAgentComplete { ... }) {
                    Ok(()) => {}
                    Err(TrySendError::Full(msg)) => {
                        tokio::spawn(async move { let _ = inbox.send(msg).await; });
                    }
                    Err(TrySendError::Closed(_)) => tracing::warn!(...),
                }
            }
        } else {
            self.session_mgr.finish_loop(session_id, SessionStatus::Idle);
        }
    }
}

fn extract_result(&self, session_id: &str) -> String { ... }
```

#### 🧪 测试 → 执行

```bash
cargo test -p visp-agent -- orchestrator && cargo clippy -p visp-agent -- -D warnings
```

#### ♻️ 重构

- `try_send_or_spawn` 辅助方法（inbox 满时 spawn 重试的模式在 handle_agent_message 也用到）

#### 👁️ 代码审核

- `pending_call_id.unwrap_or_default()` 对空的 `call_id` 安全（父 agent match 时找不到对应 tool_call 会 skip，但不会 crash）
- `extract_result` 用 `get_messages` 从持久化存储查询而非内存（SessionManager 内部已缓存）

#### 📦 提交

```bash
git add crates/visp-agent/src/orchestrator.rs
git commit -m "feat(agent): implement handle_done and extract_result"
```

---

### 5h：Orchestrator — cancel_agent

**文件**：`crates/visp-agent/src/orchestrator.rs`

#### 🔴 红 — 测试

| # | 测试用例 |
|---|---------|
| 1 | 取消 agent → 该 agent 的 `cancel_token.cancel()` 被调用 |
| 2 | 取消 agent → 递归取消所有子孙 agent 的 cancel_token |
| 3 | 取消 agent 无子孙 → 只取消自身，不 panic |

#### 🟢 绿 — 实现

```rust
fn cancel_agent(&mut self, session_id: &str) {
    if let Some(agent) = self.active_agents.get(session_id) {
        agent.cancel_token.cancel();
    }
    for child in self.active_agents.descendants_of(session_id) {
        child.cancel_token.cancel();
    }
}
```

#### 🧪 测试 → 执行

```bash
cargo test -p visp-agent -- orchestrator && cargo clippy -p visp-agent -- -D warnings
```

#### ♻️ 重构

- 无

#### 👁️ 代码审核

- 不等待子 agent 实际停止（cancel 是异步信号）
- 子 agent 的 Done 会通过正常路径清理注册表

#### 📦 提交

```bash
git add crates/visp-agent/src/orchestrator.rs
git commit -m "feat(agent): implement cancel_agent"
```

---

### 5i：Orchestrator — run 主循环

**文件**：`crates/visp-agent/src/orchestrator.rs`

#### 🔴 红 — 测试

| # | 测试用例 |
|---|---------|
| 1 | `cancel_rx` 消息优先处理（biased select 保证） |
| 2 | `global_rx` 消息被 `handle_agent_message` 处理 |
| 3 | `grpc_rx` 消息被 `handle_client_message` 处理 |

#### 🟢 绿 — 实现

```rust
pub struct Orchestrator {
    cancel_rx: mpsc::Receiver<CancelSignal>,
    global_rx: mpsc::Receiver<Envelope>,
    grpc_rx: mpsc::Receiver<ClientMessage>,
    grpc_tx: mpsc::Sender<ServerMessage>,
    active_agents: ActiveAgentRegistry,
    pending_queries: HashMap<String, (String, oneshot::Sender<UserQueryResult>)>,
    session_mgr: Arc<SessionManager>,
    agent_registry: Arc<AgentRegistry>,
    tool_registry: Arc<ToolRegistry>,
    rule_engine: Arc<RuleEngine>,
    agent_config: AgentConfig,
    providers: HashMap<String, Arc<dyn LlmProvider>>,
    default_provider_key: String,
}

impl Orchestrator {
    pub async fn run(&mut self) {
        loop {
            tokio::select! {
                biased;
                Some(signal) = self.cancel_rx.recv() => {
                    self.cancel_agent(&signal.session_id);
                }
                Some(envelope) = self.global_rx.recv() => {
                    self.handle_agent_message(envelope).await;
                }
                Some(msg) = self.grpc_rx.recv() => {
                    self.handle_client_message(msg).await;
                }
                else => break,
            }
        }
    }
}
```

#### 🧪 测试 → 执行

```bash
cargo test -p visp-agent -- orchestrator && cargo clippy -p visp-agent -- -D warnings
```

#### ♻️ 重构

- 构造 `Orchestrator` 用 builder 模式？当前直接构造 struct 即可（字段多但 daemon 组装时一次性创建）

#### 👁️ 代码审核

- biased select 保证 cancel 最高优先级
- `else => break`：所有通道关闭时优雅退出
- 所有通道 buffer size 是否合理（global 256，grpc 256，cancel 16）

#### 📦 提交

```bash
git add crates/visp-agent/src/orchestrator.rs crates/visp-agent/src/lib.rs
git commit -m "feat(agent): implement Orchestrator main loop"
```

---

## 步骤 6：集成层（Wave 5）

### 6a：V3 DB Migration

**文件**：`crates/visp-db/src/schema.rs`

#### 🔴 红 — 测试

| # | 测试用例 |
|---|---------|
| 1 | V3 迁移后 session 表有 `agent_name TEXT NOT NULL DEFAULT 'default'` 列 |
| 2 | V3 迁移后 session 表有 `parent_id TEXT` 列 |
| 3 | V3 迁移后 session 表有 `permission_json TEXT` 列 |
| 4 | 重复执行 V3 迁移不报错（幂等） |

#### 🟢 绿 — 实现

```sql
ALTER TABLE session ADD COLUMN agent_name TEXT NOT NULL DEFAULT 'default';
ALTER TABLE session ADD COLUMN parent_id TEXT;
ALTER TABLE session ADD COLUMN permission_json TEXT;
```

使用 `has_column` 检查保证幂等。

更新 `Migrator::VERSION = 3`。

#### 🧪 测试 → 执行

```bash
cargo test -p visp-db -- schema && cargo clippy -p visp-db -- -D warnings
```

#### ♻️ 重构

- 无

#### 👁️ 代码审核

- `permission_json` 的序列化/反序列化格式（serde_json::Value，用 `Vec<PermissionRule>`）
- 旧 session 迁移后 `agent_name = "default"`，`parent_id = NULL`，`permission_json = "[]"`

#### 📦 提交

```bash
git add crates/visp-db/src/schema.rs
git commit -m "feat(db): V3 migration for multi-agent fields"
```

---

### 6b：Agent 文件加载

**文件**：`crates/visp-daemon/src/main.rs`

#### 🔴 红 — 测试

| # | 测试用例 |
|---|---------|
| 1 | 从 `.visp/agents/` 目录加载 `.md` 文件为 AgentDefinition |
| 2 | 空目录只返回内建 default agent |
| 3 | 格式错误的文件跳过（不 panic，log warning） |
| 4 | 同名 agent 覆盖内建 default |

#### 🟢 绿 — 实现

```rust
fn load_agents(project_path: &Path) -> AgentRegistry {
    let mut registry = AgentRegistry::new();
    // 内建 default
    registry.register(AgentDefinition {
        name: "default".into(), ..默认
    }).ok();
    // 扫描 .visp/agents/*.md
    for entry in glob(&format!("{}/.visp/agents/*.md", project_path.display())) {
        match parse_agent_md(&entry) {
            Ok(def) => { let _ = registry.register(def); }
            Err(e) => tracing::warn!("skip invalid agent file {entry}: {e}"),
        }
    }
    registry
}
```

`parse_agent_md`：读取文件 → 分离 YAML frontmatter 和 Markdown body → 解析元数据为 AgentDefinition。

#### 🧪 测试 → 执行

```bash
cargo test -p visp-daemon && cargo clippy -p visp-daemon -- -D warnings
```

#### ♻️ 重构

- `parse_agent_md` 可提取到单独文件 `agent_loader.rs` 供测试

#### 👁️ 代码审核

- YAML frontmatter 解析库选型（现有的 `serde_yaml` 或 `toml`？使用 `yaml-rust2` 或 `serde_yaml`）
- glob 路径跨平台兼容

#### 📦 提交

```bash
git add crates/visp-daemon/src/main.rs
git commit -m "feat(daemon): load agent definitions from .visp/agents/"
```

---

### 6c：Daemon 初始化 Orchestrator

**文件**：`crates/visp-daemon/src/main.rs`

#### 🟢 绿 — 实现

```rust
// 创建通道
let (global_tx, global_rx) = mpsc::channel(256);
let (cancel_tx, cancel_rx) = mpsc::channel(16);
let (grpc_tx, grpc_rx) = mpsc::channel(256);

// 预创建 provider HashMap
let providers: HashMap<String, Arc<dyn LlmProvider>> = model_configs.iter()
    .filter_map(|mc| create_llm_provider(mc).ok().map(|p| (mc.key(), p)))
    .collect();

// 创建并启动 Orchestrator
let orchestrator = Orchestrator {
    cancel_rx,
    global_rx,
    grpc_rx,
    grpc_tx: grpc_tx.clone(),
    active_agents: ActiveAgentRegistry::new(),
    pending_queries: HashMap::new(),
    session_mgr: session_mgr.clone(),
    agent_registry: Arc::new(agent_registry),
    tool_registry: tool_registry.clone(),
    rule_engine: rule_engine.clone(),
    agent_config,
    providers,
    default_provider_key: default_model_key,
};
tokio::spawn(async move { orchestrator.run().await });
```

`cancel_tx` 和 `grpc_tx` 传入 `service.rs`。

#### 🧪 测试 → 类型检查

```bash
cargo build && cargo clippy -- -D warnings
```

#### 👁️ 代码审核

- provider 创建时 `filter_map` 跳过失败的（不 panic）
- 通道容量合理

#### 📦 提交

```bash
git add crates/visp-daemon/src/main.rs
git commit -m "feat(daemon): initialize Orchestrator with all dependencies"
```

---

### 6d：service.rs 简化

**文件**：`crates/visp-daemon/src/service.rs`

#### 🟢 绿 — 实现

`Chat` RPC 简化为：

```rust
async fn chat(&self, req: Request<Streaming<ClientMessage>>) -> Result<Response<Self::ChatStream>, Status> {
    let mut stream = req.into_inner();
    let (response_tx, response_rx) = mpsc::channel(256);
    let response_stream = ReceiverStream::new(response_rx);

    // inbound: CLI → Orchestrator
    let client_tx = self.grpc_client_tx.clone();
    tokio::spawn(async move {
        while let Some(Ok(msg)) = stream.next().await {
            if client_tx.send(msg).await.is_err() { break; }
        }
    });

    // outbound: Orchestrator → CLI
    let mut grpc_rx = self.grpc_server_rx.clone();
    tokio::spawn(async move {
        while let Some(msg) = grpc_rx.recv().await {
            if response_tx.send(Ok(msg)).await.is_err() { break; }
        }
    });

    Ok(Response::new(Box::pin(response_stream)))
}
```

**取消信号**：通过独立通道 `cancel_tx`，不走 gRPC 流。

#### 🧪 测试 → 类型检查

```bash
cargo build && cargo clippy -- -D warnings
```

#### 👁️ 代码审核

- `grpc_client_tx` / `grpc_server_rx` 在 `Service` 结构体中通过 `Arc<Mutex<...>>` 或通道包装共享
- 取消信号通过 `cancel_tx` 独立发送（避免被 gRPC 流阻塞）

#### 📦 提交

```bash
git add crates/visp-daemon/src/service.rs
git commit -m "refactor(daemon): simplify service.rs to pure protocol adapter"
```

---

## 步骤 7：端到端验证（Wave 6）

### 7a：集成测试

**文件**：`crates/visp-agent/tests/e2e.rs` 或 `crates/visp-agent/src/lib.rs` 中的 `#[cfg(test)] mod integration`

#### 🔴 红 — 测试

| # | 测试用例 |
|---|---------|
| 1 | LLM 返回 `task(subagent_type="code-review")` → 子 agent 启动并运行至完成 |
| 2 | 子 agent 完成 → 父 agent 收到 SubAgentComplete，继续下一轮 LLM 调用 |
| 3 | 子 agent 继承父 agent 的 deny 规则（如 `edit: deny`） |
| 4 | 取消主 agent → 子 agent 的 cancel_token 也被 cancel |
| 5 | `max_depth = 1` 时创建子 agent 的子 agent → SubAgentError |

#### 🟢 绿 — 实现

- 使用 mock LLM provider 预设返回 `tool_call: task(...)`
- 使用 `InMemorySessionStore`
- 注册 `TaskTool` + `MockTool("read", ...)` + `MockTool("edit", ...)`
- 完整实例化 `Orchestrator`
- 模拟 `UserInput` → 走完完整子 agent 生命周期

#### 🧪 测试 → 执行

```bash
cargo test && cargo clippy -- -D warnings && cargo fmt -- --check
```

#### ♻️ 重构

- `MockLlmProvider` 提取到测试工具模块供复用

#### 👁️ 代码审核

- 测试覆盖了主要成功路径和关键错误路径
- mock 足够真实（模拟 LLM 多次调用 + 工具返回）

#### 📦 提交

```bash
git add crates/visp-agent/tests/ && git commit -m "test(agent): add end-to-end integration tests"
```

---

## 验证总标准

所有步骤执行完毕后：

```bash
cargo test && cargo clippy -- -D warnings && cargo fmt -- --check
```

**说明**：
- 步骤 1a~1f（Wave 0）只有 `🟢 绿` + `🧪 类型检查`，无测试（纯类型定义由编译器保证）
- 从步骤 2 起每步严格 `🔴 红` → `🟢 绿` → `🧪 执行` → `♻️ 重构` → `👁️ 审核` → `📦 提交`
