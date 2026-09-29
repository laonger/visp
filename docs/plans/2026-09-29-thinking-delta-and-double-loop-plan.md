# visp 实施工作计划：thinking 流式语义修复（B3）与 daemon 双 loop 拒绝式加固

| 项目 | 内容 |
|---|---|
| 状态 | **待评审**（用户批准后方可实施） |
| 日期 | 2026-09-29 |
| 分支 | `acp` |
| 仓库 | visp（Rust workspace，edition 2024） |
| 输入设计 | `docs/design/2026-09-29-thinking-delta-and-double-loop-design.md`（问题一选定 **B3（不带语义标记）**，问题二选定**拒绝式 G1–G4**） |
| 质量门 | `cargo test --workspace && cargo clippy --workspace -- -D warnings && cargo fmt -- --check` |
| 实施顺序 | **先问题二（步骤 1–5），后问题一（步骤 6–11）**（依设计 §11.2） |

---

## 概述

本计划把两项已拍板的修复拆为 **11 个实现子步骤（其中 1 个可选：5a；原步骤 10 已并入步骤 7）+ 1 个收尾步骤（12a 门禁 + 12b 落 TODO/文档同步）、7 个 Wave 行（6 个执行 Wave + 收尾）、约 63 条测试用例**，每子步骤 = 一个「红 → 绿 → 测试 → 类型检查 → 重构 → 提交」循环 = 一个 commit。

### 目标与范围

- **问题二（拒绝式，先做）**：建立并守住「同一会话任意时刻至多一个在途 agent loop」不变式。做法是：① `visp-core::session` 新增「按会话判断是否存在在途循环令牌」的只读查询；② `visp-daemon::service` 在**用户输入分支、斜杠命令解析之前**加守卫并新增 `SessionBusy` 忙错误，同时移除「`Running` 则静默重置为 `Idle`」；③ `visp-agent::orchestrator` 把非 `Idle` 重置**窄化为仅终态→`Idle`**，并让启动循环的忙失败**向客户端发忙错误**（不再静默）；④（可选纵深 G4）`visp-acp::agent` 终态确认 + 兜底补发取消。
- **问题一（B3，后做）**：在 core 事件层产出**真增量**（内部仍保留本轮完整块以支撑持久化与签名回传）；`ThinkingBlock.thinking` 的语义直接改为「增量」但**不新增标记字段**（仅更新 proto 注释）；daemon 把增量事件映射为 proto 帧；TUI 由「覆盖」改为「追加」；ACP 预期不改，仅补端到端验收断言。

### 两项修复的关系与顺序

- 二者**代码重叠极少**（`visp-daemon::service` 的不同函数区域、`visp-core` 的不同模块），**无相互依赖**，可各自独立发布与回滚（设计 §11.1）。
- 但**先做问题二**：双循环存在时会叠加「两个循环同时发思考帧」，先消除可让问题一的验证更干净（设计 §11.2）。问题一依赖问题二所建立的「单一在途状态机」前提来构造干净的验收通道。

### 关键实施约定（来自设计非显然项）

1. **守卫层级**：守卫**只能加在用户输入分支内**，绝不加在入站层——否则工具审批/LLM 提问的答复会被一并拦截，导致提问永不回填（V12 死锁，设计 §5.5）。
2. **守卫顺序**：必须排在斜杠命令解析之前，否则生成期 `/init`、`/init-agent` 会被执行（设计 §5.5）。
3. **判据是令牌不是 `status`**：守卫只读在途令牌（设计 §6.2 G2）。被拒时**零副作用**，不改状态、不移除令牌。
4. **文案禁用词**：忙错误的 code 与 message **不得**含 `cancelled`（大小写不敏感）、不得等于 `Operation cancelled`，否则被 ACP 误判为用户取消（设计 §5.6）。
5. **重置窄化而非删除**：orchestrator 的非 `Idle` 重置不可整段删除——但**理由**是「删除后 `Error`/`Cancelled` 终态无法恢复，以及 `agent_loop:912`→`handle_done` 的竞态窗口」，**不是**「所有会话第二回合被拒」（正常回合终态其实是 `Idle`，见设计 §6.2 G3 与 §8）。
6. **可测性接缝**：设计 §10.2 要求在**真实分支**上断言负路径。既有 daemon 测试以「复制一份判定逻辑」的方式断言（`service.rs` 内至少 5 处，含被改写的 `3699` 自身），无法证明真实路径正确。本计划要求在 `service.rs` 内把用户输入判定/分支抽为**无副作用纯函数**（保持文件粒度不变），测试直接调用真实逻辑，并把上述复制逻辑的用例**全部**改为调用真实函数；若实施者认为抽取超出范围，可退化为 daemon 级集成测试（详见步骤 2a、4a 备注）。
7. **行号会漂移**：设计文档中的 `xxx.rs:NNN` 为评审快照，实施以 grep/读源码重新定位为准。

---

## 步骤 1：`visp-core::session` 新增在途令牌只读查询

### 1a：按会话判断是否存在在途循环令牌

**文件归属**：`crates/visp-core/src/session.rs`（含同文件内既有 `#[cfg(test)] mod tests`，**沿用该文件既有内联测试风格**）。

现无公开查询入口（`running_tokens` 私有）。新增一个**最小只读**查询：输入会话 id，输出「是否存在在途循环令牌」。不触发任何写操作、不动 `status`、不改锁粒度。

#### 🔴 红 — 测试

| # | 测试用例 | 简明描述 |
|---|---|---|
| 1 | `start_loop` 后查询为「有在途」 | 启动循环即登记令牌，查询应命中 |
| 2 | `finish_loop` 后查询为「无在途」 | 收尾移除令牌，查询应落空 |
| 3 | `cancel_agent` 后查询为「无在途」 | 取消动作「移除即生效」，查询应落空 |
| 4 | 未知会话 id → 「无在途」 | 不 panic、不报错，按不存在处理 |
| 5 | 未 `start_loop` 的 Idle 会话 → 「无在途」 | 初始态无令牌 |
| 6 | 收尾语义未变：`finish_loop` 移除令牌但**不取消** | 克隆 `start_loop` 返回的 `cancel_token`，收尾后断言其 `is_cancelled()==false`，固定「移除但不取消」既有语义 |
| 7 | 子会话 `start_loop` 同样计入在途 | 判据按会话维度，与是否有 `parent_id` 无关 |

#### 🟢 绿 — 实现

在 `SessionManager` 上新增只读查询方法：读取 `running_tokens`，返回该 id 是否在映射中。仅此一处改动，不触碰 `start_loop` / `finish_loop` / `cancel_agent` 语义（设计 §7：收尾动作保持「移除但不取消」且已无害）。

#### 🧪 测试 → 🔍 类型检查

```bash
cargo test -p visp-core session
cargo clippy -p visp-core -- -D warnings
cargo fmt -- --check
```

#### 📦 提交

```
feat(core): 新增在途循环令牌只读查询接口
```

---

## 步骤 2：daemon 受理守卫 + `SessionBusy` 忙错误 + 移除静默重置（必做）

### 2a：用户输入分支加守卫、移除 `Running` 静默重置

**依赖**：步骤 1a（需用到新查询接口）。
**文件归属**：`crates/visp-daemon/src/service.rs`（生产分支 + 同文件测试）。

变更职责：

1. 用户输入分支先判定并分类：**非主会话（有父会话）→ 拒绝**（码 `SessionNotActive`，语义与文案保持现状）；**主会话且存在在途令牌 → 拒绝**（码 `SessionBusy`）；其余 → 受理。**分类排在任何斜杠命令解析之前**。
2. **移除**「`Running` 则静默重置为 `Idle`」的旁路（现 `service.rs:617-631`）。
3. 被拒路径**零副作用**：不改 `status`、不移除令牌、不触发命令副作用、不向 orchestrator 转发。
4. 新增忙错误码 `SessionBusy` 与文案：中文「正在生成，请稍候」；英文 `Session is busy: generation in progress, please wait`。**禁用词约束**：code 与 message 均不得含 `cancelled`（大小写不敏感）。
5. 为满足可测性（设计 §10.2 要求真实分支断言），把用户输入的判定/受理决策抽为 `service.rs` 内**无副作用纯函数**（入参为「是否主会话 + 是否存在在途令牌」，出参为受理/拒绝分类；**守卫只覆盖 UserInput，不覆盖 UserResponse**）。

#### 🔴 红 — 测试

| # | 测试用例 | 简明描述 |
|---|---|---|
| 1 | **改写** `test_user_input_to_running_session_resets_and_accepts`（现 `service.rs:3699`） | 断言改为：`Running` 且有在途令牌 → **拒绝（忙）**，且 `status` 仍为 `Running`、令牌仍在、不再被重置 |
| 2 | 主会话 Idle/Completed/Error → 受理 | 保持现状：三种非忙主会话均放行（`Completed`/`Error` 交由 orchestrator 转 Idle） |
| 3 | 子会话（有 parent）→ 拒绝 | 码为 `SessionNotActive`（对照既有 `service.rs:3654` / `3731`，语义不变） |
| 4 | 忙错误码与文案合规 | 码 == `SessionBusy`；message 非空，且**不含** `cancelled`（大小写不敏感）、不等于 `Operation cancelled` |
| 5 | 判据是令牌而非 `status` | 构造成「有令牌但 `status != Running`」→ 仍拒绝（证明守卫不依赖 `status`） |
| 6 | 仅 `status==Running` 但无令牌 → 放行 | 守卫放行、且**不再重置**该状态（证明静默重置已移除；后续由 orchestrator 状态闸兜底） |
| 7 | 生成期提交 `/init`、`/init-agent` 被拦下 | 返回忙错误；**不**转发命令；**不**产生文件写入副作用（对照 `/init-agent` 的建目录/写文件路径） |
| 8 | 被拒路径幂等无副作用 | 连续两次被拒后，`get()` 的 status 与查询接口结果均不变 |
| 9 | 既有**全部**「复制判定逻辑」的用例改为调用真实函数 | 逐个 grep 确认（至少含 `service.rs` 中 `3671 / 3709 / 3748 / 3791 / 3820` 诸块与被改写的 `3699` 自身），消除假覆盖 |

#### 🟢 绿 — 实现

按上表实现分类、拒绝与文案；删除静默重置；新增可单测的判定函数并从真实分支调用（保持仅 `service.rs` 改动）。

> 备注：接缝**保留**（审核结论：它比自建真实入站夹具更小、更低风险——现有夹具只能造空流，且「有令牌但非 Running」这类场景在集成层无法构造）。若实施评审认为代价过高，可保留原分支结构并改以 daemon 级集成测试（步骤 4a）覆盖真实路径——但**不得**继续用「在测试中复制判定逻辑」的方式冒充覆盖。

#### 🧪 测试 → 🔍 类型检查

```bash
cargo test -p visp-daemon service
cargo clippy -p visp-daemon -- -D warnings
cargo fmt -- --check
```

#### 📦 提交

```
fix(daemon): 拒绝在途循环期间的新用户输入
```

---

## 步骤 3：orchestrator 非 `Idle` 重置窄化 + 忙失败发忙错误（必做）

### 3a：仅终态→Idle；忙失败上报并后移准备动作；新增收尾归属判据 G5

**依赖**：无（可与步骤 2a 并行；文件不重叠）。
**文件归属**：`crates/visp-core/src/error.rs`、`crates/visp-agent/src/orchestrator.rs`、`crates/visp-agent/src/orchestrator_tests.rs`。

变更职责：

1. `orchestrator.rs` 的 `handle_client_message` 用户输入分支：非 `Idle` 重置**窄化为仅终态（`Completed` / `Error`）→ `Idle`**；`Running` **不再重置**（随后由启动循环状态闸以 `SessionBusy` 拒绝）。
2. 启动循环因忙失败时（现只记日志静默返回）：改为**向客户端发「会话忙」错误**（经总线显示帧；code 语义与 §5.6 一致，文案禁用词同上）。**并把准备工作后移**：`append_system_prompt_template`（`orchestrator.rs:476/491`）与 `active_agents.register`（`505`）现都发生在 `start_loop`（`533`）之前、失败时不回滚；**选定把它们挪到 `start_loop` 成功之后**（`append_system_prompt_template` 只有追加、无撤销 API，故不取「回滚」方案，避免新增 API）。
3. 为实现 (2)，需新增一个 `AgentErrorCode` 枚举值（Display 文本恰为 `SessionBusy`），使其经 daemon 现有映射（`error.rs` 的 Display → 帧 code 字符串）得到与 G1 一致的 proto 码。**该新增需同步 `error.rs` 内既有 Display 测试用例表**。
4. **G5 最小归属判据（三轮审核新增）**：`handle_done` / `handle_agent_error` 在写入终态前**先校验会话当前状态是否仍为本回合写下的终态**（`Completed` / `Error`）；若已被新回合改写为 `Running`，则本次收尾为**幂等空操作**（不移除令牌、不改状态）。用于封堵设计 §5.2 的 B6（正常完成路径的收尾时序窗口）。**这不是完整的世代机制**，只是一句前置检查。

#### 🔴 红 — 测试

| # | 测试用例 | 简明描述 |
|---|---|---|
| 1 | **保持** `test_session_start_not_repeated_on_second_turn`（现 `orchestrator_tests.rs:3253/3262`） | 终态→Idle 的合法路径仍成功，`SessionStart` 只发一次 |
| 2 | `Completed` 主会话经 `handle_client_message` → 重置→成功启动第二回合 | 证明窄化未破坏正常多轮 |
| 3 | `Error` 主会话同上 | 终态同样可恢复 |
| 4 | `Running` 主会话经 `handle_client_message` → 不重置、不启动、不发第二组生命周期 hook | 证明 `Running` 已从重置集合移出 |
| 5 | `Running` 场景下发布的 Error 帧 code 语义为 `SessionBusy` 且 message 不含 `cancelled` | 与 daemon 侧码语义一致 |
| 6 | **保持** `test_no_hook_events_when_start_loop_fails`（现 `orchestrator_tests.rs:3392`） | 仍无 hook 事件；**补充**断言「确实发布了忙错误帧」（不再静默丢弃） |
| 7 | （**移至步骤 4a 的集成层**）忙拒绝后旧 loop 收尾 → 会话只有唯一终态 | 依赖真实 loop 与 Done 时序，orchestrator 单测层不可写 |
| 8 | 忙失败后无残留 registration、prompt 模板不累积 | 准备动作后移后，失败路径不产生副作用 |
| 9 | **G5**：先起新循环、再投递旧回合的 `Done` → 新循环令牌仍在、状态仍 `Running` | 复现设计 §5.2 B6；当前实现会失败 |
| 10 | **G5**：单回合正常完成仍能被置回 `Idle`（判据不误伤） | 正常路径不回归 |

#### 🟢 绿 — 实现

窄化重置条件为终态集合；忙失败路径改为发布忙错误帧并把准备动作后移；新增错误码枚举值并更新其 Display 测试表；新增 G5 收尾归属判据。

#### 🧪 测试 → 🔍 类型检查

```bash
cargo test -p visp-agent orchestrator
cargo test -p visp-core error
cargo clippy -p visp-agent -p visp-core -- -D warnings
cargo fmt -- --check
```

#### 📦 提交

```
fix(agent): 窄化重置、忙失败上报并后移准备动作、新增收尾归属判据
```

---

## 步骤 4：daemon 负路径集成（V12 防死锁 / 命令顺序 / 端到端拒绝）（必做）

### 4a：真实分支上的负路径断言

**依赖**：步骤 2a（守卫）、步骤 3a（忙错误）。
**文件归属**：`crates/visp-daemon/src/service.rs`（测试区）或新增 `crates/visp-daemon/tests/` 集成文件（二选一，避免同时改两处）。

#### 🔴 红 — 测试

| # | 测试用例 | 简明描述 |
|---|---|---|
| 1 | **V12 回归**：输入被拒期间，答复仍按 `query_id` 回填 | 在途循环等待工具审批/LLM 提问；此时提交新用户输入被拒；随后 `UserResponse` 仍能送达等待者并发布 `PermissionResult`（守卫只覆盖 UserInput，不覆盖 UserResponse） |
| 2 | 端到端拒绝：生成期新输入 → 唯一忙错误、旧 loop 继续、无第二个 loop、令牌可取消 | 断言客户端收到 code `SessionBusy` 的错误；会话仍 `Running`；查询接口仍为「有在途」；`cancel` 仍能取消该令牌（无「不可取消孤儿」） |
| 3 | 命令顺序端到端：生成期 `/init` | 不产生文件副作用、不转发为 prompt |
| 4 | 忙拒绝后旧 loop 收尾 → 会话只有唯一终态（原步骤 3a #7 移入） | 依赖真实 loop 与 Done 时序，集成层可写 |

#### 🟢 绿 — 实现

无新增生产逻辑（仅补测试与必要接缝）。若步骤 2a 采用「分支内直接调用查询接口」而非抽取判定函数，则本步骤需以能驱动真实入站的 harness 覆盖第 1、3 条。

#### 🧪 测试 → 🔍 类型检查

```bash
cargo test -p visp-daemon
cargo clippy -p visp-daemon -- -D warnings
cargo fmt -- --check
```

#### 📦 提交

```
test(daemon): 忙拒绝负路径（答复回填、命令顺序、端到端拒绝）
```

---

## 步骤 4b：触发窗口响应（TUI 错误码特判 + 分层覆盖）

### 4b：`SessionBusy` 不置 tab 为 Error，并分层覆盖触发窗口

**依赖**：步骤 2a（守卫）、步骤 3a（忙错误）。
**文件归属**：`crates/visp-tui/src/app.rs`、`crates/visp-tui/src/app_tests.rs`、`crates/visp-tui/src/event_tests.rs`、`crates/visp-acp/tests/integration.rs`。

变更职责：

1. **TUI 错误码特判（生产改动，反思审核新增）**：在 tab 状态更新处，对 `err.code == "SessionBusy"` **不置 `Error`**，改为追加一行 Status 提示（与相邻的 `SessionNotActive` 特判同构）。理由：否则取消/重连窗口内的重提会把**正在正常流式**的 tab 永久置 `Error`，且后续 `Done` 不会恢复（`Done` 仅在状态为 `Running` 时改回）。
   ⚠️ **注意：实际是三处**——`route_frame` 的两处相似状态更新块（`tab.status` / `self.tab_bar.tabs[idx].status`），**以及 `render_pending` 的 Error 臂**（除置状态外，还会 `flush_streaming()` 丢掉正在流式的消息、并 `stop_generating()` 当场放开输入门禁）。三处必须一致，否则「特判」形同虚设。
2. **分层覆盖（仅测试）**：TUI 窗口存在性 + 唯一一条 ACP 端到端；不做全矩阵端到端（驱动 TUI 按键窗口成本高且易 flaky，且窗口本身不是缺陷位置）。

#### 🔴 红 — 测试

| # | 测试用例 | 简明描述 |
|---|---|---|
| 1 | `SessionBusy` 不把 tab 置为 `Error`，且**不 flush 正在流式的消息**、**不提前 `stop_generating`** | 三处站点均需生效；断言状态不变 + 追加一行 Status 提示 + `generating` 保持 + 流式内容未被 flush |
| 2 | `SessionNotActive` 行为不回归 | 与既有特判不混淆 |
| 3 | （**特征化测试**，改动前即通过，非红）TUI 单元：取消后输入门禁确实放开 | Ctrl+C / Esc 后 `generating` 为 false（窗口存在性，B1a/B1b/B1c） |
| 4 | （**特征化测试**，改动前即通过，非红）TUI 单元：重连后输入门禁确实放开 | `reset_after_reconnect` 后 `generating` 为 false（B4） |
| 5 | ACP 集成（唯一保留的端到端）：daemon 返回忙错误时以错误收尾、不悬挂、不双循环 | 用既有脚本化夹具（B2/B3 的兜底路径） |

#### 🟢 绿 — 实现

按职责 1 加特判（约 5 行，仿既有模式）；职责 2 仅补测试。

#### 🧪 测试 → 🔍 类型检查

```bash
cargo test -p visp-tui
cargo test -p visp-acp
cargo clippy -p visp-tui -p visp-acp -- -D warnings
cargo fmt -- --check
```

#### 📦 提交

```
fix(tui): SessionBusy 不将 tab 置为 Error，并分层覆盖忙拒绝触发窗口
```

> 说明：核心不变式（在途循环时收到用户输入 → 忙拒绝）由步骤 4a 的 daemon 集成覆盖，本步骤不重复断言。

---

## 步骤 5（可选纵深）：ACP 终态确认 + 兜底补发取消（G4）

### 5a：仅在确认 daemon 终态后结束本轮

**依赖**：无（只影响 ACP，不阻塞 G1–G3，可与问题一并行）。
**文件归属**：`crates/visp-acp/src/agent.rs`（生产 + 该文件内 tests）。**测试钉在 `agent.rs` 内**，**不得**写 `crates/visp-acp/tests/integration.rs`（该文件由步骤 4b 使用，同 Wave 不可双写）。

变更职责：结束本轮前先确认 daemon 终态；兜底超时窗口内若无终态信号则**补发取消**再收尾。仅收敛 ACP 侧行为，不解决 daemon 侧并发。

#### 🔴 红 — 测试

| # | 测试用例 | 简明描述 |
|---|---|---|
| 1 | 兜底窗口内无终态信号 → 补发取消并收尾 | 断言发出取消请求、turn 以取消收尾、不悬挂 |
| 2 | 已收到终态 → 不重复补发取消 | 幂等 |
| 3 | **保持** `crates/visp-acp/tests/integration.rs` 既有「提问等待中取消」用例 | 不回归 |

#### 🟢 绿 — 实现

在 `agent.rs` 的 pump 循环中接入终态确认与兜底取消补发；不改 `translate.rs`。

#### 🧪 测试 → 🔍 类型检查

```bash
cargo test -p visp-acp
cargo clippy -p visp-acp -- -D warnings
cargo fmt -- --check
```

#### 📦 提交

```
fix(acp): 结束本轮前确认终态并在兜底超时时补发取消
```

> 标注：**可选**。若工期紧张可延后，不阻塞其余步骤。

---

## 步骤 6：proto 语义注释更新（无字段变更）

### 6a：把 `ThinkingBlock.thinking` 的语义写进契约

**依赖**：无（**不再阻塞任何后续子步骤**——不改字段，无编译连锁）。
**文件归属**：`crates/visp-proto/proto/visp.proto`（仅注释）。

变更职责：**不新增标记字段**（设计 §3.5 已决议）。把 `ThinkingBlock.thinking` 的注释由「推理/思考内容文本」改写为**明确声明其为「增量」语义**（相对上一帧的新增文本，消费方应追加），并**指向语义契约回归测试**（core 侧「连续同块快照 → 只发增量」与「流式期间不产生快照变体」两条用例），以消除本次根因中「契约语义未写明」的问题（设计 §1.4、§4.3）。字段编号、类型与既有 1/2/3 编号全部不动。

#### 🔴 红 — 测试

| # | 测试用例 | 简明描述 |
|---|---|---|
| 1 | 构建与既有 proto 测试不回归 | 无字段变更，故无新增行为断言；以「不破坏既有构建与测试」为验收 |

> 备注：本子步骤为**契约文档性改动**，无可执行行为变更，因此不设行为级红测试；其验收由后续 7a/8a 的消费侧断言共同保证。

#### 🟢 绿 — 实现

仅更新 proto 注释；不新增/不删除字段。生成代码无变化（仅注释改动）。

#### 🧪 测试 → 🔍 类型检查

```bash
cargo build -p visp-proto
cargo test -p visp-proto
cargo clippy -p visp-proto -- -D warnings
cargo fmt -- --check
```

#### 📦 提交

```
docs(proto): 明确 ThinkingBlock.thinking 为增量语义（无字段变更）
```

---

## 步骤 7：core 产出真增量（B3）

### 7a：core 产出真增量 + daemon 映射同批补齐（原子变更）

**依赖**：无（不依赖 proto；可与步骤 6a、9a 并行）。⚠️ **但 daemon 映射必须同批**：新增 `AgentEvent` 变体会使 `crates/visp-daemon/src/service.rs` 的穷尽匹配 `agent_event_to_server_message`（无 `_` 兜底）**编译失败**，故本步骤必须一并补齐 daemon 映射臂，否则 `visp-daemon` 不可编译、全量门禁必红。**原步骤 10 已并入本步骤**。
**文件归属**：`crates/visp-core/src/agent.rs`、`crates/visp-core/src/agent_loop.rs`、`crates/visp-daemon/src/service.rs`（沿用各文件既有测试风格）。

变更职责：

1. `AgentEvent` 新增**显式的「思考增量」事件变体**，仅承载增量文本（不复用快照事件，语义自描述）。
2. `collect_stream_events`：接收快照序列，维护「本轮当前块基线」；按设计 §4.2 规则产出增量：
   - 新快照以前一快照为前缀 → 增量为差集尾部；
   - 非前缀（新块 / 新一轮 / 内容跳变 / 回退变短）→ 视为新块起点，本帧增量 = 全文并重置基线；
   - 增量为空 → **不发射**。
3. 流式路径**只发射增量变体**，不再发射快照事件（否则 ACP 会同时追加增量与快照造成更严重重复）；`thinking_blocks` 内部完整块保留逻辑保持不变（保证持久化/签名回传，设计 §2.4）。
4. `event_to_msg` 为新变体补映射：返回 None（orchestrator 继续忽略思考）。该处为穷尽匹配，新增变体将强制编译报错，属预期。
5. 基线生命周期天然以「一轮 LLM 响应」为界（`collect_stream_events` 的局部状态），函数返回即丢弃（设计 §3.4 优势 1）。
6. **daemon 映射（并入本步骤）**：`agent_event_to_server_message` 新增分支——思考增量事件 → proto 思考块帧（`thinking` 即增量文本、`signature` 为空）；既有快照事件映射保留不动。
7. **发射点唯一性**：流式路径**只**发射增量变体；补断言「流式期间不产生快照变体事件」。

#### 🔴 红 — 测试

| # | 测试用例 | 简明描述 |
|---|---|---|
| 1 | 连续同块快照 → 只发增量且拼接等于全文 | 输入 `A` → `AB` → `ABC`，输出增量序列拼接 == `ABC` |
| 2 | 文本未变的终帧（带签名）→ 不发射 | 空增量不产生事件 |
| 3 | 非前缀快照 → 首发全文并重置基线 | `AB` 后到达 `XYZ` → 发 `XYZ`；随后 `XYZW` → 发 `W` |
| 4 | 回退/更短快照（异常）→ 视为新块起点 | 发全文，不产生负增量 |
| 5 | 仅思考无正文轮次：控制流不变 | 内部完整块仍触发既有「继续循环」判定 |
| 6 | `event_to_msg(增量变体) == None` | 不转发至 orchestrator |
| 7 | 流式期间不产生快照变体事件（发射点唯一性） | 断言本轮事件序列中只有增量变体 |
| 8 | daemon：增量事件 → 帧的 `thinking` == 增量文本、`signature` 为空 | 单测 `agent_event_to_server_message` |
| 9 | daemon：快照事件 → 既有映射不变 | 不回归 |
| 10 | 发布 k 个增量帧 → 客户端收到帧总字节 == 全文长度 | 证明总线消费侧 O(n)，不再 O(n²) |

#### 🟢 绿 — 实现

按上述职责实现；既有 `ThinkingBlock` 事件变体仅为**兼容既有匹配臂**而保留（当前不存在非流式思考生产者，确认无路径后应删除，见设计 §4.2）；**并同批补齐 daemon 映射臂**（否则 `visp-daemon` 不可编译）。

> TDD 说明：新增 `AgentEvent` 变体前，引用该变体的测试**无法编译**，故本步骤的「红」表现为**编译失败**而非断言失败；可先声明变体并留 `unimplemented!()` 取得可编译的红，再填实现。这是「一个原子批同时改 core 与 daemon」的固有代价。

#### 🧪 测试 → 🔍 类型检查

```bash
cargo test -p visp-core agent_loop
cargo test -p visp-daemon service
cargo clippy -p visp-core -p visp-daemon -- -D warnings
cargo fmt -- --check
```

#### 📦 提交

```
feat(core): 事件层产出真思考增量并保留完整块，同批补齐 daemon 映射
```

---

## 步骤 8：TUI 思考渲染改追加

### 8a：覆盖 → 追加后缀（视觉等价）

**依赖**：**步骤 7a 必须先落地**。语义已全局改为增量后，TUI 的「无条件追加」才正确；若先于 7a 提交，会把显示从「只显示最后一片」**恶化为平方级双倍**。与步骤 9a、6a 无依赖。
**文件归属**：`crates/visp-tui/src/app.rs`、`crates/visp-tui/src/app_tests.rs`。

变更职责：

1. 思考帧一律按**增量**语义消费：**向同一 Thinking 行追加后缀**（设计 §3.5 已决议不加标记，故无分流）。
2. `[Thinking] ` 前缀只在**新建**该行时出现一次，后续增量仅追加后缀（避免每片重复前缀）。
3. 子 agent 路由路径同样按追加语义。
4. 行内内容始终为全文，速率估算（依赖行内为全文）不受影响。

#### 🔴 红 — 测试

| # | 测试用例 | 简明描述 |
|---|---|---|
| 1 | **改写** `test_update_thinking_to_session_routes_by_session_id`（现 `app_tests.rs:1063-1077`） | 第二次到达由「覆盖」断言改为「追加」（内容为两段拼接），行数仍为 1 |
| 2 | 增量路径：多次增量 → 同一行追加 | 行内为全文、前缀不重复 |
| 3 | 首帧 → 新建 Thinking 行 | 行内为第一段增量，`[Thinking] ` 前缀出现一次 |
| 4 | 子 agent 路由路径同样追加 | 与主路径一致 |
| 5 | 速率估算仍基于行内全文 | 既有相关用例（`app.rs:503-526` 覆盖范围）不回归 |
| 6 | 跨轮追加到同一行 | thinking-only 跨多轮时，新块首帧被追加到上一轮仍在的同一 Thinking 行（固化设计 §4.4） |
| 7 | 多块：**显示**为两段拼接（落库断言归步骤 11a，TUI 层不涉及持久化） | 固化设计 §4.4 的已知差异（显示部分） |
| 8 | 思考被工具/正文打断后再来增量 → 新建行只含该后缀 | 覆盖「最后一条是否为 Thinking」这一分支的边界 |

#### 🟢 绿 — 实现

实现「向同一行追加后缀」；`[Thinking] ` 前缀只在新建行时出现一次。

#### 🧪 测试 → 🔍 类型检查

```bash
cargo test -p visp-tui app
cargo clippy -p visp-tui -- -D warnings
cargo fmt -- --check
```

#### 📦 提交

```
fix(tui): 思考增量改为同一行追加而非覆盖
```

---

## 步骤 9：ACP 端到端验收断言（预期不改生产代码）

### 9a：增量经 ACP 追加到同一 `messageId` 后文本无重复

**依赖**：无（proto 无字段变更；与步骤 7a、8a、6a 均可并行）。
**文件归属**：`crates/visp-acp/src/translate.rs`（测试区）、`crates/visp-acp/tests/integration.rs`（可与步骤 7a、8a 并行）。

变更职责：**无生产代码改动**。新增断言以固化本 bug 的验收条件。

#### 🔴 红 — 测试

| # | 测试用例 | 简明描述 |
|---|---|---|
| 1 | 连续增量帧 → 同一 `messageId` 的 thought chunk 序列 | 序列拼接 == 全文，无重复、无平方级膨胀 |
| 2 | 子 agent 增量帧仍被抑制 | 与既有抑制策略一致 |
| 3 | 端到端（`ScriptedDaemon`）：完整 turn | 客户端可见的思考文本 == 全文，证明追加语义正确 |

#### 🟢 绿 — 实现

无（`translate.rs` 不修改）。若测试暴露 ACP 需要改动，须回到设计评审后再动，不得在本步骤顺手改生产代码。

#### 🧪 测试 → 🔍 类型检查

```bash
cargo test -p visp-acp
cargo clippy -p visp-acp -- -D warnings
cargo fmt -- --check
```

#### 📦 提交

```
test(acp): 断言思考增量追加至同一 messageId 且无重复
```

---

## 步骤 10：（已取消——职责并入步骤 7）

**取消原因（审核阻断项）**：新增 `AgentEvent` 变体会使 `crates/visp-daemon/src/service.rs` 的穷尽匹配 `agent_event_to_server_message`（该函数无 `_` 兜底，区间 `1742-1910` 已核实）**编译失败**，因此 daemon 映射**必须与步骤 7 同批提交**，不能拆成独立子步骤（否则 Wave P1-2 结束时 `cargo test --workspace` 必红，违反「每子步骤可编译可测试」与提交前全量门禁）。

本步骤原有的职责、测试与断言（增量事件 → 帧、快照映射不回归、O(n) 字节、拼接等于全文）**已全部并入步骤 7a**。

---

## 步骤 11：core 集成断言（增量拼接 == 全文；`extra_blocks` 仍为完整块）

### 11a：一轮含思考响应的装配/回传断言

**依赖**：步骤 7a。
**文件归属**：`crates/visp-core/src/agent_loop.rs`（测试区）。

> ⚠️ **与步骤 7a 同文件，不能与其并行**；必须排在 Wave P1-2 之后（本计划置于 Wave P1-3，与步骤 8a 并行）。

变更职责：无生产改动（或仅测试）。用脚本化 provider 跑一轮「思考 + 正文」响应，断言对外增量序列拼接 == 完整文本，且同一轮落库/history 的 `extra_blocks` 仍为完整块（正文路径含签名）。

#### 🔴 红 — 测试

| # | 测试用例 | 简明描述 |
|---|---|---|
| 1 | 增量序列拼接 == 完整思考文本 | 正文与思考交错场景 |
| 2 | `extra_blocks` 仍为完整块且含签名 | 正文装配路径（设计 §2.4 硬约束） |
| 3 | 仅思考无正文路径既有断言不回归 | 控制流不变 |
| 4 | 多块场景：**落库 / history 仍只有最后一块**（与步骤 8a #7 的显示拼接互为对照） | 固化设计 §4.4 的已知差异（持久化部分） |

#### 🟢 绿 — 实现

无（如测试暴露装配被增量改造影响，须回到设计评审）。

#### 🧪 测试 → 🔍 类型检查

```bash
cargo test -p visp-core agent_loop
cargo clippy -p visp-core -- -D warnings
cargo fmt -- --check
```

#### 📦 提交

```
test(core): 断言增量流拼接为全文且 extra_blocks 保持完整块
```

---

## 步骤 12：全量门禁与回归验收 + 工程收尾

按顺序执行，任一失败即回到对应子步骤修复：

```bash
# 问题一 provider 契约回归：这两处快照断言必须保持不变（不得改动源码）
cargo test -p visp-llm test_byte_stream_reasoning_then_text
cargo test -p visp-llm test_byte_stream_reasoning_only

# 全量门禁（与 CI 一致）
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo fmt -- --check
```

验收判定：

- `openai_tests.rs:1228-1282 / 1284-1343` 的快照断言**未被改动**且通过（provider 契约未动）。
- `app_tests.rs:1063-1077` 已由「覆盖」改写为「追加」并通过。
- daemon 全量测试通过，含步骤 4a 的 V12 回归与步骤 4b 的分层覆盖。
- `cargo test --workspace` 无新增失败。

### 12b：工程收尾——落 TODO 与文档同步（**非代码，需提交**）

**依据**：`work-plan` skill 第 6 步（把「有意不做 / 已知边界 / 后续方向」记录到 TODO 文档）+ 设计 §12.3。

**变更职责**：

1. 把设计 §9 的三条「另立项」（回放不下发思考块、Anthropic `signature_delta` 未解析、多块完整性与签名回传）写入 `docs/todo/TODO.md`，并与既有 `docs/todo/thinking-history-issues.md` **互相引用**。
2. 把设计 §12.3 的「文档同步」立为独立条目：`docs/design/visp-design-acp.md` 的 V10/V11、§6.6、§13.10、§13 风险表已与代码过时。
3. 把本计划「备注 3 已知限制」的条目（增量不自愈、回放不含思考、语义静默变更、多块显示多于持久化、跨轮同一行）登记到 `docs/todo/TODO.md`；并注明**本方案不触及** `docs/todo/thinking-history-issues.md` 记录的「thinking 双存储重复发送」路径，两者无交集。
4. 为「`ThinkingBlock.thinking` 语义由快照改为增量」补一条**发布说明条目**（契约静默变更 + 旧客户端降级表现）。

#### 🔴 红 — 测试

| # | 测试用例 | 简明描述 |
|---|---|---|
| 1 | 不适用（非代码改动） | 以「`docs/todo/TODO.md` 与发布说明可检索到上述条目」为验收 |

#### 🧪 测试 → 🔍 类型检查

复用步骤 12a 的全量门禁（不重复列命令）。

#### 📦 提交

```
docs: 登记思考语义变更与双 loop 加固的已知边界与另立项
```

---

## Wave 并行策略

> 原则：互相无文件与依赖关系的子步骤进同一 Wave 以并行；有依赖或同文件者必须串行。**同一 Wave 内不得有两个子步骤写同一文件。**

| Wave | 任务数 | 子步骤 | 串/并行 | 文件冲突判定 |
|---|---|---|---|---|
| **P2-1** | 1 | 1a | 串行（前置） | 仅 `visp-core/src/session.rs` |
| **P2-2** | 2 | 2a ∥ 3a | **并行** | 2a 仅 `visp-daemon/src/service.rs`；3a 仅 `visp-core/src/error.rs` + `visp-agent/src/{orchestrator,orchestrator_tests}.rs`；无交集 |
| **P2-3** | 3 | 4a ∥ 4b ∥ 5a | **并行** | 4a 仅 `crates/visp-daemon`；4b 仅 `crates/visp-tui` + `crates/visp-acp/tests/`；5a 仅 `crates/visp-acp/src/agent.rs`；互不相交 |
| **P1-1** | 1 | 6a | 可与 P1-2 并行（无编译连锁） | 仅 `visp-proto/proto/visp.proto`（注释） |
| **P1-2** | 2 | 7a ∥ 9a | **并行**（与 6a 亦可并行） | 7a: `visp-core/{agent,agent_loop}.rs` + `visp-daemon/src/service.rs`；9a: `visp-acp/{translate.rs,tests/integration.rs}`；互不相交 |
| **P1-3** | 2 | 8a ∥ 11a | **并行** | 8a: `visp-tui/{app,app_tests}.rs`（**须在 7a 之后**）；11a: `visp-core/src/agent_loop.rs`（与 7a 同文件，须在 7a 之后） |
| **收尾** | 1 | 12 | 串行 | 12a 无写入（仅运行门禁）；12b 仅写 `docs/` |

### Wave 内不可并行的显式标注（同文件，须跨 Wave 串行）

- **`crates/visp-daemon/src/service.rs`**：2a → 4a → 7a（三个 Wave 顺序写同一文件，严禁并行；6a 与已取消的 10a 均不再改此文件）。
- **`crates/visp-acp/src/translate.rs`**：仅 9a（6a 已不再改此文件）。
- **`crates/visp-tui/src/app.rs`**：4b → 8a（跨 Wave 串行）。
- **`crates/visp-tui/src/app_tests.rs`**：4b → 8a（跨 Wave 串行；6a 已不再改此文件）。
- **`crates/visp-acp/tests/integration.rs`**：4b → 9a（跨 Wave 串行）。
- **`crates/visp-core/src/agent_loop.rs`**：7a → 11a（串行）。
- **`visp-core`（不同文件）**：1a（`session.rs`）与 3a（`error.rs`）可并行，但 1a 是 2a 的前置，故 1a 独占 Wave P2-1。

---

## 依赖关系总览

```
                       ┌─────────────────────────── 问题二（先做） ───────────────────────────┐
Wave P2-1  步骤1a ──┬──► 步骤2a (daemon 守卫) ──┬──► 步骤4a (daemon 负路径集成) ∥ 步骤4b (分层覆盖)
                    │                          │
                    └──► 步骤3a (orchestrator) ─┘
                                               └────────────────────────► 步骤5a (G4, 可选)
        (1a 仅前置 2a；3a 与 2a 无依赖，可并行)

                       ┌─────────────────────────── 问题一（后做） ───────────────────────────┐
Wave P1-1/2 (可并行)  步骤6a (proto 注释)   步骤7a (core + daemon 映射同批)   步骤9a (acp, 测试)
                                              │
                                              ├──────────────┐
                                              ▼              ▼
Wave P1-3                             步骤8a (tui)     步骤11a (core 集成)
                                      (依赖 7a)        (依赖 7a，与 7a 同文件须串行)

收尾        步骤12 (全量门禁 + provider 回归，不提交)
```

关键路径：`1a → 2a → 4a`（问题二）与 `7a → 8a`（问题一）；`6a`、`3a`、`4b`、`5a`、`9a`、`11a` 为可并行旁支。

---

## 测试覆盖汇总

| Wave | 并行数 | 模块/包 | 步骤 | 测试用例数 |
|---|---|---|---|---|
| P2-1 | 1 | `visp-core`（session） | 1a | 7 |
| P2-2 | 2 | `visp-daemon`（service）/ `visp-agent` + `visp-core`（error） | 2a, 3a | 9 + 9 = 18 |
| P2-3 | 3 | `visp-daemon`（集成）/ `visp-tui`+`visp-acp` / `visp-acp` | 4a, 4b, 5a | 4 + 5 + 3 = 12 |
| P1-1 | 1 | `visp-proto`（仅注释） | 6a | 1 |
| P1-2 | 2 | `visp-core` + `visp-daemon`（service）/ `visp-acp` | 7a, 9a | 10 + 3 = 13 |
| P1-3 | 2 | `visp-tui` / `visp-core`（agent_loop） | 8a, 11a | 8 + 4 = 12 |
| 收尾 | 1 | 全 workspace（验证） | 12 | 0（复用既有回归） |
| **合计** | — | — | 11 个实现子步骤（1 可选）+ 1 收尾；原 10a 已并入 7a | **63**（含可选 5a 的 3 条） |

---

## 备注

### 1. 需要用户额外拍板的点

| # | 待定项 | 本计划的处理 | 影响范围 |
|---|---|---|---|
| A | **proto 语义标记**（设计 §12.1） | **已决议：不加标记**（用户拍板）。`ThinkingBlock.thinking` 语义直接改为「增量」并写入注释；已接受的代价见设计 §3.5。 | 步骤 6a（仅注释）、8a（不再有标记逻辑） |
| B | 版本错配兼容开关 | **不适用**（已决议不加标记，无错配协商面）。 | 无 |
| C | 问题二错误码与文案最终定稿（设计 §12.4 B1） | 计划按推荐码 `SessionBusy` + §5.6 文案实现；若改码/文案，仅影响步骤 2a/3a 的常量与断言。**除 `SessionBusy` 特判（步骤 4b）外**，其余任意 Error 仍置 tab 为 Error 并显示 daemon message（即除该特判外不改 TUI 状态机）。 | 步骤 2a、3a、4b |
| D | 在途令牌查询接口是否顺带暴露其他只读状态（设计 §12.4 B2） | 本计划仅实现**最小职责**「是否存在在途令牌」；是否扩展交由实现评审，**不预设**。 | 步骤 1a |
| E | `status` 失步的常态化收敛（设计 §12.4 B3） | **本期不做**。守卫本身免疫失步；残留 `Running` 的持续拒绝交由 daemon 重启的孤儿重置恢复。若纳入，需另立步骤并改动 `session.rs`/`main.rs`。 | 未纳入 |
| F | 取消动作移除令牌的时机（设计 §12.4 B4） | **本期不改**（保持「取消即移除」）。由此产生的「取消后短暂窗口内守卫放行、由状态闸以 `SessionBusy` 拒绝」在步骤 4a 中以**显式用例记录为预期行为**，不作为失败。 | 步骤 4a |
| G | **新增 `AgentErrorCode` 忙码**（设计 §7 未列出的实现必需项） | G3 附带要求「orchestrator 忙失败发忙错误」需要错误码承载；daemon 现有映射以「错误码 Display 文本」作为 proto code，故须新增一个 Display 恰为 `SessionBusy` 的枚举值（并同步 `error.rs` 内既有 Display 用例表）。**已确认接受**在 `visp-core/src/error.rs` 新增枚举值。 | 步骤 3a |
| H | **orchestrator 忙失败前的准备动作不回滚**（审核新发现） | 已在步骤 3a 增补「忙失败必须回滚 registration 与 prompt 模板追加，或把二者挪到 `start_loop` 成功之后」。**请确认接受**该改动范围（略超出原计划的「只改受理逻辑」）。 | 步骤 3a |
| I | **触发窗口的分层覆盖** | 已按「TUI 单元 + daemon 集成 + ACP 端到端（仅一条）」写进步骤 4b；不要求三个 TUI 按键窗口做端到端。**已确认接受**。 | 步骤 4b |
| J | **TUI `SessionBusy` 错误码特判**（反思审核新增） | **已确认实施**：对 `SessionBusy` 不置 tab 为 `Error`、改为一行 Status 提示；已并入步骤 4b。注意**实际是三处站点**（`route_frame` 两处 + `render_pending` 的 Error 臂）。 | 步骤 4b |
| K | **G5 收尾归属判据**（三轮审核新增） | **已确认实施**：`handle_done` / `handle_agent_error` 收尾前校验状态未被新回合改写，否则幂等空操作；封堵设计 §5.2 的 B6（正常完成路径时序窗口）。已并入步骤 3a。 | 步骤 3a |
| L | **准备动作后移**（而非回滚） | **已定案**：`append_system_prompt_template` 无撤销 API，故把它与 `active_agents.register` 后移到 `start_loop` 成功之后，不新增 API。 | 步骤 3a |

### 2. 环境依赖与前置

- **protoc**：步骤 6a 改动 `.proto` 后需重新构建（`crates/visp-proto/build.rs` 用 tonic-build 编译）；本机需安装 protoc，否则构建失败。
- **测试二进制**：`cargo test -p <crate> <name>` 与全量门禁命令需在仓库根执行。
- **daemon 运行态**：步骤 2a/3a 的改动在 daemon 重启后生效（若手工冒烟验证，需重启 `visp-daemon`）。

### 3. 已知限制（本期显式接受，写入交付说明）

- **增量方案丢帧不自愈**：快照语义下丢帧可由下一帧全文自愈；改增量后丢失的增量**永久缺失**。这是 B1/B2/B3 的共有代价（设计 §3.3 第 4 条）。注：该自愈窗口仅存在于**同一连接内的后续帧**；Lagged replay 与重连回放**本就不含思考帧**，故实际可见窗口有限。
- **回放路径本就不下发思考帧**：增量改造不使其更差也不引入自愈（设计 §9 第 1 条）。
- **`extra_blocks` 的既有缺陷不改**：core「只保留最后一块」、Anthropic 交错思考/工具丢签名、`signature_delta` 未解析、终块分支疑似不可达——均**另立项**（设计 §9 第 2/3 条），本期不使其恶化。
- **TUI 门禁 / cancelling 态不改**（用户已确认，设计 §12.2）：仅在取消/重连窗口内可能收到一条「正在生成，请稍候」。
- **思考块语义变更为静默契约变更**（设计 §3.5）：不加标记字段，旧版客户端无法程序化识别；旧 TUI 会静默降级为只显示最后一片。缓解：proto 注释写明 + 发布说明点明。
- **多块思考时显示内容多于持久化内容**（设计 §4.4）：core 仍只保留最后一块用于 `extra_blocks`，对外增量按块累积；TUI/ACP 显示「块1+块2」拼接而落库只有最后一块。相比旧快照语义不丢显示内容，属可接受差异，以用例固化。
- **跨轮追加到同一 TUI 行**（设计 §4.4）：thinking-only 跨多轮时，新块首帧会被追加到上一轮仍在的同一 Thinking 行；以用例固化。

### 4. 实施风险提示

- **不能并行写同一文件**：见「Wave 并行策略」的显式标注；若派多个实施 agent，务必按 Wave 分批。
- **可测性接缝**：步骤 2a 的「抽取可单测判定函数」是本计划为满足设计 §10.2「真实分支负路径断言」而设的必要接缝；若实施者选择保留原分支结构，则必须由步骤 4a 的真实入站 harness 覆盖，**不得**用「测试内复制判定逻辑」冒充覆盖（既有 `service.rs:3699` 即此类反例）。
- **行号漂移**：本文所有 `file:line` 均为评审快照，实施前以 grep/读源码重新定位为准。
- **测试风格偏差（有意）**：`session.rs` / `agent_loop.rs` / `translate.rs` 使用文件内 `#[cfg(test)] mod tests`，与 AGENTS.md 的 `*_tests.rs` 约定不一致；本次**沿用各文件既有风格**，不为此搬迁测试（避免无关 diff）。
- **恢复路径保留**：`main.rs` 启动时对 DB 中孤儿 `Running` 的恢复性重置**保留且不得被守卫误拦**（设计 §6.2 保留项）；其集成断言可能需要 daemon 级/手工验证，本计划列为可选验证，不单独设提交。

### 5. 与设计文档的一致性声明

本计划严格对齐设计 §6.2（G1–G4）、§10（测试分层）与 §11.2（先问题二后问题一）。凡设计 §12.4 未定案项，均在本文档「需要用户额外拍板的点」中显式列出，不做静默假设。
