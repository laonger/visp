# vibewisp Phase 3 阶段设计：Agent 核心 + Daemon

## 1. 阶段目标

实现 Agent 编排循环和 gRPC daemon 服务，让系统端到端可运行：前端通过 gRPC 发送消息 → Agent 调用 LLM 和工具 → 流式返回结果。

**一句话总结**：daemon 启动后，gRPC Chat 能完成一轮 "用户输入 → LLM → 工具 → LLM → 响应" 的完整循环。

## 2. 模块划分

Phase 3 涉及一个已有 crate 的扩展（vbw-core），一个已有 crate 的协议升级（vbw-proto），和一个新 crate：

| Crate | 职责 | 类型 |
|---|---|---|
| **vbw-core** | 新增：Rule Engine、Session Manager、Prompt Builder、Tool Registry、Agent 编排循环 | 扩展 |
| **vbw-proto** | 新增：UserQuery / UserResponse 消息类型，支持 Human-in-Loop | 扩展 |
| **vbw-daemon** | gRPC server、配置加载、CoderDaemon Service 实现 | 新建 |

### 2.1 vbw-core 扩展

vbw-core 从纯类型定义扩展为承载核心业务逻辑。新增 5 个模块。

**新增依赖**：
- `tokio`：Agent 循环的异步执行和 mpsc 事件通道
- `notify`：规则文件变更监听（热重载）

#### 2.1.1 Rule Engine（`rules.rs`）

**职责**：加载、管理、热重载规则文件，将活跃规则拼接为 system prompt 片段。

**规则来源**（优先级从高到低）：
- 全局规则：`~/.config/vibewisp/rules/`
- 项目规则：`.vibewisp/rules/`

**规则文件格式**：Markdown 文件，首行（或前几行内）包含 `alwaysApply: true` 或 `alwaysApply: false` 标记。其余内容为规则正文。

标记识别规则：
- 在文件前 5 行内查找 `alwaysApply:` 行（宽松匹配，允许前后空白）
- 未找到标记的文件默认视为 `alwaysApply: false`（不加载）
- `alwaysApply: true` 的规则始终注入 system prompt
- `alwaysApply: false` 的规则预留，后续按需触发

**关键行为**：
- 启动时扫描规则目录，解析所有 `.md` 文件
- 将 `alwaysApply: true` 的规则按下述顺序拼接成 system prompt 片段：
  - 项目规则在前，全局规则在后
  - 同目录下按文件名字典序
- 使用 `notify` 监听规则目录的文件变更
- 文件增删改时重新扫描并更新内存缓存
- 规则内容变更不影响正在运行的 Agent 循环（变更在下一轮对话生效）
- 对外暴露获取当前活跃规则内容的方法

**设计决策**：
- 规则引擎维护一个 `Arc<RwLock<RuleSet>>`，允许多个 Agent 任务并发读取
- `RuleSet` 包含：当前活跃规则内容（String，按目录优先级 + 文件名排序拼接）、最后加载时间、规则文件列表及各自的内容
- 热重载在独立的 tokio task 中运行（`notify` 的异步事件循环）
- 不解析非 `.md` 文件
- 不做复杂的 YAML/TOML frontmatter 解析——仅正则匹配 `alwaysApply:` 行

#### 2.1.2 Session Manager（`session.rs`）

**职责**：管理多个并发会话的生命周期，包含数据持久化和运行时状态控制。

**分层设计**：

```
SessionManager (struct)          ← 高层 API，daemon service 和 Agent 循环使用
  │
  ├── SessionStore (trait)       ← 纯数据 CRUD 接口
  │     └── InMemorySessionStore  ← MVP 实现（基于 HashMap）
  │
  └── running_tokens               ← 各会话的 CancellationToken 管理
```

**Session 数据结构**：
- 唯一标识（UUID v4）
- 项目路径
- 状态：Idle / Running / Completed / Error
- 创建时间
- 对话历史（`Vec<Message>`）
- LLM 配置（`LlmConfig`，创建时从 daemon 全局配置初始化，后续通过 ConfigUpdate 更新）
- 工作目录（即项目路径，Agent 循环用其构建 ToolContext）
- 系统 prompt 模板（Session 创建时根据项目路径加载，后续不变）

**SessionStore trait**（纯数据持久化接口）：

- `create(session)` — 创建新会话
- `get(session_id)` — 获取会话
- `list()` — 列出所有活跃会话
- `delete(session_id)` — 删除会话
- `update(session)` — 更新会话（状态/历史/配置）

**InMemorySessionStore**：基于 `Arc<RwLock<HashMap>>` 的内存实现，MVP 使用此实现。

**SessionManager**（运行时管理层）：

包装 `SessionStore`，添加状态机逻辑、取消令牌管理、并发控制。持有：
- `store: Arc<dyn SessionStore>` — 持久化存储
- `running_tokens: RwLock<HashMap<String, CancellationToken>>` — 活跃会话的取消令牌

**SessionManager 高层 API**：

| 方法 | 行为 |
|---|---|
| `create(project_path, config)` | 创建会话，写入 SessionStore，状态 Idle，加载系统 prompt 模板 |
| `delete(id)` | 如有运行中的 agent 则先 cancel，再删数据 |
| `start_loop(id)` | 检查状态（必须 Idle）→ 切 Running → 创建 CancellationToken → 返回 `AgentLoopContext` |
| `finish_loop(id, status)` | 切状态（Completed→Idle / Error→Idle），清除 token |
| `append_message(id, msg)` | 追加消息到会话历史 |
| `update_config(id, config)` | 更新会话的 LlmConfig |
| `list()` | 列出所有会话 |

**AgentLoopContext**（Agent 循环启动时从 SessionManager 获取）：

Agent 循环不直接操作 SessionStore，通过此结构获取所需的全部上下文：
- `session_id: String` — 会话标识
- `history: Vec<Message>` — 当前对话历史
- `working_dir: PathBuf` — 项目目录
- `config: LlmConfig` — 当前 LLM 配置
- `cancel_token: CancellationToken` — 取消令牌

**状态机**：`Idle → Running → Completed / Error → Idle`

- `Idle`：会话刚创建或上一轮对话已结束
- `Running`：Agent 正在处理用户消息
- `Completed`：Agent 正常完成一轮对话，过渡到 Idle
- `Error`：Agent 遇到不可恢复错误，过渡到 Idle（用户可继续对话）

**关键行为**：
- `start_loop` 检查状态为 Idle，拒绝 Running 状态（返回 SessionBusy 错误）
- `delete` 时触发 CancellationToken，Agent 循环检测到后退出，再删数据
- 同一会话不能同时运行两个 Agent 循环

**设计决策**：
- 会话历史不设上限（由 LLM context window 自然限制）
- `CancellationToken` 创建于 `start_loop`，销毁于 `finish_loop`，生命周期与一次 Agent 循环绑定
- `SessionStore` trait 的分离为后续 SQLite 持久化预留扩展点
- SessionManager 内部的 `Arc<RwLock<>>` 采用粗粒度锁（整个 session map），MVP 可接受；后续可优化为按 session 粒度锁

**SessionError 扩展**（在现有 `SessionError` 枚举中新增变体）：

为支持 Phase 3 的运行时错误处理，新增以下变体：
- `SessionBusy { session_id: String }` — 会话正在运行，拒绝并发请求
- `ProtocolError { message: String }` — UserResponse 的 query_id 与等待中的 UserQuery 不匹配

#### 2.1.2.1 工具配置传递

工具特定配置（如 bash 超时、文件大小限制）通过**构造时注入**方式传递给工具实例，不改动 `Tool` trait 接口。

- `BashTool::new(timeout_secs)` — 构造时传入超时值
- `ReadFile::new(max_size_bytes)` — 构造时传入大小限制
- 其他工具类似

Daemon 启动时根据配置文件构造工具实例，注册到 `ToolRegistry`。

#### 2.1.3 Prompt Builder（`prompt.rs`）

**类型**：`PromptBuilder` 是 struct，无内部状态。提供 `build` 方法。

**输入**：
- 系统 prompt 模板
- 规则内容（来自 Rule Engine）
- 可用工具列表（来自 Tool Registry，转为 function calling 格式）
- 对话历史（来自 Session）

**输出**：`Vec<Message>`，可直接传给 `LlmProvider::chat_stream`。

**组装规则**：
- System 消息：系统 prompt 模板 + 规则内容拼接为一条 System 消息
- 工具定义：以 `ToolDefinition` 形式通过 provider 接口传入（不由 Prompt Builder 直接拼入消息内容）
- 对话历史：按时间顺序追加 user/assistant/tool 消息

**系统 prompt 模板来源（优先级从高到低）**：

1. 项目级：`.vibewisp/system-prompt.md`
2. 用户全局：`~/.config/vibewisp/system-prompt.md`
3. 内置默认：vbw-core 硬编码的 vibewisp 角色定义

加载时从上往下查找，第一个存在的文件作为模板。全部不存在时使用内置默认。

**拼接结构**：

```
<系统 prompt 模板（角色定义、能力描述、行为准则）>
<规则内容（来自 Rule Engine，项目规则在前，全局规则在后）>
```

规则内容按 Rule Engine 的拼接顺序追加在模板之后，两端有明确分隔。

#### 2.1.4 Tool Registry（`tool_registry.rs`）

**职责**：管理所有可用工具，提供注册、查找、执行、定义导出能力。

**结构**：维护一个 `Vec<Box<dyn Tool>>` 集合。

**关键方法**：
- `register(tool)` — 注册工具
- `get(name)` — 按名称查找工具
- `definitions()` — 列出所有工具的定义（`Vec<ToolDefinition>`），用于传给 LLM
- `execute(name, arguments, context)` — 执行指定工具
- `names()` — 列出所有工具名称

**设计决策**：
- Tool Registry 本身不实现 `Send + Sync` 的自动分发——由外层统一在 Agent 循环中处理并发
- 注册表中工具名称必须唯一，重复注册视为错误
- 所有工具对所有会话可用（MVP 不做权限白名单）

**Tool trait 扩展**：

在现有 `Tool` trait 中新增方法，支持 Human-in-Loop 确认：

- `fn requires_approval(&self) -> bool` — 返回 `true` 表示执行前需要用户确认。默认实现返回 `false`。

Bash 工具重写此方法返回 `true`（受 `AgentConfig.bash_confirm_mode` 控制时可进一步判断），其他工具（ReadFile、WriteFile 等）使用默认 `false`。Agent 循环在执行工具前调用此方法，返回 `true` 则先发 `UserQuery` 等待确认。

#### 2.1.5 Agent 编排循环（`agent.rs`）

**职责**：实现 "用户输入 → LLM → 工具 → LLM → ..." 的核心循环。

**AgentEvent 类型**：

Agent 循环产生的事件流类型，映射到 gRPC ServerMessage 的各变体：

| 事件变体 | 触发时机 | 携带数据 |
|---|---|---|
| `TextDelta` | LLM 流式返回文本增量 | 文本片段 |
| `UserQuery` | Agent 需要用户确认（bash 确认模式等） | `query_id: String`（UUID v4）、`message: String`、`oneshot::Sender<bool>` 用于回传用户决定 |
| `ToolCallRequest` | LLM 请求执行工具 | 调用 ID、工具名、参数 JSON |
| `ToolCallResult` | 工具执行完成 | 调用 ID、结果内容、是否出错 |
| `StatusUpdate` | Agent 状态变化（如"正在读取文件..."） | 状态描述文本 |
| `Error` | 不可恢复错误 | `AgentErrorCode` 枚举 + 错误消息 |
| `Done` | 本轮对话完成 | 无 |

**AgentErrorCode 枚举**（定义于 vbw-core，`AgentEvent::Error` 携带）：

| 变体 | 触发条件 |
|---|---|
| `LlmAuth` | LLM API key 无效（不可重试） |
| `LlmRateLimit` | 速率限制，重试耗尽 |
| `LlmApi` | LLM 服务端返回错误（4xx/5xx） |
| `LlmNetwork` | 网络错误，重试耗尽 |
| `LlmStream` | LLM 流解析失败 |
| `MaxIterations` | 达到最大工具调用轮次 |
| `Cancelled` | CancellationToken 被触发 |
| `Internal` | 其他未分类内部错误 |

Proto `Error` 消息的 `code` 字段使用枚举变体名的字符串形式（如 `"LlmAuth"`）。

注意：`AgentEvent` 与 `ChatEvent` 是不同层级的类型。`ChatEvent` 是 LlmProvider 返回的底层事件，`AgentEvent` 是 Agent 循环产生的上层事件。

**Agent 循环流程**：

```
接收用户消息 → 追加到对话历史
  ↓
循环（最多 max_iterations 次）：
  ↓
1. 发送 StatusUpdate("Thinking...")
2. 构建 Prompt（规则 + 工具定义 + 对话历史）
3. 调用 LLM（流式），收集本轮所有 ChatEvent：
   ├─ TextDelta → 转为 AgentEvent::TextDelta，发送到事件通道
   │              → 追加到本轮 LLM 响应文本缓冲区
   │
   ├─ ToolCall(s) → 收集到本轮工具队列
   │
   └─ Done → 进入步骤 4

4. 判断本轮是否包含工具调用：
   ├─ 无工具调用：
   │   → 将 assistant(文本) 追加到 history
   │   → 发送 AgentEvent::Done，退出循环
   │
   └─ 有工具调用（并行执行）：
       → 将 assistant(tool_calls) 追加到 history（包含本轮所有工具调用）
       → 对本轮所有 ToolCall 并行 spawn tokio task：
          每个 task 执行：
          ├─ 发送 AgentEvent::ToolCallRequest（通知前端）
          ├─ 检查 `tool.requires_approval()`：
          │   ├─ true → 发送 AgentEvent::UserQuery → await oneshot
          │   │          ├─ approved=true → 继续执行
          │   │          └─ approved=false → ToolResult(is_error=true, "User denied")
          │   └─ false → 直接执行
          ├─ 发送 AgentEvent::StatusUpdate("Executing {tool_name}...")
          ├─ 通过 ToolRegistry 执行工具
          └─ 发送 AgentEvent::ToolCallResult
       → 等待所有 task 完成（join_all）
       → 将各 tool(result) 追加到 history
       → 回到循环开始（步骤 2）
```

**工具调用与确认的事件序列**：

LLM 一次请求多个工具时，事件可能交错（并行执行，通过同一 mpsc 通道发送）：

```
ToolCallRequest(call_id=1, name=read_file, args=...)       ← 无需确认，直接执行
ToolCallRequest(call_id=2, name=bash, args="rm ...")       ← 需确认，暂停
StatusUpdate("Executing read_file...")                      ← call_id=1 在执行
ToolCallResult(call_id=1, content="...")                    ← call_id=1 完成
UserQuery(query_id=xxx, message="是否允许执行: rm?")        ← call_id=2 等待确认
... 等待客户端 UserResponse ...
StatusUpdate("Executing bash...")                           ← call_id=2 恢复执行
ToolCallResult(call_id=2, content="...")
```

**终止条件**：
- LLM 返回 Done（无工具调用，正常结束）
- 达到最大迭代次数（默认 50，可配置）
- 收到 CancellationToken 取消信号（检查点：每次 LLM 调用前 + 每个工具执行前）
- 发生不可恢复的 LLM 错误

**错误处理**：
- 工具执行出错（`ToolResult::is_error == true`）：将错误结果返回给 LLM，由 LLM 决定如何响应
- LLM 调用出错：判断错误类型：
  - `RateLimit` / `Network`：自动重试（指数退避，最多 3 次）
  - `Auth` / `Api` / `Stream`：发送 `AgentEvent::Error`，退出循环
- 最大迭代次数耗尽：发送 `AgentEvent::Error("Max iterations reached")`

**Agent 循环签名（逻辑描述）**：

Agent 循环是一个异步任务，接收以下输入：
- LLM Provider（`Arc<dyn LlmProvider>`，共享）
- Tool Registry（`Arc<ToolRegistry>`，共享）
- Rule Engine（`Arc<RuleEngine>`，共享）
- Agent Loop 上下文（`AgentLoopContext`，包含 session_id、history、working_dir、config、cancel_token）
- 用户消息内容（`Message`）
- 事件通道（`mpsc::Sender<AgentEvent>`）

Agent 循环通过 mpsc 通道向外发送 `AgentEvent`，不直接返回 Stream。外层（daemon service）从 mpsc receiver 读取事件并转为 gRPC 消息。

Agent 循环执行期间通过 `SessionManager::append_message` 和 `SessionManager::finish_loop` 与 SessionManager 交互（不直接操作 SessionStore）。

**配置项**（AgentConfig 结构体，定义于 vbw-core）：

| 字段 | 类型 | 默认值 | 说明 |
|---|---|---|---|
| `max_iterations` | u32 | 50 | 最大工具调用轮次 |
| `llm_retry_attempts` | u32 | 3 | LLM 调用失败重试次数 |
| `llm_retry_base_delay_ms` | u64 | 1000 | 重试指数退避基础延迟 |
| `bash_confirm_mode` | bool | true | bash 工具是否需要用户确认 |
| `file_max_size_bytes` | u64 | 1048576 | 文件读取最大字节数 |

AgentConfig 由 daemon 配置文件生成，存入 Session 中（通过 `SessionManager::create` 的 config 参数传入）。

**Human-in-Loop 取消路径**：

当 Agent 循环发送 `UserQuery` 并 await oneshot 时，若客户端断开连接（Daemon 丢弃 `Sender`），oneshot Receiver 返回 `Err(RecvError::Closed)`。此时 Agent 循环视为用户拒绝（`approved = false`），工具不执行，将"User disconnected, skipping tool"作为 `ToolCallResult(is_error=true)` 继续。不中断循环。若所有工具都被拒绝，LLM 收到的工具结果全部为 error——由 LLM 决定如何响应。

**并行工具 + UserQuery 串行化**：

当一轮 LLM 请求多个工具并行执行时，多个需要确认的工具（如两个 bash）可能同时发送 `UserQuery` 到 mpsc 通道。daemon service 同一时刻只处理一个 `UserQuery`：收到第一个后阻塞等待客户端 `UserResponse`，后续的 `UserQuery` 在 mpsc 缓冲区排队，按发送顺序逐个处理。

与此同时，不需要确认的工具（如 read_file、grep）不受影响，继续并行执行。它们的 `ToolCallResult` 事件也在 mpsc 缓冲区中排队，在 daemon service 处理完当前 `UserQuery` 后依次发送。事件流的整体顺序为：

```
ToolCallRequest(bash_1) → UserQuery(bash_1) → [daemon 阻塞，排队中:
  ToolCallRequest(read_file) → ToolCallResult(read_file)
  ToolCallRequest(bash_2) → UserQuery(bash_2)
] → UserResponse(bash_1) → [daemon 继续消费:]
  ToolCallResult(read_file) → ToolCallRequest(bash_2) → UserQuery(bash_2) → ...

#### 2.1.6 lib.rs 模块重组

新增模块声明和 re-export：

```
pub mod agent;
pub mod error;
pub mod message;
pub mod prompt;
pub mod provider;
pub mod rules;
pub mod session;
pub mod tool;
pub mod tool_registry;

// 新增 re-export
pub use agent::{AgentConfig, AgentEvent};
pub use prompt::PromptBuilder;
pub use rules::RuleEngine;
pub use session::{AgentLoopContext, InMemorySessionStore, Session, SessionManager, SessionStore};
pub use tool_registry::ToolRegistry;
```

### 2.2 vbw-proto 扩展

Phase 3 需要在 gRPC 协议中增加 Human-in-Loop 支持，使 Agent 能够向用户提问并等待回答。

**新增消息类型**：

- **`ServerMessage` 新增变体**：`UserQuery` — Agent 向用户提问。携带 `query_id`（唯一标识，UUID v4）、`message`（提示文本）、`session_id`
- **`ClientMessage` 新增变体**：`UserResponse` — 用户对提问的回答。携带 `query_id`（与 UserQuery 对应）、`approved`（是否同意）
- **`ConfigUpdate` 重构**：将现有 `map<string, string>` 改为类型化字段，字段列表：`session_id`、`model`（optional string）、`temperature`（optional double）、`max_tokens`（optional uint32）、`extra`（map<string, string>，未知键值对）。字段使用 proto3 `optional` 语义——只更新显式设置的字段，未设置的保持原值。

**使用场景**：

| 场景 | Agent 发送 UserQuery | 用户回答 approved=true | 用户回答 approved=false |
|---|---|---|---|
| bash 确认模式 | "是否允许执行: rm -rf node_modules?" | 执行命令 | 跳过，返回 "User denied" |

**Chat 流的双向交互协议**：

```
客户端                            Daemon
  │                                │
  │── UserInput("删除 node_modules")──▶│ 启动 Agent 循环
  │                                │── LLM 返回 ToolCall(bash)
  │◀── ToolCall(bash, "rm -rf...")──│
  │◀── UserQuery(query_id=1,    │  Agent 暂停，等待确认
  │       "是否允许执行?")          │
  │                                │
  │── UserResponse(query_id=1,    │
  │       approved=true) ──────────▶│ 继续执行
  │◀── StatusUpdate("Executing...")──│
  │◀── ToolResult(...) ──────────────│
  │◀── Done ────────────────────────│
```

**设计决策**：
- `UserQuery` 是 `ServerMessage` 的一部分，与其他事件（TextDelta、ToolCallResult 等）在同一流中顺序发送
- Agent 循环发送 `UserQuery` 后必须暂停，直到收到匹配的 `UserResponse`——通过 `oneshot::Sender<bool>` 实现
- Service 层处理 `UserQuery` 时，阻塞 mpsc 读取循环，转而等待客户端下一条 `ClientMessage`
- `UserResponse` 的 `query_id` 必须与 `UserQuery` 的 `query_id` 匹配，否则视为协议错误

### 2.3 vbw-daemon crate

#### 2.3.1 模块结构

```
vbw-daemon/
├── Cargo.toml
└── src/
    ├── main.rs          # 入口：解析 CLI 参数、加载配置、启动 server
    ├── config.rs        # 配置结构体定义和 TOML 加载逻辑
    ├── server.rs        # gRPC server 启动（tonic）
    └── service.rs       # CoderDaemon trait 实现
```

**依赖**：vbw-core, vbw-proto, vbw-llm, vbw-tools, tonic, tokio, toml, tracing, tracing-subscriber

#### 2.3.2 配置系统（`config.rs`）

**DaemonConfig 结构**包含所有可配置项：

- **daemon 配置**：监听地址（默认 `[::1]:50051`）、日志级别
- **LLM 配置**：provider 类型、API key（可选，支持环境变量 `ANTHROPIC_API_KEY`）、base_url（可选，支持自定义 API 代理）、模型名称、温度、最大 token
- **工具配置**：bash 超时、文件大小限制
- **Agent 配置**：最大迭代次数、重试次数
- **Rule 配置**：额外规则目录列表

**配置层级**（优先级从高到低）：
1. 命令行参数（`--config` 指定配置文件路径）
2. 环境变量（`VBW_` 前缀）
3. 用户配置（`~/.config/vibewisp/daemon.toml`）
4. 内置默认值

**TOML 配置示例**（说明性）：

```toml
[daemon]
listen_addr = "[::1]:50051"
log_level = "info"

[llm]
provider = "anthropic"
model = "claude-sonnet-4-20250514"
temperature = 0.7
max_tokens = 4096
# api_key 可选，未配置则从 ANTHROPIC_API_KEY 环境变量读取
# api_key = "sk-ant-xxx"
# base_url 可选，未配置则使用默认值 https://api.anthropic.com
# base_url = "https://api.anthropic.com"

[tools]
bash_timeout_secs = 120
file_max_size_bytes = 1048576

[agent]
max_iterations = 50
llm_retry_attempts = 3
llm_retry_base_delay_ms = 1000
```

**设计决策**：
- API key 优先从配置文件读取，未配置则 fallback 到环境变量（如 `ANTHROPIC_API_KEY`）
- base_url 可选，未配置则使用 provider 默认值（`https://api.anthropic.com`）
- 配置加载失败（如找不到配置文件）使用内置默认值继续运行
- 配置热重载不做（需重启 daemon 生效）

#### 2.3.3 gRPC Server（`server.rs`）

**职责**：启动 tonic gRPC 服务器，注册 CoderDaemon service。

**启动流程**：
1. 解析 CLI 参数（`--config` 指定配置文件路径、`--listen-addr` 覆盖监听地址）
2. 加载配置文件（TOML → DaemonConfig）并合并命令行参数
3. 初始化日志（`tracing-subscriber`，按 `log_level` 配置）
4. 创建 `LlmProvider`（根据 `provider` 配置选择实现，API key 优先从配置文件读取，fallback 环境变量；base_url 可选配置）
5. 创建 `ToolRegistry`，注册所有工具实例（构造时注入超时、大小限制等配置参数）
6. 创建 `RuleEngine`（启动规则目录的 notify 监听）
7. 创建 `SessionManager`（初始化 `InMemorySessionStore`）
8. 组装 `CoderDaemonService`（注入 LlmProvider、ToolRegistry、RuleEngine、SessionManager）
9. 启动 gRPC server（见 2.3.3）
10. 监听 shutdown signal（SIGINT / SIGTERM），优雅关闭

**设计决策**：
- MVP 仅支持本地通信（`localhost` / `[::1]`），不暴露到公网
- 不做 TLS，依赖本地 loopback 的安全隔离
- Shutdown 信号处理使用 `tokio::signal`

#### 2.3.4 CoderDaemon Service 实现（`service.rs`）

**职责**：实现 proto 定义的 `CoderDaemon` trait，将 gRPC 请求翻译为 vbw-core 模块调用。

**CoderDaemonService 结构**持有的依赖：
- 共享的 `LlmProvider`（`Arc<dyn LlmProvider>`，如 `AnthropicProvider`）
- 共享的 `ToolRegistry`（`Arc<ToolRegistry>`）
- 共享的 `RuleEngine`（`Arc<RuleEngine>`）
- 共享的 `SessionManager`（`Arc<SessionManager>`）

**各 RPC 方法实现逻辑**：

---

**CreateSession**：

1. 解析请求中的 `project_path` 和初始 `config`
2. 调用 `SessionManager::create(project_path, config)` 创建新会话（自动加载系统 prompt 模板）
3. 返回 `Session` proto 消息（id, status=IDLE, project_path, created_at）

---

**ListSessions**：

1. 调用 `SessionManager::list()`
2. 将会话列表转为 proto `Session` 消息返回

---

**DeleteSession**：

1. 调用 `SessionManager::delete(session_id)`（自动处理 cancel + 清理）
2. 返回空响应

---

**Chat**（双向流，最复杂的 RPC）：

这是核心方法，实现长连接的双向流处理。

**整体流程**：

```
连接建立 → 循环接收 ClientMessage：
  ├─ UserInput(text):
  │   1. 调用 SessionManager.start_loop(sid) → 获取 AgentLoopContext
  │      → 若返回 SessionBusy，发送 Error(SESSION_BUSY) 并继续等待
  │   2. 构建用户 Message，调用 SessionManager.append_message
  │   3. 创建 mpsc channel
  │   4. 发送 StatusUpdate("Starting agent loop...")
  │   5. spawn agent 循环任务（传入 AgentLoopContext）
  │   6. 读取 mpsc receiver 的 AgentEvent：
  │      ├─ TextDelta → 转为 proto TextDelta，发送
  │      ├─ ToolCallRequest → 转为 proto ToolCall，发送
  │      ├─ ToolCallResult → 转为 proto ToolResult，发送
  │      ├─ StatusUpdate → 转为 proto StatusUpdate，发送
  │      ├─ UserQuery → 转为 proto UserQuery，发送
  │      │               → 阻塞等待客户端下一条 ClientMessage
  │      │               → 必须为 UserResponse(query_id 匹配)
  │      │               → 将 approved 通过 oneshot 回传 Agent 循环
  │      │               → 继续读取 mpsc receiver
  │      ├─ Error → 转为 proto Error，发送
  │      └─ Done → 转为 proto Done，发送，结束本轮
  │   7. 调用 SessionManager.finish_loop(sid, Complete)
  │   8. 回到步骤 1，等待下一条 ClientMessage
  │
  ├─ ConfigUpdate(config):
  │   校验会话存在（SessionManager.get），不存在则返回 Error(SESSION_NOT_FOUND)
  │   若当前会话状态为 Running → 返回 Error(SESSION_BUSY, "配置只能在会话空闲时修改")
  │   若当前会话状态为 Idle → 直接调用 SessionManager.update_config(sid, config)，立即生效
  │
  ├─ UserResponse（仅当有等待中的 UserQuery 时有效）：
  │   将 approved 通过对应 oneshot 回传 Agent 循环
  │
  └─ 流关闭：清理资源，不删除会话
```

**并发隔离**：
- 每个 Chat 流内部串行处理 UserInput（一次只处理一条消息）
- 不同会话的 Chat 流完全独立、并行运行
- 同一会话不能同时有两个 Chat 流（SessionManager 拒绝 Running 状态下的新 Chat 请求）

**Error 映射规则**：

Chat 流（双向流）中的内部错误通过 `ServerMessage::Error` 发回，不中断 gRPC 流。映射规则：

| 内部错误 | Error code | 说明 |
|---|---|---|
| 会话不存在 | `SESSION_NOT_FOUND` | UserInput 或 ConfigUpdate 的 session_id 无效 |
| 会话状态为 Running | `SESSION_BUSY` | 上一轮 Agent 循环未结束，拒绝新消息 |
| UserResponse 的 query_id 不匹配 | `PROTOCOL_ERROR` | UserResponse 与等待中的 UserQuery 不一致 |
| 配置解析/赋值失败 | `CONFIG_ERROR` | ConfigUpdate 的字段值无效 |
| 其他/内部错误 | `INTERNAL` | 未分类错误 |

非 Chat 流的 RPC（CreateSession、DeleteSession、HealthCheck 等）使用 `tonic::Status` 标准 gRPC 错误。

**mpsc send 失败处理**：

Agent 循环每处 `tx.send()` 调用后检查返回值。若返回 `Err(SendError)`（表示 daemon service 已 drop receiver，通常因客户端断开 gRPC 流），Agent 循环立即 `break` 退出。不发进一步的 AgentEvent，因为客户端已不在了。不需要依赖 CancellationToken。

---

**ReadFile**（快速通道，跳过 LLM）：

1. 创建 `ToolContext`（从会话获取 working_dir）
2. 调用 `ReadFile` 工具的 `execute` 方法
3. 返回文件内容

---

**HealthCheck**：

1. 返回 daemon 存活状态、版本号、运行时长

---

**Shutdown**：

1. 若 `force == true`：立即关闭（不等待现有请求）
2. 若 `force == false`：发送 shutdown signal，等待现有请求完成
3. 返回空响应后，`main.rs` 执行进程退出

---

**SearchSymbols / GetSymbolDetails**：

Phase 3 不实现（Phase 5 CodeGraph 功能）。返回 `unimplemented` 错误。

## 3. 依赖关系

```
vbw-daemon (二进制)
    │
    ├──→ vbw-proto   (gRPC 类型 + service trait)
    ├──→ vbw-core    (Agent, Session, Rules, Prompt, ToolRegistry)
    ├──→ vbw-llm     (LlmProvider 实现)
    └──→ vbw-tools   (Tool 实现)
    
vbw-core (扩展)
    │ 新增依赖: tokio, notify
    │
    ├──→ vbw-llm     (通过 LlmProvider trait，编译时依赖)
    └──→ vbw-tools   (通过 Tool trait，编译时依赖)

注意：vbw-core 不直接依赖 vbw-llm 或 vbw-tools crate，
      而是通过 trait 接口（LlmProvider, Tool）解耦。
```

## 4. 核心数据流

### 4.1 Chat RPC 完整数据流

```
┌─────────────┐                          ┌───────────────────────────────┐
│  gRPC 客户端 │                          │        vbw-daemon              │
└──────┬──────┘                          └───────────────┬───────────────┘
       │  Chat stream 建立                                 │
       │──────────────────────────────────────────────────→│
       │                                                   │
       │  UserInput("重构 src/lib.rs")                     │
       │──────────────────────────────────────────────────→│
       │                                                   │──→ SessionManager.get(sid)
       │                                                   │──→ SessionManager.set_status(Running)
       │                                                   │──→ 创建 mpsc channel
       │                                                   │──→ spawn agent loop
       │                                                   │
       │  StatusUpdate("Starting...")                       │
       │←──────────────────────────────────────────────────│
       │                                                   │    Agent Loop:
       │  TextDelta("好的，让我")                           │    ├─ Build Prompt
       │←──────────────────────────────────────────────────│    ├─ Call LLM (stream)
       │  TextDelta("先看看")                               │    │
       │←──────────────────────────────────────────────────│    │
       │  TextDelta("这个文件")                             │    │
       │←──────────────────────────────────────────────────│    │
       │                                                   │    ├─ LLM → ToolCalls
       │  ToolCall(call_id=1, read_file, ...)             │    │   收集两个工具
       │←──────────────────────────────────────────────────│    │
       │  ToolCall(call_id=2, bash, "rm...")              │    │
       │←──────────────────────────────────────────────────│    │
       │                                                   │    ├─ 执行 call_id=1
       │  StatusUpdate("Executing read_file...")            │    │   (无需确认)
       │←──────────────────────────────────────────────────│    │
       │  ToolResult(call_id=1, content="...")             │    │
       │←──────────────────────────────────────────────────│    │
       │                                                   │    ├─ call_id=2 需确认
       │  UserQuery(query_id=x,                          │    │   Agent 暂停
       │    "是否允许执行: rm node_modules?")              │    │   await oneshot
       │←──────────────────────────────────────────────────│    │
       │                                                   │    │
       │  UserResponse(query_id=x, approved=true)         │    │
       │──────────────────────────────────────────────────→│    │
       │                                                   │    ├─ oneshot 恢复
       │  StatusUpdate("Executing bash...")                 │    │   继续执行
       │←──────────────────────────────────────────────────│    │
       │  ToolResult(call_id=2, content="...")             │    │
       │←──────────────────────────────────────────────────│    │
       │                                                   │    ├─ 结果返回 LLM
       │  TextDelta("重构完成，主要改动：")                  │    ├─ LLM → TextDelta
       │←──────────────────────────────────────────────────│    ├─ ...
       │  TextDelta("1. 提取公共逻辑")                      │    └─ Done
       │←──────────────────────────────────────────────────│
       │  Done                                             │
       │←──────────────────────────────────────────────────│
       │                                                   │
       │  UserInput("还有一处需要改")                       │  (新一轮)
       │──────────────────────────────────────────────────→│
       │  ...                                              │
```

### 4.2 Agent 循环内部数据流

```
用户 Message
    │
    ▼
追加到 Session.history
    │
    ▼
┌─ 循环开始 ──────────────────────────────────────────┐
│                                                      │
│  PromptBuilder.build(                               │
│    system_template,  ← 内置角色定义                  │
│    rules,            ← RuleEngine 返回               │
│    tools,            ← ToolRegistry.definitions      │
│    history           ← Session.history               │
│  ) → Vec<Message>                                   │
│    │                                                 │
│    ▼                                                 │
│  LlmProvider::chat_stream(messages, tools, config)   │
│    │                                                 │
│    ├─ 流式收集本轮所有 ChatEvent：                     │
│    │   ├─ TextDelta → AgentEvent::TextDelta          │
│    │   │              → 追加到本轮文本缓冲区           │
│    │   ├─ ToolCall → 追加到本轮工具队列               │
│    │   └─ Done → 停止收集                             │
│    │                                                 │
│    ▼                                                 │
│  ┌─ 本轮无 ToolCall？                                │
│  │  YES → assistant(text) 追加到 history             │
│  │       → AgentEvent::Done → break                  │
│  │                                                   │
│  │  NO → assistant(tool_calls) 追加到 history        │
│  │       → 并行 spawn 所有 ToolCall 的 task：           │
│  │          每个 task 中：                               │
│  │           ├─ AgentEvent::ToolCallRequest              │
│  │           ├─ tool.requires_approval()?                │
│  │           │   YES → AgentEvent::UserQuery             │
│  │           │        → await oneshot(bool)              │
│  │           │        → false → 跳过, 错误结果           │
│  │           ├─ AgentEvent::StatusUpdate                 │
│  │           ├─ ToolRegistry.execute(name, args)        │
│  │           ├─ AgentEvent::ToolCallResult               │
│  │       → 等待所有 task 完成（join_all）                │
│  │       → 将各 tool(result) 追加到 history              │
│  │       → continue 循环（回到 Build Prompt）            │
│                                                      │
│  错误处理:                                            │
│    LlmError::RateLimit/Network → 重试(指数退避)      │
│    LlmError::Auth/Api/Stream → Error → break         │
│    Tool 执行失败 → ToolCallResult(is_error=true)     │
│                   → 结果返回 LLM 继续迭代            │
│    max_iterations 耗尽 → Error → break               │
│    CancellationToken 触发 → break                    │
└──────────────────────────────────────────────────────┘
```

## 5. 并发模型

```
┌─────────────────────────────────────────────────────────┐
│                    vbw-daemon 进程                       │
│                                                         │
│  ┌──────────┐  ┌──────────┐  ┌──────────┐              │
│  │ Session 1│  │ Session 2│  │ Session 3│  ...          │
│  │ Chat RPC │  │ Chat RPC │  │ Chat RPC │              │
│  │  stream  │  │  stream  │  │  stream  │              │
│  │    │     │  │    │     │  │    │     │              │
│  │    ▼     │  │    ▼     │  │    ▼     │              │
│  │ Agent    │  │ Agent    │  │ Agent    │              │
│  │ Loop     │  │ Loop     │  │ Loop     │              │
│  │ Task     │  │ Task     │  │ Task     │              │
│  └──────────┘  └──────────┘  └──────────┘              │
│                                                         │
│  共享组件（Arc 引用，并发读）:                             │
│  ┌──────────┐  ┌──────────┐  ┌──────────┐              │
│  │LlmProvider│  │ToolRegistry│ │RuleEngine│              │
│  │ (Anthropic)│  │  (所有工具)│ │(规则缓存)│              │
│  └──────────┘  └──────────┘  └──────────┘              │
│                                                         │
│  SessionManager (Arc<RwLock<HashMap>>)                   │
│  ┌──────────────────────────────────────┐               │
│  │  session_id → SessionInfo             │               │
│  │    ├─ Session 元数据                  │               │
│  │    ├─ 对话历史                        │               │
│  │    └─ CancellationToken               │               │
│  └──────────────────────────────────────┘               │
└─────────────────────────────────────────────────────────┘
```

## 6. Phase 3 不做什么

明确边界：

- ❌ 不实现 MCP 客户端或 MCP 工具
- ❌ 不实现会话持久化（daemon 重启后会话丢失）
- ❌ 不实现 Agent 委派（多 specialist 调度）
- ❌ 不实现工具权限白名单（所有工具对所有会话可用）
- ❌ 不实现 SearchSymbols / GetSymbolDetails（Phase 5）
- ❌ 不实现配置热重载（需重启 daemon）
- ❌ 不实现 gRPC TLS
- ❌ 不实现 `alwaysApply: false` 规则的按需触发
- ❌ 不实现 token 计数或 context window 管理
- ❌ 不做非 `.md` 文件的规则加载

## 7. 验收标准

- `cargo build --workspace` 编译通过（包含新 crate vbw-daemon）
- `cargo test --workspace` 所有测试通过
- `cargo clippy --workspace -- -D warnings` 通过
- `cargo fmt --check --all` 通过
- vbw-core 新增模块的单元测试覆盖（每个模块至少 3 个测试用例）
- **集成测试 1**：daemon 启动后，HealthCheck 返回正常
- **集成测试 2**：通过 gRPC 客户端创建会话、发送消息，完成一轮无工具调用的对话
- **集成测试 3**：通过 gRPC 客户端发送消息，触发工具调用（如 ReadFile），Agent 正确执行工具并返回最终响应
- MockProvider 可用于完整的 Agent 循环集成测试
