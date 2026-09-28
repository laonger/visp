# visp 通用可配置 lifecycle hook 系统设计

> 状态：**v3 — 已吸收两轮独立评审 + 代码级事件通路勘查，待用户最终确认**
> 日期：2026-09-28（v1：09-27；v2：09-28；v3：09-28）
> 关联文档：`docs/design/2026-09-27-herdr-integration-design.md`（§11）
> 本文只描述架构、流程、职责、数据流与关键决策，不含实现代码。
> **v3 说明**：v2 的第二轮评审发现 3 个硬问题（N1 信任半道门 / N2 关停无宿主 / N3 事件通路缺失）。本轮先派**代码溯源**（core 产出点 + daemon 传输/宿主）产出「**事件通路表**」（§6.4），再据此重写。裁定记录见 §19。

---

## 1. 背景与目标

### 1.1 为什么需要 hook

visp 是 `launcher(visp) + daemon(visp-daemon) + TUI(visp-tui)` 三进程结构，会话生命周期与回合状态由 daemon 权威持有（`SessionStatus`、`pending_queries`），语义事件经 gRPC `Chat` 双向流由 daemon 推送 TUI。目前 visp 对外只有两条扩展面：

1. **改变 agent 行为**：`.visp/{rules,skills,agents,system-prompt.md}` + `~/.config/visp/` + MCP（仅客户端）。
2. **终端副作用**：TUI 的 `notify.rs`（OSC 9/777/99 / BEL），仅 `Done`/`UserQuery` 两个挂点。

缺的是第三类：**用户在自己的环境里对 visp 生命周期做出反应**（写状态文件、审批时推送、回合结束触发 git/CI、错误告警、喂给多路复用器）。这是「事件驱动的本地副作用」。

### 1.2 目标

1. **事件可达**：以**稳定、可机读**的契约暴露会话/回合/工具/审批/错误事实。
2. **零侵入**：未配置 hook 时，**用户可见行为逐字节不变**（内部通道有改，见 §13）。
3. **不阻塞**：hook 的慢/卡/崩溃不影响 agent 主流程与 TUI。
4. **可控安全**：项目级 hook 是任意代码执行面，必须有明确信任边界。
5. **单源顺序**：同一事件一个发射源，序号单调、跨重启可辨。
6. **可迁移**：事件名与载荷字段对齐主流惯例（见 §2.4）。

### 1.3 非目标

- 不做同步拦截作为一期能力（见 D1）。
- 不做图形化编辑器 / 市场 / 跨机器分发。
- 不把 notify.rs 改造成 hook（D9）。
- **不改变现有 gRPC 语义**；仅**新增一个只读诊断 RPC**（`GetHookStats`）。
- 一期**不做** hook 配置热重载（D11）。

---

## 2. 现状分析

### 2.1 既有扩展面

| 扩展面 | 位置 | 与 hook 的关系 |
|---|---|---|
| rules / skills / agents / system-prompt | `.visp/*`、`~/.config/visp/*` | 静态内容，无执行、无事件 |
| MCP | `[mcp]`，客户端 | agent 入站工具；未来可作 transport |
| notify | `crates/visp-tui/src/notify.rs` | 显示层；分层范式可复用，能力不合并 |
| OTel/Langfuse | `[observability]` | 遥测导出；与 hook 目标端不同（§3.2） |

### 2.2 事件源、通道与既有缺陷（代码事实）

| 事实 | 位置 | 说明 |
|---|---|---|
| 会话状态机 | `visp-core/src/session.rs:25` | `Idle/Running/Completed/Error`；`start_loop`（`:347`）置 Running，`finish_loop`（`:391`）收敛 |
| Agent 事件 | `visp-core/src/agent.rs:39` | `AgentEvent` 全变体（含 `ToolCallRequest/Result`、`UserQuery`、`Done`、`Error`…） |
| 事件帧 | `visp-core/src/agent.rs:103` | `AgentEventFrame`；**当前未 derive `Clone`** |
| 控制面通道 | `visp-daemon/src/main.rs:552` | `global_tx/rx`（`AgentMessage`），供 `Orchestrator::run` 单消费者 |
| **显示面通道** | `visp-daemon/src/main.rs:554-555` | `orchestrator_grpc_tx/rx` = **单消费者 mpsc(256)**；显示面唯一来源 |
| **既有缺陷** | `service.rs:429-434` + `:883-931` | `chat()` 对 receiver `.take()`，**永不归还** → 重连 `"already taken"`、显示流停摆 |
| 向显示面的发布点 | `orchestrator.rs:542`（主）、`:846`（子）、`:1055`（root Done）、`reload.rs:166`、`watch.rs:481` | 详见 §6.4 |
| UserQuery 构造 | `agent_loop.rs:702`（`[USER_QUERY]` 标记=question）、`:1190`（审批=approval） | **无 `kind` 字段** |
| ToolCallResult 四处 | `agent_loop.rs:1152`(取消)/`:1230`(拒绝)/`:1285`(截断)/`:1350`(真实) | **四态共用 `is_error`，仅靠 `content` 字符串区分** |
| `ToolCallRequest` | `agent_loop.rs:1169` | 审批**之前**发出；`requires_approval` 在 `:1179` 才算出 |
| `start_loop` 调用 | `orchestrator.rs:490-501`（主）、`:765-783`（子） | **每回合/OFF 每次 spawn**；daemon 不可见 |
| `Session` 字段 | `visp-core/src/session.rs:34-54` | **无 `source`/`resume`/`turn` 字段**；恢复信息只在 CLI（`visp-tui/src/main.rs:129`） |
| 压缩锚点 | `prompt.rs:79-82`（唯一） | 仅**每请求窗口裁剪**；无「压缩发生」判定，历史不改写 |
| 子 agent 锚点 | spawn `orchestrator.rs:935`；收敛 `:967`/`:1077` | 确定性良好 |
| **关停无优雅钩子** | `service.rs:1079-1086`（只关 MCP）+ `main.rs:649`（仅 ctrl_c）+ launcher `:179-186`（5s 后 SIGKILL） | 正常退出路径无 drain 宿主 |
| 文件监听 | `main.rs:624-638`、`watch.rs:82-131/261-277` | `[hooks]`/`.visp/hooks/` **当前天然不被监听**（需补显式排除+测试） |
| 项目配置合并 | `visp-config/src/config.rs` | 项目级只合并 `[llm]`/`[[agent.builtin]]` |

### 2.3 缺口

- 无 hook 机制；无面向用户的稳定事件契约；无多订阅者总线（单消费者 mpsc）；无跨重启序号；无优雅关停宿主。

### 2.4 跨工具事件约定与命名决策

- **无官方跨工具标准**，但 Claude 族 PascalCase 是**事实标准**（Codex/Kimi/Qwen/Droid/Copilot/Qoder/Cursor/Grok 等 9+ 家沿用）。
- **Codex 自身有完整 hooks**；herdr 只是选择只注册 `SessionStart`。
- **OpenCode 是异类**（小写点分、无 exit code），且 2.0.9 做过 v1→v2 **不兼容改名**。
- **herdr 不在事件命名层**（只消费三态 + `session_start_source`）→ **兼容不依赖事件名**。

**命名决策**：事件名对齐 Claude 族 PascalCase，载荷字段对齐 `hook_event_name`/`session_id`/`cwd`/`source`，引入 `matcher`。
**承诺收紧**：一期只做到「**命名同族**」——无 `transcript_path`、默认脱敏、无 `exit 2`、无完整 gate，**既有 Claude 脚本不可直接复用**；真正迁移能力是未来的 `--format claude` 适配层。
**修正**：`source` = 会话来源（visp 实际 `{startup, resume}`），传输来源改名 `origin`。

---

## 3. 反思

### 3.1 需求本质

用户在 visp 生命周期节点挂自己的可执行逻辑；要求 (a) 稳定事件概念、(b) 可靠传递、(c) 不反噬 visp。

### 3.2 备选方案对比

| 备选 | 结论 |
|---|---|
| skills/rules/agents 扩展 | 否（静态、无执行） |
| MCP 工具 | 否（需 agent 主动调用；生命周期事件不由工具触发） |
| 复用 notify | 否（仅 2 挂点、TUI-only）；但其分层是好范式 |
| OTel/Langfuse | **部分**（目标端是遥测后端，非用户进程）；两者都能做的优先 OTel |
| 仅 herdr 内置 | 否 |

### 3.3 YAGNI 边界与「主动投入」决策

第二轮评审指出：v1「等第二个消费者」的措辞自相矛盾（第二个消费者被定义为自己要建的诊断命令）。**用户裁定：明确作为「主动投入的产品能力」立项**。因此不再声称等待消费者；改为承认这是**产品级投入**，验收标准为「契约稳定 + 执行器正确 + herdr 作为首个内置消费者可用」。

**诚实声明（v3.2 修订）**：herdr 集成**最终采用外置脚本**（`assets/hooks/herdr.hook.sh` + 一条 `[hooks]` 规则），因此**通用脚本执行路径在一期即有真实消费者**；visp 内**不保留任何 herdr 专属 Rust**（曾短暂实现的进程内 `HerdrBinding`/`CompositeHandler` 已回退，见 §20）。

**YAGNI 仍有效**：只想看指标 → OTel；只想响铃 → notify；只想执行前确认 → permission；只想让 agent 做事 → tools/skills。

---

## 4. 方案对比与选择

- **能力范围**：一期仅观察，二期受控介入（D1）。
- **发射源**：**事件总线（broadcast）**（D3）——单源、连接无关、顺带修复 `.take()` 缺陷。
- **基线**：总线 + daemon 侧发射（事实产出于 orchestrator/agent_loop，**通路见 §6.4**）+ 一期仅观察 + 项目级默认惰性 + 规则级策略（保序/丢弃/白名单 env）+ 优雅关停 drain + 不热重载。

---

## 5. 核心决策

### D1 能力范围：一期仅观察，二期受控介入

介入要求主循环同步等待；超时 fail-open/closed 两难；把「任意代码执行」升级为「任意代码决策」。一期主循环零耦合。

### D2 事件域与清单

事件域 = **agent 语义事实**；不纳入 TUI 渲染/UI 事件。`PreToolUse` 与 visp 时点错位 → **二分**：`ToolCallRequested`（审批前）+ `PreToolUse`（审批后、执行前）。

### D3 发射源与总线（D 方案）

**`tokio::sync::broadcast` 替换 `orchestrator_grpc_tx/rx`**（`main.rs:554-555`）；`global_tx` 不动。

- **生产者**：orchestrator 转发任务、reload/watch 下行、**新增的 daemon/orchestrator 生命周期与审批发布点**（§6.4）。
- **载荷（v3.1 修订）**：总线载荷为 **`BusEvent` 枚举**——`Frame(AgentEventFrame)`（**显示域**，TUI 消费）｜`Lifecycle(…)`（**hook 域**：`SessionStart`/`UserPromptSubmit`/`PermissionResult`/`SessionEnd` 等**不在帧流里**的生命周期事实）。TUI 出站**只消费 `Frame`**；hook 执行器**消费两类**。`AgentEvent` 保持纯显示语义，不被生命周期污染。`BusEvent`/发布 trait 定义在共享层（`visp-core`）。
- **订阅者**：① hook 执行器（启动订阅一次、**常驻**）；② 每个 TUI 连接（`chat()` 内 `subscribe()`，连接结束即 drop；重连重新订阅）。
- **前置硬约束**：`AgentEventFrame` 非 `Clone`（`agent.rs:97` 的 `oneshot::Sender`）→ **方式 a**：`oneshot → mpsc::Sender`，并补 `Clone`。
- **容量/滞后**：1024 起步；满则**丢最旧 + `Lagged(n)`**。
- **sentinel receiver（N8 修复）**：总线结构体**常持一个 `Receiver`**，使「无订阅者」时 `send` 不报错；发布任务**不得因 send 失败而 break**。
- **背压变更（已接受）**：生产者永不阻塞、永不因订阅者报错。
- **`Lagged` 责任方（裁定 A）**：**daemon outbound 在 `Lagged` 时主动做一次 join 式 replay**（复用 `replay_session_history`），**TUI 保持无改动**。
- **会话过滤**：服务端不过滤；TUI 侧按 session 路由。
- **重连**：重连只拿到订阅后事件；断线窗口事件不补（靠 `send_join` → `replay_session_history`，`service.rs:686/1203`）。
- **seq**：总线发布点发放单一 `AtomicU64`；`base = epoch_ms`；跨重启**通常**递增（极端见 §17）；**可选字段**。

### D4 执行语义

- 异步 fire-and-forget；每规则强制 `timeout_ms`（超时杀进程树）；失败隔离。
- **顺序**：**默认 per-rule 串行**；跨规则并发；`parallel = true` 放开（🟠-3）。
- **规则间排序**：同一事件多规则按 `id` 字典序（可选 `order`）。
- **丢弃**：规则级 `on_full`（`drop_new` 默认 / `drop_old` / `coalesce_latest`）；状态型用 `coalesce_latest`（🟠-2）。
- **不重试**。
- **关停 drain**：见 D13。

### D5 投递约定

stdin JSON（写完即关）+ `VISP_HOOK_*` 标量；`VISP_HOOK_SCHEMA=1`；cwd=project_path；**环境白名单注入**（🟠-4：`PATH/HOME/TERM/LANG/VISP_*/HERDR_*` + 规则显式 `env`）；`VISP_IN_HOOK=1` 防递归。

### D6 配置形态

新增 `[hooks]` 节（全局 + 项目）；规则字段见 §7.2；项目级作用域需扩展合并逻辑。

### D7 安全模型

1. 全局 hook 默认允许。
2. **项目 hook 默认不运行**，须显式信任。
3. **信任绑定（🔴-4 + N1 修复）**：信任 = canonical 项目路径 + **`.visp/hooks/` 目录内容快照 + 生效的项目级 hook 规则全集（`.visp/daemon.toml` 的 `[hooks]`：command/args/env）**；任一变更即失效。
4. **禁止项目级 `sh -c`**（N1）：项目级 `command` 必须解析到 `.visp/hooks/` 目录内的可执行文件。
5. 无 shell 直执；隐私默认脱敏；**信任是门不是沙箱**（诚实边界）。

### D8 与 herdr 集成

herdr 集成 = **一条 `[hooks]` 规则 + 一份随包脚本**（`assets/hooks/herdr.hook.sh`），**零 visp 专属代码**（对应 herdr 设计 §11.1 的「外置」）。脚本读 `VISP_HOOK_EVENT` 判定状态并调 `herdr pane report-agent`；安装由用户按文档在 `daemon.toml` 添加规则。**无内置绑定、无 `[herdr]` 配置节**。这使得通用脚本路径获得**真实消费者**（修正 §3.3 的 YAGNI 遗留）。

### D9 与既有机制的关系

notify / rules·skills·agents / MCP / observability 与 hooks **职责隔离，不合并**。

### D10 可观测性与调试

结构化日志（不写终端）+ 计数（`emitted/dropped/executed/failed/timed_out`）+ `visp hooks list/doctor/test/logs` + `--dry-run`。**计数经只读 RPC `GetHookStats`**（proto 有改动）。

### D11 配置热重载

一期**不热重载** `[hooks]`/信任文件；**显式排除 + 测试**（现状天然不监听，需固化为显式行为）；启动日志/doctor 提示需重启。

### D12 事件发射宿主（v3 新增）

各事件的**宿主与通路**由 §6.4 代码级表定义，摘要：

- `SessionStart` / `UserPromptSubmit` → **orchestrator**（首次 `start_loop` 成功后；`source` 由绑定时刻 `history` 是否为空推导）。
- `Stop` / `StopFailure` ← `Done` / `Error` 帧。
- `ToolCallRequested` / `PreToolUse` / `PostToolUse` / `PostToolUseFailure` ← `agent_loop`（`PreToolUse` 需**新增 `AgentEvent` 变体**；`ToolCallResult` 需**新增 `outcome` 枚举**）。
- `PermissionRequest` ← **总线上的 `UserQuery` 帧**（不再锚 per-connection `pending_queries`）。
- `PermissionResult` ← **daemon 侧响应路由成功后的单一发布点**。
- `SessionEnd` ← 会话删除 + 优雅关停管道（D13）。

### D13 优雅关停管道（v3 新增，修 N2）

现在正常退出路径**无优雅钩子**（Shutdown RPC 只关 MCP；launcher 5s 后 SIGKILL）。新增通路：

```
gRPC Shutdown（service.rs:1079-1086）
  → 发 bus 事件 SessionEnd
  → hook executor 有界 drain（≤2s）
  → cancel_tx 发 CancelSignal（该通道已存在但闲置：service.rs:140/243）
  → Notify 唤醒 daemon main（main.rs:649 改 select!{ctrl_c, shutdown}，并纳入 SIGTERM）
  → main 走既有清理（watcher.stop / mcp.shutdown_all / server.abort）

launcher：由「5s 后 SIGKILL」改为「等待 daemon 自行退出（带超时兜底）」
```
注意：`mcp.shutdown_all` 会被双调，需确认幂等（`manager.rs:262`）。

---

## 6. 事件契约

### 6.1 边界与命名

属 hook 域 = daemon/orchestrator 可权威判定的语义事实；不属 = 流式文本/渲染/按键。契约冻结后只做加法；破坏性变更升 schema 版本。命名对齐 Claude 族（同族 ≠ 可迁移，见 §2.4）。

### 6.2 事件表（v3）

> **phase 0 冻结范围**：仅冻结「锚点 + 字段均已确认」的事件。`PreCompact/PostCompact` **不在一期契约内**（二期再评估）；`turn_id` **已删除**；`SubagentStart` 二期可选。
>
> **完成事件分两层（本轮裁定 B）**：**执行级**（`agent_loop`，一个 agent run 一次，按 `parent_id` 命名主/子）与**会话级**（`orchestrator`，清理置 Idle 之后）。两层在不同时点，**不是重复**。

| 事件 | 层级 | 宿主（见 §6.4） | 关键载荷字段 | 一期 |
|---|---|---|---|---|
| `SessionStart` | 会话级 | orchestrator：**该 session 在本次 daemon 内首次 `start_loop` 成功**（`parent_id.is_none()` 过滤） | `session_id, short_id, cwd, project_path, source, model, model_key, agent_name, parent_session_id, origin` | 是 |
| `UserPromptSubmit` | 会话级 | orchestrator（主 agent 受理且 `start_loop` 成功） | `session_id, origin, prompt_chars`（`prompt` 可选、默认脱敏） | 是 |
| `AgentRunEnd` | **执行级（主）** | `agent_loop`（`:906`）经**主**转发任务 | `session_id, status` | 是 |
| `SubagentStop` | **执行级（子）** | `agent_loop`（`:906`）经**子**转发任务（`parent_id.is_some()`） | `session_id, parent_session_id, agent_name, status` | 是 |
| `Stop` | **会话级** | orchestrator `handle_done` root 分支（`:1053-1062`，**清理置 Idle 之后**：`:994`→`:1053`） | `session_id, status, input_tokens, output_tokens, tool_calls`（取同回合最近 `UsageInfo`） | 是 |
| `StopFailure` | 会话级 | orchestrator `handle_agent_error`（`:1077`，语义收敛后） | `session_id, code, message` | 是 |
| `ToolCallRequested` | `agent_loop`（`ToolCallRequest`，审批前） | `session_id, tool_use_id, tool_name` | 是（默认无规则） |
| `PreToolUse` | `agent_loop`（审批后、执行前；新变体） | `session_id, tool_use_id, tool_name, requires_approval` | 是（默认无规则） |
| `PostToolUse` | `agent_loop`（真实执行结果且 `outcome=success`） | `session_id, tool_use_id, tool_name` | 是（默认无规则） |
| `PostToolUseFailure` | `agent_loop`（真实执行结果且 `outcome=failure`） | `session_id, tool_use_id, tool_name, message` | 是（默认无规则） |
| `PermissionRequest` | 总线 `UserQuery` 帧 | `session_id, query_id, kind, options_count`（`message` 默认脱敏） | 是 |
| `PermissionResult` | daemon 响应路由成功后 | `session_id, query_id, outcome, selected_index` | 是 |
| `SessionEnd` | 会话删除 / 优雅关停 | `session_id, reason, exit_code` | 是 |

**`outcome` 枚举（🔴-2 + 本轮裁定 A）**：给 `ToolCallResult` 增加 `outcome ∈ {success, failure, denied, cancelled, truncated}`（替换「仅靠 `content` 字符串区分」）。`denied` 由 `PermissionResult` 一并表达；`cancelled` 归 `Stop` 语义；`truncated` 为参数畸形。

**`source`**：`{startup, resume}`，由绑定时刻 `history` 是否为空推导（orchestrator 侧）。
**`origin`**：`{tui, headless, other}`；由 **daemon** 在会话被客户端驱动时写入 session 并透传给 orchestrator（今天只有 `tui`；`headless` 为未来非 TUI 客户端预留）。
**`Stop.status`**：`{completed, cancelled}`——正常 `Done`→`completed`；取消（cancel 标记判定）→`cancelled`；真错误走 `StopFailure`。
**`SessionEnd.exit_code`**：`Option<i32>`——会话删除→`None`；daemon 正常关停→`0`（异常退出无事件）。
**`PermissionRequest.kind`**：`{approval, question}`（由 `AgentEvent::UserQuery` 显式携带，见 D12）。
**`PermissionResult.outcome`**：`{selected, cancelled}`。
**执行级 `status`**（`AgentRunEnd`/`SubagentStop`）：`{completed, cancelled, failed}`（**执行级**完成态，与会话级 `Stop.status ∈ {completed, cancelled}` 区分）。
**`StopFailure.code`**：沿用 core 既有 `AgentErrorCode` 的序列化字符串（取值以 core 为准）。
**`parent_session_id`**：`Option<String>`，`None`（主会话）时**省略**（与 `seq`/`exit_code` 同一省略原则）。
**`SessionStart` 首次语义（本轮裁定 #2）**：**该 session（而非「本 daemon 全局」）在本次 daemon 内首次 `start_loop` 成功**；实现为 **per-session 标记集合** + `parent_id.is_none()` 过滤。→ 同一 daemon 内**第二个新建会话仍会发** `SessionStart`；子 agent 的 `start_loop` **不触发**。
**完成事件分层（本轮裁定 B，参考 opencode 2.0.9 的 execution 终结 + `sessionID`/`parentID` 寻址）**：执行级（`AgentRunEnd`/`SubagentStop`，发在 `agent_loop`）先于会话级（`Stop`/`StopFailure`，发在 orchestrator 清理后）。消费者若只想要「回合真正结束」用会话级；若要「某 agent 的 loop 返回」用执行级。

### 6.3 投递约定

| 变量 | 含义 |
|---|---|
| `VISP_HOOK_EVENT` | 事件名（PascalCase） |
| `VISP_HOOK_SEQ` | 单调序号（**可选**，面向复用器/严格序消费者） |
| `VISP_HOOK_SCHEMA` | schema 版本（一期 `1`） |
| `VISP_HOOK_RULE_ID` | 命中规则 id（内置为 `builtin:...`） |
| `VISP_SESSION_ID` / `VISP_SESSION_SHORT_ID` | 会话标识 |
| `VISP_PROJECT_PATH` | canonical 项目路径 |
| `VISP_AGENT_NAME` / `VISP_PARENT_SESSION_ID` | agent / 父子关系 |
| `VISP_IN_HOOK` | 防递归 |

stdin JSON：顶层 `schema`/`hook_event_name`/`session_id`/`cwd`/`source`/`origin` 及事件字段；`seq` 可选。
**机读契约**：phase 0 产出 **JSON Schema + golden fixtures**（仅覆盖已冻结事件）。

### 6.4 事件通路表（代码级，v3 新增）

> 本表是 v3 的核心修订：为每个「事实在 core/agent、总线在 daemon」的事件写明「**产出点 → 载体 → 总线**」。

| 事件 | 事实产出点（`文件:行`） | 载体 | 到总线的方式 | 需新增/改动 | 宿主 |
|---|---|---|---|---|---|
| `SessionStart` | `orchestrator.rs:490-501`（主）`start_loop` Ok 之后 | 新事件 | orchestrator 直发 bus | 新增 **per-session**「本次 daemon 内首次」**标记集合** + `parent_id.is_none()` 过滤；`source` 由 `history` 空否推导；`origin` 由 daemon 透传 | orchestrator |
| `UserPromptSubmit` | `orchestrator.rs:343-357`（`handle_client_message`）+ `start_loop` Ok | 新事件 | 同上 | — | orchestrator |
| `AgentRunEnd` / `SubagentStop` | `Done` `agent_loop.rs:906`（经主/子转发任务 `orchestrator.rs:542`/`:846`，按 `parent_id` 分层命名） | 已有 Frame | 转发任务 → bus | **保留**（执行级） | 转发任务 |
| `Stop` / `StopFailure` | root `Done` 帧 `orchestrator.rs:1055`（`handle_done`，置 Idle `:994` 之后）；`Error` `orchestrator.rs:1077` | 已有 Frame | orchestrator → bus | **保留**（会话级，与执行级分层、非重复）；`Error` 语义收敛 | orchestrator |
| `ToolCallRequested` | `agent_loop.rs:1169` | 已有 | 同上 | — | 同上 |
| `PreToolUse` | **无 → 插在 `agent_loop.rs:1299`↔`:1300`** | **新增 AgentEvent 变体** | 同上 | 适配 `event_to_msg`（`agent_loop.rs:99`）、`agent_event_to_server_message`（`service.rs:1340/1450`）；**拒绝路径不得误发** | 同上 |
| `PostToolUse` / `…Failure` | `agent_loop.rs:1350`（真实结果）；其余三处不发工具事件 | 已有 + **新增 `outcome`** | 同上 | `ToolCallResult` 加 `outcome` 枚举；仅真实结果处发 | 同上 |
| `PermissionRequest` | 总线上的 `UserQuery` 帧（源 `agent_loop.rs:702`/`:1190`） | 已有 Frame | 转发任务 | **改锚到 bus**（不再锚 per-connection `pending_queries`）；`+kind` | 转发任务 |
| `PermissionResult` | `service.rs:580-609`（响应路由成功点，先 daemon map 后 orchestrator 回退） | 需新增 | 新增单一发布点 → bus | 在路由成功后发一次 | daemon service |
| `SessionEnd` | 删除 `service.rs:410-419`；关停见 D13 | 需新增 | 直发 bus | 优雅关停管道（D13） | daemon |
| ~~`PreCompact/PostCompact`~~ | 仅 `prompt.rs:79-82` 每请求窗口裁剪，**语义不存在** | — | — | **移出一期契约** | — |
| ~~`SubagentStart/Stop`~~ | spawn `orchestrator.rs:935`；stop `:967`/`:1077` | 新/已有 | 直发 | **二期再评估** | — |
| ~~`turn_id`~~ | 无 turn 概念 | — | — | **已删除** | — |

**横切**：`orchestrator.rs:290-305` 的 UserQuery 发布**生产不可达**（仅测试）——**不接通**；若将来接通，`kind` 需一并穿透 `AgentMessage::UserQuery`（`agent.rs:147`）。

---

## 7. 配置形态

### 7.1 配置节

新增 `[hooks]`（全局 + 项目）；缺省无规则；与 `[herdr]`/`[mcp]` 平级。

### 7.2 规则字段

| 字段 | 含义 | 默认 |
|---|---|---|
| `id` | 规则唯一名（内置 `builtin:` 前缀）；**决定同事件多规则顺序**（字典序） | 必填（内置可免） |
| `order` | 显式顺序覆盖 | 无 |
| `event` | 命中事件名（可多选） | 必填 |
| `matcher` | 正则匹配 `tool_name`/`source`/`kind` | 空（全匹配） |
| `command` | argv 首元素（**无 shell**）；**项目级须在 `.visp/hooks/` 内** | 必填 |
| `args` / `env` / `cwd` / `enabled` | 同前 | — |
| `timeout_ms` | 单次超时（**硬上限**由执行器在 1b-2 强制） | `60000`（Claude 族惯例） |
| `on_full` | `drop_new` / `drop_old` / `coalesce_latest` | `drop_new`（状态型显式 `coalesce_latest`） |
| `parallel` | 放弃默认保序 | `false` |
| `cooldown_ms` | 最小触发间隔 | 0 |
| `include` | 载荷可选字段（`prompt`/`tool_input`/`tool_response`） | 空（默认脱敏） |
| `scope` | `global` / `project` | 按来源 |

### 7.3 作用域与优先级

全局默认可用；项目默认惰性须信任；优先级：用户规则 > 内置；同 id 项目覆盖全局。

---

## 8. 安全模型

### 8.1 威胁

| 威胁 | 后果 | v3 处置 |
|---|---|---|
| 项目投毒 | 克隆即执行 | 默认惰性 + 信任 |
| **信任后篡改（配置）** | 改 `.visp/daemon.toml` 加恶意规则 | **信任快照覆盖生效规则全集 + 禁项目级 `sh -c`** |
| 注入 | 借 shell 注入 | 无 shell 直执 |
| 隐私外泄 | 密钥被项目脚本读取 | **环境白名单**（非黑名单） |
| 递归/风暴 | 资源耗尽 | `VISP_IN_HOOK` + `cooldown_ms` |

### 8.2 分层信任

全局（默认可信）→ 项目（默认惰性）→ 信任 = canonical 路径 + `.visp/hooks/` 目录快照 + 生效规则文本；任一变更即失效。信任存储位于全局数据目录，不写入仓库。

### 8.3 执行硬化

无 shell 直执；stdin 写完即关；强制超时 + 进程组终止；环境白名单；防递归。

### 8.4 隐私默认

`UserPromptSubmit` 默认只给长度；工具事件默认省略 `tool_input`/`tool_response`；`PermissionRequest.message` 默认脱敏。原文需规则 + 全局双重显式开启。

---

## 9. 执行语义（汇总）

| 维度 | 决定 |
|---|---|
| 同步性 | 全异步；发射点非阻塞入队 |
| 超时 | 每规则强制，超时杀进程树 |
| 隔离 | 每规则独立子进程 |
| 顺序 | 默认 per-rule 串行；跨规则并发；`parallel` 放开 |
| 规则间排序 | 同事件多规则按 `id` 字典序（`order` 覆盖） |
| 丢弃 | 规则级 `on_full`；状态型 `coalesce_latest` |
| 重试 | 不重试 |
| seq | 总线发布点；`base=epoch_ms`；可选字段 |
| 失败 | 仅日志 + 计数 |
| 关停 | 优雅关停管道 + 有界 drain（≤2s） |
| 消费端合并 | coalesce 在规则层 |

---

## 10. 架构与组件关系

```
                    ┌──────────────────── visp-daemon / visp-agent ────────────────────┐
  agent_loop 事件 ──▶ 转发任务(补 session/agent 上下文) ─┐
  orchestrator：SessionStart/UserPromptSubmit ──────────┤
  daemon：PermissionResult / SessionEnd ────────────────┘
        global_tx(256) ─▶ Orchestrator::run   [控制面, 不动]
        bus broadcast(1024) + AtomicU64(seq) + sentinel Receiver ◀── 发布点（单源）
             │                                   │
    subscribe│                                   │subscribe (常驻)
             ▼                                   ▼
  TUI outbound（每连接；Lagged→join 式 replay）   hook executor（常驻）
     │  send_join 重同步                             └ 规则匹配(id 序/串行/on_full/白名单env)
     ▼                                                └─ 用户脚本子进程（herdr 走 herdr.hook.sh）
    TUI
  visp（launcher）: `visp hooks list/doctor/test/logs` ──GetHookStats──▶ daemon
  proto：+ 只读 GetHookStats；TUI 无改动
```

**crate 划分**：`visp-hooks`（**新建**，主动投入）；扩展 `visp-core`/`visp-agent`/`visp-daemon`/`visp-proto`/`visp-config`/launcher；`visp-tui` 无改动。

---

## 11. 数据流

```
事件产生（agent_loop / orchestrator / daemon）
  → 构造载荷（按 include，默认脱敏）
  → 总线发布点：发放 seq（base=epoch_ms + 计数）
  → bus.send(BusEnvelope)   ← 非阻塞；有 sentinel receiver，永不因无订阅者报错
  → 订阅者：
      ├─ TUI outbound（每连接）：转 proto；Lagged → join 式 replay
      └─ hook executor（常驻）：规则匹配(event+matcher+id 序+cooldown) → per-rule 队列(on_full)
            → 同规则串行 spawn 子进程（白名单 env、stdin JSON、timeout、杀进程树）
```

**失败降级**：任何一步失败仅日志 + 计数，不回写主流程。
**关闭**：Shutdown RPC → SessionEnd → drain(≤2s) → cancel → Notify → main 退出。

---

## 12. herdr 集成：外置脚本 + 默认规则

| herdr §11 项 | 回应 |
|---|---|
| D1 上报源 TUI → | 改 **daemon 总线发射**；TUI 内置上报退役 |
| D2 不建 crate → | 建 **`visp-hooks`** |
| D3 挂点/seq → | 迁 daemon 总线；seq 总线发放（可选） |
| D4 通道 → | 通用执行器承接；herdr 为**随包脚本** `assets/hooks/herdr.hook.sh`（用户加一条 `[hooks]` 规则） |
| D5 metadata | 不变 |
| D6 屏幕竞争 | **不变** |
| D7 会话恢复 | **不变**（hook 无法让 herdr 记住第三方 session id） |
| D8 skill | 可附带 herdr 参考脚本 |
| D9 配置 | `[hooks]` + `[herdr]` |
| D10 能力边界 | **不变且强化：hook ≠ authority** |

### 12.1 安装（示例，用户自装）

把 `assets/hooks/herdr.hook.sh` 放到 `~/.config/visp/hooks/herdr.hook.sh`（项目级放 `.visp/hooks/`，需信任），并在 `~/.config/visp/daemon.toml` 添加：

```toml
[[hooks.rules]]
id = "herdr"
event = ["SessionStart", "UserPromptSubmit", "PermissionRequest", "Stop", "StopFailure", "AgentRunEnd", "SubagentStop", "SessionEnd"]
command = "~/.config/visp/hooks/herdr.hook.sh"
on_full = "coalesce_latest"
timeout_ms = 2000
```

脚本仅在 `HERDR_ENV=1`（herdr pane 内）时生效；状态映射：`UserPromptSubmit→working`、`PermissionRequest→blocked`、`Stop/StopFailure/AgentRunEnd/SubagentStop/SessionStart→idle`。`SessionEnd` 时脚本调用 `pane release-agent` 释放 herdr agent 权威，避免 visp 退出后边栏残留。用 `visp hooks list/doctor` 校验。

herdr 设计 §11.5 的 snake_case 草案**已被本设计取代**。

---

## 13. 影响范围与向后兼容

| 模块 | 改动 | 性质 |
|---|---|---|
| `visp-core` | `AgentEvent`/`AgentEventFrame` `Clone`；`UserQuery.respond` mpsc；`+kind`；`ToolCallResult +outcome`；**新增 `PreToolUse` 变体** | 类型扩展 |
| `visp-agent` | 转发任务去 `break`；`SessionStart`/`UserPromptSubmit` 发射点；UserQuery 占位类型 | 中改 |
| `visp-daemon` | 总线+sentinel+每连接 subscribe；`PermissionRequest/Result`、`SessionEnd` 发射；`GetHookStats`；优雅关停管道；watcher 显式排除 `[hooks]` | 中改 |
| `visp-proto` | 新增只读 `GetHookStats` | 新增 |
| `visp-config` | `[hooks]` + 信任（规则全集快照） | 新增 |
| `visp-hooks` | 新建 | 新增 |
| `visp`（launcher） | `visp hooks ...`；关停改为等待而非 SIGKILL | 新增/改 |
| `visp-tui` | **无改动** | — |

**兼容保证**：未配置 `[hooks]` → 无规则/无子进程/无输出，**用户可见行为不变**。**诚实声明**：总线替换与关停管道改变 daemon 内部行为（core/agent/daemon/proto 有改），对既有 RPC 语义与 TUI 交互不变。项目 hook 默认惰性。

---

## 14. 边界情况

| 场景 | 期望 |
|---|---|
| daemon/TUI 分离 | hook 在 daemon 发射；TUI 断连 ≠ SessionEnd |
| TUI 重连 | 重新 subscribe（修 `.take()`）；`send_join` 重同步；窗口事件不补 |
| `Lagged` | daemon outbound 主动 join 式 replay |
| 无订阅者 | sentinel receiver 兜底；发布任务不 break |
| 关停 | Shutdown → SessionEnd → drain(≤2s) → cancel → main 退出；launcher 等待 |
| 配置热重载 | `[hooks]`/信任文件显式排除监听 + 测试；需重启（提示） |
| 跨重启 seq | `base=epoch_ms`；同毫秒重启/时钟回拨为已知限制 |
| 信任后篡改 | `.visp/hooks/` 或规则文本变更 → 重新惰性化 |
| 项目级 `sh -c` | **禁止**（须在 `.visp/hooks/` 内可执行文件） |
| 队列溢出 | 按 `on_full`（状态型 coalesce_latest） |
| 递归 | `VISP_IN_HOOK=1` |
| Windows | 一期不测 |

---

## 15. 可观测性与运维入口

`visp hooks list/doctor/test/logs` + `--dry-run`；计数经 `GetHookStats`（daemon 只读 RPC）；运行日志不写终端。

---

## 16. 分期实施与验收标准（v3：拆分总线）

| 阶段 | 内容 | 验收 |
|---|---|---|
| **0. 契约冻结** | 冻结事件（PascalCase）+ 载荷 + env + schema + **JSON Schema + golden fixtures**（**仅覆盖已冻结事件**：SessionStart/UserPromptSubmit/AgentRunEnd/SubagentStop/Stop/StopFailure/ToolCallRequested/PreToolUse/PostToolUse/PostToolUseFailure/PermissionRequest/PermissionResult/SessionEnd） | 契约评审通过；schema 校验通过；不含执行器 |
| **1a. 总线（可独立交付）** | broadcast 替换 + sentinel receiver + 每连接 subscribe + 修 `.take()`/`break` + Lagged join 式 replay + 优雅关停管道 | **未接 hook 即验收**：无 hook 时用户可见行为不变；重连不再 `already taken`；无 TUI 时发布不报错；关停可 drain |
| **1b. 观察执行器** | daemon/agent 发射点落地；异步执行器（超时/白名单env/per-rule 串行/id 序/on_full）；`visp hooks ...`；内置 herdr 绑定 | hook 失败/超时**从不**阻塞主流程；项目 hook 惰性；`SessionStart` 首次判定正确 |
| **2. 信任 + 受控介入** | 项目 hook 信任（目录 + 规则全集快照）；可选 Pre 介入 | 未信任不执行；变更即失效；介入不违反超时 |
| **3. 生态** | `--format claude`；compact / Subagent 事件（若落地）；MCP-as-transport；热重载策略 | 按需评估 |

**验收共性**：`cargo test --workspace`、`cargo clippy --workspace -- -D warnings` 全绿；TDD 先红后绿。

---

## 17. 风险与未确认项（v3）

1. 通用脚本路径一期无真实外部消费者（§3.3 已接受为主动投入）。
2. 介入的 fail 语义（二期）。
3. 项目信任 UX（无头/CI 非交互信任途径）。
4. seq 跨重启在同毫秒重启/时钟回拨下不严格递增。
5. TUI 在线 `Lagged` 的 replay 开销与正确性需实测。
6. `Clone` 化 / `outcome` 新增的语义需单测锁定。
7. `tokio-stream` `sync` feature（BroadcastStream）需确认。
8. 内存：环形缓冲 = 容量 × 帧大小。
9. 命名对齐目标自身漂移（opencode 先例）。
10. 对齐不完整（无 transcript / 默认脱敏 / 无 exit 2）→ Claude 脚本不可直接复用。
11. **`Error` 语义收敛**：34 处 `AgentEvent::Error` 归并为 `StopFailure` 的判定规则需明确。
12. **优雅关停**：`McpManager::shutdown_all` **幂等性已由第三轮评审代码核实（`manager.rs:262-278`，可结案）**；仍需注意 `SessionEnd` 之后不再派发 hook；launcher 等待策略需实测。
13. **`PreToolUse` 新变体**的连带适配点（`event_to_msg`、`agent_event_to_server_message`）易漏。
14. 性能：每事件 spawn 未量化。
15. Windows 未验证。

---

## 18. 待讨论问题（真 open）

1. 无头/CI 的非交互信任途径。
2. `--format claude` 适配层是否立项与优先级。
3. 是否随包发布 herdr 参考脚本（供验证通用路径）。
4. `SubagentStart/Stop` 与 `PreCompact/PostCompact` 二期是否落地（后者语义尚不存在）。
5. `Error` → `StopFailure` 的收敛规则（哪些算致命）。

---

## 19. 裁定记录

### v1 评审（🔴×5 / 🟠×8 / 🟡×7）

| 编号 | 裁定 |
|---|---|
| 🔴-1 | D：broadcast 总线；oneshot→mpsc + `Clone`；seq base=epoch_ms；接受不回压 |
| 🔴-2 | 逐字段「具名消费者+产出点」；`PreToolUse` 二分；`kind` 补显式字段；删 `duration_ms`；`source∈{startup,resume}` |
| 🔴-3 | 影响表改：core/agent/daemon 有改动，删「visp-core 零改动」 |
| 🔴-4 | 信任 = `.visp/hooks/` 目录快照（**v3 再扩为规则全集**） |
| 🔴-5 | 建 `visp-hooks` crate + 诊断命令，明确主动投入 |
| 🟠-1 | 有界 drain ≤2s（**v3 补关停管道**） |
| 🟠-2 | 规则级 `on_full`；状态型 `coalesce_latest` |
| 🟠-3 | 按 id 排序 + 默认 per-rule 串行 |
| 🟠-4 | 环境白名单注入 |
| 🟠-5 | 不热重载 + 显式排除 + 提示 |
| 🟠-6 | 保持 `visp hooks`；新增只读 RPC |
| 🟠-7 | 保留命名、删除过度承诺 |
| 🟠-8 | `SessionStart` = 首次成功 `start_loop`；`source={startup,resume}` |
| 🟡-1..3,5,6 | 文档修正（can_accept / §18 拆分 / 命名统一 / seq 单位 / 一期不测） |
| 🟡-4 | seq 保留但标注可选 |
| 🟡-7 | phase 0 产出 JSON Schema + fixtures |

### v2 二轮评审（N1–N11）

| 编号 | 裁定 |
|---|---|
| N1 信任半道门 | **信任快照扩为「`.visp/hooks/` 目录 + 生效规则全集」+ 禁项目级 `sh -c`** |
| N2 关停无宿主 | **建优雅关停管道（D13）**：Shutdown → SessionEnd → drain → cancel → Notify → exit；launcher 等待 |
| N3 事件通路缺失 | **新增 §6.4 代码级事件通路表**（本轮） |
| N4 总线未解耦 | **phase 1 拆 1a（总线，可独立验收）/ 1b（执行器）** |
| N5 冻结 vs 未决字段 | **turn_id 删除；compact/subagent 移出一期契约** |
| N6 seq 与 herdr 冲突 | 保留 `base=epoch_ms` + 已知限制（可选字段）——**待确认是否需持久化高水位** |
| N7 TUI 无改动 vs Lagged | **Lagged 由 daemon 侧 join 式 replay**（TUI 保持无改动） |
| N8 无订阅者 send 报错 | **总线持 sentinel receiver** |
| N9 YAGNI overclaim | 收紧为「herdr 进程内绑定；脚本路径仅测试覆盖」 |
| N10 GetHookStats 欠优 | 维持只读 RPC（用户裁定） |
| N11 其它 | `ToolCallResult +outcome`；`global_tx` 行号修正；拒绝/取消不误算工具失败 |

### 本轮（v3）用户裁定

| 项 | 裁定 |
|---|---|
| SessionStart 宿主/source | orchestrator 首次 `start_loop`；`source` 由 `history` 空否推导 |
| 工具四态 | 加 `outcome` 枚举 |
| turn_id / compact | 删 turn_id；compact 留二期（不进一期契约） |
| SessionEnd 关停 | 建优雅关停管道 |
| Lagged | daemon 侧 join 式 replay |

### v3 第三轮（收敛性）评审 → 收口

第三轮评审判定「**修 3 项后可以进入实施计划**」（骨架成立、不需重做）。3 项已收口：

| 编号 | 问题 | 收口 |
|---|---|---|
| #1 | phase 0 冻结清单有 3 个无来源字段 | 补来源：`origin`（daemon 写入并透传）、`Stop.status ∈ {completed, cancelled}`、`SessionEnd.exit_code = Option<i32>` |
| #2 | `SessionStart`「首次」会误判（全局 bool / 子 agent 误触发） | 改为**该 session 在本次 daemon 内首次**（per-session 标记集合 + `parent_id.is_none()` 过滤） |
| #3 | `Stop` 双源，root 回合双触发 | 保留两条，**按「执行级（`AgentRunEnd`/`SubagentStop`，`agent_loop`）／会话级（`Stop`/`StopFailure`，orchestrator 清理后）」语义分层**；参考 opencode 2.0.9 的 execution 终结 + `sessionID`/`parentID` 寻址 |

**已由第三轮代码核实、可结案**：`PreToolUse` 插入点 `agent_loop.rs:1299↔1300` 覆盖全部执行路径；`cancel_tx` 闲置可用且不冲突；`McpManager::shutdown_all` **幂等**；`PermissionResult` 发布点基本单一。

**非阻塞项**（写入实施计划的验收/测试项）：信任目录 canonicalize / 递归快照 / 符号链接防逃逸；`outcome→is_error` 映射不变量；`PermissionResult` 仅 daemon map 命中时发布；`Lagged` 回放目标与去重；`SessionEnd` 后不再派发 hook；`Error→StopFailure` 收敛规则；launcher 描述纠正（真正改动在 daemon）；1a 的 broadcast 语义回归测试（多连接各自全量、订阅前事件会丢）。

---

## 20. 实现注记与遗留（v3.2，实现后回填）

> 实施已按本设计完成（30 个 TDD 提交；`cargo test --workspace` 2163 passed / 0 failed；clippy/fmt 全绿）。以下为落地时的**必要收口**与**遗留**。

### 20.1 已按实现确认的细化（与 v3 的差异）
1. 总线载荷 = **`BusEvent::{Frame(AgentEventFrame), Hook(HookEvent)}`**；`EventBus` 常持 sentinel receiver；seq 由**发布点**发放（`base = epoch_ms`）。
2. **会话级 `Stop`/`StopFailure` 由 orchestrator 以 `Hook` 发出**；根回合**重复的 `Done` 帧已移除**（TUI 的 `Done` 由主 agent_loop 帧独立送达，已验证仍恰一条）。执行级 `AgentRunEnd`/`SubagentStop` 由 daemon `hook_map` 从 `Done`/`Error` **帧**按 `parent_session_id` 分层派生。
3. `AgentEvent::UserQuery` 增显式 **`kind: PermissionKind{Approval,Question}`**；`hook_map` 从帧读取（不再靠字符串嗅探）。
4. `ToolCallResult` 增 **`outcome`**；`PostToolUse`/`PostToolUseFailure` 仅由**真实执行结果**产出，`Denied/Cancelled/Truncated` 不产出工具完成事件。
5. `PreToolUse` 发射点落在 `agent_loop` **审批后/执行前**（`agent_loop.rs:1311`）；拒绝/取消/截断路径不误发。
6. **零生效规则 → 不订阅总线、零开销**（含「仅有未信任项目规则」）。
7. 关停：`Shutdown` RPC → `SessionEnd`（daemon 级）→ 有界 drain(≤2s) → `cancel_tx` → `Notify` → main `select!{ctrl_c,RPC,SIGTERM}`。
8. `Lagged` → daemon 侧 join 式**增量** replay（水位去重 + 500ms 节流）；**TUI 无改动**。
9. 新增只读 **`GetHookStats`** RPC（**proto 变更**）；`visp hooks list/doctor/test/logs`（`test` 恒 dry-run）。
10. **herdr 集成已回退为外置脚本**（`assets/hooks/herdr.hook.sh` + 一条 `[hooks]` 规则，用户按文档自装）；曾短暂实现的进程内 `HerdrBinding`/`CompositeHandler`/daemon 注册**已全部移除**（`f45c6890`），visp 内**无 herdr 专属 Rust**。

### 20.2 遗留 / 与设计的偏差（详见 `docs/todo/hooks-followups.md`）
- （已消解）曾计划的 `[herdr]` 配置节**不再需要**：herdr 走外置脚本后，「开关」即是否在 `daemon.toml` 配置该规则。
- **`2b`（受控 Pre 介入）未实现**（fail-open/closed 仍为开放问题）。
- `HookRule→DispatchRule` 适配在 daemon 与 launcher **各有一份（约 15 行重复）**，建议下沉到 `visp-hooks`。
- `dropped` 计数**不含** executor 内部 `on_full` 丢弃（缺可观测出口）。
- 关停 `SessionEnd` 为 **daemon 级**（`session_id=""`），未按活跃会话细化。
- `PermissionResult.outcome` 由 `selected_index`/`text` **推断**（协议无显式 cancel 标志）。
- **seq 跨重启**为 `epoch_ms` 基数（同毫秒重启 / 时钟回拨不严格递增）。
- **Windows 未验证**。
- `PermissionResult` 的 `source/cwd` 取 daemon 缺省（未回查会话 `project_path`）。
