# visp 工作计划：通用可配置 lifecycle hook 系统（TDD 实施）

> 状态：**草案，待用户审核批准后方可执行**
> 日期：2026-09-28
> 依据设计：`docs/design/2026-09-27-visp-hooks-design.md`（**v3 定稿**，吸收三轮评审）
> 质量门：`cargo test --workspace`、`cargo clippy --workspace -- -D warnings`、`cargo fmt -- --check`
> 分期映射：Phase 0 契约冻结 → Phase 1a 总线（可独立验收）→ Phase 1b 观察执行器 → Phase 2 信任 + 受控介入 → Phase 3（仅登记 TODO）

## 概述

按设计 v3 实施 hook 系统，共 **5 个阶段、17 个子步骤、约 132 条测试用例**：

1. **Phase 0 契约冻结**（步骤 0）：新建 `visp-hooks` crate 承载事件契约（PascalCase 事件名 + 载荷 + env + schema），产出 **JSON Schema + golden fixtures**（仅覆盖 13 个已冻结事件）；并在 `visp-core` 前置「类型级」硬约束（`Clone`、`UserQuery.respond` oneshot→mpsc、`ToolCallResult +outcome`、新增 `PreToolUse` 变体）。**不含执行器**。
2. **Phase 1a 总线**（步骤 1）：`broadcast` 替换 `orchestrator_grpc_tx/rx` + sentinel receiver + 每连接 `subscribe()` + 修 `.take()`/`break` + `Lagged` join 式 replay + 优雅关停管道。**未接 hook 即可独立验收**。
3. **Phase 1b 观察执行器**（步骤 2）：各产出点发射事件；`visp-hooks` 执行器（匹配/id 序/per-rule 串行/`on_full`/白名单 env/超时杀树）；`visp hooks list/doctor/test/logs` + 只读 `GetHookStats`；内置 `builtin:herdr.*`。
4. **Phase 2 信任 + 受控介入**（步骤 3）：项目级信任（目录递归快照 + 规则全集 + 符号链接防逃逸 + 禁项目级 `sh -c`）；可选 Pre 介入（仅信任规则、显式开启）。
5. **收尾**（步骤 4）：全量回归 + 未配置 hook「逐字节不变」对照 + 验收映射 + 手工冒烟。

### 关键落位决策（实施前约定，来自设计非显然项）

1. **类型/接口先行**：`Clone`、`respond` mpsc、`outcome`、`PreToolUse` 变体全部在 Phase 0 完成，避免 1a/1b 反复改签名。
2. **`outcome→is_error` 是不变量而非重构**：`denied/cancelled/truncated` 必须仍映射 `is_error=true`，**TUI 可见行为不变**；改前需先锁定「四处构造点」的黄金断言。
3. **`PreToolUse` 不覆盖拒绝/取消/截断路径**：插在审批后、执行前；`ToolCallRequested` 才覆盖审批前。
4. **`PermissionResult` 只在 daemon map 命中点发布**，不走 orchestrator 回退路径；`PermissionRequest` 锚到总线 `UserQuery` 帧。
5. **`Stop` 双源分层**：执行级（`AgentRunEnd`/`SubagentStop`，`agent_loop`）**先**；会话级（`Stop`/`StopFailure`，orchestrator 清理置 Idle **之后**）**后**。
6. **`SessionStart` 首次判定** = per-session 标记集合 + `parent_id.is_none()`；同一 daemon 内第二个主会话仍发，子 agent 不发。
7. **`SessionEnd` 之后不再派发任何 hook 事件**（终止态抑制）。
8. **1a broadcast 语义差异必须显式测**：多连接各自全量；**订阅前发布的事件会丢**（与 mpsc 的差异）。
9. **`[hooks]`/`.visp/hooks/` 显式排除监听**：现状「天然不监听」需固化为显式行为 + 测试（D11）。
10. **行号会漂移**：设计中的 `orchestrator.rs:542`/`:846` 等为评审快照，现 `orchestrator.rs` 位于 `crates/visp-agent/src/`；实施以 grep/读源码重新定位为准。

---

## 步骤 0：Phase 0 契约冻结

### 0a：`visp-hooks` 契约 crate + 事件模型（类型先行）

新 crate `crates/visp-hooks`（加入 workspace members），仅含契约层：13 个冻结事件的枚举/常量、每事件载荷结构、公共信封字段（`schema`/`hook_event_name`/`session_id`/`cwd`/`source`/`origin`/可选 `seq`）、env 变量名常量、`VISP_HOOK_SCHEMA = 1` 常量。**零执行逻辑、零 IO**；依赖仅 `serde`/`serde_json`。

#### 🔴 红 — 测试
| # | 测试用例 |
|---|---|
| 1 | 13 个冻结事件名逐一序列化为精确 PascalCase 字符串（SessionStart/UserPromptSubmit/AgentRunEnd/SubagentStop/Stop/StopFailure/ToolCallRequested/PreToolUse/PostToolUse/PostToolUseFailure/PermissionRequest/PermissionResult/SessionEnd） |
| 2 | 公共信封字段必现：`schema`/`hook_event_name`/`session_id`/`cwd`/`source`/`origin`；`seq` 为 `None` 时省略（不输出 null） |
| 3 | 枚举取值域：`source ∈ {startup,resume}`；`origin ∈ {tui,headless,other}`；`Stop.status ∈ {completed,cancelled}`；`SessionEnd.exit_code` 为 `Option`（`None` 时省略） |
| 4 | 每事件载荷字段集合精确（不缺不多），逐事件按设计 §6.2 表断言 |
| 5 | 默认脱敏：`UserPromptSubmit` 仅 `prompt_chars`（无 `prompt`）；`PermissionRequest` 仅 `options_count`（无 `message`）；工具事件无 `tool_input`/`tool_response` |
| 6 | schema 版本常量 = 1 且随每条事件 JSON 输出 |
| 7 | 未冻结事件（`PreCompact`/`PostCompact`/`SubagentStart`/`turn_id`）不属于契约模型（构造/反序列化均不可达） |

#### 🟢 绿 — 实现
定义契约 crate 骨架与事件模型；事件名以常量/枚举单源；公共信封抽为可复用的 context 结构；脱敏字段在类型层控制（默认不携带原文）。不含任何执行/匹配/配置逻辑。

#### 🧪 测试 → 🔍 类型检查
`cargo test -p visp-hooks` / `cargo clippy -p visp-hooks -- -D warnings` / `cargo fmt -- --check`

#### ♻️ 重构
无（新 crate）。

#### 📦 提交
`feat(hooks): visp-hooks contract crate with frozen event model and env constants`

### 0b：JSON Schema + golden fixtures

在 `visp-hooks` 内产出 `schema/visp-hook-event.v1.schema.json` 与 `tests/fixtures/*.json`（13 个事件各一份，含 happy 样本）。测试以 schema 校验 fixture 并回环反序列化。**schema 校验依赖需显式引入**（建议 `jsonschema` crate 作为 dev-dependency，需在 Cargo 中锁版本）。

#### 🔴 红 — 测试
| # | 测试用例 |
|---|---|
| 1 | 13 个 golden fixture 各自通过 JSON Schema 校验 |
| 2 | fixture 反序列化为契约模型后再序列化，与原文稳定一致（幂等） |
| 3 | schema 顶层事件名枚举恰好覆盖 13 个冻结事件（无多余、无缺失） |
| 4 | 非法 fixture（缺 `hook_event_name` / `source` 越界 / `schema≠1`）被 schema 拒绝 |
| 5 | 每个 fixture 的必填公共字段与事件专属字段均存在（结构化断言） |

#### 🟢 绿 — 实现
以契约为单一来源产出 schema 与 fixtures；fixtures 手工评审为「契约样本」。不改动 0a 模型（若发现字段不可序列化则回改 0a，属发现性重构）。

#### 🧪 测试 → 🔍 类型检查
同 0a

#### ♻️ 重构
若 schema 与模型出现双写漂移风险，将「字段清单」抽为单一表驱动生成（可选，仅在重复出现时）。

#### 📦 提交
`feat(hooks): JSON Schema and golden fixtures for frozen hook events`

### 0c-1：`visp-core` 类型前置 —— Clone + respond→mpsc（**挡住总线**）

`AgentEvent`/`AgentEventFrame` derive `Clone`；`UserQuery.respond`（`AgentEvent` 与 `AgentMessage` 两处）oneshot → `mpsc::Sender`；`AgentMessage::UserQuery` 保持**不接通**。仅此两项，**不牵涉 outcome/PreToolUse**，以尽早解锁 Wave 1 的总线工作。

#### 🔴 红 — 测试
| # | 测试用例 |
|---|---|
| 1 | `AgentEventFrame` 克隆后 `event`/`session_id`/`agent_name`/`parent_*` 全等；`mpsc::Sender` 可克隆 |
| 2 | `UserQuery.respond` 改 mpsc 后，既有「拒绝 / Always Allow / 超时视为拒绝」三类 agent_loop 测试不回归 |

#### 🟢 绿 — 实现
仅改通道类型 + derive `Clone`；不动 `ToolCallResult`/`AgentEvent` 变体集合。

#### 🧪 测试 → 🔍 类型检查
`cargo test -p visp-core` / `cargo clippy -p visp-core -- -D warnings` / `cargo fmt -- --check`

#### ♻️ 重构
无。

#### 📦 提交
`refactor(core): cloneable agent frames and mpsc user-query respond`

### 0c-2：`visp-core` 类型前置 —— outcome + PreToolUse（**不挡总线**）

`ToolCallResult` 增 `outcome: ToolOutcome`（`Success/Failure/Denied/Cancelled/Truncated`）与 `outcome→is_error` 单一映射；新增 `AgentEvent::PreToolUse { call_id, tool_name, requires_approval }`，并连带适配 `event_to_msg`（`agent_loop.rs` 顶部）与 daemon `agent_event_to_server_message`。

#### 🔴 红 — 测试
| # | 测试用例 |
|---|---|
| 1 | `ToolOutcome` 五值齐全且可序列化/映射 |
| 2 | **映射不变量**：`Success→is_error=false`；`Failure/Denied/Cancelled/Truncated→is_error=true` |
| 3 | 黄金回归：取消/拒绝/截断/真实结果四处构造点的 `is_error` 与改造前一致（**TUI 可见行为不变**） |
| 4 | `PreToolUse` 变体经 `event_to_msg` 与 `agent_event_to_server_message` 的适配路径可达/或显式 None（二选一并锁定） |

#### 🟢 绿 — 实现
扩展枚举/字段；四处构造点显式标注 `outcome`，不改变任何 `is_error` 取值路径。

#### 🧪 测试 → 🔍 类型检查
`cargo test -p visp-core` / `cargo clippy -p visp-core -- -D warnings` / `cargo fmt -- --check`

#### ♻️ 重构
把四处 `is_error` 布尔字面量收敛为 `outcome.into_is_error()`，消除「字符串区分四态」的残留。

#### 📦 提交
`refactor(core): ToolOutcome, is_error mapping and PreToolUse variant`

---

## 步骤 1：Phase 1a 事件总线（可独立验收）

### 1a-1：总线抽象（broadcast + seq + sentinel）

替换 `crates/visp-daemon/src/main.rs:554-555` 的 `orchestrator_grpc_tx/rx` 为 bus 结构：容量 **1024** 的 `broadcast::Sender<BusEnvelope>`、单一 `AtomicU64` seq（`base=epoch_ms`）、**常持 sentinel `Receiver`**。`global_tx` 不动。`Orchestrator::new` 的 `grpc_tx` 参数改为 bus 发布句柄；reload/watch downlink 同步改造。

#### 🔴 红 — 测试
| # | 测试用例 |
|---|---|
| 1 | 多订阅者各自收到全量帧（2 个订阅者收到相同 N 帧、顺序一致） |
| 2 | **订阅前发布的事件会丢**（先 publish 后 subscribe → 收不到；与 mpsc 差异显式断言） |
| 3 | 无订阅者时 publish 不报错（sentinel receiver 兜底） |
| 4 | 发布任务不因 send 结果 `break`（lag/closed 下发布循环继续） |
| 5 | seq 单调严格递增；`base ≈ epoch_ms`；同一 bus 内无重复 |
| 6 | 容量满 → 订阅者收到 `Lagged(n)`，发布端**不阻塞** |

#### 🟢 绿 — 实现
实现 bus 结构体（publish/subscribe/seq）；发布 API 无返回值可致 break；sentinel 在结构体创建时即持有。仅替换显示面通道，控制面 `global_tx` 不变。

#### 🧪 测试 → 🔍 类型检查
`cargo test -p visp-daemon` / `cargo clippy -p visp-daemon -- -D warnings` / `cargo fmt -- --check`

#### ♻️ 重构
把 `AgentEventFrame` 包装为 `BusEnvelope`（含 seq）时，保留既有 proto 转换入口单点。

#### 📦 提交
`feat(daemon): broadcast event bus with monotonic seq and sentinel receiver`

### 1a-2：每连接 `subscribe()` + 移除 `.take()`/`break`

`chat()` 改为在连接内 `bus.subscribe()`（移除 `service.rs:429-434` 的 `.take()` 与 `"already taken"` 错误）；连接结束即 drop receiver。各转发任务（`orchestrator.rs:534-555`/`:833-859`）与 daemon 发布点移除 `break`。

#### 🔴 红 — 测试
| # | 测试用例 |
|---|---|
| 1 | 重连不再 `already taken`：连续两次 `chat` 均成功，断开后可重连续流 |
| 2 | 多连接并发各自收到全量帧（语义与 1a-1 一致） |
| 3 | 某连接结束 drop receiver 不影响其它连接与发布 |
| 4 | 转发任务在无订阅者时仍不 `break`（发布不失败） |
| 5 | 回归：单连接既有 Chat 行为（TextDelta/Done/UserQuery）结构不变 |

#### 🟢 绿 — 实现
每连接独立 subscribe；`chat` 不再持有全局单例 receiver；去掉 `already taken` 分支；转发任务对发布结果不退出。

#### 🧪 测试 → 🔍 类型检查
同 1a-1

#### ♻️ 重构
删除为「单消费者」存在的 receiver 锁/`Mutex<Option<Receiver>>` 死代码（仅限本次改动产生的）。

#### 📦 提交
`fix(daemon): per-connection bus subscription replacing taken receiver`

### 1a-3：`Lagged` → daemon join 式 replay（目标 session + 去重）

daemon outbound 在 `Lagged(n)` 时对**该连接正在查看的目标 session** 触发一次 `replay_session_history`，并做去重/节流，避免重复回放与风暴。TUI 保持无改动。

#### 🔴 红 — 测试
| # | 测试用例 |
|---|---|
| 1 | `Lagged` 触发一次 join 式 replay，目标 session 正确（非全局广播） |
| 2 | 连续 `Lagged` 去重（风暴下不重复回放，有节流窗口/标记） |
| 3 | replay 帧与已收帧去重，不产生重复 |
| 4 | 非 `Lagged` 路径不触发 replay（回归） |
| 5 | 断言改动仅在 daemon outbound（TUI 侧零改动） |

#### 🟢 绿 — 实现
outbound 收 `Lagged` → 记录去重标记 → 复用 `replay_session_history`；失败仅日志。

#### 🧪 测试 → 🔍 类型检查
同 1a-1

#### ♻️ 重构
复用既有 `send_join` 重同步入口，避免两条重放路径漂移。

#### 📦 提交
`feat(daemon): join-style replay for lagged outbound subscribers`

### 1a-4a：launcher 关停等待策略（`crates/visp`）

把 launcher 的「5s 后 SIGKILL」（`crates/visp/src/main.rs:178-186`）改为「等待 daemon 自行退出 + 超时兜底」。

#### 🔴 红 — 测试
| # | 测试用例 |
|---|---|
| 1 | daemon 在超时内自行退出 → launcher 不 kill |
| 2 | 超时仍未退出 → 兜底 kill（保底不悬挂） |
| 3 | 启动失败路径仍执行 kill（回归） |
| 4 | `send_shutdown` 失败时仍进入等待/兜底，不 panic |

#### 🟢 绿 — 实现
调整等待逻辑与超时兜底；不改 gRPC 交互。

#### 🧪 测试 → 🔍 类型检查
`cargo test -p visp` / `cargo clippy -p visp -- -D warnings` / `cargo fmt -- --check`

#### ♻️ 重构
无。

#### 📦 提交
`fix(launcher): wait for graceful daemon exit with timeout fallback`

### 1a-4b：daemon 优雅关停管道

实现设计 D13：`Shutdown` RPC → 发 bus `SessionEnd`（1a 阶段以可注入的 drain host 桩承接，真实 drain 在 1b 接执行器）→ 有界 drain（≤2s）→ `cancel_tx` 发 `CancelSignal` → `Notify` 唤醒 daemon main → main 改 `select!{ctrl_c, shutdown, SIGTERM}` → 既有清理（`watcher.stop`/`mcp.shutdown_all`/`server.abort`）。`shutdown_all` 已确认幂等。

#### 🔴 红 — 测试
| # | 测试用例 |
|---|---|
| 1 | `Shutdown` RPC → `cancel_tx` 收到 `CancelSignal` → orchestrator `run()` 退出 |
| 2 | daemon main 被 `Notify` 唤醒并执行清理三步（watcher.stop / mcp.shutdown_all / server.abort） |
| 3 | `SIGTERM` 与 `ctrl_c` 走同一路径 |
| 4 | drain 有界：超过 ≤2s 仍继续退出，不悬挂 |
| 5 | `mcp.shutdown_all` 双调幂等（无副作用） |
| 6 | **`SessionEnd` 之后不再派发 hook 事件**（drain host 计数断言） |
| 7 | 无 hook 时关停用户可见行为不变（daemon 正常退出、launcher 不 kill） |

#### 🟢 绿 — 实现
串起 Shutdown→SessionEnd→drain 桩→cancel→Notify→main；main select 纳入 SIGTERM；drain 上限固定。真实执行器留到 1b 接线。

#### 🧪 测试 → 🔍 类型检查
`cargo test -p visp-daemon` / `cargo clippy -p visp-daemon -- -D warnings` / `cargo fmt -- --check`

#### ♻️ 重构
关停清理收敛为单一函数，供 ctrl_c/SIGTERM/Shutdown 三入口共用。

#### 📦 提交
`feat(daemon): graceful shutdown pipeline with bounded drain and cancel signal`

---

## 步骤 2：Phase 1b 观察执行器

### 1b-0：`[hooks]` 配置 + 规则模型 + watcher 显式排除

`visp-config`：新增 `[hooks]` 节与 `HookRule` 反序列化（字段见设计 §7.2）、全局 + 项目合并逻辑（项目 scope 标注）、`.visp/hooks/` 路径 helper、`[hooks]`/信任文件**显式排除**于 daemon 监听计划。

#### 🔴 红 — 测试
| # | 测试用例 |
|---|---|
| 1 | `[hooks]` 全字段解析（id/order/event/matcher/command/args/env/cwd/timeout_ms/enabled/on_full/parallel/cooldown_ms/include/scope） |
| 2 | 缺省无 `[hooks]` → 空规则集且不报错 |
| 3 | 项目级合并：同 id 项目覆盖全局；新增追加；`scope` 标注正确 |
| 4 | 非法规则（未知 event / 缺 command / 重复 id）→ 校验失败且信息明确 |
| 5 | `on_full` 默认 `drop_new`；`timeout_ms` 默认值锁定 |
| 6 | watcher 排除：写 `.visp/hooks/x.sh` 或改 `[hooks]` **不触发**热重载（D11 固化） |
| 7 | 未配置 hook 时配置加载路径行为不变（回归） |

#### 🟢 绿 — 实现
配置反序列化 + 合并 + 校验；监听计划显式排除 hooks 相关路径；加载失败降级为空规则集 + 警告。

#### 🧪 测试 → 🔍 类型检查
`cargo test -p visp-config -p visp-daemon` / `cargo clippy -p visp-config -p visp-daemon -- -D warnings` / `cargo fmt -- --check`

#### ♻️ 重构
规则校验与 `scope` 推导抽为单一入口，供 `hooks doctor` 复用。

#### 📦 提交
`feat(config): [hooks] section, rule model and explicit watcher exclusion`

### 1b-1：事件发射点（观察）

按设计 §6.4 在各产出点发射。宿主：orchestrator（SessionStart/UserPromptSubmit/Stop/StopFailure）、转发任务（AgentRunEnd/SubagentStop/PermissionRequest）、agent_loop（ToolCallRequested/PreToolUse/PostToolUse/PostToolUseFailure）、daemon（PermissionResult/SessionEnd）。

> **可拆 3 个工作流**（文件域不同、接口冻结后可并行）：**1b-1a** `visp-core`/`agent_loop` 工具事件（含 `outcome` 分流）；**1b-1b** `visp-agent`/`orchestrator` 生命周期 + 转发任务；**1b-1c** `visp-daemon` `PermissionResult`/`SessionEnd`（受 `service.rs` 单写者约束）。下方测试用例按此三类分组标注。

#### 🔴 红 — 测试
| # | 测试用例 |
|---|---|
| 1 | 主 session 首次 `start_loop` 成功 → 发一次 `SessionStart` |
| 2 | 同 session 第二回合 `start_loop` → 不再发（per-session 标记集合） |
| 3 | 同一 daemon 内第二个新建主会话 → 仍发 `SessionStart` |
| 4 | 子 agent `start_loop` → 不发（`parent_id.is_none()` 过滤） |
| 5 | `source`：绑定时刻 history 空 → `startup`；非空 → `resume` |
| 6 | `origin`：daemon 透传 `tui`；`headless/other` 预留可达 |
| 7 | `start_loop` 失败 → 不发 `SessionStart` |
| 8 | `UserPromptSubmit`：主 agent 受理且 `start_loop` 成功 → 发一次 |
| 9 | 子 session / 被拒输入 → 不发 `UserPromptSubmit` |
| 10 | `prompt_chars` 为字符数；默认无 `prompt`，显式 `include` 才带原文 |
| 11 | 主 agent `Done` → `AgentRunEnd`（`parent_id=None`，含 status） |
| 12 | 子 agent `Done`/`Error` → `SubagentStop`（`parent_id=Some`，含 agent_name/status） |
| 13 | 执行级先于会话级：同一回合 `AgentRunEnd` 早于 `Stop` |
| 14 | 子 agent 不产生会话级 `Stop`（仅 root） |
| 15 | root `Done` → `Stop`（在置 Idle 之后），`status=completed`，tokens/tool_calls 取同回合最近 `UsageInfo` |
| 16 | 取消导致 Done → `Stop(status=cancelled)`（cancel 标记判定） |
| 17 | 真 `Error` → `StopFailure`（语义收敛后，含 code/message） |
| 18 | `Error→StopFailure` 收敛：用户取消不误入 `StopFailure` |
| 19 | 双源分层不重复：root 回合 `AgentRunEnd`+`Stop` 各一次，语义不同 |
| 20 | 子 agent Error → 执行级 `SubagentStop`，父继续；父失败才出 `StopFailure` |
| 21 | `ToolCallRequested` 在审批**前**发出（被拒也发，记录请求事实） |
| 22 | `PreToolUse` 覆盖真实执行路径：无审批 / 审批通过 / Always Allow / 子 agent |
| 23 | `PreToolUse` **不误发**：拒绝路径 / cancel 路径 / 参数截断路径均不发 |
| 24 | `PostToolUse`：真实结果 `outcome=success` → 发 |
| 25 | `PostToolUseFailure`：真实结果 `outcome=failure` → 发（含 message） |
| 26 | 四态其余（denied/cancelled/truncated）**不误发**工具完成事件 |
| 27 | `PermissionRequest` 由总线 `UserQuery` 帧触发，`kind`/`options_count` 正确 |
| 28 | `PermissionRequest.message` 默认脱敏（仅显式 include 才带） |
| 29 | `PermissionResult` **仅在 daemon map 命中点**发布一次 |
| 30 | `PermissionResult` 不走 orchestrator 回退路径发布（避免过期/重复响应误发） |
| 31 | 重复响应不重复发布 `PermissionResult`；`query_id` 关联正确 |
| 32 | `delete_session` → `SessionEnd`（reason=delete，exit_code=None） |
| 33 | 优雅关停 → `SessionEnd`（reason=shutdown，exit_code=Some(0)） |
| 34 | `SessionEnd` 之后不再派发任何 hook 事件（终止态抑制） |
| 35 | 异常退出无 `SessionEnd`（标注手工/不可自动断言） |

#### 🟢 绿 — 实现
在各产出点把事实转换为契约事件并发布到总线；`SessionStart` 用 per-session 标记集合 + 父过滤；`Stop`/`StopFailure` 用收敛后的语义；工具事件按 `outcome` 分流；`PermissionRequest` 改为锚总线 `UserQuery` 帧；`PermissionResult` 只在 daemon 路由成功点发。

#### 🧪 测试 → 🔍 类型检查
`cargo test -p visp-agent -p visp-daemon -p visp-core` / `cargo clippy -p visp-agent -p visp-daemon -p visp-core -- -D warnings` / `cargo fmt -- --check`

#### ♻️ 重构
把「事件构造 + 发布」抽为发射 helper，避免各产出点重复拼装；`Stop` 取值（tokens/tool_calls）收敛为单次查询。

#### 📦 提交
`feat(hooks): emit lifecycle events at orchestrator, agent_loop and daemon seams`

### 1b-2：执行器核心（`visp-hooks`）

规则匹配（event + matcher 正则匹配 `tool_name`/`source`/`kind`）、同事件多规则按 `id` 字典序（`order` 覆盖）、per-rule 串行 + 跨规则并发（`parallel=true` 放开）、`on_full`（`drop_new`/`drop_old`/`coalesce_latest`）、`cooldown_ms`、强制 `timeout_ms` + 杀进程树、白名单 env、stdin JSON 写完即关、`cwd=project_path`、`VISP_IN_HOOK` 防递归、失败隔离（仅日志 + 计数）。

#### 🔴 红 — 测试
| # | 测试用例 |
|---|---|
| 1 | 匹配：event 命中/未命中；matcher 命中 `tool_name`/`source`/`kind`；空 matcher 全匹配 |
| 2 | 排序：同事件多规则按 `id` 字典序执行；`order` 覆盖；内置 `builtin:` 前缀参与排序 |
| 3 | 串行：同规则连续事件按序执行（不并发交错） |
| 4 | `parallel=true` 放开同规则并发 |
| 5 | `on_full`：`drop_new` 丢新 / `drop_old` 丢旧 / `coalesce_latest` 合并为最新 |
| 6 | `cooldown_ms`：窗口内事件被丢弃 |
| 7 | 超时：超 `timeout_ms` 杀进程，**含孙进程**（进程组/树终止验证） |
| 8 | 白名单 env：子进程仅见白名单 + 规则 `env`；父进程秘钥不可见 |
| 9 | stdin：完整 JSON 写入后关闭；大载荷边界不截断 |
| 10 | `cwd` = canonical `project_path` |
| 11 | `VISP_IN_HOOK=1` 防递归：hook 内再触发被抑制 |
| 12 | 失败隔离：脚本崩溃/非零退出/无法启动 → 仅计数 + 日志，主流程不受影响 |

#### 🟢 绿 — 实现
在 `visp-hooks` 内实现执行器（纯决策 + 进程 spawn 分离，便于单测）；每规则独立子进程；状态型队列支持 coalesce；所有外部失败不回写调用方。

#### 🧪 测试 → 🔍 类型检查
`cargo test -p visp-hooks` / `cargo clippy -p visp-hooks -- -D warnings` / `cargo fmt -- --check`

#### ♻️ 重构
匹配与排序抽为纯函数；队列策略枚举化，避免 `on_full` 分支散落。

#### 📦 提交
`feat(hooks): async hook executor with matching, ordering, on_full and sandboxed env`

### 1b-3：`visp hooks` CLI + 只读 `GetHookStats` RPC

`visp-proto` 新增只读 `GetHookStats`；daemon 暴露计数（`emitted/dropped/executed/failed/timed_out`）与生效规则列表；launcher 新增 `visp hooks list/doctor/test/logs` 与 `--dry-run`。

> **可拆 2 个工作流**（proto 契约定后并行）：**1b-3a** `visp-proto` + daemon RPC/计数/规则快照；**1b-3b** launcher CLI 子命令（`crates/visp`）。

#### 🔴 红 — 测试
| # | 测试用例 |
|---|---|
| 1 | proto `GetHookStats` 请求/响应可构造且 round-trip |
| 2 | 计数随事件/执行正确增长（五类计数） |
| 3 | `hooks list` 输出规则（含 scope/trusted 状态） |
| 4 | `hooks doctor` 检出：配置非法 / 项目未信任 / 项目级命令不在 `.visp/hooks/` 内 |
| 5 | `hooks test <event>` 以样本载荷运行并输出 stdout/exit_code；`--dry-run` 不执行 |
| 6 | `hooks logs` 读取 hook 运行日志（不写终端） |
| 7 | 只读 RPC 不影响既有 RPC 语义（回归） |

#### 🟢 绿 — 实现
proto 扩展 + daemon 计数与规则快照 + CLI 子命令；`doctor`/`test` 复用 1b-0 校验与 1b-2 执行器（dry-run 只做匹配与打印）。

#### 🧪 测试 → 🔍 类型检查
`cargo test -p visp-proto -p visp-daemon -p visp` / `cargo clippy -p visp-proto -p visp-daemon -p visp -- -D warnings` / `cargo fmt -- --check`

#### ♻️ 重构
规则快照与计数抽为 daemon 单点，CLI 只读消费。

#### 📦 提交
`feat(hooks): visp hooks list/doctor/test/logs and readonly GetHookStats rpc`

### 1b-4：内置 `builtin:herdr.*`

herdr 作为进程内消费者，经与脚本规则**同一 executor 接口**分发，受 `[herdr]` 门控。

#### 🔴 红 — 测试
| # | 测试用例 |
|---|---|
| 1 | 仅 `[herdr]` 开启时注册绑定 |
| 2 | `SessionStart` 触发 herdr 上报（进程内，无子进程） |
| 3 | 与脚本规则共用分发路径（同一接口） |
| 4 | herdr 执行失败不影响其它规则/主流程 |
| 5 | herdr 关闭时零行为 |

#### 🟢 绿 — 实现
在 daemon 启动时按 `[herdr]` 注册内置绑定；复用执行器的匹配/分发，不新建并行机制。

#### 🧪 测试 → 🔍 类型检查
`cargo test -p visp-hooks -p visp-daemon` / `cargo clippy -p visp-hooks -p visp-daemon -- -D warnings` / `cargo fmt -- --check`

#### ♻️ 重构
内置与脚本消费者在接口层统一，避免 herdr 特化分支。

#### 📦 提交
`feat(hooks): builtin herdr consumer over shared executor interface`

---

## 步骤 3：Phase 2 信任 + 受控介入

### 2a：项目级信任（目录快照 + 规则全集 + 符号链接防逃逸）

信任存储位于全局数据目录（不写入仓库）：canonical 项目路径 + `.visp/hooks/` **递归快照** + 生效项目级规则全集（command/args/env）。**禁止项目级 `sh -c`**：`command` 必须解析到 `.visp/hooks/` 内可执行文件。

#### 🔴 红 — 测试
| # | 测试用例 |
|---|---|
| 1 | canonicalize：信任绑定 canonical 路径（symlink/`..` 解析一致） |
| 2 | 递归快照：`.visp/hooks/` 内新增/修改/删除文件 → 快照变化 → 信任失效 |
| 3 | 规则全集快照：`.visp/daemon.toml` `[hooks]` 的 command/args/env 变更 → 失效 |
| 4 | 符号链接防逃逸：`.visp/hooks/` 内 symlink 指向目录外可执行 → 拒绝 |
| 5 | 项目级 `sh -c`：`command=sh` + `args=["-c",...]` → 拒绝（即使已信任） |
| 6 | canonicalize 失败（路径不存在/权限）→ 保守判为不信任 |
| 7 | 未信任项目 hook 默认不运行（惰性） |
| 8 | 信任存储不写入仓库（断言位置为全局数据目录） |
| 9 | 全局 hook 默认可运行（回归） |
| 10 | 未配置项目 hook 时行为不变 |

#### 🟢 绿 — 实现
信任记录结构与校验（canonical + 快照哈希）；项目级路径约束与 `sh -c` 拒绝；失效即重新惰性化。

#### 🧪 测试 → 🔍 类型检查
`cargo test -p visp-config -p visp-hooks` / `cargo clippy -p visp-config -p visp-hooks -- -D warnings` / `cargo fmt -- --check`

#### ♻️ 重构
canonical/快照校验抽为单一 `trust::verify(project)`，供执行前与 `doctor` 共用。

#### 📦 提交
`feat(hooks): project-level trust with recursive snapshot and symlink escape guard`

### 2b：可选 Pre 介入（仅信任规则 + 显式开启）

设计列为 Phase 2 可选能力（fail 语义为开放问题），此处仅落最小闭环；未落地部分登记 TODO。

#### 🔴 红 — 测试
| # | 测试用例 |
|---|---|
| 1 | 仅信任规则可参与介入 |
| 2 | 必须显式开启（默认关闭，关闭时无行为变化） |
| 3 | 超时按策略 fail-open/closed |
| 4 | 介入不违反主循环超时（有界等待） |
| 5 | 未开启介入时与 Phase 1b 观察行为逐字段一致（回归） |

#### 🟢 绿 — 实现
在 PreToolUse 处增加可选的同步 gate（默认关闭）；仅信任规则；超时受主循环约束。

#### 🧪 测试 → 🔍 类型检查
`cargo test -p visp-hooks -p visp-core` / `cargo clippy -p visp-hooks -p visp-core -- -D warnings` / `cargo fmt -- --check`

#### ♻️ 重构
介入与观察共用规则匹配与前缀校验，避免两套判定。

#### 📦 提交
`feat(hooks): opt-in trusted pre-tool intervention gate`

---

## 步骤 4：收尾与验收

### R1：全量回归 + 未配置不变 + 验收映射

#### 🔴 红 — 测试
| # | 测试用例 |
|---|---|
| 1 | `cargo test --workspace` 全绿，既有测试零回归 |
| 2 | `cargo clippy --workspace -- -D warnings`、`cargo fmt -- --check` 全绿 |
| 3 | **未配置 hook 时用户可见行为逐字节不变**：空 `[hooks]` 下总线帧序列与基线对照一致 |
| 4 | 总线语义回归复核：多连接各自全量、订阅前事件会丢（1a 结论在集成层复现） |
| 5 | 设计 §16 验收逐条映射到「被哪个子步骤测试覆盖 / 需手工验收」 |
| 6 | 手工冒烟：herdr 上报、Shutdown 优雅退出与 launcher 等待、`hooks doctor` 提示重启 |

#### 🟢 绿 — 实现
仅修复回归暴露的问题（各自小提交）；无新增功能。

#### 🧪 测试 → 🔍 类型检查
`cargo test --workspace` / `cargo clippy --workspace -- -D warnings` / `cargo fmt -- --check`

#### ♻️ 重构
无。

#### 📦 提交
`chore: full regression and no-hook behavioral parity for visp hooks`

---

## Wave 并行策略（修订版：峰值 4 / 均值 ~3）

> **并行前提**：接口冻结（Phase 0 的契约 crate + `visp-core` 类型）是并行的前提；未冻结接口前**不要**并行同 crate 的接口工作。
>
> **单写者文件（跨任务必须串行，同一时刻一个 writer）**：`visp-daemon/src/{main.rs,service.rs}`（**关键路径瓶颈**）、`visp-core/src/{agent.rs,agent_loop.rs}`、`visp-agent/src/orchestrator.rs`。

### Wave 0：契约冻结（2 并行）
| 任务 | 内容（文件域） |
|---|---|
| A | `0a → 0b`（`crates/visp-hooks`，串行链） |
| B | `0c-1`（`visp-core`：`Clone` + `respond→mpsc`）——**挡住总线** |

### Wave 1：总线 + 类型 + 配置 + launcher（**4 并行**）
| 任务 | 内容（文件域） |
|---|---|
| A | `1a-1 → 1a-2`（`visp-daemon`：新 `bus.rs` + `main.rs` wiring + `service.rs` chat subscribe） |
| B | `0c-2`（`visp-core`：`+outcome` + `+PreToolUse`）——不挡总线 |
| C | `1b-0`（`visp-config` `[hooks]` + daemon `watch.rs` 排除） |
| D | `1a-4a`（launcher `crates/visp`） |
> A 与 C 同触 `visp-daemon/src/main.rs`（A 加 wiring、C 改 watcher 注册）→ **需按同一接口先对齐，或 C 的 `main.rs` 改动并入 A**；D 独立 crate，B 独立 crate。

### Wave 2：总线尾 + 关停 + 执行器 + 信任（3 并行）
| 任务 | 内容（文件域） |
|---|---|
| A | `1a-3`（Lagged replay）+ `1a-4b`（关停管道）——`visp-daemon` `service.rs`/`main.rs`，**串行于 Wave 1-A** |
| B | `1b-2`（`visp-hooks` 执行器） |
| C | `2a`（信任：`visp-config` + `visp-hooks`） |
> 信任（2a）只依赖规则模型（1b-0），故提前到本 Wave 与执行器并行。

### Wave 3：事件发射点（3 并行，按 crate 分）
| 任务 | 内容（文件域） |
|---|---|
| A | `1b-1a`：`visp-core`/`agent_loop` 工具事件（`ToolCallRequested`/`PreToolUse`/`PostToolUse`/`PostToolUseFailure`，含 `outcome` 分流） |
| B | `1b-1b`：`visp-agent`/`orchestrator` 生命周期（`SessionStart`/`UserPromptSubmit`/`AgentRunEnd`/`SubagentStop`/`Stop`/`StopFailure`/`PermissionRequest`） |
| C | `1b-1c`：`visp-daemon`（`PermissionResult`/`SessionEnd`）——`service.rs`，**串行于 Wave 2-A** |
> A、B 文件域不同（core / agent），**可真并行**；C 受 `service.rs` 单写者约束。

### Wave 4：CLI/RPC + 内置 + 介入（**4 并行**）
| 任务 | 内容（文件域） |
|---|---|
| A | `1b-3a`：`visp-proto` `GetHookStats` + daemon 计数/规则快照 |
| B | `1b-3b`：launcher `visp hooks list/doctor/test/logs`（`crates/visp`） |
| C | `1b-4`：`builtin:herdr.*`（`visp-hooks` + daemon 启动 wiring） |
| D | `2b`：可选 Pre 介入（`visp-hooks` + `visp-core` gate） |
> A 与 C 若同触 `visp-daemon/src/lib.rs`/`main.rs` 需先对齐模块边界。

### Wave 5：收尾（1）
| 任务 | 内容 |
|---|---|
| A | `R1` 全量回归 + 「未配置 hook 逐字节不变」对照 |

**并发曲线**：2 → 4 → 3 → 3 → 4 → 1（均值 ≈ 3，峰值 4）。
**关键路径**：`visp-daemon` 单文件链（`1a-1 → 1a-2 → 1a-3 → 1a-4b → 1b-1c → 1b-3a`）——**总工期由它决定**；要再压缩，须先做一次 `service.rs`/`main.rs` 的模块化，属另一笔投入。

## 依赖关系总览

```
Wave 0   0a─►0b (visp-hooks 契约)        0c-1 (core: Clone+mpsc → 挡总线)
              │                             │
Wave 1   ┌────┴─────────────┬───────────────┼────────────┬──────────┐
         1a-1─►1a-2        0c-2            1b-0(配置)   1a-4a(启动器)
         (daemon bus)      (core:outcome)  +watch 排除
              │              │               │
Wave 2   1a-3  1a-4b(关停)    │            1b-2(执行器)  2a(信任)
         └── daemon 单写者链 ─┼──────────────┘
              │              │
Wave 3   1b-1c(daemon 发射) ◄─┼──────── 1b-1a(core 工具事件)
                              └──────── 1b-1b(agent 生命周期)
              │                             │
Wave 4   1b-3a(proto+RPC)  1b-3b(启动器 CLI)  1b-4(herdr)  2b(介入)
              │
Wave 5   R1 (全量回归 + 未配置逐字节不变对照)
```

## 测试覆盖汇总

| Wave | 并行数 | 模块/包 | 步骤 | 测试用例 |
|---|---|---|---|---|
| 0 | 2 | visp-hooks（新）；visp-core | 0a；0b；0c-1 | 7；5；2 |
| 1 | **4** | visp-daemon；visp-core；visp-config；visp | 1a-1；1a-2；0c-2；1b-0；1a-4a | 6；5；4；7；4 |
| 2 | 3 | visp-daemon；visp-hooks；visp-config+visp-hooks | 1a-3；1a-4b；1b-2；2a | 5；7；12；10 |
| 3 | 3 | visp-core；visp-agent；visp-daemon | 1b-1a；1b-1b；1b-1c（1b-1 共 35） | 35 |
| 4 | **4** | visp-proto+daemon；visp；visp-hooks；visp-hooks+visp-core | 1b-3a；1b-3b；1b-4；2b | 7；5；5 |
| 5 | 1 | workspace | R1 | 6 |
| **合计** | **峰值 4 / 均值 ≈ 3** | 7 个包（含 1 个新 crate） | 18 子步骤 | **约 132** |

## 备注

1. **提交纪律**：每子步骤一个 commit；发现设计偏差时停下更新设计文档，不带病推进。
2. **行号漂移**：设计 §2.2/§6.4 的行号为评审快照；`orchestrator.rs` 现位于 `crates/visp-agent/src/`，实施前以 grep/读源码重新定位。
3. **`outcome→is_error` 是回归高危区**：0c 必须先锁定「取消/拒绝/截断/真实」四处的改前取值，任何 `is_error` 漂移都会改变 TUI 可见行为。
4. **`PreToolUse` 插入点易漏**：连带适配 `event_to_msg`（`agent_loop.rs` 顶部）与 `agent_event_to_server_message`（`service.rs`），0c 与 1b-1 各覆盖一次。
5. **1a 可独立验收**：Wave 1–2 完成后即可按设计 §16 验证「未接 hook 即验收」（无 hook 行为不变、重连不再 `already taken`、无 TUI 发布不报错、关停可 drain），无需等执行器。
6. **drain 验收位置**：1a 只验「管道与有界性」，真实 drain 效果在 1b-2/1b-1 接线后由 R1 复核。
7. **JSON Schema 校验依赖**：0b 引入校验器 dev-dependency 需锁定版本；若不愿引入，则退化为「结构化字段断言」（须在 0b 明确二选一）。
8. **`[hooks]` 显式排除是行为固化而非新机制**：现状天然不监听，1b-0 需用测试把它变成显式契约，防止未来 watch 计划扩展时误纳入。
9. **手工验收项**（无法自动化）：herdr 真实上报、launcher 等待体感、`hooks doctor` 重启提示、异常退出无 `SessionEnd`。
10. **已知限制 / 有意不做的 TODO 记录建议**（写入 `docs/todo/`，标注归属阶段）：
    - seq 跨重启在同毫秒重启/时钟回拨下不严格递增（是否需持久化高水位——设计 §18 开放）；
    - Windows 未验证（一期不测）；
    - `PreCompact/PostCompact` 语义不存在、`SubagentStart/Stop` 二期再评估；
    - `--format claude` 适配层、MCP-as-transport、热重载策略（Phase 3）；
    - 无头/CI 非交互信任途径（Phase 2 开放问题）；
    - 介入的 fail-open/closed 语义（Phase 2 开放问题）；
    - 通用脚本路径一期无真实外部消费者，仅测试覆盖（设计 §3.3 已接受的主动投入）。
11. **并行度与单写者约束（修订）**：峰值并行 4、均值 ≈3。**单写者文件**（跨任务必须串行）：`visp-daemon/src/{main.rs,service.rs}`、`visp-core/src/{agent.rs,agent_loop.rs}`、`visp-agent/src/orchestrator.rs`。
12. **关键路径 = daemon 单文件链**（`1a-1→1a-2→1a-3→1a-4b→1b-1c→1b-3a`）：总工期由它决定。要再压缩需先对 `service.rs`/`main.rs` 做模块化（另立任务）。Wave 1 中 `1a-1/1a-2`(A) 与 `1b-0`(C) 同触 `main.rs`，须先对齐接口或把 C 的 `main.rs` 改动并入 A。
13. **新增/拆分步骤**：`0c` 拆为 `0c-1`（Clone+mpsc，挡总线）/`0c-2`（outcome+PreToolUse，不挡总线）；`1b-1` 可拆 `1b-1a/1b-1b/1b-1c`（core/agent/daemon 三文件域）；`1b-3` 可拆 `1b-3a/1b-3b`（proto+RPC / launcher CLI）。
