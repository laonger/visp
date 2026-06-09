# vibewisp MCP 支持设计

## 1. 目标

为 vibewisp 添加 MCP（Model Context Protocol）支持，使 daemon 能够连接外部 MCP 服务器，将其提供的工具动态集成到 Tool Registry 中，供 Agent 编排器调用。

**一句话总结**：用户配置 MCP 服务器列表后，daemon 启动时自动连接，MCP 工具与内置工具无缝集成，Agent 循环一视同仁。

## 2. 背景

### 2.1 当前状态

- 所有工具都是**内置硬编码**的：ReadFile、WriteFile、Bash、Grep 等
- 工具注册在 `vbw-daemon/src/main.rs` 中静态完成
- `Tool` trait 设计良好，已有 `Send + Sync` 支持
- `ToolRegistry` 支持动态注册（`register` 方法）
- Agent 循环通过 `tool_registry.execute()` 统一调用工具

### 2.2 MCP 协议要点

MCP 是 Anthropic 提出的模型上下文协议，核心能力：

| 能力 | 说明 | 用途 |
|---|---|---|
| `tools/list` | 列出服务器提供的所有工具 | 注册到 ToolRegistry |
| `tools/call` | 调用指定工具 | 执行工具逻辑 |
| `resources/list` | 列出资源（可选） | 未来可扩展 |
| `prompts/list` | 列出提示模板（可选） | 未来可扩展 |

**传输方式**：
- **stdio**：daemon 以子进程方式启动 MCP 服务器，通过 stdin/stdout 通信
- **SSE**：HTTP Server-Sent Events 方式连接已有 MCP 服务器

## 3. 模块划分

| Crate | 职责 | 类型 |
|---|---|---|
| **vbw-mcp** | MCP 客户端封装、工具适配器、服务器进程管理 | 新建 |
| **vbw-daemon** | 配置加载 MCP 服务器列表、启动连接、注册工具到 Registry | 扩展 |

### 3.1 vbw-mcp crate

```
vbw-mcp/
├── Cargo.toml
└── src/
    ├── lib.rs         # 模块声明 + McpManager 主结构体
    ├── error.rs       # McpError 类型
    ├── transport.rs   # transport 工厂函数（创建 rmcp transport）
    ├── client.rs      # McpSession: 封装 rmcp::Client
    ├── tool.rs        # McpToolAdapter: 将 MCP 工具转为 Tool trait
    └── config.rs      # MCP 服务器配置类型
```

**新增依赖**：
- `rmcp`：MCP 协议 Rust SDK
- `tokio`：workspace 已有
- `serde` / `serde_json`：workspace 已有
- `tracing`：workspace 已有
- `vbw-core`：项目内，依赖 `Tool` trait 和 `ToolResult`
- `thiserror`：workspace 已有，用于定义 `McpError`

#### 3.1.0 错误类型（`error.rs`）

```rust
#[derive(Debug, thiserror::Error)]
pub enum McpError {
    #[error("Transport error: {0}")]
    Transport(String),

    #[error("Protocol error: {0}")]
    Protocol(String),

    #[error("Tool call timed out after {0}s")]
    Timeout(u64),

    #[error("Server not connected: {0}")]
    NotConnected(String),

    #[error("Tool not found: {0}")]
    ToolNotFound(String),
}
```

#### 3.1.1 配置类型（`config.rs`）

```rust
/// MCP 服务器配置
#[derive(Debug, Clone, Deserialize)]
pub struct McpServerConfig {
    /// 唯一标识名
    pub name: String,
    /// 传输方式
    pub transport: McpTransport,
    /// 是否在启动时自动连接（默认 true）
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    /// 工具名称前缀，防止不同服务器的工具名冲突
    #[serde(default)]
    pub tool_prefix: Option<String>,
    /// 工具调用超时秒数（默认 60）
    #[serde(default = "default_tool_timeout")]
    pub tool_timeout_secs: u64,
}

fn default_enabled() -> bool { true }
fn default_tool_timeout() -> u64 { 60 }

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type")]
pub enum McpTransport {
    /// 子进程方式：daemon 启动并管理
    #[serde(rename = "stdio")]
    Stdio {
        /// 启动命令
        command: String,
        /// 命令参数
        #[serde(default)]
        args: Vec<String>,
        /// 环境变量（继承 daemon 环境，此处的为叠加/覆盖）
        #[serde(default)]
        env: HashMap<String, String>,
    },
    /// HTTP SSE 方式：连接已运行的 MCP 服务器
    #[serde(rename = "sse")]
    Sse {
        /// SSE 端点 URL
        url: String,
        /// HTTP 请求头（用于认证等）
        #[serde(default)]
        headers: HashMap<String, String>,
    },
}
```

#### 3.1.2 传输层（`transport.rs`）

使用 `rmcp` 提供的现成传输实现，不自定义 transport trait。

**使用的 rmcp 传输类型**：
- `rmcp::transport::TokioChildProcess` — 客户端 stdio 传输（spawn 子进程并通过 stdin/stdout 通信）
- `rmcp::transport::StreamableHttpClientTransport` — 客户端 Streamable HTTP 传输（连接已运行的 MCP 服务器）

**创建方式**：

```rust
// Stdio（client 端）
use rmcp::transport::{TokioChildProcess, ConfigureCommandExt};
let transport = TokioChildProcess::new(
    tokio::process::Command::new("npx")
        .args(["@anthropic-ai/mcp-playwright"])
);

// Streamable HTTP（client 端）
use rmcp::transport::StreamableHttpClientTransport;
let transport = StreamableHttpClientTransport::new(
    reqwest::Client::new(),
    "http://localhost:3000/mcp",
);
```

**SSE endpoint 发现**：`StreamableHttpClientTransport` 内部自动处理 endpoint 发现（从初始响应中获取 `endpoint` 字段作为后续操作的 POST URL）。

**客户端初始化**——`rmcp` 不通过 `Client::new(transport)` 构造客户端，而是通过 `ServiceExt::serve()`：

```rust
use rmcp::ServiceExt;

// Stdio
let client = ().serve(TokioChildProcess::new(
    tokio::process::Command::new("npx")
)).await?;

// Streamable HTTP
let client = ().serve(StreamableHttpClientTransport::new(
    reqwest::Client::new(),
    "http://localhost:3000/mcp",
)).await?;
```

`().serve(transport)` 返回的 `client` 实现了 `ClientHandler`，提供 `list_tools()`、`call_tool()` 等方法。

**封装**：在 `McpSession::connect()` 内部完成 transport 创建和 `serve()` 调用，不对外暴露细节。

#### 3.1.3 MCP 客户端会话（`client.rs`）

**职责**：封装 `rmcp` client handler，提供连接管理、工具发现和工具调用接口。

**核心流程**：

```
1. 创建 transport（TokioChildProcess spawn / StreamableHttpClient connect）
2. 通过 ().serve(transport) 完成初始化（rmcp 内部自动处理 initialize 握手）
3. 发送 tools/list 请求 → 获取工具列表
4. 等待后续 tools/call 请求
5. 关闭时 drop client handler（自动发送 shutdown）
```

**状态管理**：

```rust
pub struct McpSession {
    /// 服务器名称
    name: String,
    /// rmcp client handler（通过 ().serve(transport) 创建）
    /// 实际类型为 rmcp::service::client::ClientService<H, T>，这里用 Box<dyn> 擦除
    handler: Box<dyn ClientHandler>,
    /// 已发现的工具定义
    tools: Vec<McpToolDefinition>,
    /// 传输是否已连接
    connected: bool,
    /// 进程句柄（stdio 模式）
    child: Option<tokio::process::Child>,
}
```

**关键方法**：
- `new(name, config)` → 创建 session（不含连接）
- `connect()` → 创建 transport + 调用 `().serve(transport)` 获取 handler
- `list_tools()` → 通过 handler 获取工具列表
- `call_tool(name, args)` → 通过 handler 调用工具，调用前检查 `connected` 状态，断开时返回错误
- `shutdown()` → drop handler，kill 子进程，标记 `connected = false`

**断开处理**：进程退出回调中标记 `connected = false` 并通知 `McpManager` 触发工具移除。重连成功后重新构造 `rmcp::Client` 并通过 `ToolRegistry::update()` 重新注册工具。

#### 3.1.4 MCP 工具适配器（`tool.rs`）

**职责**：定义 MCP 工具相关类型，并将 MCP 服务器发现的工具包装为 `Tool` trait 实现。

```rust
/// MCP 服务器返回的工具定义
#[derive(Debug, Clone)]
pub struct McpToolDefinition {
    /// 工具名称（原始名称，不含前缀）
    pub name: String,
    /// 工具描述（MCP 协议中为可选字段）
    pub description: Option<String>,
    /// 输入参数 JSON Schema
    pub input_schema: serde_json::Value,
}

/// 将 MCP 工具适配为 Tool trait
pub struct McpToolAdapter {
    /// 工具名称（可能带前缀）
    name: String,
    /// 原始工具名称（不含前缀，用于调用 MCP 服务器时传回）
    original_name: String,
    /// 工具描述
    description: String,
    /// 参数定义（JSON Schema，复用 MCP 的 inputSchema）
    parameters: serde_json::Value,
    /// 工具调用超时秒数
    timeout_secs: u64,
    /// 所属 MCP 会话
    session: Arc<Mutex<McpSession>>,
}
```

**Tool trait 实现**：
- `name()` → 带前缀的名称
- `description()` → MCP 服务器提供的描述，为空时返回 `"MCP tool from {server_name}"`
- `parameters()` → 从 MCP 的 `inputSchema` 转换
- `execute()` → 通过 `client.call_tool()` 发送到 MCP 服务器，`tokio::time::timeout` 控制超时
- `requires_approval()` → 默认 true（外部工具默认需审批）

#### 3.1.5 MCP 管理器（`lib.rs`）

**职责**：管理多个 MCP 服务器的生命周期，对外提供统一接口。

```rust
pub struct McpManager {
    /// 所有已连接的会话，按服务器 name 索引
    sessions: Mutex<HashMap<String, Arc<Mutex<McpSession>>>>,
    /// 配置的服务器列表
    configs: Vec<McpServerConfig>,
}
```

**关键方法**：
- `new(configs)` → 创建管理器
- `start_all(on_ready)` → 后台连接所有 enabled 的 MCP 服务器。每个服务器就绪后调用 `on_ready(tools)` 回调将工具注册到 ToolRegistry。
- `shutdown_all()` → 优雅关闭所有连接
- `shutdown(name)` → 关闭指定服务器
- `restart(name)` → 重启指定 MCP 服务器（从 sessions 中移除旧 session，从 ToolRegistry 移除旧工具，重新连接并注册新工具）。期间该服务器的工具短暂不可用。

### 3.2 vbw-daemon 扩展

#### 3.2.1 配置扩展

在 `daemon.toml` 中新增 `[mcp]` section：

```toml
[mcp]

[[mcp.servers]]
name = "playwright"
transport = { type = "stdio", command = "npx", args = ["@anthropic-ai/mcp-playwright"] }
enabled = true

[[mcp.servers]]
name = "filesystem"
transport = { type = "stdio", command = "npx", args = ["@modelcontextprotocol/server-filesystem"] }
enabled = false

[[mcp.servers]]
name = "custom-api"
transport = { type = "sse", url = "http://localhost:3000/mcp" }
enabled = true
tool_prefix = "my_"
```

#### 3.2.2 启动流程变更

在 `vbw-daemon/src/main.rs` 中，新增 MCP 初始化步骤（在工具注册之后）：

```
原流程：
  3. LLM Provider
  4. Tool Registry（内置工具注册）
  5. Rule Engine
  6. Session Manager

新流程：
  3. LLM Provider
  4. Tool Registry（内置工具注册）  
  5. MCP Manager（启动 MCP 连接 → 发现工具 → 注册到 Registry）
  6. Rule Engine
  7. Session Manager
```

**关键代码变更**（简化示意）：
```rust
// 5. MCP Manager — 后台异步连接，不阻塞 daemon
let mcp_manager = Arc::new(McpManager::new(config.mcp.servers));
let mcp_tx = tool_registry.clone();
mcp_manager.start_all(move |tools| {
    for tool in tools {
        if let Err(e) = mcp_tx.register(tool) {
            tracing::warn!("failed to register MCP tool: {e}");
        }
    }
});
// start_all 是同步的，内部 spawn 后台 task
// 连接在后台进行，不阻塞 daemon 启动
// McpManager 传入 CoderDaemonService 供后续管理
```

#### 3.2.3 DaemonConfig 扩展

```rust
#[derive(Debug, Clone, Deserialize)]
pub struct DaemonConfig {
    pub daemon: DaemonSection,
    pub llm: LlmSection,
    pub tools: ToolsSection,
    pub agent: AgentSection,
    #[serde(default)]
    pub mcp: McpConfig,       // 新增
    #[serde(default)]
    pub tool: HashMap<String, toml::Value>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct McpConfig {
    #[serde(default)]
    pub servers: Vec<McpServerConfig>,
}
```

## 4. 核心数据流

### 4.1 启动流程

```
Daemon 启动
  │
  ├─ 1. 加载配置（含 MCP 服务器列表）
  ├─ 2. 初始化 LLM Provider
  ├─ 3. 注册内置工具
  ├─ 4. 初始化 McpManager（后台异步连接 MCP 服务器）
  │     │
  │     ├─ spawn 独立 task 逐个连接 MCP 服务器
  │     │   ├─ Stdio: tokio::process::Command::new("npx").arg(...).spawn()
  │     │   │          连接 stdin/stdout → McpSession
  │     │   └─ SSE: 连接 URL → McpSession
  │     │
  │     ├─ 后台完成握手 initialize → tools/list
  │     └─ 就绪后自动通过回调将 McpToolAdapter 注册到 ToolRegistry
  │          （未就绪前不影响 daemon 启动和使用）
  │
  ├─ 5. 初始化 Rule Engine
  ├─ 6. 初始化 Session Manager
  └─ 7. 启动 gRPC Server（Agent 循环可用 MCP 工具）
```

### 4.2 Agent 循环中的工具调用

```
Agent 循环收到 LLM 的 ToolCall
  │
  ├─ 工具名 "read_file" → ToolRegistry 分发给内置 ReadFile
  ├─ 工具名 "playwright_click" → ToolRegistry 分发给 McpToolAdapter
  │     │
  │     └─ McpToolAdapter.execute()
  │           │
  │           ├─ 通过 McpSession.call_tool("click", args) 发送 JSON-RPC
  │           ├─ MCP 服务器返回结果
  │           └─ 返回 ToolResult
  │
  └─ Agent 拼接结果，继续循环
```

### 4.3 MCP 服务器进程生命周期

```
启动 → 连接 → 初始化 → 运行中 → 关闭
                              │
                     ┌────────┴────────┐
                     │                 │
                进程崩溃            用户关闭
                     │                 │
                 自动重连            shutdown
              （最多 N 次）        → kill 进程
```

## 5. 关键设计决策

### 5.1 ToolRegistry 支持运行时动态更新

**决策**：`ToolRegistry.tools` 改为 `RwLock<Vec<Box<dyn Tool>>>`，增加 `remove(name)` 和 `update(name, tool)` 方法。

**理由**：
- MCP 服务器崩溃→重连后需移除旧适配器并注册新的
- 工具名冲突时需能替换
- 内部使用 `RwLock` 而非 `Mutex`：读操作频繁（`definitions()` / `get()`），写操作极少（仅重连时）
- 外层 `Arc<ToolRegistry>` 不变，调用方无需修改

**变更**：只影响 `vbw-core/src/tool_registry.rs`，Agent 循环和 daemon 的调用代码不变。

### 5.2 使用 `rmcp::Client` 高层 API

**决策**：直接使用 `rmcp::Client` 的高层 API，而非自定义传输层 trait。

**理由**：
- `rmcp::Client` 已处理完整协议生命周期（initialize 握手、请求-响应路由、错误处理）
- `rmcp` 提供现成的 `StdioTransport` 和 `SseTransport`
- 减少自实现代码量，只需封装 `McpSession` 作为薄层包装
- `McpToolAdapter` 通过 `client.call_tool()` 直接调用

### 5.3 串行通道约束

每个 MCP 服务器（尤其是 Stdio 传输）本质上是串行通道——一根 stdin/stdout 管道，JSON-RPC 一问一答。Agent 循环批量并行调用同一 MCP 服务器的多个工具时，调用会退化为串行执行。

**决策**：接受此约束。不引入请求队列或多路复用方案，因为：
- MCP 协议标准不要求服务器支持乱序响应
- 工具调用瓶颈通常在服务器端执行耗时，而非锁竞争
- 与内置工具（如 Bash 是独立进程）不同，MCP 工具受限于服务器实现

### 5.4 异步连接 + 超时机制

MCP 服务器的连接和工具发现不应阻塞 daemon 启动。

**决策**：
- daemon 启动时 spawn 独立 task 在后台连接 MCP 服务器，不阻塞主流程
- 每个服务器连接设独立超时（默认 10s，可配置）
- 连接成功 → 自动将工具注册到 ToolRegistry
- 连接超时 → log warning，后台继续重试（最多 3 次）
- Agent 调用未就绪的 MCP 工具时返回 `"tool {name} from {server} is not ready yet"`
- daemon 启动后即可使用，MCP 工具延迟可用

### 5.6 MCP 服务器认证

Stdio 方式通过 `env` 字段传递认证凭据（如 API key）。SSE 方式需要 HTTP header 支持。

**决策**：`McpTransport::Sse` 增加 `headers: HashMap<String, String>` 字段。

```rust
#[serde(rename = "sse")]
Sse {
    url: String,
    #[serde(default)]
    headers: HashMap<String, String>,
},
```

配置示例：
```toml
[[mcp.servers]]
name = "custom-api"
transport = { type = "sse", url = "http://localhost:3000/mcp", 
              headers = { Authorization = "Bearer sk-xxx" } }
```

### 5.7 MCP 工具调用超时

MCP 工具调用需要超时控制，防止慢服务器卡住 Agent 循环。

**决策**：每个 MCP 服务器独立配置 `tool_timeout_secs`，默认 60 秒。

`McpServerConfig` 新增字段：
```rust
#[serde(default = "default_tool_timeout")]
pub tool_timeout_secs: u64,  // 默认 60
```

`McpToolAdapter.execute()` 通过 `tokio::time::timeout` 控制：
- 正常返回 → 透传 MCP 服务器结果
- **超时处理**：标记 `McpSession.connected = false`，废弃当前连接。下一次工具调用时自动重建连接（重新 spawn 子进程或重新连 SSE）。防止协议失步（残留 JSON-RPC 响应被误认）。
- 超时 → 返回 `ToolResult::error("Tool call timed out, reconnecting...")`

### 5.8 McpManager 生命周期归属

`McpManager` 需在 daemon 生命周期内持续可访问，用于重连、健康检查、优雅关闭。

**决策**：存到 `CoderDaemonService` 中，与 `codegraphs` 字段采用相同模式。

```rust
pub struct CoderDaemonService {
    // ... 现有字段 ...
    mcp_manager: Arc<McpManager>,    // 新增
}
```

- daemon 启动时构造并传入
- shutdown 时 `mcp_manager.shutdown_all().await`
- 未来 `/mcp restart <name>` 等管理命令可访问

### 5.9 MCP 协议版本协商

**决策**：协议版本协商委托给 `rmcp` crate 处理（`initialize` / `InitializeResult` 握手由 rmcp 内部完成）。初始化失败时（如版本不兼容），该服务器标记为不可用，log error。

### 5.10 工具名称冲突

**MCP vs 内置**：MCP 工具不允许覆盖内置工具。冲突时 log warning 并跳过注册。

**MCP vs MCP**：同一名称的 MCP 工具通过 `tool_prefix` 配置区分。无前缀时后注册的覆盖先注册的（log warning）。

### 5.11 进程崩溃检测

Stdio 模式通过独立 tokio task 执行 `child.wait()` 异步检测进程退出，退出时自动触发重连逻辑。不使用轮询方式。

### 5.12 环境变量继承

Stdio 子进程默认继承 daemon 的完整环境变量。配置的 `env` 字段作为叠加/覆盖。

### 5.13 MCP 工具分类

`McpToolAdapter::category()` 返回 `"mcp"`，动态工具指南中 `render_tool_guide()` 的分类映射表增加 `("External (MCP)", "mcp")`，渲染为 "External (MCP)" 分组。

### 5.14 MCP 工具默认需审批

外部 MCP 工具（非内置）默认 `requires_approval = true`，因为：
- MCP 服务器可能有安全隐患（如可执行任意 shell 命令）
- 用户应知道"有一个外部工具要被调用了"

用户可通过 `Always Allow`（已实现）记住选择。

### 5.15 进程崩溃与重连

Stdio 模式的 MCP 服务器可能崩溃。`TokioChildProcess` 内部管理子进程生命周期，子进程退出时 `rmcp` 传输层会报错。

检测与重连策略：
- 工具调用（`call_tool()`）返回传输错误时触发重连逻辑
- 也可通过独立 tokio task 定期发送 `ping`（`list_tools()`）检测连接健康
- 自动重连策略：最多重试 3 次，指数退避
- 重连后重新发现工具并更新 ToolRegistry（通过 `ToolRegistry::update()`）
- 重连失败后该服务器的工具从 Registry 移除

### 5.16 关闭顺序

Daemon 关闭时需确保 MCP 服务器进程被优雅终止：

```
Daemon 关闭
  │
  ├─ 1. 停止接受 gRPC 新请求
  ├─ 2. 取消所有运行中的 Agent loop（CancellationToken）
  ├─ 3. McpManager.shutdown_all()
  │     ├─ 发送 shutdown 通知
  │     └─ kill 子进程 / 关闭 SSE 连接
  └─ 4. 退出进程
```

### 5.17 资源/提示的扩展预留

MCP 协议除工具外还支持 Resources 和 Prompts：
- 当前阶段 **不做** Resources 和 Prompts 的支持
- 在 `McpSession` 中预留 `list_resources()` 和 `list_prompts()` 的接口
- 未来可通过 `McpResourceAdapter` 类似机制扩展

### 5.18 锁顺序约束

涉及三个锁，获取顺序必须严格遵守，否则可能死锁：

| 锁 | 持有者 | 获取时机 |
|---|---|---|
| ToolRegistry.RwLock（写锁） | ToolRegistry | MCP 注册/更新/移除工具 |
| ToolRegistry.RwLock（读锁） | 任意 get/definitions 调用方 | Agent 循环遍历工具 |
| McpSession.Mutex | McpToolAdapter | 执行工具调用时获取 session |
| McpManager.Mutex | McpManager | 管理 sessions HashMap |

**强制约束**：任何路径都不允许在持有 `McpSession.Mutex` 的同时获取 `ToolRegistry` 的写锁。

```
✅ 允许的路径：
  ToolRegistry(读锁) → McpSession.Mutex        (Agent 调用工具)
  ToolRegistry(写锁) → [不获取 McpSession 锁]   (MCP 重连后注册)

❌ 禁止的路径：
  McpSession.Mutex → ToolRegistry(写锁)         (重连时在持有 session 锁的情况下注册)
```

**实现要点**：
- `McpManager` 重连时：先准备好所有 `McpToolAdapter`（此时可持有 McpSession 锁），**释放 McpSession 锁后**，再获取 ToolRegistry 写锁执行注册
- 工具调用路径天然安全：Agent 先获取 ToolRegistry 读锁找到工具，再调用 `McpToolAdapter.execute()` 获取 McpSession 锁——读锁在 execute 前已释放（读锁不跨 await 持有）

## 6. 不做什么

- ❌ 不支持 MCP Resources（文件/数据资源读取）
- ❌ 不支持 MCP Prompts（提示模板）
- ❌ 不支持 MCP 采样（sampling，即 LLM → 服务器 → LLM 的回调）
- ❌ 不做 MCP 服务器热加载（修改配置后需重启 daemon）
- ❌ 不做 MCP 服务器图形化管理界面
- ❌ 不支持 MCP 通知（notifications）的处理
- ❌ 不支持 Streaming 工具调用结果（MCP 支持流式返回，初期按完整结果处理）

## 6.1 环境依赖

- **Node.js**：通过 `npx` 启动的 MCP 服务器（如 `@anthropic-ai/mcp-playwright`）依赖 Node.js 运行时，用户需确保已安装。
- **Python**：通过 `pipx`/`uvx` 启动的 MCP 服务器依赖 Python 环境，非本项目职责。
- 原生二进制的 MCP 服务器无额外依赖。

## 7. 验收标准

- `cargo build --workspace` 编译通过
- `cargo test --workspace` 所有测试通过（含 vbw-mcp 新增测试）
- `cargo clippy --workspace -- -D warnings` 通过
- **单元测试 1**：`MockTransport`（实现 `rmcp::transport::ClientTransport`）模拟 MCP 服务器 → McpSession 握手成功、工具列表发现正确
- **单元测试 2**：McpToolAdapter 执行工具调用 → 返回正确的 ToolResult
- **单元测试 3**：McpManager 管理多个会话 → 生命周期正常
- **单元测试 4**：StdioTransport 子进程启动和关闭
- **集成测试 1**：配置一个真实的 MCP 服务器（如 MCP Playwright）→ 工具注册到 Registry
- **集成测试 2**：通过 Agent 循环调用 MCP 工具 → 成功返回结果
- **文档**：MCP 服务器的配置方式和示例写入 README
