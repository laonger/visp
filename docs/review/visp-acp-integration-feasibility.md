# visp 接入 ACP（Agent Client Protocol）可行性与方案调研

> 调研目标：评估如何让 visp 作为 ACP agent，在 Zed 等兼容 ACP 的编辑器中运行。
>
> 本文聚焦 **visp 侧接入分析**；外部协议事实见配套文档 [`visp-acp-protocol-research.md`](./visp-acp-protocol-research.md)。
>
> 调研日期：2026-09-20 · 分支：`acp`（origin `laonger/visp`）

---

## 0. 结论摘要（TL;DR）

1. **可行，且成本可控**。visp 的 daemon 已经是一个「前后端分离、事件流驱动、带会话/审批/取消语义」的后端，与 ACP 的 `session/*` 模型高度同构。接入主要是在**前端边界**增加一个 ACP 适配器，核心 agent 逻辑基本不需要改。
2. **推荐方案：新增独立 crate `visp-acp`**，作为一个 stdio 上的 ACP agent server，通过 gRPC 连接 visp-daemon（必要时自行拉起 daemon）。这与官方推荐的 adapter 范式一致（codex-acp、claude-agent-acp），也规避了 daemon 自身的 stdout 纪律问题。
3. **最小可用范围（M1）**：`initialize` + `session/new` + `session/prompt` + `session/update`（`agent_message_chunk` / `tool_call` / `tool_call_update`）+ `session/cancel` + `session/request_permission`。完成后即可在 Zed 中作为一个可用的 external agent。
4. **关键架构约束**：daemon 的 `Chat` 双向流是**进程内单例**（`orchestrator_grpc_rx.take()`），同一 daemon 同一时刻只允许一条 Chat 流。因此 ACP 适配器必须**用一条 Chat 流复用/多路传输所有 ACP 会话**（以 `session_id` 区分），或让每个 `visp-acp` 进程独占自己的 daemon 实例。**后者更简单、更稳，建议采用**。
5. **不做的事（M1 明确排除）**：`fs/read_text_file`、`fs/write_text_file`、`terminal/*`（visp 自行读写磁盘、自行执行 bash），`session/load` 历史回放（M2 再做），`plan`、`available_commands_update` 等增强项。

---

## 1. 目标与范围

### 1.1 目标
让 Zed（以及 avante / codecompanion 等 ACP client）能够把 visp 配置为 **external agent**：在编辑器内发起对话、看到流式回复与工具调用、对危险工具进行审批、中断正在执行的 turn。

### 1.2 非目标
- 不做远程/HTTP 传输的 ACP（先用本地 stdio）。
- 不替换 visp 自有 TUI（`visp-tui` 继续存在）。
- 不把 ACP 引入 daemon 内部作为一等协议（见 §4 方案 B 的否决理由）。

---

## 2. visp 现状（与接入相关的事实）

### 2.1 进程与通信
```
visp (launcher) ──spawn──> visp-daemon ──gRPC(tonic, TCP)──> visp-tui (TUI, ratatui)
```
- daemon 是唯一持有 AI/工具/资源能力的进程；前端通过 gRPC `CoderDaemon` 服务交互。
- gRPC 走 TCP（默认 `[::1]:50051`，launcher 会自增寻找可用端口，经 `VISP_LISTEN_ADDR` 注入 daemon）。

### 2.2 前端契约（`crates/visp-proto/proto/visp.proto`）
- `Chat(stream ClientMessage) returns (stream ServerMessage)`：核心双向流。
- **客户端 → daemon**（`ClientMessage.payload`）：
  - `UserInput { text, session_id, request_id }`
  - `ConfigUpdate`（切模型等）
  - `UserResponse { query_id, selected_index, text }`（对 `UserQuery` 的答复）
  - `Cancel { session_id }`
  - `Ack`、`JoinSession`
- **daemon → 客户端**（`ServerMessage.payload`）：
  - `TextDelta { delta, session_id, agent_name }`
  - `ToolCall { call_id, tool_name, arguments, agent_name }`
  - `ToolResult { call_id, content, is_error, tool_name, agent_name }`
  - `ThinkingBlock { thinking, signature }`
  - `UserQuery { query_id, message, options, allow_other }` ← **审批与提问的统一出口**
  - `StatusUpdate`、`UsageInfo`、`UsageDelta`、`ImageBlock`、`ImageError`、`Error`、`Done`
  - `UserMessage`（仅历史回放）
- 会话管理 RPC：`CreateSession`、`ListSessions`、`GetSession`、`DeleteSession`、`ReadFile`、`SearchSymbols`、`GetSymbolDetails`、`HealthCheck`、`Shutdown`。

### 2.3 审批语义（对 ACP 映射至关重要）
- 工具审批与「LLM 向用户提问」共用 `UserQuery`：
  - `options` **为空** ⇒ 工具审批模式（CLI 渲染 Approve / Deny / Always Allow）。
  - `options` **非空** ⇒ LLM 提问，选项列表 + 可选 `Other` 自定义输入。
- `UserResponse.selected_index`：正常为 0-based；`-1` 表示自定义输入，`text` 承载内容。
- 审批在 **agent loop 内部**发起（`AgentEvent::UserQuery` 携带 `respond` oneshot），daemon 把它转发给前端并等待答复。⇒ **ACP 适配器无需改动审批判定逻辑**，只要在边界把 `UserQuery` 转成 `session/request_permission` 再把结果回填即可。

### 2.4 取消语义
- `Cancel { session_id }` 经 Chat 流传入 orchestrator，触发 `cancel_token`；agent loop 收到后中止当前生成。
- 已有测试覆盖 `cancel_agent` / idle 会话 no-op。

### 2.5 单 Chat 流约束（重点）
`service.rs::chat()` 通过 `self.orchestrator_grpc_rx.lock().unwrap().take()` 取走全局 orchestrator 接收端：
- 第二条 `Chat` 调用会返回 `Status::internal("orchestrator receiver already taken")`。
- ⇒ 一个 daemon 进程**只能服务一条 Chat 流**；多会话是在这条流上用 `session_id` 多路复用的（CLI 即如此，`JoinSession` 用来切换/回放）。
- **接入含义**：ACP 适配器要么(a) 与 CLI 同样只开一条 Chat 流、靠 `session_id` 路由所有 ACP 会话；要么(b) 让每个 `visp-acp` 进程独占一个 daemon 实例。**推荐 (b)**：`visp-acp` 自己 spawn daemon（复用 launcher 的端口探测 + health check 逻辑），从而与任何正在运行的 CLI daemon 互不干扰。

### 2.6 会话持久化
- 会话存 SQLite（`visp-db`），支持重启后恢复、前缀匹配。
- `JoinSession` 会回放历史（`UserMessage` + `StatusUpdate.user_inputs` + 子 agent 帧），这是 M2 实现 ACP `session/load` 的现成基础。

### 2.7 子 agent 与事件归属
- 所有事件都带 `agent_name`（空 = 主 agent，非空 = sub-agent 名，如 `explorer`/`fixer`）。
- visp 通过 `TaskTool` 派生 sub-agent，各自有独立 session（`parent_id`）。
- ACP 没有 sub-agent 概念，需要设计降级表达（见 §6.4）。

---

## 3. ACP 侧要求（速览，详见配套协议文档）

- **传输**：本地 agent 为编辑器子进程，JSON-RPC 2.0 over stdio；**stdout 只准输出合法 ACP 消息**，日志走 stderr。
- **Baseline 必须**：`initialize`、`session/new`、`session/prompt`、`session/cancel`（通知）、`session/update`（通知）。
- **可选**：`authenticate`、`session/load`/`session/resume`/`session/close`/`session/delete`/`session/list`、`session/set_mode`、`session/set_config_option`、`logout`。
- **agent → client 请求**：`session/request_permission`、`fs/read_text_file`、`fs/write_text_file`、`terminal/*`、`elicitation/create`。
- **SDK**：官方 Rust crate `agent-client-protocol`（当前 2.2.0），API 为 **role + builder + handler**（`Agent.builder().on_receive_request(...).connect_to(Stdio::new())`），非旧的 trait 写法。
- **要点**：路径绝对化、行号 1-based、capabilities 省略即不支持、`SessionUpdate` 为 `#[non_exhaustive]`。

---

## 4. 候选接入方案对比

### 方案 A：独立 crate `visp-acp`（stdio ACP agent + gRPC 客户端）—— **推荐**
- 新二进制 `visp-acp`：实现 ACP agent 角色；内部通过 `CoderDaemonClient` 连接（或 spawn）daemon。
- ACP 请求 ↔ gRPC 调用/事件的双向翻译层。
- **优点**：完全不侵入 daemon/core；stdout 纪律天然满足（该进程只做 ACP）；可独立迭代、独立测试；与 codex-acp / claude-agent-acp 的成熟范式一致；单个 Zed 窗口 = 一个 `visp-acp` 进程 = 一个 daemon，无并发冲突。
- **缺点**：多一层翻译；需要维护 ACP crate 依赖。

### 方案 B：在 daemon 内建 ACP stdio 模式
- daemon 增加 `--acp` 启动模式，直接说 ACP。
- **缺点**：daemon 是常驻/多客户端服务，stdout 被 ACP 独占与日志输出冲突；单 Chat 流约束会让「daemon 服务多前端」彻底失效；把协议耦合进核心，违背现有分层。**否决**。

### 方案 C：让 `visp-tui` 兼顾 ACP
- **缺点**：CLI 是 ratatui TUI，与无头 stdio 协议模型冲突，等于把 adapter 塞进错误的位置。**否决**。

**结论：采用方案 A。**

---

## 5. 推荐架构

```
Zed ──ACP(JSON-RPC over stdio)──> visp-acp ──gRPC(Chat bidi stream)──> visp-daemon
                                     │                                   │
                                     └── spawn / health check（复用 launcher 逻辑）
```

- `visp-acp` 启动流程：解析参数（`--project`/`cwd`、`--addr` 可选）→ 若无可用 daemon 则复用 launcher 的端口探测 + spawn + health check → 建立 gRPC 连接 → 以 ACP agent 身份接入 stdio。
- 生命周期：`visp-acp` 退出（Zed 关闭）时向 daemon 发 `Shutdown`（若由它拉起），与现有 launcher 的清理流程一致。
- 一条 Chat 流，按 ACP `sessionId` ↔ visp `session_id` 映射路由。

---

## 6. 关键映射设计

### 6.1 生命周期 / 会话

| ACP（agent 侧） | visp 侧动作 |
|---|---|
| `initialize` | 不需 gRPC；返回 `protocolVersion`、`agentCapabilities`（先只声明基础能力）、`agentInfo`（name=visp）、`authMethods: []`（visp 用 daemon.toml / 环境变量里的 API key，无需交互认证） |
| `session/new { cwd, mcpServers[] }` | `CreateSession(project_path = cwd)`；返回的 visp session id 直接作为 ACP `sessionId`（或建映射表）。**`mcpServers[]` 暂忽略**（daemon 的 MCP 在启动时静态加载，见 §9 待确认） |
| `session/prompt { sessionId, prompt }` | 在该会话上发送 `UserInput { text, session_id }`；随后把事件流翻译为 `session/update` 通知；收到 `Done` 后响应 `PromptResponse{ stopReason: end_turn }` |
| `session/cancel { sessionId }`（通知） | 发送 `Cancel { session_id }`；保证正在处理的 `session/prompt` 以 `stopReason: cancelled` 响应（**不要抛 JSON-RPC error**） |
| `session/load`（M2） | `GetSession` + 复用 `JoinSession` 的历史回放，把 `UserMessage`/`StatusUpdate`/工具帧用 `session/update` 流式重放，全部回放完再响应 |
| `session/list` / `close` / `delete`（M3） | 对应 `ListSessions` / `Cancel`+释放 / `DeleteSession` |

### 6.2 事件流 → `session/update`

| visp `ServerMessage` | ACP `session/update` 变体 | 备注 |
|---|---|---|
| `TextDelta` | `agent_message_chunk` | 同一 assistant turn 用一个稳定 `messageId`；主/子 agent 分不同 `messageId` |
| `ThinkingBlock` | `agent_thought_chunk` | 推理过程 |
| `ToolCall` | `tool_call` | `toolCallId = call_id`；映射 `tool_name` → `kind`（read/edit/search/execute/fetch…）；`status: pending`；`title` 用人类可读描述；能定位文件时填 `locations`（绝对路径 + 1-based 行） |
| `ToolResult` | `tool_call_update` | `status: completed` / `failed`（按 `is_error`）；`content` 用 `ContentBlock::Text`（超长需截断或分块） |
| `UserQuery`（options 为空） | `session/request_permission` | 见 §6.3 |
| `UserQuery`（options 非空） | `elicitation/create`（M3）或消息降级（M1） | ACP 无「选项选择」原语；M1 可把选项渲染成文本消息并让用户自由回复，M3 用 elicitation form |
| `UsageInfo` / `UsageDelta` | `usage_update`（M3，可选） | 上下文占用 + 成本 |
| `StatusUpdate` | `agent_message_chunk` 或 `notice`（unstable） | 建议 M1 折成文本片段前缀，避免依赖 unstable 变体 |
| `Error` | 视情况：turn 内错误 → 文本 chunk + `stopReason: refusal`/`end_turn`；致命 → JSON-RPC error | 不要吞掉错误 |
| `ImageBlock` / `ImageError` | `agent_message_chunk` 内嵌 image content（M3） | 需处理 base64 / 资源链接 |
| `Done` | 触发 `session/prompt` 的 `PromptResponse` | |
| `UserMessage` | `user_message_chunk`（仅 `session/load` 回放） | |

### 6.3 审批映射（M1 必须做对）

visp `UserQuery{options: []}` ⇒ ACP `session/request_permission`：
- 选项固定映射（与 CLI 的 Approve/Deny/Always Allow 对齐）：
  - `allow_once`（允许）
  - `allow_always`（始终允许）
  - `reject_once`（拒绝）
- 响应对照回 `UserResponse`：
  - `outcome=selected` 且 `optionId=allow_once` → `selected_index=0`
  - `allow_always` → `selected_index=2`（或 visp 约定的 always 索引）
  - `reject_once` → `selected_index=1`
  - `outcome=cancelled` → 按拒绝处理，并配合 turn cancel
- **注意**：具体索引必须与 agent loop 中 `requires_approval_for` 产出的选项顺序严格核对（需读 `visp-core/src/agent_loop.rs` 的 `UserQuery` 构造点确认），不能臆测。
- turn 被取消时，所有 pending 的 permission 请求会返回 `cancelled`，适配器必须能处理「非 selected」结果。

### 6.4 子 agent 的降级表达（需设计决策）

ACP 无 sub-agent 原语。可选：
- **方案 1（推荐，M1）**：把 visp sub-agent 的文本/思考作为**带前缀的 `agent_message_chunk`**（如 `[explorer] ...`），工具调用照常以 `tool_call` 呈现（`toolCallId` 天然唯一）。
- **方案 2**：把 TaskTool 视为一个 `tool_call`，sub-agent 的输出作为该 tool_call 的流式 `content`。
- M1 用方案 1 即可保证信息不丢失；后续再优化观感。

---

## 7. 需要新增/改动的工作项（不含实现代码）

| # | 工作项 | 位置 | 说明 |
|---|---|---|---|
| 1 | 新增 crate `visp-acp`（binary） | `crates/visp-acp/` | ACP agent 主体；加入 workspace members |
| 2 | daemon 拉起 + 健康检查复用 | `visp-acp` | 复用 `crates/visp/src/main.rs` 的端口探测/spawn/health/shutdown 逻辑（建议抽公共函数，但在不必要时应保持复制而非过早抽象） |
| 3 | gRPC 会话层 | `visp-acp` | 单 Chat 流建立、`session_id` 路由、`UserInput`/`Cancel`/`UserResponse` 发送 |
| 4 | 事件翻译层 | `visp-acp` | `ServerMessage` → `SessionUpdate`（§6.2），含 messageId 生命周期管理 |
| 5 | 审批桥接 | `visp-acp` | `UserQuery` ↔ `session/request_permission`（§6.3） |
| 6 | capabilities 声明 | `visp-acp` | 诚实声明（M1 不声明 fs/terminal/loadSession） |
| 7 | 日志与 stdout 纪律 | `visp-acp` | 所有日志走 stderr；daemon 日志进文件 |
| 8 | 单元/集成测试 | `visp-acp` | 用 SDK 的测试 client（如 `yolo_one_shot_client`）或自建最小 ACP client 驱动，断言事件映射与 stopReason |
| 9 | Zed 配置说明 | `docs/` | `agent_servers` 自定义配置示例 + 调试命令 |

**daemon/core 侧改动：预期为零**（这是本方案的核心优势）。唯一需要确认的是 §2.5 的单流约束在多会话场景下的行为（见 §9）。

---

## 8. 分阶段路线

### M1 — 最小可用（Zed 里能对话、能审批、能取消）
- `initialize` / `session/new` / `session/prompt` / `session/cancel`
- `session/update`：`agent_message_chunk`、`agent_thought_chunk`、`tool_call`、`tool_call_update`
- `session/request_permission`
- 端到端验证：Zed 自定义 agent 手测 + 冒烟 client

### M2 — 会话能力
- `session/load`（复用 `JoinSession` 回放）、`session/list`（`ListSessions`）
- 图片内容（`ImageBlock` → image content block），声明 `promptCapabilities.image`
- `usage_update`、斜杠命令（`available_commands_update`）映射 visp 命令系统

### M3 — 进阶
- `fs/read_text_file`（读取编辑器未保存缓冲）——需要在 tool 层拦截 `read_file` 改走 client，属于核心改动，需单独评估
- `terminal/*`（把 bash 委托给 client 终端）——同上，改动大，收益存疑
- `elicitation/create`、`plan`、`session/set_config_option`（映射 visp 的模型/温度切换）

---

## 9. 风险与待确认问题

1. **单 Chat 流约束**（见 §2.5）：若同一 daemon 被多个前端使用，第二个 `Chat` 会失败。→ 决策：`visp-acp` 自 spawn daemon，规避共享。**需确认**：daemon 是否支持一个进程内多 Chat 是长期设计（若未来支持，适配器可简化）。
2. **审批索引语义**：`UserResponse.selected_index` 与 CLI 的 Approve/Deny/Always 顺序需从 `agent_loop.rs` 读实际代码确认，避免映射错位。
3. **ACP `mcpServers[]`**：Zed 可能把其配置的 MCP server 通过 `session/new` 下发；visp 的 MCP 是 daemon 启动时静态加载。M1 忽略并声明不支持，避免「看似支持实则无效」。
4. **`SessionUpdate` 为 `#[non_exhaustive]`**：match 必须留通配分支，协议演进时不会编译失败。
5. **SDK 版本与文档陈旧**：`agent-client-protocol` 2.2.0 的 API 是 builder/handler，官方站点仍有旧 trait 文档；实现前以 docs.rs 与仓库 README 为准，并锁定版本。
6. **stdout 被污染**：任何依赖（含 daemon 子进程继承的 fd）若向 stdout 写日志会破坏 ACP。→ `visp-acp` spawn daemon 时显式把 daemon 的 stdout/stderr 重定向到文件（launcher 已这么做）。
7. **超长工具输出**：`ToolResult` 可能很大，ACP `tool_call_update` 的 content 需截断/分块策略。
8. **一次 turn 内的多会话并发**：Zed 可能同时发起多个 `session/prompt`；需确认 visp orchestrator 对「不同 session 并发运行」的支持程度（同一 main session 并发显然不支持）。

---

## 10. 工作量与依赖

- **新增依赖**：`agent-client-protocol`（2.x，锁版本）、复用现有 `visp-proto` / `tonic` / `tokio` / `visp-config`。
- **工作量粗估**（不含 M3 的 fs/terminal 核心改动）：
  - M1：约 1 个 crate + 3 个主要模块（会话层 / 事件翻译 / 审批桥接）+ 测试，属于**中等**规模。
  - M2：在 M1 基础上增量。
  - M3 的 `fs/*`、`terminal/*`：需要改工具层，**单独评估**。
- **验证手段**：Zed 手测（`dev: open acp logs`）+ SDK 冒烟 client + 单元测试断言映射。

---

## 11. 待用户决策

1. 是否认可**方案 A（独立 `visp-acp` crate）**？
2. M1 范围是否按本文 §8 设定（不含 `session/load`、`fs/*`、`terminal/*`）？
3. 子 agent 表达采用 §6.4 方案 1（前缀文本）是否可接受？
4. daemon 生命周期：`visp-acp` **自 spawn daemon**（推荐）还是连接已存在的 daemon？

---

## 12. 参考

- 外部协议事实：[`visp-acp-protocol-research.md`](./visp-acp-protocol-research.md)
- visp 前端契约：`crates/visp-proto/proto/visp.proto`
- daemon 服务实现：`crates/visp-daemon/src/service.rs`（`chat()` 与 `agent_event_to_server_message()`）
- launcher 编排：`crates/visp/src/main.rs`
- CLI 客户端用法参考：`crates/visp-tui/src/client.rs`
