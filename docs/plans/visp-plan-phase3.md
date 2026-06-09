# visp Phase 3 工作计划：Agent 核心 + Daemon

## 概述

Phase 3 实现 Agent 编排循环和 gRPC daemon 服务。先扩展 visp-core 的基础类型，再实现核心模块，最后构建 visp-daemon。

每个子步骤都是一个独立的 TDD 循环：**红 → 绿 → 测试 → 类型检查 → 重构 → 提交**。

---

## 步骤 1：visp-core 基础类型扩展

### 1a：SessionError 扩展 + AgentErrorCode 枚举

#### 🔴 红 — 测试

在 `crates/visp-core/src/error.rs` 的测试模块中编写：

| # | 测试用例 |
|---|---|
| 1 | `test_session_busy_display` — `SessionError::SessionBusy { session_id: "x" }.to_string()` 包含 "x" |
| 2 | `test_session_protocol_error_display` — `SessionError::ProtocolError { message: "mismatch" }.to_string()` 包含 "mismatch" |
| 3 | `test_agent_error_code_display` — 验证 `AgentErrorCode` 各变体的 Display |

运行 `cargo test -p visp-core` 确认失败。

#### 🟢 绿 — 实现

- `SessionError` 新增：`SessionBusy { session_id: String }`、`ProtocolError { message: String }`
- 新增 `AgentErrorCode` 枚举（8 个变体：LlmAuth, LlmRateLimit, LlmApi, LlmNetwork, LlmStream, MaxIterations, Cancelled, Internal），derive `Debug, Clone, Display`

#### 🧪 测试 → 🔍 类型检查

```bash
cargo test -p visp-core && cargo clippy -p visp-core -- -D warnings
```

#### 📦 提交

```bash
git add crates/visp-core/src/error.rs
git commit -m "feat(visp-core): add SessionBusy, ProtocolError to SessionError; add AgentErrorCode enum"
```

---

### 1b：AgentConfig 结构体

#### 🔴 红 — 测试

在 `crates/visp-core/src/agent.rs` 新建测试模块：

| # | 测试用例 |
|---|---|
| 1 | `test_agent_config_default` — `AgentConfig::default()` 各字段为预期默认值 |
| 2 | `test_agent_config_custom` — 自定义字段值能正确读写 |

运行 `cargo test -p visp-core` 确认失败。

#### 🟢 绿 — 实现

- 新建 `crates/visp-core/src/agent.rs`
- 定义 `AgentConfig` 结构体，字段：`max_iterations: u32 (50)`, `llm_retry_attempts: u32 (3)`, `llm_retry_base_delay_ms: u64 (1000)`, `bash_confirm_mode: bool (true)`, `file_max_size_bytes: u64 (1048576)`
- 实现 `Default`

#### 🧪 测试 → 🔍 类型检查

```bash
cargo test -p visp-core && cargo clippy -p visp-core -- -D warnings
```

#### 📦 提交

```bash
git add crates/visp-core/src/agent.rs crates/visp-core/src/lib.rs
git commit -m "feat(visp-core): add AgentConfig struct with defaults"
```

---

### 1c：Tool trait 扩展（requires_approval）

#### 🔴 红 — 测试

在 `crates/visp-core/src/tool.rs` 测试模块中编写：

| # | 测试用例 |
|---|---|
| 1 | `test_tool_default_requires_approval` — mock 实现 Tool trait，不覆写 `requires_approval`，返回 `false` |

#### 🟢 绿 — 实现

- `Tool` trait 新增方法 `fn requires_approval(&self) -> bool { false }`（带默认实现）

#### 🧪 测试 → 🔍 类型检查

```bash
cargo test -p visp-core && cargo clippy -p visp-core -- -D warnings
```

#### 📦 提交

```bash
git add crates/visp-core/src/tool.rs
git commit -m "feat(visp-core): add requires_approval method to Tool trait"
```

---

### 1d：AgentEvent + AgentLoopContext

#### 🔴 红 — 测试

在 `crates/visp-core/src/agent.rs` 中编写：

| # | 测试用例 |
|---|---|
| 1 | `test_agent_event_text_delta` — 构造 TextDelta 变体能正确读取内容 |
| 2 | `test_agent_event_tool_call` — ToolCallRequest 变体能正确读取各字段 |
| 3 | `test_agent_event_user_query` — UserQuery 变体能携带 oneshot sender |
| 4 | `test_agent_loop_context_construction` — AgentLoopContext 各字段可正确构造和读取 |

#### 🟢 绿 — 实现

- `AgentEvent` 枚举：`TextDelta(String)`, `ToolCallRequest { call_id, tool_name, arguments }`, `ToolCallResult { call_id, content, is_error }`, `StatusUpdate(String)`, `Error { code: AgentErrorCode, message: String }`, `Done`, `UserQuery { query_id: String, message: String, respond: tokio::sync::oneshot::Sender<bool> }`
- `AgentLoopContext` 结构体：`session_id: String`, `history: Vec<Message>`, `working_dir: PathBuf`, `config: LlmConfig`, `cancel_token: CancellationToken`

#### 🧪 测试 → 🔍 类型检查

```bash
cargo test -p visp-core && cargo clippy -p visp-core -- -D warnings
```

#### 📦 提交

```bash
git add crates/visp-core/src/agent.rs
git commit -m "feat(visp-core): add AgentEvent enum and AgentLoopContext struct"
```

---

## 步骤 2：visp-core 核心模块

### 2a：Rule Engine（rules.rs）

#### 🔴 红 — 测试

新建 `crates/visp-core/src/rules.rs`，编写测试：

| # | 测试用例 |
|---|---|
| 1 | `test_rule_engine_loads_always_apply_true` — 创建临时 `.md` 文件含 `alwaysApply: true`，引擎加载后能获取规则内容 |
| 2 | `test_rule_engine_skips_always_apply_false` — `alwaysApply: false` 的规则不被加载 |
| 3 | `test_rule_engine_skips_no_marker` — 无 `alwaysApply:` 标记的文件不被加载 |
| 4 | `test_rule_engine_skips_non_md` — 非 `.md` 文件被忽略 |
| 5 | `test_rule_engine_missing_dir` — 规则目录不存在时静默跳过，引擎正常工作 |
| 6 | `test_rule_engine_project_before_global` — 验证项目规则排在全局规则前面 |

#### 🟢 绿 — 实现

- `RuleEngine` 结构体：`rules: Arc<RwLock<RuleSet>>`
- `RuleSet` 结构体：`content: String`, `last_loaded: Instant`, `files: Vec<RuleFile>`
- `RuleFile` 结构体：`path: PathBuf`, `content: String`
- `RuleEngine::new(project_path: &Path)` — 扫描项目 + 全局规则目录，构建 RuleSet
- `get_active_rules() -> String` — 返回当前活跃规则文本
- 扫描逻辑：递归 `project_path/.visp/rules/` 和 `~/.config/visp/rules/`，仅 `.md` 文件，前 5 行内查找 `alwaysApply: true`
- notify 热重载：独立 tokio task 监听两个目录，变更时重新扫描

#### 🧪 测试 → 🔍 类型检查

```bash
cargo test -p visp-core && cargo clippy -p visp-core -- -D warnings
```

#### 📦 提交

```bash
git add crates/visp-core/src/rules.rs crates/visp-core/Cargo.toml
git commit -m "feat(visp-core): RuleEngine with file loading and notify hot reload"
```

---

### 2b：Session Manager（session.rs）

#### 🔴 红 — 测试

新建 `crates/visp-core/src/session.rs`，编写测试：

| # | 测试用例 |
|---|---|
| 1 | `test_in_memory_store_crud` — create/get/list/delete 基本 CRUD 操作 |
| 2 | `test_session_manager_create` — 创建会话，状态为 Idle |
| 3 | `test_session_manager_start_loop` — start_loop 返回 AgentLoopContext，状态切为 Running |
| 4 | `test_session_manager_start_loop_busy` — Running 状态下再次 start_loop 返回 SessionBusy 错误 |
| 5 | `test_session_manager_finish_loop` — finish_loop 后状态切回 Idle，token 清除 |
| 6 | `test_session_manager_delete_cancels` — delete Running 会话，CancellationToken 被触发 |
| 7 | `test_session_manager_append_message` — append_message 将消息加入历史 |
| 8 | `test_session_manager_update_config` — update_config 修改会话的 LlmConfig |

#### 🟢 绿 — 实现

- `SessionStore` trait（已有基础，可能需要微调）
- `InMemorySessionStore`：`Arc<RwLock<HashMap<String, Session>>>`
- `Session` 结构体：`id: String`, `project_path: PathBuf`, `status: SessionStatus`, `created_at: Instant`, `history: Vec<Message>`, `config: LlmConfig`, `system_prompt_template: String`
- `SessionManager` 结构体：`store: Arc<dyn SessionStore>`, `running_tokens: RwLock<HashMap<String, CancellationToken>>`
- `SessionManager::create(project_path, config)` — 创建会话，加载系统 prompt 模板
- `SessionManager::start_loop(id) -> Result<AgentLoopContext, SessionError>` — 状态校验 + 切 Running + 创建 CancellationToken
- `SessionManager::finish_loop(id, status)` — 切状态 + 清理 token
- `SessionManager::append_message(id, msg)` — 追加消息
- `SessionManager::update_config(id, config)` — 更新 LlmConfig

#### 🧪 测试 → 🔍 类型检查

```bash
cargo test -p visp-core && cargo clippy -p visp-core -- -D warnings
```

#### 📦 提交

```bash
git add crates/visp-core/src/session.rs crates/visp-core/src/lib.rs
git commit -m "feat(visp-core): SessionManager with state machine and CancellationToken management"
```

---

### 2c：Prompt Builder（prompt.rs）

#### 🔴 红 — 测试

新建 `crates/visp-core/src/prompt.rs`，编写测试：

| # | 测试用例 |
|---|---|
| 1 | `test_prompt_builder_system_message` — 系统模板 + 规则内容拼接为一条 system 消息 |
| 2 | `test_prompt_builder_history_order` — 对话历史按原始顺序追加 |
| 3 | `test_prompt_builder_empty_rules` — 规则为空时 system 消息只包含模板 |
| 4 | `test_prompt_builder_empty_template` — 模板为空时 system 消息只包含规则 |

#### 🟢 绿 — 实现

- `PromptBuilder` 空 struct，提供 `build(system_template: &str, rules: &str, history: &[Message]) -> Vec<Message>`
- system 消息 = 模板 + 分隔 + 规则内容
- 对话历史按原始顺序追加

#### 🧪 测试 → 🔍 类型检查

```bash
cargo test -p visp-core && cargo clippy -p visp-core -- -D warnings
```

#### 📦 提交

```bash
git add crates/visp-core/src/prompt.rs crates/visp-core/src/lib.rs
git commit -m "feat(visp-core): PromptBuilder for system message and history assembly"
```

---

### 2d：Tool Registry（tool_registry.rs）

#### 🔴 红 — 测试

新建 `crates/visp-core/src/tool_registry.rs`，编写测试：

| # | 测试用例 |
|---|---|
| 1 | `test_tool_registry_register_and_get` — 注册工具后能按名称查找 |
| 2 | `test_tool_registry_definitions` — definitions() 返回所有工具的 ToolDefinition |
| 3 | `test_tool_registry_execute` — 通过 registry 执行工具，结果正确 |
| 4 | `test_tool_registry_duplicate_name` — 注册同名工具返回错误 |
| 5 | `test_tool_registry_get_not_found` — 查找不存在的工具返回 None |

#### 🟢 绿 — 实现

- `ToolRegistry` 结构体：`tools: Vec<Box<dyn Tool>>`
- `register(tool) -> Result<(), String>` — 注册，名称重复则报错
- `get(name) -> Option<&dyn Tool>` — 按名查找
- `definitions() -> Vec<ToolDefinition>` — 列出所有定义
- `names() -> Vec<String>` — 列出所有名称

#### 🧪 测试 → 🔍 类型检查

```bash
cargo test -p visp-core && cargo clippy -p visp-core -- -D warnings
```

#### 📦 提交

```bash
git add crates/visp-core/src/tool_registry.rs crates/visp-core/src/lib.rs
git commit -m "feat(visp-core): ToolRegistry for tool registration, lookup, and execution"
```

---

## 步骤 3：Agent 循环（agent.rs）

### 🔴 红 — 测试（使用 MockProvider）

编写测试（可同文件测试模块或独立 tests/agent_tests.rs）：

| # | 测试用例 |
|---|---|
| 1 | `test_agent_loop_simple_response` — MockProvider 返回单条 TextDelta → Done，验证输出 TextDelta + Done |
| 2 | `test_agent_loop_tool_call` — MockProvider 返回 ToolCall → Done，验证 ToolCallRequest + ToolCallResult 输出 |
| 3 | `test_agent_loop_multi_tool_batch` — MockProvider 返回 2 个 ToolCall → Done，验证两个工具都被执行 |
| 4 | `test_agent_loop_max_iterations` — 设置 max_iterations=1，MockProvider 一直返回 ToolCall，验证 Error(MaxIterations) |
| 5 | `test_agent_loop_cancellation` — 在 agent 循环运行中触发 CancellationToken，验证 Error(Cancelled) |
| 6 | `test_agent_loop_user_query` — MockProvider 返回 ToolCall（工具 requires_approval=true），验证 UserQuery 事件发送，oneshot 回复 true 后工具执行 |
| 7 | `test_agent_loop_user_query_denied` — oneshot 回复 false，验证 ToolCallResult(is_error=true, "User denied") |
| 8 | `test_agent_loop_mpsc_closed` — drop mpsc receiver，验证 agent 循环在下次 send 后退出（不 panic） |
| 9 | `test_agent_loop_history_appended` — 验证 agent 循环结束时 assistant 消息和 tool 结果已追加到 history |

#### 🟢 绿 — 实现

Agent 循环函数：

```
run_agent_loop(
    provider: Arc<dyn LlmProvider>,
    tool_registry: Arc<ToolRegistry>,
    rule_engine: Arc<RuleEngine>,
    session_mgr: Arc<SessionManager>,
    ctx: AgentLoopContext,
    agent_config: &AgentConfig,
    user_message: Message,
    tx: mpsc::Sender<AgentEvent>,
)
```

核心逻辑：
1. 追加 user_message 到 Session.history
2. 构建 Prompt（规则快照 + 历史）
3. 循环（最多 agent_config.max_iterations 次）：
   a. 检查 CancellationToken
   b. 调用 LLM（流式），收集所有 ChatEvent
   c. TextDelta → AgentEvent::TextDelta（检查 tx.send 返回值）
   d. 若无 ToolCall：追加 assistant(text) → Done → break
   e. 若有 ToolCall：追加 assistant(tool_calls)，并行 spawn 所有工具 task
   f. 每个 tool task：检查 CancellationToken → ToolCallRequest → requires_approval 判断 → 执行 → ToolCallResult
   g. join_all，追加各 tool(result) 到 history
4. 调用 session_mgr.finish_loop(sid, status)

错误处理：
- LLM 重试逻辑（RateLimit/Network 指数退避，Auth/Api/Stream 不可重试）
- 工具错误 → ToolCallResult(is_error=true)，返回 LLM
- CancellationToken 检查点：每次 LLM 调用前 + 每个工具执行前
- mpsc send 失败 → 立即 break

#### 🧪 测试 → 🔍 类型检查

```bash
cargo test -p visp-core && cargo clippy -p visp-core -- -D warnings
```

#### 📦 提交

```bash
git add crates/visp-core/src/agent.rs
git commit -m "feat(visp-core): Agent orchestration loop with tool execution and human-in-loop"
```

---

## 步骤 4：visp-core lib.rs 更新

更新 `lib.rs` 模块声明和 re-export：

```rust
pub mod agent;
pub mod prompt;
pub mod rules;
pub mod session;
pub mod tool_registry;

pub use agent::{AgentConfig, AgentEvent};
pub use prompt::PromptBuilder;
pub use rules::RuleEngine;
pub use session::{AgentLoopContext, Session, SessionManager, SessionStore, InMemorySessionStore};
pub use tool_registry::ToolRegistry;
```

#### 📦 提交

```bash
git add crates/visp-core/src/lib.rs
git commit -m "feat(visp-core): update lib.rs module declarations and re-exports"
```

---

## 步骤 5：visp-proto 扩展

### 5a：添加 UserQuery / UserResponse 消息

修改 `crates/visp-proto/proto/visp.proto`：

- `ServerMessage` oneof 新增 `UserQuery user_query = 7`
- 新增消息 `UserQuery { string query_id = 1; string message = 2; string session_id = 3; }`
- `ClientMessage` oneof 新增 `UserResponse user_response = 3`
- 新增消息 `UserResponse { string query_id = 1; bool approved = 2; }`

### 5b：重构 ConfigUpdate

- `ConfigUpdate` 从 `map<string, string>` 改为类型化字段：
  - `session_id: string`
  - `model: optional string`
  - `temperature: optional double`
  - `max_tokens: optional uint32`
  - `extra: map<string, string>`

### 验证

- `cargo build -p visp-proto` 编译通过，生成的 Rust 代码包含新消息
- `cargo clippy -p visp-proto -- -D warnings` 通过

#### 📦 提交

```bash
git add crates/visp-proto/proto/visp.proto crates/visp-proto/src/
git commit -m "feat(visp-proto): add UserQuery/UserResponse for human-in-loop; refactor ConfigUpdate to typed fields"
```

---

## 步骤 6：visp-daemon crate

### 6a：项目骨架

#### 🔴 红 — 验证

`cargo build -p visp-daemon` 失败（crate 尚不存在）。

#### 🟢 绿 — 实现

- 创建 `crates/visp-daemon/Cargo.toml`，依赖：visp-core, visp-proto, visp-llm, visp-tools, tonic, tokio, toml, tracing, tracing-subscriber
- 创建 `crates/visp-daemon/src/main.rs`（最小入口）
- Workspace Cargo.toml 中 `members` 添加 `"crates/visp-daemon"`

#### 🧪 测试 → 🔍 类型检查

```bash
cargo build -p visp-daemon && cargo clippy -p visp-daemon -- -D warnings
```

#### 📦 提交

```bash
git add crates/visp-daemon/ Cargo.toml Cargo.lock
git commit -m "feat(visp-daemon): create crate skeleton"
```

---

### 6b：配置加载（config.rs）

#### 🔴 红 — 测试

| # | 测试用例 |
|---|---|
| 1 | `test_load_config_from_file` — 从临时 TOML 文件加载配置，各字段正确 |
| 2 | `test_load_config_defaults` — 无配置文件时使用内置默认值 |
| 3 | `test_load_config_env_override` — 环境变量 VBW_LLM_MODEL 覆盖配置 |

#### 🟢 绿 — 实现

- `DaemonConfig` 结构体，包含 `[daemon]`, `[llm]`, `[tools]`, `[agent]` sections
- `load_config(path: Option<PathBuf>) -> Result<DaemonConfig>` — 加载 TOML + 环境变量覆盖 + 默认值

#### 📦 提交

```bash
git add crates/visp-daemon/src/config.rs
git commit -m "feat(visp-daemon): TOML config loading with env var override"
```

---

### 6c：gRPC Server（server.rs）

#### 🟢 绿 — 实现

- `start_server(addr, service, shutdown_rx)` — 创建 tonic Server，注册 service，启动监听

无需独立测试（依赖 service 实现，在集成测试中验证）。

#### 📦 提交

```bash
git add crates/visp-daemon/src/server.rs
git commit -m "feat(visp-daemon): gRPC server startup with graceful shutdown"
```

---

### 6d：CoderDaemon Service 实现（service.rs）

#### 🔴 红 — 集成测试

使用 MockProvider 编写集成测试（`tests/` 目录）：

| # | 测试用例 |
|---|---|
| 1 | `test_health_check` — gRPC HealthCheck 返回 alive=true |
| 2 | `test_create_and_list_sessions` — CreateSession → ListSessions 包含新会话 |
| 3 | `test_delete_session` — CreateSession → DeleteSession → ListSessions 不包含 |
| 4 | `test_chat_simple_message` — Chat 流中发送 UserInput，用 MockProvider 返回固定文本，验证流式输出 TextDelta + Done |
| 5 | `test_chat_with_tool` — MockProvider 返回 ToolCall，Agent 执行 ReadFile 工具，验证 ToolCallRequest + ToolCallResult |
| 6 | `test_chat_user_query` — MockProvider 返回 ToolCall（requires_approval=true），验证 UserQuery 发送，回复 UserResponse 后工具执行 |

#### 🟢 绿 — 实现

- `CoderDaemonService` 结构体：持有 `Arc<dyn LlmProvider>`, `Arc<ToolRegistry>`, `Arc<RuleEngine>`, `Arc<SessionManager>`
- 实现 proto `CoderDaemon` trait 的所有 RPC 方法
- CreateSession、ListSessions、DeleteSession：直接委托 SessionManager
- Chat（双向流）：处理 UserInput → Agent 循环，处理 ConfigUpdate → SessionManager.update_config，处理 UserResponse → oneshot 回传
- ReadFile：构造 ToolContext，调用工具
- HealthCheck、Shutdown：简单实现
- SearchSymbols、GetSymbolDetails：返回 `unimplemented`

#### 🧪 测试 → 🔍 类型检查

```bash
cargo test -p visp-daemon && cargo clippy -p visp-daemon -- -D warnings
```

#### 📦 提交

```bash
git add crates/visp-daemon/src/service.rs crates/visp-daemon/tests/
git commit -m "feat(visp-daemon): CoderDaemon service with Chat, Session, and HealthCheck"
```

---

### 6e：main.rs 入口

#### 🟢 绿 — 实现

按启动流程实现：

1. CLI 参数解析（`clap` derive）
2. 加载配置
3. 初始化日志
4. 创建 `AnthropicProvider`（API key 从环境变量）
5. 创建 `ToolRegistry`，注册所有工具（构造时注入配置）
6. 创建 `RuleEngine`
7. 创建 `SessionManager`（`InMemorySessionStore`）
8. 组装 `CoderDaemonService`
9. 启动 gRPC server
10. 监听 shutdown signal

#### 📦 提交

```bash
git add crates/visp-daemon/src/main.rs crates/visp-daemon/Cargo.toml
git commit -m "feat(visp-daemon): main entry with startup orchestration"
```

---

## 步骤 7：全 Workspace 质量门

```bash
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo fmt --check --all
```

全部通过后 Phase 3 完成。

---

## Wave 并行策略

### Wave 1：visp-core 基础类型（1 个 Agent，串行）

```
Agent A: 1a → 1b → 1c → 1d
```

4 个子步骤串行执行。visp-core 的基础类型是后续所有模块的依赖。

### Wave 2：visp-core 核心模块（4 个 Agent，并行）

```
Agent A: 2a (RuleEngine)
Agent B: 2b (SessionManager)
Agent C: 2c (PromptBuilder)
Agent D: 2d (ToolRegistry)
```

4 个模块各自独立 `.rs` 文件，无代码冲突。依赖关系已在步骤 1 中满足。

### Wave 3：Agent 循环 + lib.rs（2 个 Agent，并行）

```
Agent A: 步骤 3 (Agent 循环)
Agent B: 步骤 4 (lib.rs 更新)
```

Agent 循环依赖 Wave 2 全部模块（编译时依赖），lib.rs 更新是独立的文件修改。

### Wave 4：visp-proto + visp-daemon 骨架（2 个 Agent，并行）

```
Agent A: 步骤 5 (visp-proto)
Agent B: 6a (visp-daemon 骨架)
```

### Wave 5：visp-daemon 模块（3 个 Agent，并行）

```
Agent A: 6b (config.rs) + 6c (server.rs)
Agent B: 6d (service.rs)
Agent C: 6e (main.rs)
```

6b+6c 可串行，6d+6e 依赖 6b+6c，但可作为独立文件并行编写。

### Wave 6：质量门

```
cargo test --workspace && cargo clippy --workspace -- -D warnings && cargo fmt --check --all
```

---

## 依赖关系总览

```
Wave 1:  1a → 1b → 1c → 1d                              [1 Agent, 串行]
                │
      ┌─────────┼─────────┬─────────┐
Wave 2:  2a      2b       2c       2d                     [4 Agent, 并行]
      └─────────┼─────────┴─────────┘
                │
Wave 3:  步骤3              步骤4                         [2 Agent, 并行]
                │
      ┌─────────┴─────────┐
Wave 4:  步骤5              6a                            [2 Agent, 并行]
                │
      ┌─────────┼─────────┬─────────┐
Wave 5:  6b+6c   6d       6e                              [3 Agent, 并行]
      └─────────┼─────────┴─────────┘
                │
Wave 6:   质量门                                          [全 workspace]
```

---

## 测试覆盖汇总

| Wave | Agent 数 | Crate | 步骤 | 测试用例 |
|---|---|---|---|---|
| 1 | 1 | visp-core | 1a~1d (4) | 10 |
| 2 | 4 | visp-core | 2a~2d (4) | 23 |
| 3 | 2 | visp-core | 3~4 (2) | 9 |
| 4 | 2 | visp-proto + visp-daemon | 5+6a (2) | 0 |
| 5 | 3 | visp-daemon | 6b~6e (3) | 9 |
| 6 | — | 全 workspace | 质量门 | — |

总计：**13 个子步骤，51 个测试用例，最多 4 Agent 并行**。

## 备注

- Agent 循环测试使用 `MockProvider`（visp-llm 已提供），不依赖真实 API
- visp-core 新增依赖：`tokio`（mpsc + CancellationToken）、`notify`（Rule Engine 热重载）
- visp-daemon 新增依赖：`toml`（配置解析）、`tonic`（gRPC）
- Proto 字段编号注意避让已有编号：ServerMessage 新字段用 7，ClientMessage 新字段用 3
- Agent 循环通过 mpsc channel 产生事件流，不使用直接 Stream 返回
