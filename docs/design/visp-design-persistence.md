# visp 数据持久化设计

## 1. 背景与动机

### 1.1 现状

visp 当前所有数据（会话、消息、工具调用记录）存储在内存中，通过 `InMemorySessionStore` 实现 `SessionStore` trait。进程退出后所有数据丢失。

### 1.2 需求

- 会话和聊天记录持久化存储，进程重启后可恢复
- 工具调用记录（含参数、结果、耗时、是否出错）可回溯
- token 用量和费用统计持久化
- 支持按项目路径、时间范围等条件查询历史会话
- 不增加 visp-core 的 IO 依赖

### 1.3 非目标

- 不实现 Event Sourcing（可后续演进）
- 不支持会话分叉（当前无此需求）
- 不支持分布式部署（单机本地存储）
- 不实现后台历史压缩（留待后续）

## 2. 参考分析：opencode 数据架构

### 2.1 opencode 整体架构

opencode 采用 **Event Sourcing + CQRS + SQLite** 架构，数据库文件在 `~/.local/share/opencode/opencode.db`。

核心表：

| 表 | 用途 |
|---|---|
| `session` | 会话元信息，含 tokens/cost 统计字段 |
| `session_message` | CQRS 读模型，从事件投影而来 |
| `session_input` | 用户输入收件箱（Inbox），admitted→promoted 两阶段 |
| `event` | 事件溯源写模型，25+ 种事件类型 |
| `event_sequence` | 事件序列号追踪 |

### 2.2 opencode 消息分类

opencode 定义 11 种结构化消息类型，每种带独立 Schema：

```
user          → { text, files[], agents[], references[] }
assistant     → { text, tool_calls[], provider_metadata }
system        → { text }
shell         → { call_id, command, exit_code, stdout, stderr }
tool          → { tool, input, output, is_error, duration_ms }
agent-switched → { agent }
model-switched → { model }
synthetic     → { session_id, text }
compaction    → { summary }
error         → { text, error }
text          → { text, provider_metadata }
```

### 2.3 opencode 事件类型

工具调用相关的完整事件链：

```
Tool.Input.Started → Tool.Input.Delta* → Tool.Input.Ended
         ↓
Tool.Called  ← 最终完整的 input
         ↓
Tool.Progress*  ← 运行时状态 checkpoint
         ↓
Tool.Success | Tool.Failed
```

### 2.4 对 visp 的启发

1. **JSON 存储结构化数据** — 避免频繁改表，`data` 字段支持灵活扩展
2. **角色 + 类型分层** — `role` 决定谁说的，`type` 决定内容结构
3. **消息有序性** — 自增 id 或 seq 天然保证消息顺序
4. **工具调用独立追踪** — 从 LLM 发起到结果返回，完整的生命周期
5. **用量统计到会话级别** — opencode 在 session 表直接聚合统计（visp 相反，采用从 message 表延迟聚合的方案，见 7.4）

## 3. 数据模型设计

### 3.1 数据实体关系

```
Session (1) ──── (N) Message
```

用量统计由 message 表实时聚合，session 表不再预存派生数据。

### 3.2 实体定义

#### Session（会话）

| 字段 | 类型 | 说明 |
|---|---|---|
| id | TEXT PK | UUID v4，格式 `ses_xxxx` |
| project_path | TEXT | 项目路径，NOT NULL |
| title | TEXT | 会话标题，默认空字符串 |
| status | TEXT | idle / running / completed / error |
| model | TEXT | 使用的模型名称 |
| system_prompt_template | TEXT | 系统提示词模板 |
| config_json | TEXT | LlmConfig 的 JSON 序列化 |
| approved_tools | TEXT | 已审批工具的 JSON 数组 |
| created_at | INTEGER | 创建时间（Unix 毫秒） |
| updated_at | INTEGER | 最后更新时间（Unix 毫秒） |

> ⚠️ **Rust 结构体兼容**：现有 `Session.created_at: Instant` 无法从 DB 时间戳还原。Rust `Session` 需新增 `created_at_unix: Option<i64>` 字段用于 DB 存取，`created_at: Instant` 保留用于运行时。`SqliteSessionStore::get()` 加载时 `created_at` 以 `Instant::now()` fallback，`created_at_unix` 存真实时间戳。

#### Message（消息）

| 字段 | 类型 | 说明 |
|---|---|---|
| id | INTEGER PK | 自增主键，天然有序 |
| session_id | TEXT FK | 所属会话，CASCADE 删除 |
| role | TEXT | system / user / assistant / tool |
| type | TEXT | 消息子类型（见 3.3） |
| content | TEXT | 文本内容 |
| tool_call_id | TEXT | 工具调用 ID（tool role 使用） |
| tool_name | TEXT | 工具名称 |
| tool_arguments | TEXT | 工具参数 JSON |
| tool_result_is_error | INTEGER | 0/1，工具执行是否出错 |
| tool_result_duration_ms | INTEGER | 工具执行耗时（毫秒） |
| estimated_tokens | INTEGER | 本地估算 token 数 |
| extra_blocks | TEXT | 额外内容块 JSON 数组（thinking 等） |
| provider_metadata | TEXT | LLM 提供商元数据 JSON |
| actual_tokens_input | INTEGER | 实际输入 token（来自 provider，仅 assistant 消息非空） |
| actual_tokens_output | INTEGER | 实际输出 token（来自 provider，仅 assistant 消息非空） |
| actual_cache_read | INTEGER | 实际 cache read token |
| actual_cache_write | INTEGER | 实际 cache write token |
| actual_cost | REAL | 实际费用 |
| created_at | INTEGER | 创建时间（Unix 毫秒） |

**索引**：

```sql
CREATE INDEX idx_message_session ON message(session_id, id);
CREATE INDEX idx_message_session_role ON message(session_id, role);
CREATE INDEX idx_message_tool_call ON message(tool_call_id);
CREATE INDEX idx_session_project ON session(project_path, created_at);
CREATE INDEX idx_session_updated ON session(updated_at);
```

### 3.3 消息类型体系

每条消息同时有 `role` 和 `type` 两个维度：

| role | type | 说明 |
|---|---|---|
| system | system | 系统提示词 |
| user | user | 用户输入 |
| assistant | text | LLM 普通文本回复 |
| assistant | thinking | LLM 思考块 |
| assistant | tool_call | LLM 发起的工具调用请求 |
| tool | tool_result | 工具执行结果 |
| assistant | error | 错误消息 |
| assistant | status | 状态更新 |

role 决定谁说的，type 决定如何解析 content 和附加字段。

> **字段映射**：DB 表中的 `type` 列对应 Rust `Message` 结构体的 `kind: MessageType` 字段，两者值枚举一致（system/user/text/thinking/tool_call/tool_result/error/status）。

### 3.4 工具调用生命周期

一条工具调用在 DB 中由两条消息共同记录：

```
Message A: role=assistant, type=tool_call
    content="" (空，工具名称不在此存储)
    tool_call_id="call_xxx"
    tool_name="bash"
    tool_arguments='{"command":"ls"}'

Message B: role=tool, type=tool_result
    content="src/\nCargo.toml\n..."
    tool_call_id="call_xxx"  (关联到 Message A)
    tool_name="bash"
    tool_result_is_error=0
    tool_result_duration_ms=1234
```

关联查询：`WHERE tool_call_id = ?` 可找到完整的调用-结果对。

## 4. 存储架构

### 4.1 数据库选型

**SQLite**（通过 `rusqlite` crate），原因：

- 无需独立数据库进程，嵌入式，零运维
- 单文件，备份/迁移简单
- Rust 生态成熟（`rusqlite`、`diesel`、`sqlx`）
- 性能足够（单机本地，单进程访问）
- WAL 模式提供并发读

### 4.2 文件位置

```
~/.visp/
├── data/
│   └── visp.db              ← SQLite 数据库文件
├── logs/
│   └── daemon-<timestamp>.log
└── rules/                    ← 全局规则（已有）
```

数据库路径可通过配置 `daemon.toml` 覆盖：

```toml
[storage]
# 默认：~/.visp/data/visp.db
path = "/custom/path/visp.db"
```

> **路径 `~` 展开**：配置路径若以 `~/` 开头（如 `~/.visp/data/visp.db`），在配置加载时自动替换为 `$HOME`。纯 `~` 或非 `~/` 开头的路径不展开。

```rust
// visp-core 中将 home_dir 提升为公共函数（当前 session.rs 和 rules.rs 各有私有实现）
pub fn home_dir() -> Option<PathBuf> {
    std::env::var("HOME").ok().map(PathBuf::from)
}

// 使用方（如 visp-db 或 visp-daemon 的配置加载）调用 home_dir 实现路径展开
fn expand_tilde(path: &str) -> PathBuf {
    if let Some(rest) = path.strip_prefix("~/") {
        PathBuf::from(home_dir().unwrap_or_default()).join(rest)
    } else {
        PathBuf::from(path)
    }
}
```

### 4.3 数据库初始化

首次启动时自动执行 schema 迁移（建表 + 索引），迁移脚本内置在二进制中，不依赖外部 SQL 文件。使用简单的版本号检查机制（`PRAGMA user_version`）。

```
┌──────────────┐
│ daemon 启动  │
└──────┬───────┘
       ↓
┌──────────────┐     ┌──────────────────┐
│ 打开/创建 DB  │────→│ PRAGMA user_version │
└──────┬───────┘     └────────┬─────────┘
       ↓                      ↓
┌──────────────────┐     ┌────────────┐
│ 按需执行迁移脚本   │←────│ version < N │
└──────────────────┘     └────────────┘
```

### 4.4 PRAGMA 配置

```sql
PRAGMA journal_mode = WAL;        -- WAL 模式，读不阻塞写
PRAGMA synchronous = NORMAL;      -- 平衡性能与持久性
PRAGMA busy_timeout = 5000;       -- 等待 5s 而非立即失败
PRAGMA foreign_keys = ON;         -- 外键约束
PRAGMA cache_size = -64000;       -- 64MB 缓存
```

**文件权限**：创建 DB 文件后立即设为 `0o600`（仅所有者可读写），防止多用户机器上信息泄露。

```rust
use std::fs::set_permissions;
use std::os::unix::fs::PermissionsExt;

let conn = Connection::open(&path)?;
set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
```

### 4.5 分层架构（crate 职责）

```
┌────────────────────────────────────────────────┐
│              visp-core (无 IO)                  │
│  - SessionStore trait（已有，纯抽象接口）         │
│  - Message struct（扩展字段）                    │
│  - SessionManager（不变）                       │
└───────────────────────┬────────────────────────┘
                        │ implements
┌───────────────────────▼────────────────────────┐
│               visp-db                           │
│  - SqliteSessionStore                          │
│    - rusqlite 实现 SessionStore trait          │
│    - 自动迁移 (user_version)                    │
│    - DAO 层（session_repo, message_repo）       │
└───────────────────────┬────────────────────────┘
                        │ 注入
┌───────────────────────▼────────────────────────┐
│             visp-daemon                         │
│  - 组装时选择 InMemory 或 Sqlite 实现            │
│  - 配置 -> 存储路径                              │
└────────────────────────────────────────────────┘
```

### 4.6 `SessionStore` trait 扩展

现有 trait 增删查改已满足基本需求，新增查询方法以支持历史检索：

```rust
pub trait SessionStore: Send {
    // 已有方法（签名改为返回 owned type）
    fn create(&mut self, session: Session) -> Result<(), SessionError>;
    fn get(&self, session_id: &str) -> Result<Session, SessionError>;
    fn list(&self) -> Result<Vec<Session>, SessionError>;
    fn delete(&mut self, session_id: &str) -> Result<(), SessionError>;
    fn update(&mut self, session: Session) -> Result<(), SessionError>;

    // 新增方法
    fn get_messages(&self, session_id: &str) -> Result<Vec<Message>, SessionError>;
    fn append_message(&mut self, session_id: &str, message: Message) -> Result<(), SessionError>;
    fn list_by_project(&self, project_path: &str) -> Result<Vec<Session>, SessionError>;
}
```

> **注意**：
> - 所有返回 Session 的方法改为 `Session`（owned type），而非 `&Session`。`SqliteSessionStore` 从 DB 查询后直接构造，无需缓存。`InMemorySessionStore` 从 HashMap clone 返回，代价极低（Session 元数据量小）
> - `get()` 返回的 Session 中 `history` 为空（仅元数据），需要 history 的调用方应使用 `get_messages()`。现有调用方已无通过 `get()` 读 history 的场景
> - **多进程风险**：当前设计假设单进程独占 DB。如果未来支持多进程共享 DB，`SqliteSessionStore` 的 `Mutex` 仅保护单进程内的并发

### 4.7 内存实现与 SQLite 实现共存

| 场景 | 实现 | 说明 |
|---|---|---|
| 测试 | InMemorySessionStore | 不依赖文件系统，快速 |
| 生产 | SqliteSessionStore | 默认持久化 |
| 配置可切换 | 通过 daemon.toml | `[storage] driver = "sqlite"` |

```rust
// daemon/src/main.rs
let store: Arc<dyn SessionStore> = if config.storage.driver == "sqlite" {
    Arc::new(SqliteSessionStore::new(&config.storage.path)?)
} else {
    Arc::new(InMemorySessionStore::new())
};
```

## 5. 实现策略（三阶段）

### 5.1 Phase 1：Message 字段扩展 + 数据库 Schema

**目标**：定义完整的数据结构和 SQLite 表

**改动范围**：

| 文件 | 改动 |
|---|---|
| `visp-core/src/message.rs` | 新增 `created_at`、`tool_result_is_error`、`tool_result_duration_ms`、`provider_metadata`、`actual_tokens_input`、`actual_tokens_output`、`actual_cache_read`、`actual_cache_write`、`actual_cost` 字段 |
| `visp-core/src/session.rs` | `Session` 新增 `created_at_unix: Option<i64>` 字段；`SessionStore` trait 新增 `get_messages`、`append_message`、`list_by_project` 方法；`home_dir()` 提取为公共函数（当前在 session.rs 和 rules.rs 各有私有实现） |
| `visp-core/src/message.rs` | 新增 `MessageType` 枚举（text/thinking/tool_call/tool_result/error/status）；更新 `user()`/`system()`/`tool()` 自动设置 `kind`；新增 `Message::tool_call()`、`Message::thinking()`、`Message::error()`、`Message::status()` 构造器 |
| `crates/visp-db/src/schema.rs` | SQL schema + 迁移代码 |

**Message 字段扩展细节**：

```rust
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Message {
    // 现有字段
    pub role: Role,
    pub content: String,
    pub tool_call_id: Option<String>,
    pub tool_calls: Option<Vec<ToolCallRequest>>,
    pub extra_blocks: Option<Vec<serde_json::Value>>,
    pub skip_context: bool,
    pub estimated_tokens: u32,

    // 新增字段（均 Optional / 有默认值，向后兼容）
    pub kind: MessageType,                 // "text" / "thinking" / "tool_call" / "tool_result" / "error" / "status"
    pub created_at: Option<i64>,           // Unix 毫秒时间戳
    pub tool_name: Option<String>,         // 工具名称
    pub tool_arguments: Option<String>,    // 工具参数 JSON
    pub tool_result_is_error: Option<bool>,
    pub tool_result_duration_ms: Option<u64>,
    pub provider_metadata: Option<serde_json::Value>,
    pub actual_tokens_input: Option<i64>,
    pub actual_tokens_output: Option<i64>,
    pub actual_cache_read: Option<i64>,
    pub actual_cache_write: Option<i64>,
    pub actual_cost: Option<f64>,
}

**构造器更新**：
- `Message::user()` / `Message::system()` → 自动设 `kind = User / System`
- `Message::tool()` → 自动设 `kind = ToolResult`
- `Message::assistant()` → 自动设 `kind = Text`
- 新增 `Message::tool_call(calls)` → `kind = ToolCall`，接收 `Vec<ToolCallRequest>`
- 新增 `Message::thinking(text)` → `kind = Thinking`
- 新增 `Message::error(text)` → `kind = Error`
- 新增 `Message::status(text)` → `kind = Status`
```

### 5.2 Phase 2：SqliteSessionStore 实现

**目标**：实现可运行的 SQLite 持久化

**新文件或新 crate**：推荐新增 `visp-db` crate，因为：

- 避免在 `visp-tools` 中混入存储逻辑
- 独立编译单元，测试更清晰
- 未来可替换为其他存储后端

```
crates/visp-db/
├── Cargo.toml
└── src/
    ├── lib.rs          ← 模块导出
    ├── schema.rs       ← SQL schema + 迁移
    ├── session_repo.rs ← session 表 DAO
    ├── message_repo.rs ← message 表 DAO
    └── store.rs        ← SqliteSessionStore 实现
```

**`Cargo.toml` 依赖**：

```toml
[dependencies]
visp-core = { path = "../visp-core" }
rusqlite = { version = "0.32", features = ["bundled"] }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
chrono = "0.4"
```

**`SqliteSessionStore` 实现要点**：

```rust
pub struct SqliteSessionStore {
    conn: Mutex<rusqlite::Connection>,
}
```

写入策略：**同步写**（每次 append_message / update 都直接写 DB）。`get()` 从 DB 查询 session 表字段构造 Session（`history` 为空），`get_messages()` 单独从 message 表查询。

> 注意：`rusqlite::Connection` 非 Send，需用 `std::sync::Mutex` 包装以符合 `SessionStore: Send` 约束。

### 5.3 Phase 3：Daemon 集成与配置

**目标**：daemon 启动时选择存储后端，CLI 可查询历史

**改动点**：

| 文件 | 改动 |
|---|---|
| `visp-daemon/src/config.rs` | 新增 `[storage]` 配置节 |
| `visp-daemon/src/main.rs` | 组装时创建 SqliteSessionStore 并注入 |
| `visp-daemon/src/service.rs` | 在 Chat 流结束时持久化 |
| `visp-daemon/Cargo.toml` | 添加 visp-db 依赖 |

**daemon.toml 配置**：

```toml
[storage]
driver = "sqlite"           # "sqlite" | "memory"
path = "~/.visp/data/visp.db"
```

### 5.4 与 AgentLoop 的交互流程

```
AgentLoop                    SqliteSessionStore
    │                               │
    │  append_message(msg1)         │
    │──────────────────────────────→│  INSERT INTO message ...
    │                               │
    │  append_message(msg2)         │
    │──────────────────────────────→│  INSERT INTO message ...
    │                               │
    │  get_messages(session_id)     │  <- 进程重启后恢复
    │──────────────────────────────→│
    │←─────────────────────────────│  SELECT * FROM message WHERE ...
```

> **注意**：运行时消息缓冲区在 `AgentLoopContext.history` 中，`SessionManager` 不持有 history。`append_message` 同步写入 DB，同时由 AgentLoop 追加到 `ctx.history`。启动时通过 `get_messages` 将历史恢复到 `ctx.history`。

## 6. 影响分析

### 6.1 对 visp-core 的侵入

| 改动 | 影响 |
|---|---|
| `Message` 新增字段 + 构造器更新 | 已有构造器 `user()`/`system()`/`tool()`/`assistant()` 内部自动设 `kind`，调用方零改动；新增 4 个构造器供新场景使用 |
| `SessionStore` 签名变更：`get()`/`list()` 从 `&Session` 改为 `Session`（owned type） | `InMemorySessionStore` 对应方法需改为 clone 返回；`SessionManager` 已返回 `Session`，调用方零改动 |
| `SessionStore` 新增方法 | `InMemorySessionStore` 需要实现 `get_messages`、`append_message`、`list_by_project` |
| `SessionManager` 内部逻辑 | 新增 `append_message` 和 `get_messages` 调用点 |

**原则**：`visp-core` 不增加新依赖，所有 IO 在 `visp-core` 之外。

### 6.2 对现有测试的影响

- 现有 `InMemorySessionStore` 测试不受影响
- 新增 `SqliteSessionStore` 测试：可使用 `:memory:` 数据库，无需文件系统
- Message 序列化/反序列化测试需覆盖新增字段

### 6.3 对 gRPC proto 的影响

proto 无需修改，所有持久化在服务端透明处理。CLI/TUI 仍通过现有 gRPC 接口交互。

### 6.4 对 daemon 启动流程的影响

```
启动前（纯内存）：
  daemon → 无状态启动 → 就绪

启动后（SQLite）：
  daemon → 打开/创建 DB → 执行迁移 → 加载活跃会话 → 就绪
```

启动时间增加 50-200ms（取决于 DB 大小和迁移）。

## 7. 关键设计决策

### 7.1 新 crate 还是放 visp-tools？

| 方案 | 优缺点 |
|---|---|
| 新 crate `visp-db` | 职责清晰，独立编译，可替换后端；增加工作区数量 |
| 放 `visp-tools` | 减少 crate 数量；语义不清（tools vs store） |

**已定**：新 crate `visp-db`。

### 7.2 同步写还是异步写？

| 方案 | 优缺点 |
|---|---|
| 同步写（每次 append 直接写 DB） | 简单可靠；消息量大时可能阻塞 AgentLoop |
| 异步写（通过 channel 批量写） | 性能好；实现复杂，崩溃时可能丢最后几条消息 |
| 同步写 + 小缓存 | 折中方案 |

**已定**：同步写。每次 append 直接写 DB，`std::sync::Mutex<Connection>` 包装，单行 INSERT < 1ms，对 AgentLoop 无显著影响。

### 7.3 Message 中用 `kind` 还是依赖 role 推断？

| 方案 | 优缺点 |
|---|---|
| 新增 `kind` 字段 | 明确表达语义；与 role 有冗余关系 |
| 仅靠 role 推断 | 简单但信息不足（assistant role 可能是 text/thinking/error） |

**已定**：两者结合。role 决定"谁说的"，type 决定"说的什么"。DB 中 role + type 双字段，Rust `Message` 结构体中新增 `MessageType` 枚举。

```rust
/// 消息子类型
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum MessageType {
    #[serde(rename = "system")]
    System,
    #[serde(rename = "user")]
    User,
    #[serde(rename = "text")]
    Text,
    #[serde(rename = "thinking")]
    Thinking,
    #[serde(rename = "tool_call")]
    ToolCall,
    #[serde(rename = "tool_result")]
    ToolResult,
    #[serde(rename = "error")]
    Error,
    #[serde(rename = "status")]
    Status,
}
```

### 7.4 用量统计延迟聚合还是实时更新？

| 方案 | 优缺点 |
|---|---|
| 延迟聚合（查询时 SUM message 表） | 数据始终一致，写路径简单；查询需做 SUM |
| 每次 append 后实时更新 session 统计字段 | 读时 O(1)；写放大，存在不一致窗口（INSERT 成功但 UPDATE 失败） |

**已定**：延迟聚合。理由：

- 用量统计是**派生数据**，message 表才是数据源头。延迟聚合不存在一致性问题
- 单会话几千条 message 的 SUM 在 SQLite 上是亚毫秒级，无性能瓶颈
- 写路径保持简单：只需 INSERT message，无需额外 UPDATE session
- 实际 tokens/cost 从 `actual_tokens_*` / `actual_cost` 独立字段直接 SUM，无需解析 JSON
- session 表不再预存 `total_*` / `total_cost` 字段

### 7.5 是否支持数据库自动清理？

| 方案 | 优缺点 |
|---|---|
| 自动清理（按时间/数量阈值） | 节省磁盘；可能误删用户有价值的数据 |
| 不做自动清理，提供手动删除 | 不丢数据，用户按需控制；简单 |
| 完全不做（当前状态） | 最简单；长期数据量仍可控 |

**已定**：不做自动清理。理由：

- 单会话年数据量约 48MB，SQLite 完全无压力
- 历史消息对用户有回溯价值，不应自动删除
- 未来可通过 gRPC 暴露 `DeleteSession` 接口，让用户按需清理（`SessionStore::delete` 已存在，仅需透出到 gRPC 层）

留待后续实现 compaction 策略（按时间/数量自动清理）。

## 8. 未涵盖内容（后续演进）

- Event Sourcing：当前仅做直接持久化，不做事件溯源
- 历史 compaction：不自动合并历史消息
- 会话分叉（fork）：当前无此需求
- 全文检索：message.content 的 FTS 暂不做
- 云同步/多设备：暂不考虑
- 消息附件存储：大文件（图片等）的存储策略
- 导出/导入：JSON 或 Markdown 导出
