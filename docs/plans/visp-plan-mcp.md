# visp MCP 工作计划

## 概述

基于设计文档 `visp-design-mcp.md`，将 MCP 支持拆分为 6 个 Wave、11 个子步骤。

**影响范围**：visp-core（修改）、visp-mcp（新建）、visp-daemon（扩展）

---

## 步骤 1：ToolRegistry 支持运行时动态更新

### 1a：ToolRegistry RwLock + remove/update + 核心工具保护

#### 🔴 红 — 测试
| # | 测试用例 |
|---|---------|
| 1 | `remove(name)` 移除已注册工具，`get(name)` 返回 None |
| 2 | `remove(name)` 移除不存在的工具 → 返回 Err |
| 3 | `update(name, tool)` 替换同名工具，`get(name)` 返回新工具 |
| 4 | `update(name, tool)` 更新不存在的工具 → 返回 Err |
| 5 | 并行读：`definitions()` 和 `get()` 在写操作时不被阻塞 |
| 6 | `seal_core_tools()` 锁定当前已注册的工具为核心工具 |
| 7 | `register_mcp(name, tool)` 与核心工具冲突时 → log warning + 跳过 |
| 8 | `register_mcp(name, tool)` 无冲突 → 正常注册 |

#### 🟢 绿 — 实现
- `ToolRegistry.tools` 从 `Vec<Box<dyn Tool>>` 改为 `RwLock<Vec<Box<dyn Tool>>>`
- 新增 `core_tool_names: RwLock<HashSet<String>>` 字段
- `register()` 签名从 `&mut self` 改为 `&self`，内部 `write().lock()`
- `get()` / `definitions()` / `names()` 内部 `write().lock()` 改为 `read().lock()`
- 新增 `remove(name) -> Result<(), String>` 方法
- 新增 `update(name, tool) -> Result<(), String>` 方法
- 新增 `seal_core_tools()` — 将当前所有工具名存入 `core_tool_names`
- 新增 `register_mcp(tool) -> Result<(), String>` — 检查与 `core_tool_names` 冲突，跳过冲突工具

#### ♻️ 重构
- 现有 `register` 调用处（`main.rs`）无需改动（`&mut self` → `&self` 兼容）

---

## 步骤 2：visp-mcp crate 脚手架

### 2a：创建 crate + 配置类型 + 错误类型

#### 🔴 红 — 测试
| # | 测试用例 |
|---|---------|
| 1 | `McpServerConfig` 从 TOML 反序列化（stdio 模式，含 command + args + env） |
| 2 | `McpServerConfig` 从 TOML 反序列化（sse 模式，含 url + headers） |
| 3 | `McpServerConfig` 默认值（enabled=true, tool_timeout_secs=60, tool_prefix=None） |
| 4 | `McpConfig` 反序列化空列表（`[mcp]` 下无 servers） |
| 5 | `McpError::Display` 格式化正确 |

#### 🟢 绿 — 实现
- 创建 `crates/visp-mcp/Cargo.toml`：
  ```toml
  [package]
  name = "visp-mcp"
  version.workspace = true
  edition.workspace = true

  [dependencies]
  rmcp = { version = "1.7", features = ["client", "transport-child-process", "transport-streamable-http-client-reqwest"] }
  tokio.workspace = true
  serde.workspace = true
  serde_json.workspace = true
  tracing.workspace = true
  thiserror.workspace = true
  visp-core = { path = "../visp-core" }
  ```
- `src/config.rs`：`McpServerConfig`、`McpTransport`、`McpConfig`
- `src/error.rs`：`McpError` 枚举
- `src/lib.rs`：模块声明

---

## 步骤 3：Transport 工厂 + McpSession

### 3a：Transport 工厂函数

#### 🔴 红 — 测试
| # | 测试用例 |
|---|---------|
| 1 | `create_stdio_transport()` 返回 `TokioChildProcess` |
| 2 | transport 创建时正确传递 command + args + env |
| 3 | `create_sse_transport()` 返回 `StreamableHttpClientTransport` |
| 4 | `create_sse_transport()` 正确传递 URL + headers |

#### 🟢 绿 — 实现
- `src/transport.rs`：工厂函数
  - `create_stdio_transport(config) -> TokioChildProcess`
  - `create_sse_transport(url, headers) -> StreamableHttpClientTransport`
- 包装 `rmcp::transport::child_process::TokioChildProcess` 的构造

### 3b：McpSession（连接 + 工具发现）

#### 🔴 红 — 测试
| # | 测试用例 |
|---|---------|
| 1 | `McpSession::new()` 创建未连接的 session |
| 2 | `connect()` 通过 MockTransport 完成 initialize 握手 |
| 3 | `list_tools()` 返回 `Vec<McpToolDefinition>` |
| 4 | 连接超时 → 标记 `connected = false`，返回 Error |
| 5 | `connected` 初始为 false，连接成功后为 true，断开后为 false |

#### 🟢 绿 — 实现
- `src/client.rs`：
  - `McpSession` 结构体（name, client: RunningService, tools, connected, child）
  - `new(name, config)` — 保存配置但不连接
  - `connect()` — 创建 transport，调用 `handler.serve_with_ct()`，获取 `RunningService`
  - `list_tools()` — 通过 `RunningService` 发送 tools/list 请求
  - 实现 `ClientHandler` — `get_info()` 返回客户端信息

#### 🧪 测试方案
- 使用 `rmcp::transport::AsyncReaderWriter` 或 channel-based mock transport
- 在测试中模拟 MCP server 的 initialize response 和 tools/list response

### 3c：McpSession（工具调用 + 关闭）

#### 🔴 红 — 测试
| # | 测试用例 |
|---|---------|
| 1 | `call_tool(name, args)` 发送 JSON-RPC 并接收结果 |
| 2 | `call_tool()` 在 `connected=false` 时返回 Err(NotConnected) |
| 3 | `shutdown()` 标记 `connected=false`，关闭 transport |
| 4 | 超时后 `connected=false`（为下次自动重连准备） |

#### 🟢 绿 — 实现
- `call_tool(name, args)` — 通过 `RunningService` 发送 tools/call
- `shutdown()` — drop RunningService + child process
- 超时处理（由 `McpToolAdapter` 调用时标记断开）

---

## 步骤 4：McpToolAdapter

### 4a：McpToolDefinition + McpToolAdapter

#### 🔴 红 — 测试
| # | 测试用例 |
|---|---------|
| 1 | `McpToolDefinition` → `McpToolAdapter` 构造正确（name/description/parameters） |
| 2 | `McpToolAdapter::name()` 返回带前缀的名称 |
| 3 | `McpToolAdapter::description()` 返回正确描述，None 时返回兜底文本 |
| 4 | `McpToolAdapter::parameters()` 返回 inputSchema |
| 5 | `McpToolAdapter::execute()` 通过 session.call_tool() 成功执行 |
| 6 | `McpToolAdapter::execute()` 超时 → ToolResult::error + 标记断开 |
| 7 | `McpToolAdapter::execute()` session 断开 → ToolResult::error |
| 8 | `McpToolAdapter::requires_approval()` 返回 true |
| 9 | `McpToolAdapter::category()` 返回 "mcp" |
| 10 | `Tool trait` 的完整性：所有方法覆盖测试 |

#### 🟢 绿 — 实现
- `src/tool.rs`：
  - `McpToolDefinition` 结构体
  - `McpToolAdapter` 结构体（name, original_name, description, parameters, timeout_secs, session: Arc<Mutex<McpSession>>）
  - 实现 `Tool` trait
  - `execute()` 中使用 `tokio::time::timeout` 控制超时

---

## 步骤 5：McpManager

### 5a：McpManager 基础

#### 🔴 红 — 测试
| # | 测试用例 |
|---|---------|
| 1 | `McpManager::new(configs)` 创建管理器（无服务器时 sessions 为空） |
| 2 | `start_all(on_ready)` spawn 后台 task 连接各服务器 |
| 3 | `shutdown_all()` 关闭所有 session |
| 4 | `shutdown(name)` 关闭指定 session |
| 5 | `start_all` 中服务器连接成功后调用 `on_ready` 回调 |
| 6 | 连接失败的服务器不会导致 panic，仅 log warning |

#### 🟢 绿 — 实现
- `src/lib.rs`：
  - `McpManager` 结构体（`sessions: Mutex<HashMap<String, Arc<Mutex<McpSession>>>>`, configs）
  - `new(configs)` 
  - `start_all(on_ready)` — spawn background tasks
  - `shutdown_all()` — iterate and call shutdown
  - `shutdown(name)` — remove from map + shutdown

### 5b：McpManager 重连 + restart

#### 🔴 红 — 测试
| # | 测试用例 |
|---|---------|
| 1 | 服务器断开后自动重连（模拟进程退出 → reconnect） |
| 2 | `restart(name)` 移除旧 session + 重新连接 + 调用 on_ready |
| 3 | 重连失败（超过 3 次）→ 不再重试，log error |
| 4 | 重连成功后通过 `ToolRegistry::update()` 注册新工具 |

#### 🟢 绿 — 实现
- `restart(name)` — 移除旧 session + 重建连接 + 重新发现工具
- 进程退出检测 + 自动重连逻辑
- 通过 spawn 的 task 监控 `child.wait()` 触发重连

---

## 步骤 6：visp-daemon 集成

### 6a：Config 扩展

#### 🔴 红 — 测试
| # | 测试用例 |
|---|---------|
| 1 | `DaemonConfig` 包含 `[mcp]` section，反序列化正确 |
| 2 | 无 `[mcp]` section 时默认为空服务器列表（向后兼容） |

#### 🟢 绿 — 实现
- `visp-daemon/src/config.rs`：
  - `DaemonConfig` 增加 `mcp: McpConfig` 字段（`#[serde(default)]`）

### 6b：MCP 初始化 + Service 集成

#### 🔴 红 — 测试
| # | 测试用例 |
|---|---------|
| 1 | daemon 启动时初始化 McpManager（后台连接，不阻塞） |
| 2 | `CoderDaemonService` 持有 `mcp_manager` 字段 |
| 3 | shutdown 时调用 `mcp_manager.shutdown_all()` |
| 4 | 内置工具注册完成后调用 `seal_core_tools()`，MCP 工具跳过冲突 |

#### 🟢 绿 — 实现
- `visp-daemon/src/main.rs`：
  - 新增 MCP Manager 初始化步骤（在工具注册+`seal_core_tools()` 之后，Rule Engine 之前）
  - 内置工具注册完成后调用 `tool_registry.seal_core_tools()`
  - 注入到 `CoderDaemonService::new()`
- `visp-daemon/src/service.rs`：
  - `CoderDaemonService` 新增 `mcp_manager: Arc<McpManager>` 字段
  - 修改 `new()` 接收 McpManager 参数
  - shutdown 时调用 `mcp_manager.shutdown_all().await`

---

## Wave 并行策略

### Wave 1：基础 + 脚手架（3 个并行任务）
```
任务 A: 1a ToolRegistry 重构
任务 B: 2a visp-mcp crate + config + error
```
*Wave 1 无依赖，可完全并行*

### Wave 2：传输 + 连接（2 个并行任务）
```
任务 A: 3a Transport 工厂
任务 B: 3b McpSession 连接
```
*依赖: Wave 1（visp-mcp crate 存在）*

### Wave 3：完整交互
```
任务: 3c McpSession 工具调用 + 关闭
```
*依赖: Wave 2（transport + 基础连接）*

### Wave 4：工具适配器
```
任务: 4a McpToolAdapter
```
*依赖: Wave 1（Tool trait, ToolRegistry）+ Wave 3（McpSession）*

### Wave 5：管理器
```
任务: 5a McpManager 基础 + 5b 重连
```
*依赖: Wave 4（McpToolAdapter）*

### Wave 6：集成
```
任务 A: 6a Config 扩展
任务 B: 6b main.rs + service.rs 集成
```
*依赖: Wave 5（McpManager）+ Wave 1a（ToolRegistry）*

---

## 依赖关系总览

```
Wave 1A (ToolRegistry) ─────────────────────────────┐
                                                     │
Wave 1B (visp-mcp crate) ─→ Wave 2 (transport+session) ─→ Wave 3 (call+shutdown)
                                                           │
                                                           ├→ Wave 4 (ToolAdapter)
                                                           │       │
                                                           │       └→ Wave 5 (Manager)
                                                           │               │
                                                           │               └→ Wave 6 (Integration)
```

## 测试覆盖汇总

| Wave | 并行数 | 模块 | 步骤 | 测试用例数 |
|------|--------|------|------|-----------|
| 1 | 2 | visp-core | 1a | 8 |
| 1 | 2 | visp-mcp | 2a | 5 |
| 2 | 2 | visp-mcp | 3a | 4 |
| 2 | 2 | visp-mcp | 3b | 5 |
| 3 | 1 | visp-mcp | 3c | 4 |
| 4 | 1 | visp-mcp | 4a | 10 |
| 5 | 1 | visp-mcp | 5a | 6 |
| 5 | 1 | visp-mcp | 5b | 4 |
| 6 | 2 | visp-daemon | 6a | 2 |
| 6 | 2 | visp-daemon | 6b | 4 |
| **总计** | | | | **52** |

## 备注

1. **rmcp feature flags**：需要 `client` + `transport-child-process` + `transport-streamable-http-client-reqwest`
2. **模块定位**：`visp-mcp` 走依赖 visp-core 的 `Tool` trait 和 `ToolResult`，不依赖 ToolRegistry 自身（回调注入）
3. **Mock 测试**：通过实现 `rmcp::transport::Transport` 的 MockTransport 进行单元测试，不依赖真实 MCP 服务器
4. **集成测试**：需要 Node.js 环境（`npx @anthropic-ai/mcp-playwright`）用于端到端验证
