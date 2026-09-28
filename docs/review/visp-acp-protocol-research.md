# Agent Client Protocol (ACP) 调研报告

> 调研目标：梳理 Zed 编辑器用于与外部 coding agent 通信的 ACP 协议的定位、传输、消息格式、方法/通知清单、Rust SDK、真实实现案例、客户端配置方式与常见坑，为评估 visp（Rust coding agent）接入 ACP 提供**外部协议事实**依据。
>
> 本文只记录协议/SDK/客户端的外部事实，不包含 visp 的接入实现方案（另行撰写）。
>
> 调研日期：2026-09-20

---

## 目录

- [1. ACP 是什么](#1-acp-是什么)
- [2. Agent 端必须实现的方法](#2-agent-端必须实现的方法)
- [3. Agent 需发出 / 处理的通知与请求](#3-agent-需发出--处理的通知与请求)
- [4. Rust crate：`agent-client-protocol`](#4-rust-crateagent-client-protocol)
- [5. 现有开源 agent 的 ACP 实现方式](#5-现有开源-agent-的-acp-实现方式)
- [6. Zed 客户端如何配置外部 ACP agent](#6-zed-客户端如何配置外部-acp-agent)
- [7. 常见坑与注意事项](#7-常见坑与注意事项)
- [8. 关键来源汇总](#8-关键来源汇总)

---

## 1. ACP 是什么

### 1.1 定位

Agent Client Protocol（ACP）标准化「代码编辑器/IDE」与「coding agent」之间的通信，类比 LSP 之于语言服务器。它解决的核心问题是：每个编辑器都要为每个 agent 写定制集成、每个 agent 都要为不同编辑器实现专有 API 的碎片化问题。实现 ACP 的 agent 可与任何兼容 ACP 的编辑器互通，反之亦然。

- 官方站点：https://agentclientprotocol.com/
- 文档索引（llms.txt）：https://agentclientprotocol.com/llms.txt

**来源**：[Introduction](https://agentclientprotocol.com/get-started/introduction)、[Overview](https://agentclientprotocol.com/protocol/v1/overview)

### 1.2 传输方式

- **本地 agent**：作为编辑器子进程运行，通过 **JSON-RPC over stdio** 通信。client 启动 agent 子进程，agent 从 `stdin` 读消息、向 `stdout` 写消息。
- **远程 agent**：可托管在云端或独立基础设施，通过 HTTP / WebSocket 通信（`Streamable HTTP` 仍是 draft 提案）。
- **stdio 细节**：
  - 消息以换行符 `\n` 分隔，**MUST NOT** 包含内嵌换行
  - 必须 UTF-8 编码
  - agent **MAY** 向 `stderr` 写 UTF-8 日志（client 可捕获/转发/忽略）
  - agent **MUST NOT** 向 `stdout` 写任何非合法 ACP 消息的内容
  - client **MUST NOT** 向 agent 的 `stdin` 写任何非合法 ACP 消息的内容
- 协议是传输无关的，允许自定义传输，但必须保留 JSON-RPC 消息格式与生命周期要求。

**来源**：[Transports](https://agentclientprotocol.com/protocol/v1/transports)

### 1.3 消息格式

严格遵循 [JSON-RPC 2.0](https://www.jsonrpc.org/specification)，两类消息：

- **方法（Methods）**：请求-响应对，成功返回 `result`，失败返回 `error`（含 `code` 与 `message`）
- **通知（Notifications）**：单向，永不返回响应（成功或错误都不返回）

约定：属性键用 `camelCase`；判别字段（discriminator）的字符串值用 `snake_case`；JSON-RPC 信封字段（`jsonrpc`、`id`、`method`、`params`、`result`、`error`）遵循 JSON-RPC 2.0。

**来源**：[Overview - Conventions](https://agentclientprotocol.com/protocol/v1/overview)

### 1.4 协议版本

`protocolVersion` 是单个整数，标识 **MAJOR** 版本，仅在破坏性变更时递增。

- **v1 = 稳定版**（`protocolVersion: 1`）
- **v2 = draft**（2026-09 发布草案）

版本协商：`initialize` 请求 MUST 包含 client 支持的最新版本；agent 支持则回相同版本，否则回自己支持的最新版本；client 不支持则关闭连接并告知用户。

⚠️ 注意区分：「crate 版本号」与「协议版本号」是两回事。Rust crate `agent-client-protocol` 当前是 **2.2.0**（SDK 自身版本），协议版本是独立整数 1/2。

**来源**：[Initialization - Protocol version](https://agentclientprotocol.com/protocol/v1/initialization)、[ACP v2 is available in Draft](https://agentclientprotocol.com/announcements/acp-v2-draft)

### 1.5 设计原则

- 复用 MCP 的 JSON 表示：`ContentBlock` 与 MCP 完全兼容，便于 agent 无转换地转发 MCP 工具输出。
- 用户可读文本默认格式为 Markdown。
- 所有文件路径 **MUST 为绝对路径**；行号 **1-based**。
- 扩展机制：`_meta` 字段自定义数据；下划线 `_` 前缀自定义方法；初始化时声明自定义 capabilities。

**来源**：[Overview](https://agentclientprotocol.com/protocol/v1/overview)、[Content](https://agentclientprotocol.com/protocol/v1/content)

---

## 2. Agent 端必须实现的方法

官方按「Baseline（必须）」与「Optional（可选，需 capability）」划分。

### 2.1 Baseline 方法（MUST）

| 方法 | 语义 |
|---|---|
| **`initialize`** | 协商协议版本、交换 capabilities、交换 `agentInfo`/`clientInfo`（name/title/version）。任何 session 之前必须先调用。 |
| **`authenticate`** | 认证（仅当 agent 需要认证时）。可用认证方式见下文 2.4。 |
| **`session/new`** | 创建新会话。入参含 `cwd`（绝对路径）与 `mcpServers[]`，返回唯一 `sessionId`。 |
| **`session/prompt`** | 发送用户消息，返回 `stopReason`。整个 turn 的核心。 |

**来源**：[Overview - Agent Baseline Methods](https://agentclientprotocol.com/protocol/v1/overview#agent)

### 2.2 Optional 方法（需对应 capability）

| 方法 | capability 门槛 | 说明 |
|---|---|---|
| **`session/load`** | `loadSession: true` | 加载历史会话并**回放**全部历史消息（以 `session/update` 通知流式重放，回放完才响应） |
| **`session/resume`** | `sessionCapabilities.resume` | 恢复会话但**不回放**历史 |
| **`session/close`** | `sessionCapabilities.close` | 取消进行中工作（等价于 `session/cancel`）并释放会话资源 |
| **`session/delete`** | `sessionCapabilities.delete` | 从 `session/list` 移除会话 |
| **`session/list`** | `sessionCapabilities.list` | 枚举历史会话 |
| **`session/set_mode`** | — | 切换 agent 运行模式 |
| **`session/set_config_option`** | — | 设置会话配置选项 |
| **`logout`** | `agentCapabilities.auth.logout` | 退出当前认证状态 |

**来源**：[Overview - Agent Optional Methods](https://agentclientprotocol.com/protocol/v1/overview#agent)、[Session Setup](https://agentclientprotocol.com/protocol/v1/session-setup)

### 2.3 Agent 接收的通知（无响应）

- **`session/cancel`**：取消当前 prompt turn 的所有进行中操作。
- **`$/cancel_request`**：协议级请求取消（按 JSON-RPC 请求 id 取消，错误码 `-32800`）。

**关键约束**：所有 Agent **MUST** 支持 `session/new`、`session/prompt`、`session/cancel`、`session/update` 四个基础能力。

**来源**：[Initialization - Session Capabilities](https://agentclientprotocol.com/protocol/v1/initialization#session-capabilities)、[Cancellation](https://agentclientprotocol.com/protocol/v1/cancellation)

### 2.4 认证（authenticate）

- client 在初始化后、需要时调用 `authenticate`。
- `authMethods` 在 initialize 响应中声明，类型包括：
  - `AuthMethodAgent`：agent 自己通过 `authenticate` 处理认证
  - `AuthMethodTerminal`：终端方式认证（需 client 声明 `clientCapabilities.auth.terminal: true`，表示 client 能在交互式终端复现 agent 的调用）
- 认证后可用 `logout`（需 `agentCapabilities.auth.logout` capability）结束认证状态。

**来源**：[Authentication](https://agentclientprotocol.com/protocol/v1/authentication)、[Initialization - Terminal Authentication](https://agentclientprotocol.com/protocol/v1/initialization)

### 2.5 initialize 报文示例

```json
// Client → Agent
{
  "jsonrpc": "2.0", "id": 0, "method": "initialize",
  "params": {
    "protocolVersion": 1,
    "clientCapabilities": {
      "fs": { "readTextFile": true, "writeTextFile": true },
      "terminal": true
    },
    "clientInfo": { "name": "zed", "title": "Zed", "version": "0.180.0" }
  }
}
// Agent → Client
{
  "jsonrpc": "2.0", "id": 0,
  "result": {
    "protocolVersion": 1,
    "agentCapabilities": {
      "loadSession": true,
      "promptCapabilities": { "image": true, "embeddedContext": true },
      "mcpCapabilities": { "http": true }
    },
    "agentInfo": { "name": "my-agent", "title": "My Agent", "version": "1.0.0" },
    "authMethods": []
  }
}
```

**来源**：[Initialization](https://agentclientprotocol.com/protocol/v1/initialization)

---

## 3. Agent 需发出 / 处理的通知与请求

### 3.1 `session/update` 通知 —— 全部 16 种变体

`session/update` 是 agent 向 client 推送一切进度的**唯一通道**。结构：

```json
{
  "jsonrpc": "2.0", "method": "session/update",
  "params": { "sessionId": "...", "update": { "sessionUpdate": "agent_message_chunk", "..." } }
}
```

从 Rust schema 枚举（[`schema::v1::SessionUpdate`](https://docs.rs/agent-client-protocol/latest/agent_client_protocol/schema/v1/enum.SessionUpdate.html)）确认的 16 个变体（`sessionUpdate` 的 snake_case 值）：

| 变体（wire 值） | 含义 | 稳定性 |
|---|---|---|
| `user_message_chunk` | 用户消息流式分块（session/load 重放时） | 稳定 |
| `agent_message_chunk` | agent 回复文本流式分块 | 稳定 |
| `agent_thought_chunk` | agent 内部推理（reasoning）流式分块 | 稳定 |
| `tool_call` | 新的工具调用开始 | 稳定 |
| `tool_call_update` | 工具调用状态/结果更新 | 稳定 |
| `plan` | agent 的执行计划 | 稳定 |
| `available_commands_update` | 斜杠命令（slash command）就绪/变更 | 稳定 |
| `current_mode_update` | 会话模式切换 | 稳定 |
| `config_option_update` | 会话配置选项更新 | 稳定 |
| `session_info_update` | 会话元数据（标题/时间戳/自定义）更新 | 稳定 |
| `usage_update` | 上下文占用 + 累计成本 | 稳定 |
| `plan_update` | 计划内容更新 | unstable（`unstable_plan_operations`） |
| `plan_removed` | 计划移除 | unstable |
| `notice` | 用户提示（不进会话历史） | unstable |
| `compaction_update` | 上下文压缩创建/更新 | unstable（`unstable_session_compaction`） |
| `compaction_summary_chunk` | 压缩摘要分块 | unstable |

**`agent_message_chunk` 报文示例**（含 messageId 关联语义）：

```json
{
  "sessionUpdate": "agent_message_chunk",
  "messageId": "msg_agent_c42b9",
  "content": { "type": "text", "text": "I'll analyze your code..." }
}
```

**`plan` 报文示例**：

```json
{
  "sessionUpdate": "plan",
  "entries": [
    { "content": "Check for syntax errors", "priority": "high", "status": "pending" },
    { "content": "Identify potential type issues", "priority": "medium", "status": "pending" }
  ]
}
```

**`usage_update` 报文示例**：

```json
{
  "sessionUpdate": "usage_update",
  "used": 53000, "size": 200000,
  "cost": { "amount": 0.045, "currency": "USD" }
}
```

**`tool_call` / `tool_call_update` 报文示例**：

```json
// 创建
{
  "sessionUpdate": "tool_call", "toolCallId": "call_001",
  "name": "read_file", "title": "Reading configuration file",
  "kind": "read", "status": "pending"
}
// 更新为完成
{
  "sessionUpdate": "tool_call_update", "toolCallId": "call_001",
  "status": "completed",
  "content": [ { "type": "content", "content": { "type": "text", "text": "..." } } ]
}
```

**来源**：[Prompt Turn](https://agentclientprotocol.com/protocol/v1/prompt-turn)、[Tool Calls](https://agentclientprotocol.com/protocol/v1/tool-calls)、[Content](https://agentclientprotocol.com/protocol/v1/content)、[Agent Plan](https://agentclientprotocol.com/protocol/v1/agent-plan)、[Slash Commands](https://agentclientprotocol.com/protocol/v1/slash-commands)

### 3.2 `session/request_permission`（工具审批）

**Agent → Client 请求**（执行工具前请求授权）：

```json
{
  "jsonrpc": "2.0", "id": 5, "method": "session/request_permission",
  "params": {
    "sessionId": "sess_abc123def456",
    "toolCall": { "toolCallId": "call_001" },
    "options": [
      { "optionId": "allow-once",   "name": "Allow once",  "kind": "allow_once" },
      { "optionId": "allow-always", "name": "Always allow","kind": "allow_always" },
      { "optionId": "reject-once",  "name": "Reject",      "kind": "reject_once" }
    ]
  }
}
```

**Client 响应**（二选一）：

```json
// 用户选择了某个选项
{ "jsonrpc": "2.0", "id": 5, "result": { "outcome": { "outcome": "selected", "optionId": "allow-once" } } }
// 被取消（turn 取消时 MUST 返回）
{ "jsonrpc": "2.0", "id": 5, "result": { "outcome": { "outcome": "cancelled" } } }
```

- `PermissionOption` 字段：`optionId`（唯一标识）、`name`（展示名）、`kind`。
- `PermissionOptionKind`：`allow_once` / `allow_always` / `reject_once` / `reject_always`。
- Client 可依据用户设置**自动放行/拒绝**权限请求。

**来源**：[Tool Calls - Requesting Permission](https://agentclientprotocol.com/protocol/v1/tool-calls#requesting-permission)

### 3.3 客户端提供的文件系统方法（agent 主动调用）

调用前必须检查 `clientCapabilities.fs.readTextFile` / `writeTextFile`，未声明则 MUST NOT 调用。

- **`fs/read_text_file`**：入参 `{ sessionId, path, line?, limit? }` → 返回 `{ content }`。可读到编辑器**未保存**的改动。
- **`fs/write_text_file`**：入参 `{ sessionId, path, content }` → 返回 `{}`。文件不存在时 client **MUST** 创建。

```json
// read
{ "jsonrpc": "2.0", "id": 3, "method": "fs/read_text_file",
  "params": { "sessionId": "sess_abc123def456", "path": "/home/user/project/src/main.py", "line": 10, "limit": 50 } }
// → { "jsonrpc": "2.0", "id": 3, "result": { "content": "def hello_world():\n ..." } }
// write
{ "jsonrpc": "2.0", "id": 4, "method": "fs/write_text_file",
  "params": { "sessionId": "sess_abc123def456", "path": "/home/user/project/config.json", "content": "{\n  \"debug\": true\n}" } }
// → { "jsonrpc": "2.0", "id": 4, "result": {} }
```

**来源**：[File System](https://agentclientprotocol.com/protocol/v1/file-system)

### 3.4 `terminal/*` 方法（agent 把终端委托给 client）

需 `clientCapabilities.terminal: true`。共 5 个方法：

| 方法 | 作用 |
|---|---|
| `terminal/create` | 创建终端并执行命令，返回 `terminalId` |
| `terminal/output` | 获取当前输出 + 退出状态 |
| `terminal/wait_for_exit` | 等待命令退出 |
| `terminal/kill` | 杀进程但不释放终端 |
| `terminal/release` | 释放终端资源 |

工具调用的 `content` 里可嵌入 `{ "type": "terminal", "terminalId": "term_xyz789" }`，client 会实时展示输出，终端释放后仍持续展示。

**来源**：[Terminals](https://agentclientprotocol.com/protocol/v1/terminals)

### 3.5 其他 client 提供的方法

- **`elicitation/create`**：agent 请求结构化用户输入（form 表单 或 URL 两种模式），需对应 elicitation capability（client 通过 `elicitation.form` / `elicitation.url` 声明）。
- **`elicitation/complete`**（通知）：URL 型 elicitation 完成时 agent 发出。

**来源**：[Elicitation](https://agentclientprotocol.com/protocol/v1/elicitation)、[Overview - Client Optional Methods](https://agentclientprotocol.com/protocol/v1/overview#client)

---

## 4. Rust crate：`agent-client-protocol`

### 4.1 基本信息

| 项 | 值 |
|---|---|
| crates.io 名称 | `agent-client-protocol`（连字符），lib 名 `agent_client_protocol`（下划线） |
| 最新版本 | **2.2.0** |
| 官方仓库 | https://github.com/agentclientprotocol/rust-sdk |
| docs.rs | https://docs.rs/agent-client-protocol/latest/agent_client_protocol/ |
| cookbook | https://docs.rs/agent-client-protocol-cookbook |
| 协议支持 | 稳定 v1 + 可选 draft v2（`unstable_protocol_v2` feature） |

**来源**：[docs.rs 首页](https://docs.rs/agent-client-protocol/latest/agent_client_protocol/)、[README](https://github.com/agentclientprotocol/rust-sdk)

### 4.2 ⚠️ 重要 API 变更

官方 [libraries/rust.md](https://agentclientprotocol.com/libraries/rust.md) 页面仍写着「实现 `Agent` trait 或 `Client` trait」——**这是过时文档**。当前 2.2.0 已重构为 **role（角色）+ builder + handler（回调分发）** 架构：

- `Agent` / `Client` / `Proxy` / `Conductor` 现在是**单元结构体（role 标记）**，不再是 trait。
- 入口：`Agent.builder()`、`Client.builder()`、`Proxy.builder()`。
- 通过 `.on_receive_request(...)` / `.on_receive_notification(...)` / `.on_receive_dispatch(...)` 注册 handler。
- 通过 `.connect_to(Stdio::new())` 接入 stdio，或 `.connect_with(transport, async |cx| {...})`。
- 旧教程里的 trait 方法（`initialize()`、`new_session()`、`prompt()` 等 async trait 方法）已不存在，网上旧文章会误导。

**来源**：[README - Documentation](https://github.com/agentclientprotocol/rust-sdk)、[Agent struct](https://docs.rs/agent-client-protocol/latest/agent_client_protocol/role/acp/struct.Agent.html)

### 4.3 最小 agent 示例（官方 `simple_agent.rs`）

```rust
use agent_client_protocol::schema::v1::{AgentCapabilities, InitializeRequest, InitializeResponse};
use agent_client_protocol::{Agent, Result, Stdio};

#[tokio::main]
async fn main() -> Result<()> {
    Agent
        .builder()
        .name("my-agent") // 仅用于调试
        .on_receive_request(
            async move |initialize: InitializeRequest, responder, _connection| {
                responder.respond(
                    InitializeResponse::new(initialize.protocol_version)
                        .agent_capabilities(AgentCapabilities::new()),
                )
            },
            agent_client_protocol::on_receive_request!(),
        )
        .connect_to(Stdio::new())
        .await
}
```

**来源**：https://github.com/agentclientprotocol/rust-sdk/blob/main/src/agent-client-protocol/examples/simple_agent.rs

### 4.4 完整 agent 流程（cookbook `building_an_agent`）

```rust
use agent_client_protocol::{Agent, ConnectTo};
use agent_client_protocol::schema::v1::{
    InitializeRequest, InitializeResponse, AgentCapabilities,
    NewSessionRequest, NewSessionResponse, SessionId,
    PromptRequest, PromptResponse, StopReason,
    SessionNotification, SessionUpdate, ContentChunk,
    RequestPermissionRequest, ToolCallUpdate, ToolCallUpdateFields,
    PermissionOption, PermissionOptionKind, ToolKind, ToolCallStatus,
    RequestPermissionOutcome,
};

async fn run_agent(transport: impl ConnectTo<Agent>) -> Result<(), agent_client_protocol::Error> {
    Agent.builder()
        .name("my-agent")
        .on_receive_request(async |req: InitializeRequest, responder, _c| {
            responder.respond(InitializeResponse::new(req.protocol_version)
                .agent_capabilities(AgentCapabilities::new()))
        }, agent_client_protocol::on_receive_request!())
        .on_receive_request(async |req: NewSessionRequest, responder, _c| {
            responder.respond(NewSessionResponse::new(SessionId::new("session-1")))
        }, agent_client_protocol::on_receive_request!())
        .on_receive_request(async |req: PromptRequest, responder, connection| {
            // 流式推送文本
            connection.send_notification(SessionNotification::new(
                req.session_id.clone(),
                SessionUpdate::AgentMessageChunk(ContentChunk::new("Hello, ".into())),
            ))?;
            connection.send_notification(SessionNotification::new(
                req.session_id.clone(),
                SessionUpdate::AgentMessageChunk(ContentChunk::new("world!".into())),
            ))?;
            // 结束 turn
            responder.respond(PromptResponse::new(StopReason::EndTurn))
        }, agent_client_protocol::on_receive_request!())
        .connect_to(transport).await
}
```

**请求权限示例**（agent 侧）：

```rust
let response = connection.send_request(RequestPermissionRequest::new(
    session_id.clone(),
    ToolCallUpdate::new("dangerous-command",
        ToolCallUpdateFields::new()
            .title("Run rm -rf /")
            .kind(ToolKind::Execute)
            .status(ToolCallStatus::Pending)),
    vec![
        PermissionOption::new("allow", "Allow", PermissionOptionKind::AllowOnce),
        PermissionOption::new("deny",  "Deny",  PermissionOptionKind::RejectOnce),
    ],
)).block_task().await?;

match response.outcome {
    RequestPermissionOutcome::Selected(s) if s.option_id == "allow" => { /* 放行 */ }
    _ => { /* 拒绝或取消 */ }
}
```

**来源**：[cookbook building_an_agent](https://docs.rs/agent-client-protocol-cookbook/latest/agent_client_protocol_cookbook/building_an_agent/index.html)

### 4.5 官方示例清单（仓库内）

- 稳定 v1：`examples/simple_agent.rs`（agent）、`examples/yolo_one_shot_client.rs`（client）
- draft v2：`examples/simple_agent_v2.rs`、`examples/v2_one_shot_client.rs`

**来源**：[README - Integrations](https://github.com/agentclientprotocol/rust-sdk)

### 4.6 子 crate 生态

| crate | 用途 |
|---|---|
| `agent-client-protocol-http` | HTTP/SSE、WebSocket 传输 |
| `agent-client-protocol-rmcp` | 集成 [`rmcp`](https://docs.rs/rmcp) MCP SDK |
| `agent-client-protocol-conductor` | 编排 client↔proxy↔agent 的代理链（二进制 + 库） |
| `agent-client-protocol-cookbook` | 模式与示例（rustdoc 形式） |
| `agent-client-protocol-polyfill` | 兼容代理（如把 MCP-over-ACP 声明转 HTTP） |
| `agent-client-protocol-trace-viewer` | 交互式时序图查看器 |
| `agent-client-protocol-derive` | derive 宏 |

**来源**：[README - Crates](https://github.com/agentclientprotocol/rust-sdk)

### 4.7 关键类型速查（`schema::v1`）

| 类型 | docs.rs 链接 |
|---|---|
| `SessionId` | [struct.SessionId](https://docs.rs/agent-client-protocol/latest/agent_client_protocol/schema/v1/struct.SessionId.html) |
| `PromptRequest` / `PromptResponse` | schema::v1 |
| `SessionUpdate`（enum，16 变体，`#[non_exhaustive]`） | [enum.SessionUpdate](https://docs.rs/agent-client-protocol/latest/agent_client_protocol/schema/v1/enum.SessionUpdate.html) |
| `SessionNotification` | schema::v1 |
| `ContentBlock`（Text/Image/Audio/ResourceLink/Resource） | [enum.ContentBlock](https://docs.rs/agent-client-protocol/latest/agent_client_protocol/schema/v1/enum.ContentBlock.html) |
| `ContentChunk` | schema::v1 |
| `ToolCall` / `ToolCallUpdate` / `ToolCallUpdateFields` / `ToolCallLocation` / `ToolCallId` | schema::v1 |
| `RequestPermissionRequest/Response` / `PermissionOption` / `PermissionOptionId` | schema::v1 |
| `InitializeRequest/Response` / `AgentCapabilities` / `ClientCapabilities` / `PromptCapabilities` | schema::v1 |
| `StopReason`（`end_turn`/`max_tokens`/`max_turn_requests`/`refusal`/`cancelled`） | schema::v1 |

---

## 5. 现有开源 agent 的 ACP 实现方式

官方 Agents 列表：https://agentclientprotocol.com/get-started/agents

架构上分两类：**内建（native）** vs **适配层（adapter）**。

### 5.1 适配层型（agent 本体不支持 ACP，另写 stdio server 桥接）

**① Codex CLI** → [agentclientprotocol/codex-acp](https://github.com/agentclientprotocol/codex-acp)（npm `@agentclientprotocol/codex-acp`）
- 独立的 stdio ACP agent server，内部启动 **Codex App Server**，把 ACP 请求翻译成 Codex 操作，再把 Codex 事件映射回 client。
- 功能覆盖：ChatGPT/API key 认证、权限请求、MCP 工具调用、终端输出、plan、斜杠命令、子 agent 会话、后台终端任务。
- 安装：`npx -y @agentclientprotocol/codex-acp`。

**② Claude Agent** → [zed-industries/claude-agent-acp](https://github.com/zed-industries/claude-agent-acp)（npm `@agentclientprotocol/claude-agent-acp`）
- 用官方 **Claude Agent SDK** 实现一个 ACP agent，支持 @-mention、图片、工具调用（含权限）、follow、编辑审查、TODO、子 agent、终端、自定义斜杠命令、client MCP servers。

> 这两类适配器是「已有多端能力、后接 ACP」的典型范式。

### 5.2 内建型（agent 自身实现 ACP）

**③ Gemini CLI** → [google-gemini/gemini-cli](https://github.com/google-gemini/gemini-cli)：原生支持 ACP（无「via adapter」标注）。

**④ Goose**（Block，Rust 项目）→ [block/goose](https://github.com/block/goose) + [ACP 文档](https://block.github.io/goose/docs/guides/acp-clients)：原生 ACP，Rust 编写，直接复用 `agent-client-protocol` crate，与同为 Rust 的 agent 技术栈最贴近。

**⑤ OpenCode** → [sst/opencode](https://github.com/sst/opencode)：原生 ACP。

> 此外还有大量 agent 通过 adapter 接入（Cursor、Poolside、Pi、Kimi CLI、Qwen Code 等，见官方列表）。

**来源**：[Agents 列表](https://agentclientprotocol.com/get-started/agents)

---

## 6. Zed 客户端如何配置外部 ACP agent

Zed 文档：[External Agents](https://zed.dev/docs/ai/external-agents)

### 6.1 方式一：从 ACP Registry 安装（推荐）

命令 `zed: acp registry`，或在 Agent Settings → External Agents → Add Agent → Install from Registry。

### 6.2 方式二：自定义 agent（settings.json 的 `agent_servers`）

```json
{
  "agent_servers": {
    "my-agent": {
      "type": "custom",
      "command": "node",
      "args": ["~/projects/agent/index.js", "--acp"],
      "env": {}
    }
  }
}
```

Poolside 的实际配置示例（`pool acp`）：

```json
{
  "agent_servers": {
    "Poolside": {
      "command": "pool",
      "args": ["acp"],
      "type": "custom"
    }
  }
}
```

- 配置文件路径：`~/.config/zed/settings.json`（macOS/Linux）。
- Registry 安装的 agent 也可有 `agent_servers.<agent-id>` 下的 per-agent 设置。

### 6.3 方式三：已弃用的 Extension-provided agents

`Agent Server Extensions` 已弃用，全部迁移到 registry。

### 6.4 调试

- 命令 `dev: open acp logs` 查看 Zed 与外部 agent 之间的 ACP 消息。

### 6.5 边界（进程边界）

Zed 与外部 agent 是进程边界：模型/认证/订阅由 agent 自己管；Zed 配置的 MCP servers **可能**通过 ACP 转发给 agent；Zed Skills 不适用于外部 agent；工具权限由「Zed ACP/工具转发权限」与「agent 原生权限」共同决定。

**来源**：[External Agents](https://zed.dev/docs/ai/external-agents)

---

## 7. 常见坑与注意事项

### 7.1 传输层硬约束
- stdout **只准写合法 ACP 消息**（换行分隔、无内嵌换行、UTF-8）；日志一律走 stderr，否则 client 解析失败。
- 所有文件路径 **MUST 绝对路径**；行号 **1-based**。

### 7.2 权限审批映射
- agent 提供的 `options[].kind` 只有 4 种：`allow_once` / `allow_always` / `reject_once` / `reject_always`；`optionId` 必须稳定可识别。
- 响应 `outcome` 只能是 `selected`（带 optionId）或 `cancelled`。
- turn 被取消时，client 会对所有 pending 的 permission 请求回 `cancelled`，agent 必须能处理这种「非 selected」响应。

### 7.3 流式 chunk 的 id 关联
- `agent_message_chunk` / `user_message_chunk` / `agent_thought_chunk` 上的 `messageId` 是**可选的**，语义是：**相同 messageId = 同一条消息的不同分块，改变 messageId = 新消息**。不设 messageId 时 client 按顺序拼接。
- `session/load` 重放历史时必须用 `session/update` 通知流式回放（`user_message_chunk` + `agent_message_chunk`），全部回放完才回 `session/load` 的 response。

### 7.4 cancel 语义（最易错）
- `session/cancel` 是**通知**（无响应），agent 收到后应尽快中止所有 LLM 请求与工具调用。
- agent **MUST** 捕获底层 SDK/库抛出的「操作被中止」异常，转成 `session/prompt` 响应里的 `stopReason: "cancelled"`，**而不是**抛 JSON-RPC error——否则 client 会把取消显示成错误。
- 允许在取消后继续发 `session/update`，但**必须**在响应 `session/prompt` 之前完成。
- 独立的协议级 `$/cancel_request`（按请求 id 取消，错误码 `-32800`），用于取消 `terminal/create`、`session/request_permission` 等挂起请求。

### 7.5 tool_call 的 status / locations / name 字段
- `status` 生命周期：`pending` → `in_progress` → `completed` / `failed`。
- `locations`：`[{ "path": "/abs/path", "line": 42 }]`（line 可选，1-based），用于 client 的「follow-along」实时跟踪。
- `name`（程序化工具名）可选，首报后 **SHOULD NOT** 改，且 **v1 无法清除已报的 name**。
- `kind`：`read`/`edit`/`delete`/`move`/`search`/`execute`/`think`/`fetch`/`switch_mode`/`other`，影响 client 的图标与展示。

### 7.6 capabilities 协商（初始化最关键）
- 所有 capability **省略即视为不支持**，agent/client 必须据此裁剪行为。
- `promptCapabilities`：`image` / `audio` / `embeddedContext`，默认全 false。baseline 只有 `Text` 与 `ResourceLink` 是必须支持的。
- `fs.readTextFile` / `fs.writeTextFile` / `terminal` 不声明就不能调用对应方法。
- `loadSession`、`sessionCapabilities.{resume,close,delete,list,additionalDirectories}`、`mcpCapabilities.{http,sse}`、`agentCapabilities.auth.logout` 各自控制对应方法。
- MCP 传输：所有 agent **MUST** 支持 stdio，**SHOULD** 支持 HTTP；SSE 已被 MCP 规范弃用。

### 7.7 版本注意
- `protocolVersion` 协商：client 发最新版本，agent 支持就回相同版本，否则回自己支持的最新版本；client 不支持则关闭连接并告知用户。
- SDK 层面 v2 在 `unstable_protocol_v2` feature 之后；生产接入先用 v1 稳定版，v2 用 `Agent.protocol_router()` 同时暴露 v1/v2。

### 7.8 文档陈旧陷阱
- agentclientprotocol.com 的 [libraries/rust.md](https://agentclientprotocol.com/libraries/rust.md) 仍在讲旧的 `Agent`/`Client` trait API，与 2.2.0 实际 API 不符；以 [README](https://github.com/agentclientprotocol/rust-sdk) 与 [docs.rs](https://docs.rs/agent-client-protocol) 为准。

---

## 8. 关键来源汇总

| 主题 | 链接 |
|---|---|
| 官方协议站点 | https://agentclientprotocol.com/ |
| 文档索引（llms.txt） | https://agentclientprotocol.com/llms.txt |
| 协议 v1 概览 | https://agentclientprotocol.com/protocol/v1/overview |
| 初始化 | https://agentclientprotocol.com/protocol/v1/initialization |
| 会话创建/加载/恢复/关闭 | https://agentclientprotocol.com/protocol/v1/session-setup |
| Prompt Turn | https://agentclientprotocol.com/protocol/v1/prompt-turn |
| 工具调用/权限 | https://agentclientprotocol.com/protocol/v1/tool-calls |
| 内容块 | https://agentclientprotocol.com/protocol/v1/content |
| 文件系统 | https://agentclientprotocol.com/protocol/v1/file-system |
| 终端 | https://agentclientprotocol.com/protocol/v1/terminals |
| 取消 | https://agentclientprotocol.com/protocol/v1/cancellation |
| 传输 | https://agentclientprotocol.com/protocol/v1/transports |
| Rust SDK 仓库 | https://github.com/agentclientprotocol/rust-sdk |
| Rust crate docs.rs | https://docs.rs/agent-client-protocol |
| cookbook | https://docs.rs/agent-client-protocol-cookbook |
| Zed 外部 agent 配置 | https://zed.dev/docs/ai/external-agents |
| 官方 agents 列表 | https://agentclientprotocol.com/get-started/agents |
| codex-acp 适配器 | https://github.com/agentclientprotocol/codex-acp |
| claude-agent-acp 适配器 | https://github.com/zed-industries/claude-agent-acp |
| SessionUpdate enum | https://docs.rs/agent-client-protocol/latest/agent_client_protocol/schema/v1/enum.SessionUpdate.html |
| ContentBlock enum | https://docs.rs/agent-client-protocol/latest/agent_client_protocol/schema/v1/enum.ContentBlock.html |
