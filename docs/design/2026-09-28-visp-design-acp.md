# visp ACP 接入设计文档

> 目标：让 visp 作为 ACP（Agent Client Protocol）agent，在 Zed 等兼容 ACP 的编辑器/客户端中运行。
>
> 依据：[`docs/review/visp-acp-protocol-research.md`](../review/visp-acp-protocol-research.md)（外部协议事实）、[`docs/review/visp-acp-integration-feasibility.md`](../review/visp-acp-integration-feasibility.md)（可行性调研）。
>
> 状态：**已确认（2026-09-23 用户逐项拍板 §13，进入实施阶段）**。本文只描述架构、流程、职责与契约，不含实现代码。
>
> 日期：2026-09-21（2026-09-22 一轮：daemon 生命周期 TOCTOU 防御与直连退出策略；二轮：取消收尾例外、turn 终止过滤、子 agent `UserQuery` 桥接特例；三轮：归属主判据改为 `session_id`（主 agent `agent_name` 实为 `"default"` 非空）、审批取消按归属区分收尾；2026-09-23：§13 十项逐项确认，进入实施） · 分支：`acp`

---

## 1. 背景与目标

### 1.1 背景
- visp 采用「daemon + gRPC + 前端」的前后端分离架构：daemon 持有一切 AI/工具/资源能力，CLI 是默认 TUI 前端。
- ACP 是编辑器与 coding agent 之间的标准协议（类比 LSP），本地 agent 以子进程 + JSON-RPC over stdio 方式接入。
- 让 visp 说 ACP，即可被 Zed 等客户端当 external agent 使用，而无需为其写专用编辑器插件。

### 1.2 目标
1. 提供一个 ACP agent 入口，使 Zed 能完成：发起对话、流式看到回复与工具调用、对工具调用进行审批、中断 turn。
2. **不侵入 daemon/core**：现有 agent loop、工具系统、审批判定、会话持久化保持不动（唯一例外：§13.10 已拍板的 core 取消收尾缺陷修复，约两行）。
3. 保持现有 CLI 路径完全不变。

### 1.3 非目标（本期）
- 远程/HTTP 传输的 ACP（仅本地 stdio）。
- `fs/read_text_file`、`fs/write_text_file`、`terminal/*`（visp 自行读写磁盘、自行执行 bash）。
- 将 ACP 作为 daemon 的一等协议。
- 替换或改造 `visp-tui`。

---

## 2. 术语

| 术语 | 含义 |
|---|---|
| ACP Client | 发起方，如 Zed；负责 UI、审批展示、编辑器集成 |
| ACP Agent | 被接入方，即 visp（由本设计新增的 `visp-acp` 二进制承担） |
| daemon | visp 后端服务进程 `visp-daemon`，gRPC 服务端 |
| turn | 一次 `session/prompt` 到其响应的完整过程 |
| Chat 流 | gRPC `CoderDaemon.Chat` 双向流，前端与 daemon 的会话通道 |

---

## 3. 关键约束（设计输入）

### 3.1 visp 侧事实（已从代码核实）

| # | 事实 | 出处 | 设计影响 |
|---|---|---|---|
| V1 | daemon 的 `Chat` 流为**进程内单例**：`chat()` 用 `orchestrator_grpc_rx.take()`，第二次调用报 `orchestrator receiver already taken` | `visp-daemon/src/service.rs` | 一个 daemon 进程只服务一条 Chat 流 |
| V2 | 会话事件流统一由 `ServerMessage` 承载，带 `session_id` 与 `agent_name` | `visp.proto` | 事件翻译层以 `session_id` 路由 |
| V3 | 工具审批与「LLM 提问」共用 `UserQuery`；审批时 `options` 为空 | `visp.proto`、`agent_loop.rs:1186-1197` | 需按 options 是否为空分流 |
| V4 | 审批答复 `selected_index`：`0`=Approve(仅本次)、`2`=Always Allow、其他（含 `-1`）=拒绝 | `agent_loop.rs:1216-1244` | ACP 选项需映射到这三个索引 |
| V5 | 审批期间 cancel 或超时 → `selected_index = -1`（拒绝），并有 `APPROVAL_TIMEOUT_SECS` 超时 | `agent_loop.rs:1199-1215` | ACP `cancelled` 结果等价于拒绝 |
| V6 | `Done { session_id }` 不含 `request_id`；`UserInput.request_id` 仅用于 `Ack` 日志追踪 | `visp.proto` | turn 结束按 session 关联，而非 request id |
| V7 | 会话由 daemon 创建并持久化（SQLite），`JoinSession` 会回放历史 | `visp.proto`、`service.rs` | `session/new` ↔ `CreateSession`；`session/load` 可复用回放 |
| V8 | 主 agent 循环以独立任务运行，拥有独立 cancel token；子 agent 通过 `TaskTool` 派生 | `orchestrator.rs` | 跨会话并发在架构上可行 |
| V9 | 部分事件带 `agent_name`（5 个变体无此字段，见 V13/V14）。**实际行为（三轮评审核实）：主 agent 事件 `agent_name = "default"` 非空**（`session.rs:276` 硬编码、经 `service.rs:911-913` 原样透传），子 agent 为子类型名；proto 注释「空字符串表示主 agent」（`visp.proto:258`）与实现不符 | `visp.proto:258`、`session.rs:262-277`、`orchestrator.rs:533-540`、`service.rs:911-913` | 归属与终止判定**不得依赖「agent_name 为空 = 主 agent」**（三轮评审发现）；子 agent 表达需要降级设计 |
| V10 | **取消不发 `Done`**（多数路径）：setup/流式/retry 三类取消路径发出 `AgentEvent::Error { code: Cancelled }` 并 `finish_loop(Error)`；对应 proto `Error.code` 是该枚举的 Display 文本 `"Operation cancelled"`。**已知例外（二轮评审发现）**：LLM 提问等待中取消（`StreamDecision::UserQuery` 分支）直接 `return`——**不发任何事件、不 `finish_loop`、会话状态停留 `Running`**（见 §6.6 兜底与 §13.10） | `agent_loop.rs:189-209`、`agent_loop.rs:1730-1752`、`agent_loop.rs:1835-1841`、`error.rs:83-97`、`service.rs:1440-1446` | 适配器必须把「`Error` 且 code=`Operation cancelled`」识别为 `stopReason: cancelled`；但**不得**假定它必然出现（例外路径无收尾信号） |
| V11 | 取消后会话状态置为 `Error`（仅对 V10 的三类常规取消路径成立；**例外**：LLM 提问等待中取消后状态停留 `Running`，见 V10）；下一次 `UserInput` 前 orchestrator 会把非 Idle 主会话重置为 Idle 再启动 | `agent_loop.rs:208`、`orchestrator.rs:344-357` | 取消后（含例外路径）会话仍可继续使用；例外路径由适配器自行收尾后依赖 daemon 侧重置机制复用会话 |
| V12 | **审批有超时，LLM 提问无超时**：审批用 `select!` 等 `cancel` / 120s 超时 / 答复（`APPROVAL_TIMEOUT_SECS = 120`）；而 LLM 提问（`options` 非空）只 `select!` `cancel` 与答复，**没有超时分支** | `agent_loop.rs:24`、`agent_loop.rs:1199-1215`、`agent_loop.rs:1830-1841` | 适配器对 `options` 非空的 `UserQuery` **必须主动回填**，否则主循环永久阻塞（§6.4/§8） |
| V13 | 子 agent 使用**独立 session**（新 uuid），父子关系只写入 session 表、**不进事件流**；`TextDelta`/`ToolCall`/`ToolResult`/`Done` 均无 parent 字段，`agent_event_to_server_message()` 构造时丢弃 `parent_session_id`/`parent_session_name` | `orchestrator.rs:664-689`、`service.rs:1313-1478`、`visp.proto:254-289, 357-359` | 子 agent 事件归属**必须由适配器启发式判定**（§8.4） |
| V14 | `ThinkingBlock` 与 `UserQuery` 连 `agent_name` 都没有 | `visp.proto:183-202` | 子 agent 的思考/提问无法标注来源；子 agent 审批只按 `query_id` 回填 |
| V15 | session SQLite 存储为单 `Mutex<Connection>`，`open()` **未启用 WAL、未设置 `busy_timeout`** | `visp-db/src/store.rs:12-45` | 两个 daemon 进程写同一 DB 文件会 `database is locked`；这是「数据目录默认隔离」的依据（§4.3/§8） |

### 3.2 ACP 侧要求（摘要）

| # | 要求 |
|---|---|
| A1 | 本地 agent = 编辑器子进程；JSON-RPC 2.0 over stdio；**stdout 仅输出合法 ACP 消息**，日志走 stderr |
| A2 | Baseline 必须：`initialize`、`session/new`、`session/prompt`、`session/cancel`（通知）、`session/update`（通知） |
| A3 | `session/update` 是 agent 推送进度的唯一通道（文本、思考、工具、结果等） |
| A4 | 权限请求用 agent→client 的 `session/request_permission`，结果二选一：`selected(optionId)` 或 `cancelled` |
| A5 | capabilities `/省略即不支持`，必须诚实声明 |
| A6 | 路径必须绝对、行号 1-based；`SessionUpdate` 枚举 `#[non_exhaustive]` |
| A7 | 取消必须体现为 `PromptResponse.stopReason = cancelled`，**不得**抛 JSON-RPC error |
| A8 | 「向用户提问」有两条通道：`session/request_permission`（工具审批，无需能力协商）与 `elicitation/create`（通用提问，需 client 声明 `elicitation.form`/`.url`；已于 2026-07 稳定化）。**Zed 两者都支持**（form + url） |

---

## 4. 技术选型与备选方案对比

### 4.1 接入形态

| 方案 | 描述 | 优点 | 缺点 | 结论 |
|---|---|---|---|---|
| **A（选定）** | 新增独立二进制 `visp-acp`，说 ACP over stdio，内部以 gRPC 客户端连接（或拉起）daemon | 零侵入 daemon/core；stdout 纪律天然满足；与 codex-acp/claude-agent-acp 范式一致；一进程一 daemon，规避单流约束 | 多一层翻译；新增依赖 | ✅ 采用 |
| B | daemon 内建 `--acp` stdio 模式 | 少一跳 | stdout 与日志冲突；破坏 daemon 多客户端定位；把协议耦合进核心 | ❌ 否决 |
| C | `visp-tui` 兼顾 ACP | 复用现有客户端代码 | TUI(ratatui) 与无头 stdio 模型冲突；把 adapter 放进错误位置 | ❌ 否决 |

### 4.2 SDK 选型
- 采用官方 Rust crate `agent-client-protocol`（当前 2.2.0），并使用其 **role + builder + handler** API。
- 理由：官方维护、与 Zed 参考实现同源、协议演进跟随；Goose 等 Rust agent 已验证。
- 备选：手写 JSON-RPC（否决：重复造轮子、易漏协议细节）。
- 注意：锁版本；官网 libraries/rust 页面仍是旧 trait 文档，以 docs.rs 与仓库 README 为准。

### 4.3 daemon 生命周期策略
- **选定：`visp-acp` 自行拉起 daemon 实例**（自动探测可用端口 + health check；退出时 `Shutdown` 仅限自拉起模式，直连模式默认不 Shutdown，见 §6.1）。
- 理由：规避 V1 单流约束，与运行中的 CLI daemon 互不干扰；一个 Zed 窗口对应一套 visp 进程组，生命周期清晰。
- 备选：连接已存在的 daemon —— 保留为可选参数（`--addr`），但不作为默认。**受 V1 约束**：daemon 的 Chat 流为进程内单例，直连目标的 Chat 流必须空闲（如手动启动的调试 daemon）；连接正被 CLI 等客户端占用的 daemon 会在建流时失败（`orchestrator receiver already taken`）。
- **数据目录默认隔离，可配置共享**：session DB 路径来自 `daemon.toml` 的 `[storage].path`（由 config-dir 定位）；而 `SqliteSessionStore::open()` **未启用 WAL、未设置 `busy_timeout`**（`visp-db/src/store.rs:19-45`），两个 daemon 同时写同一 DB 文件会直接报 `database is locked`，且 `ListSessions` 会把 Zed 侧与 CLI 侧会话混杂。故默认使用独立 config-dir/DB；若用户希望 CLI/Zed 共享会话历史，可显式 `--config-dir` 指向同一目录并接受上述代价。
- **隔离粒度演进（三轮评审补充）**：M1 按**进程**隔离（临时派生 config-dir）——零成本且无损失，因 Zed 每次启动 agent 均为全新会话、`session/load` 属 M2。**M2 前置依赖**：`session/load` 需要跨进程持久的会话存储，届时须 (a) 将 config-dir 固化为持久派生路径（如按项目哈希），(b) 为 `visp-db` 补 WAL + `busy_timeout` 以容忍同项目多窗口同库并发，(c) 处理会话归属展示（避免跨窗口混杂）。缺省不做 (b) 则 M2 须按进程持久化并放弃跨窗口 `session/load`。

### 4.4 复用 launcher 逻辑的方式
- 在 `visp-acp` 内实现端口探测 / spawn / health / shutdown（复用 `visp-config` 的路径工具），**不改动** `visp` launcher 与其测试。
- 理由：仅第二处使用，抽取公共 crate 属过早抽象；若未来出现第三处需求再抽取。

---

## 5. 总体架构

```
┌──────────┐   ACP (JSON-RPC over stdio)   ┌────────────┐   gRPC (Chat bidi stream)   ┌──────────────┐
│   Zed    │ ────────────────────────────► │  visp-acp  │ ──────────────────────────► │ visp-daemon  │
│ (client) │ ◄──────────────────────────── │  (agent)   │ ◄────────────────────────── │  (backend)   │
└──────────┘   session/update, requests    └────────────┘   ServerMessage 事件流       └──────────────┘
                                                  │                                        ▲
                                                  │ ① spawn + health check                 │
                                                  └────────────────────────────────────────┘
                                                     (端口探测 / VISP_LISTEN_ADDR / 退出时 Shutdown·仅自拉起模式)
```

### 组件职责

| 组件 | 职责 | 变更 |
|---|---|---|
| `visp-acp`（新增 crate） | ACP agent 实现；daemon 生命周期编排；ACP↔gRPC 双向翻译 | 新增 |
| `visp-daemon` | 提供既有 gRPC 服务 | 不变 |
| `visp-tui` | 既有 TUI 前端 | 不变 |
| `visp-proto` | gRPC 契约 | 不变 |

### `visp-acp` 内部模块划分（职责，不含实现）

| 模块 | 职责 |
|---|---|
| 入口 / 参数解析 | 解析 `--project`、可选 `--addr`、`--config-dir`；初始化 stderr 日志 |
| daemon 编排 | 端口探测、spawn、health check（含子进程存活监控）、退出收尾（按启动模式区分） |
| gRPC 会话层 | 建立**唯一** Chat 流；发送 `UserInput`/`Cancel`/`UserResponse`/`Ack`；按 `session_id` 路由入站事件 |
| ACP agent 层 | 用 SDK 注册 `initialize`/`session/new`/`session/prompt` handler；发 `session/update` 通知；发 `session/request_permission` |
| 事件翻译器 | `ServerMessage` → `SessionUpdate`（纯函数式映射，便于单测） |
| 审批桥接 | `UserQuery(options 空)` ↔ `session/request_permission`；结果回填 `UserResponse` |
| 关联标识管理 | 为 assistant turn / 子 agent 生成稳定的 `messageId`；维护 `call_id ↔ toolCallId` |
| 会话注册表 | ACP `sessionId` ↔ visp `session_id`（本期直接复用 visp id）、每会话在途 prompt 状态 |

---

## 6. 核心流程

### 6.1 进程启动与生命周期
1. `visp-acp` 由 Zed 以子进程启动，stdin/stdout 归 ACP 使用。
2. 参数解析 → 若给定 `--addr` 则直连；否则探测可用端口、spawn `visp-daemon`（`VISP_LISTEN_ADDR` 注入，stdout/stderr 重定向到日志文件）。
3. 轮询 `HealthCheck` 直至就绪（带超时）；失败则向 stderr 输出错误并退出。**轮询期间同时监控子进程存活**：每轮先 `try_wait()`，若子进程已退出（典型为端口绑定失败，如并发启动时探测端口被抢占，TOCTOU 竞态），立即读 `startup_error_file()` 诊断并退出——**不得**继续对探测端口做 health check，否则会对占用该端口的其他 daemon 实例误连成功（见 §8、§12）。
4. 建立**唯一** Chat 流（构造出站 sender / 入站接收任务）。
5. 进入 ACP agent 事件循环，直至 stdin 关闭。
6. 退出收尾按启动模式区分：
   - **自拉起模式（默认）**：向 daemon 发 `Shutdown`，超时（如 5s）则强杀持有的子进程。
   - **`--addr` 直连模式**：daemon 非本进程拉起，可能正被其他客户端使用，默认**不发送** `Shutdown`、不强杀；仅当显式传入 `--shutdown-on-exit` 时才发送。

### 6.2 `initialize`
- 不需要 gRPC 交互。
- 返回：协商后的 `protocolVersion`；`agentCapabilities`（见 §7.5）；`agentInfo`（name=visp, version=包版本）；`authMethods: []`。
- 认证模型：visp 使用 daemon 配置 / 环境变量中的 API key，无交互式认证。若配置缺失，错误在 `session/prompt` 阶段以明确信息暴露。
- **协议版本**：只支持协议 v1；若 client 报 v2，则回 v1（不启用 SDK 的 unstable v2）。

### 6.3 `session/new`
- 入参 `cwd`（绝对路径）→ 调 `CreateSession(project_path = cwd)`。**`cwd` 优先于启动参数 `--project`**（Zed 每个会话可指向不同目录；`--project` 仅作 daemon 级默认值）。
- 返回的 visp session id 直接作为 ACP `sessionId`（字符串），无需额外映射表。
- `mcpServers[]`：本期忽略（见 §8 边界与 §13.6 未决）。
- 在会话注册表中登记该会话，绑定同一个 Chat 流。

### 6.4 `session/prompt`（turn 主流程）
1. 校验 `sessionId` 存在、该会话无在途 turn，且**全局同一时刻仅一个在途父 prompt**（M1 约束，见 §8/§8.4）。
2. 通过 Chat 流发送 `UserInput { text, session_id }`；ACP 入参为 content blocks 数组，M1 仅支持文本块——多个 text block 按顺序拼接为一条 `text`，非文本块返回明确错误（capabilities 已声明不支持 image）。
3. 进入「事件泵」：持续消费入站 `ServerMessage`，按 §7.2 翻译为 `session/update` 通知并下发；涉及审批时暂停等待（§6.5）。
4. **正常终止**：收到该 session 的 `Done` → 响应 `PromptResponse{ stopReason: end_turn }`。
5. **取消终止**：收到该 session 的 `Error` 且 code 为 `"Operation cancelled"` → 响应 `PromptResponse{ stopReason: cancelled }`，**不得**转成 JSON-RPC error（V10 / A7）。
6. **异常终止**：**仅当 `session_id == 在途父 session`** 的 `Error` 才终止 turn → 以 `refusal`（或 `end_turn`）结束，并把错误信息作为文本/错误内容下发，不静默丢弃。**不得把「`agent_name` 为空」作为父事件判据**：主 agent 事件实际带 `agent_name = "default"`（V9，三轮评审发现，proto 注释与实现不符）；按 `session_id` 过滤在 M1 单在途父 prompt 下已完备（子事件 `session_id` 恒为子 uuid）。**子 agent 的 `Error`（`session_id` 为子 session）不得终止父 turn**：子 agent 失败后父 loop 收到 `SubAgentComplete`（`ToolResult.is_error = true`）会继续正常推进并以 `Done` 结束——若误判提前收尾，父 turn 后续输出全部丢失（二轮评审发现）。
7. **LLM 提问（`options` 非空）**——ACP 支持 human-in-the-loop，正确映射是 `elicitation/create`（A8）：
   - **client 声明了 `elicitation.form`**（Zed 支持）→ 发 `elicitation/create`（form 模式，用 `options` 生成枚举 schema；`allow_other` → 附一个可选自由文本字段），并按结果回填。**schema 映射约定（三轮补充，实现者按此固化单测）**：枚举字段值直接使用选项原文 `options[i]`，响应经文本反查得 `selected_index = i`；`allow_other` 的自由文本字段**优先于**枚举字段（两者并存时视为自定义输入，回填 `selected_index = -1, text = <内容>`）：
     - `accept`（选中某项 / 给出文本）→ 回填对应 `selected_index`（自定义文本时 `selected_index = -1, text = <内容>`）
     - `decline` / `cancel` → 回填 `selected_index = -1`（视为用户未选择）
   - **client 未声明该能力** → 不得发送（否则 client 返回 `-32602`）；降级为：把问题原文作为 `agent_message_chunk` 下发，并**立即回填** `UserResponse{ selected_index: -1, text: <说明> }`。
   - 无论走哪条路径，**都必须回填**：visp 该路径无超时（V12），协议也不提供超时，不回填即永久挂起。

### 6.5 工具审批（`session/request_permission`）
1. 翻译器识别 `UserQuery` 且 `options` 为空 ⇒ 审批。
2. 合成 ACP 权限选项（visp 不提供选项，由适配器合成），与 V4 索引严格对齐。注意代码 `match` 只区分 `0`/`2`/`_`，`1` 与 `-1` 同为 deny、无特殊语义：
   - `allow_once` → 用户选「允许」→ 回填 `selected_index = 0`
   - `allow_always` → 用户选「始终允许」→ 回填 `selected_index = 2`
   - `reject_once` → 用户选「拒绝」→ 回填 `selected_index = 1`（语义上等同 deny）
   - **其余任何值（含 `-1`）一律 deny**
3. agent→client 发 `session/request_permission`（携带 `toolCallId`、标题、`kind`）。
4. 结果处理：
   - `selected(optionId)` → 按上表回填 `UserResponse{ query_id, selected_index }`。
   - `cancelled` → 回填 `selected_index = -1`（拒绝）。**turn 收尾按归属区分（三轮评审修正）**：审批属父 session → turn 大概率正被取消，收尾以 `Error{Cancelled}`/兜底超时为准，**不因权限结果提前收尾**；属子 agent → 仅等价拒绝，父 turn 照常推进至正常 `Done`（子审批可见后，用户关弹窗 ≠ 取消 turn；若强行收尾，daemon 侧父 loop 仍在跑，下次 prompt 会防御性重置并 spawn 第二个 loop，状态机失步，`service.rs:461-471`）。
   - **`reject_always`**：visp 无此语义，降级为 `reject_once`（`selected_index = 1`），作为已知差异记录。
5. 同时保留 visp 自身的审批超时（V5）作为兜底；超时后 turn 继续以「工具被拒绝」推进，适配器不得悬挂。

### 6.6 `session/cancel`
- 收到通知后，通过 Chat 流发送 `Cancel { session_id }`；无在途 prompt 时为 no-op（daemon 侧对未运行 session 天然 no-op，`orchestrator.rs:1163-1166`）。M3 `session/delete` 时同步清理会话注册表条目。
- **turn 收尾信号**：visp 在取消时**通常**不发送 `Done`，而是发送 `Error{ code: "Operation cancelled" }` 并把会话状态置为 `Error`（V10/V11）。因此适配器的「在途 prompt」结束判定必须同时接受两种信号：
  - `Done` → `stopReason: end_turn`；
  - `Error`（code=`"Operation cancelled"`）→ `stopReason: cancelled`。
- 必须保证正在处理的 `session/prompt` 以 `cancelled` 响应，而非 JSON-RPC error（A7）。
- **判定建议双向匹配**：`Error.code == "Operation cancelled"` **或** `message` 含 `"cancelled"`（取消时 `message = "agent cancelled"`），降低文案漂移风险。
- 若 cancel 发生在审批等待中，visp 会把审批结果置为 `-1`（V5），适配器同时要处理权限请求返回 `cancelled` 的情形（两者都可能出现，按 §6.5 统一收敛为拒绝 + turn 取消）。
- **若 cancel 发生在 LLM 提问等待中（`options` 非空）**：core 当前在该分支直接 `return`，**不发任何收尾事件、不改会话状态**（V10 例外，`agent_loop.rs:1835-1841`）——`Error{Cancelled}` 永远不会到来，仅靠等信号将永久悬挂。适配器兜底：发出 `Cancel` 后启动收尾超时（如 10s），超时仍未收到任何收尾信号（`Done` / `Error{Cancelled}`）→ 自行以 `stopReason: cancelled` 结束该 turn（会话复用依赖 V11 的 daemon 侧重置，正常可用）。该例外已列为 **M1 前置核心修复候选（§13.10）**：在 `select!` 取消分支补发 `Error{Cancelled}` + `finish_loop(Error)`，改动极小；若 core 修复落地，本兜底保留为防御层。
- **迟到/失效的 `UserResponse` 属正常**：`query_id` 可能已被 daemon 消费后静默丢弃（`service.rs:582-593`），适配器忽略即可、不作为错误；turn 收尾以「`Done` 或 `Error{Cancelled}`，或兜底超时」为准。
- 取消后会话状态为 `Error`（LLM 提问例外路径除外，见上），下次 prompt 前由 daemon 自动重置为 Idle（V11），适配器无需额外恢复。

### 6.7 会话加载 / 恢复（M2）
- `session/load`：调 `GetSession` 校验存在性，随后复用 `JoinSession` 的历史回放机制，把 `UserMessage` / 助手文本 / 工具帧以 `session/update` **流式重放**（`user_message_chunk`、`agent_message_chunk`、`tool_call`/`tool_call_update`），全部重放完再响应。
- capabilities 中声明 `loadSession` 后，客户端才可能调用；M1 不声明。

---

## 7. 接口与数据契约

### 7.1 ACP ↔ visp 生命周期映射

| ACP | visp | 备注 |
|---|---|---|
| `initialize` | （无 RPC） | 返回 caps / agentInfo |
| `session/new` | `CreateSession` | `cwd` → `project_path`（优先于 `--project`） |
| `session/prompt` | `UserInput`（Chat 流） | 以该 session 的 `Done` 结束 |
| `session/cancel` | `Cancel`（Chat 流） | 响应用 `cancelled` |
| `session/load`（M2） | `GetSession` + `JoinSession` 回放 | 流式重放历史 |
| `session/list`（M3） | `ListSessions` | |
| `session/delete`（M3） | `DeleteSession` | |

### 7.2 事件翻译映射

| visp `ServerMessage` | ACP `session/update` | 关联与备注 |
|---|---|---|
| `TextDelta` | `agent_message_chunk` | 父 agent 一个 turn 一个稳定 `messageId`；**子 agent 的 `TextDelta` 在 M1 抑制**（§8.4，避免双重展示） |
| `ThinkingBlock` | `agent_thought_chunk` | 同上 messageId 规则；`ThinkingBlock` 无 `agent_name`（V14），无法标注来源；**子 agent 的 `ThinkingBlock` 在 M1 抑制**（带 `session_id`，归属技术上可行，与 TextDelta 抑制策略对齐，§8.4） |
| `ToolCall` | `tool_call` | `toolCallId = call_id`；`status: pending`；`name/title` 用工具名与人类可读描述；能定位则填 `locations`（绝对路径、1-based）；`tool_name → kind` 映射见 §7.4。**子 agent 的工具帧（`agent_name` 非空）刻意照常流式**（§8.4），仅其 `TextDelta`/`ThinkingBlock` 被抑制 |
| `ToolResult` | `tool_call_update` | `status`：`is_error ? failed : completed`；`content` 用文本内容块 |
| `UserQuery`（options 空，工具审批） | `session/request_permission` | 见 §6.5；`UserQuery` 无 `agent_name`（V14） |
| `UserQuery`（options 非空，LLM 提问） | client 支持 `elicitation.form` → `elicitation/create`；否则降级为文本 chunk + 立即回填 `-1` | **visp 无超时（V12），任何路径都必须回填，否则死锁** |
| `Done` | 结束 `session/prompt` 响应 | `stopReason: end_turn` |
| `Error`（code = `"Operation cancelled"`） | 结束 turn → `stopReason: cancelled` | 取消的主要收尾信号（visp 通常不发 `Done`，V10）；**例外**：LLM 提问等待中取消无任何收尾信号，靠 §6.6 兜底超时收尾 |
| `Error`（其他 code，`session_id` == 在途父 session） | 文本/错误信息 + 结束 turn | `stopReason: refusal` 或 `end_turn`；不静默丢弃。**子 agent 的 `Error`（子 `session_id`）不终止父 turn**（§6.4/§8.4）；判据只用 `session_id`（主 agent `agent_name` 实为 `"default"` 非空，V9） |
| `UsageInfo` / `UsageDelta` | `usage_update`（M3） | 上下文占用/成本 |
| `StatusUpdate` | 文本 chunk（M1）| 避免依赖 unstable `notice` |
| `ImageBlock` / `ImageError` | image content block（M2/M3） | 需 base64 或资源链接处理 |
| `UserMessage` | `user_message_chunk` | 仅 `session/load` 回放 |

### 7.3 标识关联规则
- **`messageId`（近似）**：同一 assistant 消息的连续分块保持同一 `messageId`；切换到不同 agent（`agent_name` 变化）或工具帧插入时更换。visp 的 `TextDelta` 没有「消息结束」标记，无法精确对应每次 LLM assistant 消息，此为近似策略。
- **`toolCallId`**：直接使用 visp 的 `call_id`，保证 `tool_call` 与 `tool_call_update` 可关联。
- **turn 关联**：`Done` 仅带 `session_id`（V6），故一个会话同一时刻只允许一个在途 prompt；适配器据此维护「会话 → 在途 prompt」状态。
- `Ack`：非必需，可作为与 CLI 行为对齐的可选动作发送。

### 7.4 工具名 → ACP `kind` 映射（初版）
- 只读类（`read_file`、`grep`/搜索、`codegraph_*`）→ `read` / `search`
- 写/改类（`write_file`、`edit_file`）→ `edit`
- 执行类（`bash`）→ `execute`
- 网络类（`webfetch`）→ `fetch`
- 任务委派（`task`）→ `other`（子 agent 表达见 §8.4）
- 未识别 → `other`
- 该映射为纯函数，独立单测。

### 7.5 capabilities 声明
- M1：基础能力集合；**不**声明 `loadSession`、`fs`、`terminal`、`promptCapabilities.image`（对客户端诚实，省略即不支持）。
- M2：按实现进度增补 `loadSession`、`promptCapabilities.image`。
- 声明与实现必须一致，避免「声称支持但无行为」。
- **协议版本只声明 v1**（不声明 v2/unstable）。
- `$/cancel_request` 由 SDK 处理，适配器无需额外实现。
- **elicitation 是 client 能力**，agent 侧无需声明；适配器在 `initialize` 时记录 client 是否支持 `elicitation.form`，据此决定「LLM 提问」走 elicitation 还是降级（§6.4）。

---

## 8. 边界情况与错误处理

| 场景 | 处理策略 |
|---|---|
| stdout 被污染 | 适配器自身日志全走 stderr；spawn daemon 时重定向其 stdout/stderr 到文件（`visp-acp` 不继承 daemon 的输出） |
| daemon 启动失败/超时 | 向 stderr 输出可诊断错误（含 daemon 启动错误文件内容，如 launcher 做法），进程退出 |
| 与 CLI 共享数据目录 | session DB 路径来自 `daemon.toml` `[storage].path`（由 config-dir 定位）；SQLite 未启用 WAL/`busy_timeout`（`store.rs:19-45`），两 daemon 同写一库会 `database is locked`，且会话列表混杂。默认独立；可用 `--config-dir` 显式共享 |
| daemon 运行中崩溃 | 入站流结束 → 对在途 prompt 以错误结束；后续请求返回明确错误 |
| 并发启动竞态（TOCTOU） | 两个 `visp-acp` 同时启动可能探测到同一空闲端口：赢家的 daemon 绑定成功，输家的 daemon 绑定失败退出；但输家的 health check 会对赢家的 daemon 通过（该端口正有 daemon 应答），且 `HealthStatus` 无实例身份无法察觉；输家退出时发 `Shutdown` → 误关赢家的 daemon | health check 期间轮询子进程存活（`try_wait()`），子进程已退出立即报错退出，不继续 health check；绑定失败读 `startup_error_file()` 诊断 |
| `--addr` 直连模式退出误杀 | daemon 可能正被其他客户端使用，退出时统一发 `Shutdown` 会关闭共享 daemon，其他客户端全部断连 | 直连模式默认不发 `Shutdown`、不强杀；仅显式 `--shutdown-on-exit` 才发送（§6.1） |
| `session/prompt` 收到未知/不存在 session | 返回 JSON-RPC error，不崩溃 |
| 同一会话并发 prompt | 适配器拒绝第二个（返回错误），因 visp 主会话不支持并发 turn |
| 不同会话并发 prompt | **M1 显式禁止**：同一进程同一时刻仅允许一个在途父 prompt；因 proto 无 parent 关联（V13），并发多父 session 会导致子 agent 事件无法归属 |
| 工具输出超长 | 对 `tool_call_update.content` 做截断/分块，保留首尾与提示信息 |
| 审批超时 | 依赖 visp 的 `APPROVAL_TIMEOUT_SECS` 兜底（视为拒绝），适配器不悬挂 |
| 审批期间取消 | 权限请求返回 `cancelled` → 回填 `-1`（拒绝）。**按归属区分（三轮评审修正）**：父 session 的审批 cancelled ≈ turn 正在取消，收尾仍以 `Error{Cancelled}`/兜底超时为准；**子 agent 的审批 cancelled 仅等价拒绝**——父 turn 照常推进至 `Done`，不得强行以 `cancelled` 收尾（否则状态机失步、双 loop） |
| turn 结束判定 | 正常看 `Done`；取消看 `Error{code:"Operation cancelled"}`（V10，注意 LLM 提问等待中取消无收尾信号的例外，§6.6）。终止判定**必须过滤**：按 `session_id` 匹配父 session——`Done` 与 `Error` 同判据（主 agent `agent_name` 实为 `"default"` 非空，不得以「`agent_name` 为空」为判据，V9）；子 agent 事件（子 `session_id`）不得终止父 turn（§6.4）。以「该 session 在途 prompt」为状态机，并保留取消兜底超时 |
| 取消后会话可复用 | 状态为 `Error`，daemon 在下次 `UserInput` 前自动重置（V11），适配器无需特殊处理 |
| 取消信号为展示文案 | `"Operation cancelled"` 是枚举 Display 文本而非稳定协议码，文案变更会破坏判定（见 §12 风险） |
| `reject_always` | 降级为 `reject_once`（已知差异） |
| 错误 stopReason | visp 未向上暴露 LLM 原生 stop reason，`max_tokens` 等无法区分，统一 `end_turn`（已知限制） |
| LLM 提问（options 非空） | visp 该路径**无超时**（V12）。client 支持 `elicitation.form` 时走 `elicitation/create`（accept/decline/cancel 均须回填）；不支持时降级为文本 + 立即回填 `-1`。任何路径都必须回填，否则主循环永久挂起 |
| LLM 提问等待中取消 | core 该分支不发收尾事件、不改状态（V10 例外，`agent_loop.rs:1835-1841`），适配器若仅等 `Error{Cancelled}` 将永久悬挂 | 适配器取消兜底超时（§6.6）；M1 前置核心修复候选（§13.10） |
| `/` 开头的用户输入 | daemon 对每个 `UserInput` 先过内部命令解析（`service.rs:480-562`），`/init` 等会被劫持执行（含文件写）而非作为普通文本发给 LLM；Zed 用户输入以 `/` 开头的普通文本时被静默劫持 | M1 如实记录该行为（与 CLI 一致）；后续可评估转义或前缀豁免 |
| client 不支持 elicitation | 若仍发送未声明模式 → client 返回 `-32602`，提问失败 | 发送前检查 `elicitation.form` 能力；不支持则降级为文本 + 立即回填 `-1`（§6.4） |
| 迟到/失效 `UserResponse` | daemon 可能已丢弃该 `query_id`（`service.rs:582-593`）；适配器忽略，不作为错误 |
| 子 agent 事件归属 | 启发式：在途父 prompt 期间，`session_id ≠ 父 session` 的事件归入该父 session（`agent_name` 非空仅作辅助标注，主 agent 也非空，V9）；未知 session、无在途父 prompt 的事件（含兜底收尾后的迟到父事件）丢弃并记 stderr 警告；**`UserQuery` 例外：绕过归属直接桥接**（§8.4） |
| 子 agent 输出双重展示 | 子 agent 的 live 事件与其 `task` 的 `ToolResult` 会重复；M1 选择**不流式子 agent 的 `TextDelta`/`ThinkingBlock`**，只经 `task` 的 `tool_call`/`tool_call_update` 呈现（§8.4） |

### 8.4 子 agent 表达与归属（决策，已按评审修订）

**事实约束**：子 agent 使用独立 session（新 uuid），父子关系只存在于 session 表、不进事件流；proto 事件只有 `session_id` 与（部分事件的）`agent_name`，`ThinkingBlock`/`UserQuery` 连 `agent_name` 都没有（V13/V14）。因此适配器**无法**从协议字段直接得知某事件属于哪个父 ACP session。

**M1 策略（显式化）**：
1. **同一进程同一时刻仅一个在途父 prompt**（M1 约束，见 §8 表）。由此获得映射依据：期间收到的 `session_id ≠ 父 session` 的事件必属于该父 session 派生的子 agent。
2. **归属规则（三轮修正主判据）**：`session_id == 父 session_id` → 父事件——**主判据只有 `session_id`**（主 agent 事件 `agent_name` 实为 `"default"` 非空，V9，proto 注释不可信）；否则 `agent_name` 非空 → 子 agent 事件，归入当前在途父 prompt；**无在途父 prompt 时的迟到事件**（子事件晚于父 `Done`，或兜底超时收尾后迟到的父 `Done`/`Error`）→ 丢弃并向 stderr 记警告；否则（未知 session）→ 忽略并向 stderr 记警告。**`UserQuery` 特例（二轮评审发现）**：`UserQuery` 无 `agent_name` 字段（V14），子 agent 的审批/提问若按上述规则归属会落入「未知 session 且无 `agent_name`」桶被静默丢弃——用户看不到子 agent 审批（120s 超时判拒），或子 agent 提问永不回填（V12 无超时，子循环永久阻塞）。因此 **`UserQuery` 一律绕过归属判定、直接进入审批/提问桥接**（§6.5；daemon 侧 pending map 按 `query_id` 路由，回填与归属无关，技术上已核实可行）。
3. **展示（避免双重展示）**：M1 **不流式子 agent 的 `TextDelta`/`ThinkingBlock`**；但**刻意流式子 agent 自身的 `ToolCall`/`ToolResult`**（`agent_name` 非空、`session_id` 为子 session）为独立 `tool_call`/`tool_call_update`——该不对称是刻意的：用户能看到子 agent 在做什么，而其文本不重复。子 agent 的最终产出同时随父 loop `task` 工具调用呈现（`tool_call` → `tool_call_update`），随其 `ToolResult` 内容自然展示——既能看到「派发了子任务」及其结果，又不会让同一文本输出出现两遍。
4. **子 agent 审批**：其 `UserQuery` 无 `agent_name`（V14），按 `query_id` 回填即可；UI 上无法标注来源，作为已知限制。
5. **后续（M3）**：可评估「`task` 作为父 `tool_call`、子活动作为其嵌套 `content`」的实时表达，或为 proto 增加 parent 关联（需核心改动，超出本期）。

---

## 9. 非功能需求

- **可观测性**：适配器日志走 stderr（含结构化字段：session、event 类型、turn 阶段）；daemon 侧既有 OpenTelemetry 不变；可用 ACP trace viewer 观察消息时序。
- **性能**：事件翻译为轻量纯函数；不做额外缓冲聚合（保持流式体感）；超长内容截断。
- **可靠性**：进程组生命周期确定（起则拉起 daemon，退则关闭）；审批/取消路径不悬挂。
- **安全**：不新增网络暴露面（daemon 仍绑本机回环）；审批语义仍由 visp 工具策略决定，适配器只做透传与呈现。

---

## 10. 分阶段范围

### M1（最小可用）
**前置任务**：core 缺陷修复（§13.10）——`StreamDecision::UserQuery` 取消分支补发 `Error{Cancelled}` + `finish_loop(Error)`，附取消路径单测。

`initialize`、`session/new`、`session/prompt`、`session/cancel`；`session/update` 的 `agent_message_chunk`/`agent_thought_chunk`/`tool_call`/`tool_call_update`；`session/request_permission`；`elicitation/create`（当 client 声明 `elicitation.form` 时启用，否则按 §6.4 降级）。
**验收**：Zed 中能对话、看到工具调用、能审批、能中断、能回答 LLM 提问。

### M2（会话能力）
`session/load`（复用回放）、`session/list`；图片内容与 `promptCapabilities.image`；`usage_update`；斜杠命令映射。

### M3（进阶）
`session/delete`、`session/set_config_option`（模型/温度切换）、`plan`；以及需要核心改动的 `fs/*`、`terminal/*`（单独评估）。

---

## 11. 测试策略（TDD 导向）

| 层级 | 内容 |
|---|---|
| 单元 | 事件翻译器纯函数（每种 `ServerMessage` → `SessionUpdate`）；工具名→kind 映射；审批选项↔索引映射（含 `reject_always` 降级、`cancelled`、超时） |
| 集成 | 用 SDK 的轻量 client（或最小自建 client）驱动 `visp-acp`，断言：turn 流式 chunk 顺序、`tool_call`/`tool_call_update` 配对、审批往返、cancel 产生 `stopReason: cancelled`、**stdout 仅含合法 ACP 消息（日志不污染）**、**子 agent 事件归属与 `task` 展示不重复** |
| 集成（daemon 编排） | 预占探测端口后启动 `visp-acp` → 断言报错退出而非误连该端口上的既有 daemon；`--addr` 直连退出 → 断言未发送 `Shutdown`；自拉起退出 → 断言 daemon 已关闭 |
| 集成（负路径，二轮评审） | ① LLM 提问等待中取消 → 断言 turn 以 `stopReason: cancelled` 收尾（不悬挂）；② 子 agent 中途失败 → 断言父 turn 不提前终止、最终正常 `Done`、子 `Error` 仅展示；③ 子 agent 触发审批 → 断言审批可见且回填生效（§8.4 特例）；④ 用户输入以 `/` 开头 → 断言命令劫持行为与文档一致。用 daemon MockProvider（`service.rs:1495+`）可确定性构造 |
| 夹具 | 复用 daemon 的 MockProvider 测试夹具（`service.rs:1495+`）驱动确定性事件流，做端到端断言 |
| 端到端 | Zed 自定义 agent 手测；用 `dev: open acp logs` 核对消息时序 |
| 回归 | `visp-daemon`/`visp-core`/`visp-tui` 现有测试保持全绿（证明零侵入） |

**通用原则（三轮评审）**：归属/终止/路由等判定逻辑**不得依赖 proto 注释级事实**（如「空 `agent_name` = 主 agent」——实为 `"default"`），一律以集成测试断言事件流实际字段值为准。

质量门：`cargo test && cargo clippy -- -D warnings && cargo fmt -- --check`。

---

## 12. 风险与缓解

| 风险 | 影响 | 缓解 |
|---|---|---|
| 单 Chat 流约束被忽视 | 多客户端/多窗口下第二个 Chat 失败 | 默认一进程一 daemon；显式文档化 |
| daemon 生命周期误关（TOCTOU 误连 / 直连误 Shutdown） | 误连他人 daemon 或关闭共享 daemon，其他窗口会话中断 | §6.1 按启动模式区分退出策略；health check 期间监控子进程存活；实例级身份校验（proto `HealthStatus` 增 `instance_id`）列为 M1 后可选增强 |
| 审批索引映射错位 | 误放行/误拒绝（安全问题） | 以 V4 为准写单测；集成测试覆盖三种结果 |
| SDK API 与旧文档不一致 | 实现走弯路 | 锁版本、以 docs.rs/README 为准；先用最小 agent 冒烟 |
| stdout 被日志污染 | ACP 解析失败、连接中断 | 强制日志走 stderr + daemon 输出重定向；集成测试断言 stdout 仅 ACP 消息 |
| capabilities 过度声明 | 客户端调用失败 | 声明与实现同步推进，先省略 |
| 并发父 prompt 导致事件错投 | 子 agent 事件无法归属（V13） | M1 显式禁止多父并发；归属按 §8.4 规则；单测覆盖 |
| 会话恢复回放体量大 | 加载慢/超限 | M2 再处理，必要时分批/截断 |
| 取消信号依赖可读文案 | 文案若变，适配器把取消误判为普通 Error → 以 `refusal` 收尾 → Zed 显示为错误而非用户主动停止 | 判定同时匹配 `code == "Operation cancelled"` 或 `message` 含 `"cancelled"`；集中定义常量 + 集成测试；建议后续在 proto 增加结构化取消码 |
| LLM 提问等待中取消无收尾信号（core 缺陷，二轮评审） | 适配器永久等待 → Zed turn 卡死 | 适配器取消兜底超时（§6.6）+ 强烈建议 M1 前修复 core（`select!` 取消分支补发 `Error{Cancelled}` + `finish_loop`，改动极小，§13.10） |
| 子 agent 事件误判终止 / 审批被归属规则吞掉（二轮评审） | 父 turn 提前终止丢输出；子 agent 审批不可见、子提问永久阻塞 | 终止判定按 `session_id` + `agent_name` 过滤（§6.4）；`UserQuery` 绕过归属直接桥接（§8.4）；负路径集成测试（§11） |
| LLM 提问无超时 | 适配器不回填 → 主循环永久阻塞 | 任何路径都强制回填（§6.4）；集成测试覆盖 |
| 向不支持 elicitation 的 client 发送请求 | client 返回 `-32602`，提问失败 | 发送前检查 `elicitation.form`；否则降级为文本 + 回填 `-1` |
| 子 agent 输出双重展示 | 用户看到重复内容 | M1 不流式子 agent 的 `TextDelta`/`ThinkingBlock`，工具帧刻意流式，文本仅经 `task` 呈现（§8.4） |

---

## 13. 已决问题（2026-09-23 用户逐项确认）

1. **接入形态**：✅ 采用方案 A（独立 `visp-acp` crate）。
2. **M1 范围**：✅ 不含 `session/load`、`fs/*`、`terminal/*`；`fs`/`terminal` 走 visp 自有工具链，保持审批模型统一。
3. **子 agent 表达**：✅ M1 采用「同进程单在途父 prompt + 按 `session_id` 归属（`agent_name` 仅辅助标注，主 agent 名实为 `"default"` 非空）+ 不流式子 agent 的 `TextDelta`/`ThinkingBlock`（工具帧刻意流式，文本仅经 `task` 工具调用呈现，避免双重展示）」。
4. **daemon 生命周期**：✅ 默认由 `visp-acp` 自行拉起 daemon；`--addr` 直连保留为可选（受 V1 约束：目标 Chat 流必须空闲，见 §4.3）。
5. **`reject_always` 与 `max_tokens` 的已知差异**：✅ 接受本期降级；两项已登记 `docs/todo/TODO.md`（P2：core `rejected_tools` 集合；P2：LLM stop reason 透出）。
6. **`session/new` 的 `mcpServers[]`**：✅ 本期忽略；用户经 `daemon.toml` `[mcp.servers]` 配置 MCP；运行时注入属独立立项。
7. **LLM 提问处理**：✅ `options` 非空映射为 `elicitation/create`（发送前查 `elicitation.form` 能力），不支持时降级为「文本 + 立即回填 `-1`」；任何路径都必须回填（V12 无超时）。
8. **daemon 数据目录**：✅ 默认隔离（M1 进程级临时派生 config-dir）；M1→M2 演进路径见 §4.3（持久派生路径 + `visp-db` WAL/busy_timeout 为 M2 前置依赖）。
9. **daemon 退出策略**：✅ 自拉起退出发 `Shutdown`（超时强杀子进程）；直连默认不发（仅显式 `--shutdown-on-exit`）；health check 期间监控子进程存活规避 TOCTOU 误连（§6.1、§8、§12）。
10. **core 取消收尾缺陷修复**：✅ 采用方案 A——M1 前在 `StreamDecision::UserQuery` 的 `select!` 取消分支（`agent_loop.rs:1835-1841`）补发 `Error{Cancelled}` + `finish_loop(Error)`（约两行 + 取消路径单测）；CLI 同步受益；§1.2「不侵入 core」以此为例外。适配器兜底超时（§6.6）保留为防御层。

---

## 14. 参考

- 外部协议事实：[`visp-acp-protocol-research.md`](../review/visp-acp-protocol-research.md)
- 可行性与方案调研：[`visp-acp-integration-feasibility.md`](../review/visp-acp-integration-feasibility.md)
- visp gRPC 契约：`crates/visp-proto/proto/visp.proto`
- daemon 服务：`crates/visp-daemon/src/service.rs`
- agent loop / 审批：`crates/visp-core/src/agent_loop.rs`（`1186-1244`）
- orchestrator：`crates/visp-agent/src/orchestrator.rs`
- launcher（spawn/health 参考）：`crates/visp/src/main.rs`
