# herdr 集成设计（visp ↔ herdr 兼容方案）

> 状态：设计草案，待用户审核
> 日期：2026-09-27
> 依赖事实来源：herdr 本地源码 `../herdr @ 3f2a6e74`；visp 全仓代码调研
> **前提注记**：本设计以「visp 尚无通用 lifecycle hook 系统、由 `visp-tui` 内置上报」为前提。若 visp 后续实现通用可配置 hook 系统，需按 hook 设计重新评估本方案——见 **§11 重评估清单**（该清单不改变 §3 的现行决策）。

---

## 1. 背景与目标

### 1.1 为什么做

在 herdr 中同时开多个 agent（opencode、kilo、pi 等）时，herdr 侧边栏/状态栏会实时显示每个 pane 的 `working / blocked / idle`。用户不必逐 pane 轮询就能看出「哪个 agent 在干活、哪个在等审批」。

visp 当前在 herdr 中运行时，pane 只是一个不可识别的普通终端进程：herdr 看不到 visp 的语义状态，用户必须切回该 pane 才能判断 visp 是在生成、还是在等审批弹窗。本设计让 visp 主动向 herdr 上报语义状态，使 visp 与 herdr 原生支持的 agent 在体验上对齐。

### 1.2 目标

1. **状态可见**：在 herdr 中看到 visp pane 的 `working`（生成中）、`blocked`（等待用户处理，含审批弹窗与提问）、`idle`（等待新输入）。
2. **退出干净**：visp 进程退出时释放占用，不留僵尸状态。
3. **零侵入**：未在 herdr 中运行时，visp 行为与现状**完全一致**，无新增输出、无性能损耗。
4. **降级安全**：herdr 不可用（二进制缺失、socket 断开、版本不识别）时静默降级，绝不阻断 agent 主流程。

### 1.3 非目标

- 不追求 herdr 的**原生会话恢复**（第三方 source 被硬编码封闭，见 §2.2）。
- 不追求被 herdr **屏幕识别/进程识别**为原生 agent（需改 herdr 二进制）。
- 不做 herdr 之外的终端复用器（tmux/zellij）的等价集成——本设计只覆盖 herdr 的开放上报面。
- 不把 visp 做成 herdr plugin 的强依赖（plugin 路线是可选项，见决策 7/8）。

---

## 2. 现状分析

### 2.1 visp 侧可对接锚点

| 锚点 | 位置 | 说明 |
|------|------|------|
| pane 身份环境变量 | 进程环境 | `HERDR_ENV` / `HERDR_SOCKET_PATH` / `HERDR_BIN_PATH` / `HERDR_WORKSPACE_ID` / `HERDR_TAB_ID` / `HERDR_PANE_ID`，被 pane 内进程显式注入 |
| 三进程生命周期 | `crates/visp/src/main.rs` | launcher 启动 daemon（后台）→ health check → 启动 `visp-tui`（**前台**）→ 等 TUI 退出 → shutdown daemon。pane ↔ launcher ↔ daemon ↔ TUI 基本 1:1 |
| 事件挂接点 | `crates/visp-tui/src/event.rs` | `handle_grpc_message` 的 `UserQuery` / `Done` / `Error` 分支；`Enter` 提交分支 `set_generating(true)` |
| blocked 判定 | `event.rs` UserQuery 分支 + `app.rs::ConfirmState` | UserQuery 到达即设置 `ConfirmState`（审批弹窗），`ui.rs` 渲染弹窗 |
| 对话状态机 | `crates/visp-core/src/session.rs` | `SessionStatus = Idle / Running / Completed / Error`（daemon 权威，经 gRPC 同步） |
| 终端通知模块（可复用范式） | `crates/visp-tui/src/notify.rs` | 纯函数探测/编码 + 单一 IO 点 + 会话过滤在挂接点完成——herdr 上报模块可对齐此分层 |
| 会话持久化 | `~/.visp/data/visp.db` | UUID v4 + 前 8 位短 ID；支持 `-s <prefix>` 前缀恢复；per-project 查询（`list_by_project`） |
| 配置加载 | `crates/visp-config/src/config.rs`、`path.rs` | 全局 `~/.config/visp/`（可被 `VISP_CONFIG_DIR` / `--config-dir` 覆盖）+ 项目 `.visp/daemon.toml`；TUI 启动即加载 |
| skills 目录 | `.visp/skills/<name>/SKILL.md`、全局 `~/.config/visp/skills/` | `visp_config::skills::load_skills` |
| 既有缺口 | TUI 全仓 | **无任何 herdr 相关代码**；无终端标题 OSC 0/2 |

### 2.2 herdr 侧开放 / 封闭边界

| 能力 | 状态 | 结论 |
|------|------|------|
| 语义状态上报（`pane report-agent`） | **开放** | 任意 source 可上报 state；自定义 source 的 state 被采纳为权威 |
| 上报会话（`report-agent-session`） | 开放（但存储受限） | 第三方 session id 被存为 `None` |
| 释放（`pane release-agent`） | 开放 | 进程退出时可用 |
| 展示层 metadata（`report-metadata`） | 开放 | title / display-agent / state-label / token；token 不受守卫 |
| Socket API 等价面 | 开放 | newline-delimited JSON over `HERDR_SOCKET_PATH` |
| 状态权威白名单 `full_lifecycle_hook_authority` | **封闭** | 仅 6 组 `(herdr:<agent>, <agent>)`；visp 不在其中 |
| 屏幕识别（A 层 `Agent` 枚举） | **封闭** | 24 个编译期变体，新增识别必须改 herdr 二进制 |
| 进程识别 | **封闭** | 同上，无 manifest / 插件绕过 |
| `agent start --kind` 白名单 | 封闭 | kind 硬编码 |
| 原生会话恢复 `is_official_agent_source()` | **封闭** | 18 组白名单，resume argv 硬编码 |
| plugin（`herdr-plugin.toml`） | 开放但受限 | 可加 keybinding / pane / event 钩子 / `[[startup]]`；**不能**注册 agent kind、**不能**触发原生 resume |
| skill（`skills/herdr/SKILL.md`） | 开放 | 教 pane 内 agent 反向驱动 herdr；护栏 `test "${HERDR_ENV:-}" = 1` |

**关键推论**：visp 能做的全部事情都落在「上报面」；凡是需要 herdr 把 visp 当作一等 agent 的能力（屏幕/进程识别、原生 resume、authority 白名单）都需要**上游 PR**。

### 2.3 pane 与三进程关系

```
herdr pane (pty)
   │ 注入 HERDR_ENV=1, HERDR_PANE_ID=..., ...
   ▼
launcher `visp` ──(继承全部 HERDR_*)──┬── daemon `visp-daemon`（后台，继承 HERDR_*）
                                      └── TUI `visp-tui`（**前台**，继承 HERDR_*）
```

- 前台进程是 **TUI**，因此 herdr 的进程权威与「用户可见状态」天然指向 TUI。
- daemon 虽也继承 `HERDR_*`，但它是后台子进程；让它上报会与「前台即 TUI」的事实错位。
- launcher 是 pane 直接子进程，是**唯一能在 TUI 异常退出后仍短暂存活**的 visp 进程（负责 shutdown daemon），适合作为 release 兜底。

---

## 3. 核心决策

> 每项给出：**推荐** / 备选 / 理由。

### 决策 1：上报源 —— 由 TUI 承担

**推荐：`visp-tui` 作为唯一状态上报源。**

理由：
1. **语义最贴近**：herdr 状态本质是「这个 pane 对用户呈现什么」。`blocked` 的唯一判据是 `ConfirmState`（审批弹窗）存在，而它只在 TUI 层被显式化。
2. **持有 pane 身份**：`HERDR_*` 环境变量在 TUI 进程内可直接读取，无需跨 gRPC 传参。
3. **复用通知范式**：与 `notify.rs` 的挂点（`Done` / `UserQuery`）完全同构，可共享「会话过滤在挂接点、模块只做编码/IO」的分层。
4. **不污染核心**：上报是展示层职责，不应进入 daemon 的 agent 循环。

备选：
- **daemon 上报**：daemon 是 `SessionStatus` 权威，且不随 TUI 重连抖动。但 daemon 无从区分「审批弹窗」与「普通提问」（UserQuery 对它只是一个事件），且后台进程上报与「前台即 TUI」的事实冲突。→ 不推荐。
- **launcher 上报**：launcher 知道 pane 身份与子进程树，但不解析 gRPC 流，拿不到语义状态；要拿需额外旁路，复杂无收益。→ 不推荐，**仅保留为退出兜底**（TUI 异常退出时补发 release，见决策 3）。
- **TUI + daemon 混合**：两个上报源会导致 seq 空间分裂、权威震荡。→ 不推荐。

### 决策 2：组件划分 —— TUI 内模块，暂不新建 crate

**推荐：在 `visp-tui` 内新增独立模块（如 `herdr.rs`），与 `notify.rs` 并列；配置节落在 `visp-config`。**

理由：
- 上报逻辑（env 探测 → 状态映射 → 变化去重 → CLI/IO → 失败降级）与 `notify.rs` 高度同构，且**仅 TUI 使用**。
- 新建 crate 服务于当前不存在的复用需求，属预防性抽象（YAGNI）。
- 保持「探测/映射/编码为纯函数 + 单一 IO 点」的内部边界，使单元测试无需真实 herdr。

备选：新建 `visp-herdr` crate。
- 触发条件：二期 metadata 需要 daemon 提供 token 用量/模型名，或 launcher 需要独立发 release。届时把纯逻辑上移为 crate 即可。
- 建议：MVP 不建 crate，但目录/模块命名预留语义，便于后续提取。

### 决策 3：状态映射与挂点

**推荐映射：**

| herdr state | visp 触发事件 | 挂点 |
|---|---|---|
| `working` | 用户提交输入（非 `/` 命令）后进入生成 | `event.rs` Enter 分支 `set_generating(true)` |
| `blocked` | `AgentEvent::UserQuery`（审批弹窗或普通提问） | `event.rs` UserQuery 分支（设置 `ConfirmState` 处） |
| `idle` | `AgentEvent::Done`（非 stale）、`Error` 收敛、`Ctrl+C` 取消后、会话切换/新建后 | `event.rs` Done / Error / cancel 分支 |
| `unknown` | 启动建立权威、重连中断期间 | `run()` 起点、`on_disconnect` |

**去抖策略：**
- **变化去重**：仅在 herdr state 字段发生变化时上报。工具调用发生在 `generating=true` 期间，状态不变，天然不会 flap——无需按工具调用去抖。
- **单飞合并**：同一时刻仅允许一个上报在途；在途期间产生的新状态覆盖「待发槽」，完成后发最新态（保证顺序且不堆积）。
- **不做固定时间节流**：`blocked` 是关键信号，被节流吞掉弊大于利。仅 metadata 做节流（见决策 5）。

**seq 归属：** 由 TUI 进程内的单调计数器拥有。
- 起点必须**高于上一 TUI 进程的历史值**（herdr 丢弃同 source 的旧序号）。因此以「进程启动时的 epoch 毫秒」为基数递增，而非从 0 开始——否则 TUI 重启后新进程的低序号会被全部丢弃。
- 每次上报 `seq += 1`。

**退出 release：** TUI 事件循环退出返回前发 `release-agent`（best-effort）。
- 兜底：launcher 在 TUI 退出后、daemon shutdown 前，若检测到 `HERDR_ENV=1`，补发一次 `release-agent`（覆盖 TUI 崩溃/被 kill 的场景）。

### 决策 4：上报通道 —— 优先经 `HERDR_BIN_PATH` 调 CLI，异步降级

**推荐：经 `HERDR_BIN_PATH` 调 herdr CLI 子进程，异步 fire-and-forget。**

理由：
- 跨平台，协议细节由 herdr CLI 维护，visp 不做协议解析。
- 不绑定 socket 协议版本，降低 herdr 迭代导致的不兼容风险。

实现约束：
- **串行化**：用单个后台 task + 有界 channel 串行发送，保证 seq 顺序、避免并发 spawn。
- **超时**：单次调用设短超时（数量级秒），超时即丢弃并记 debug 日志。
- **降级**：`HERDR_BIN_PATH` 缺失或首次调用失败 → 标记禁用，本进程不再尝试；所有失败路径静默（`tracing::debug`），不影响主流程。
- **频率**：事件驱动（每用户回合数次），无轮询；状态类可被新状态覆盖丢弃，不保证送达每一条。

备选：直接 socket IPC（`HERDR_SOCKET_PATH` 的 newline-delimited JSON）。
- 省一次进程 spawn。但需自行处理连接/重连/协议/版本差异，风险与代码量都更高。
- 定位：二期可选优化，仅当实测发现短会话中 spawn 开销可观时启用。

### 决策 5：展示增强 metadata —— 二期，默认关闭 token

**推荐：**
- **title**：项目名 + 短 session id（如 `myproj [550e8400]`），便于 herdr 侧识别 pane。
- **display-agent**：`visp`。
- **state-label**：用户可读中文标签，区分 `blocked` 的子类型（「待审批」vs「待回答」）。
- **token**：**默认关闭**。`report-metadata` 的 token 字段不受守卫，更新频繁且收益有限；仅在显式开启时上报累计输出 token。

理由：title / state-label 是低成本高价值展示；token 属于易变且可能引起高频写入，须由开关控制。metadata 做最小节流（如 ≥1s）。

### 决策 6：屏幕竞争边界 —— 预期不冲突，并保留「夺回权威」手段

**推荐预期行为：**
- visp 不在 herdr 屏幕识别的 `Agent` 枚举内、也无 manifest，因此屏幕检测**预期不会识别 visp**，不会产生竞争性状态。
- 但「自定义 source 的 state 被采纳为权威**并不跳过屏幕检测**」意味着：若屏幕出现 blocker 信号，可能覆盖为 `Blocked`。
- 因此 visp 在上报权威状态后，保留**周期性心跳重发当前状态**（低频，如数秒一次，或在检测到状态被外部改动时重发）作为夺回权威的兜底。

**实测验证清单**（必须实测确认，不能只靠代码推断）：
1. visp pane 有两种状态连续切换时，`herdr agent list` / `explain` 显示是否与上报一致。
2. visp 屏幕输出（含 spinner、颜色、ANSI）是否触发 herdr 屏幕兜底误判。
3. visp 的 `blocked` 是否被屏幕信号覆盖为 `Blocked`（注意大小写语义）。
4. 未上报时 herdr 对 visp pane 的默认显示是什么（确认「不识别」）。
5. 上报 `unknown` 与不上报的差异（是否建立权威）。

### 决策 7：会话恢复 —— 自建 per-project last-session，显式开关优先

**推荐：**
- **数据来源**：复用 daemon 的持久化（`~/.visp/data/visp.db`，已有 `list_by_project`），为每个 canonical `project_path` 记录「最近 session」。
- **入口**：新增 `--resume-last`（显式）；自动恢复（无参 + `HERDR_ENV=1` 时自动续接最近 session）作为**默认关闭的可选配置**。
- **默认关闭自动恢复的理由**：误恢复风险高——用户以为开了新会话，实则继承了旧上下文，行为难以察觉且可能泄漏上下文；跨项目/跨 pane 混淆同样危险。显式开关让用户对语义有控制权。
- **可发现的引导**：`HERDR_ENV=1` 且存在可恢复会话时，TUI 可提示「可用 `--resume-last` 续接上次会话」，而非静默自动恢复。

**备选：**
- **herdr plugin `[[startup]]` 路线**：在 herdr 重启后由 plugin 执行 `pane run visp --resume-last` 恢复 pane。优点是与 herdr 生命周期绑定；缺点是 plugin 只能重启进程、无法触发原生 resume，且需 pane 绑定与用户安装 plugin。定位为三期可选项。
- **cwd marker 文件**：在项目目录写 `.visp-last-session`。缺点是污染用户仓库、易提交误入。→ 不推荐。

**风险**：跨项目混淆（以 canonical path 为 key 缓解）；pane 跨 workspace move 后 cwd 变化（以显式 `-p`/当前 cwd 为准）。

### 决策 8：herdr skill 分发 —— 可选、显式安装、不默认写入

**推荐：**
- 机制：利用 visp 现有 skills 加载（`.visp/skills/herdr/SKILL.md` 或全局 `~/.config/visp/skills/herdr/SKILL.md`），使 visp 内 agent 能按 `skills/herdr/SKILL.md` 的指引反向驱动 herdr。
- 提供**显式安装入口**（如安装命令或 `/init-skill` 模板），指向 herdr skill 内容。
- **默认不写入**用户目录，避免污染与版本漂移（herdr skill 随 herdr 迭代）。

理由：反向驱动 herdr 是进阶能力，受众有限；显式安装 + 文档指引即可，无需默认开启。

### 决策 9：配置开关

**推荐：新增 `[herdr]` 配置节（全局与项目 daemon.toml），字段如下：**

| 字段 | 含义 | 默认 |
|------|------|------|
| `enabled` | 总开关（`auto` / `true` / `false`） | `auto` |
| `report_state` | 是否上报语义状态 | `true` |
| `report_metadata` | 是否上报展示层 metadata | `false` |
| `report_title` | title 上报 | `false` |
| `report_tokens` | token 上报 | `false` |
| `heartbeat_ms` | 权威心跳重发间隔（0 = 关） | 温和默认（数秒） |
| `metadata_interval_ms` | metadata 节流间隔 | ≥1s |
| `source` | 上报 source 标签 | `custom:visp` |
| `auto_resume_last` | `HERDR_ENV=1` 时自动续接最近会话 | `false` |

**优先级（自动检测 vs 显式配置）：**
1. `enabled = false` → 强制关闭（覆盖一切）。
2. `enabled = auto`（默认）→ 仅当 `HERDR_ENV=1` 且 `HERDR_BIN_PATH` 可用时启用（两者缺一即关闭）。
3. `enabled = true` → 尝试启用，但缺少 pane 身份仍无法上报（记为 debug）。
- 一律以「能否真正上报」为准做**运行时降级**，配置只表达意图。

### 决策 10：能力边界

**无需改 herdr 二进制即可达成（开放面）：**
- 语义状态上报 `working / blocked / idle / unknown`。
- 退出 `release-agent`。
- `report-metadata` 展示增强（title / display-agent / state-label / token）。
- 自建会话恢复（daemon per-project last-session + `--resume-last`）。
- skill 分发（visp 内反向驱动 herdr）。
- 发布 herdr plugin（keybinding / `[[startup]]`）。

**必须上游 PR 才能达成（封闭面）：**
- 把 visp 纳入 `full_lifecycle_hook_authority` 白名单并**跳过屏幕兜底**。
- 屏幕识别/进程识别把 visp 当原生 agent（需扩 `Agent` 枚举 + manifest）。
- `agent start --kind` 支持 visp。
- 原生会话恢复 `is_official_agent_source()` 开放为可配置。
- 上游建议方向：把「自定义 source」与「屏幕兜底」解耦（允许声明 authoritative 并 opt-out 屏幕检测），并把 resume 白名单改为可扩展注册。

---

## 4. 架构与数据流

### 4.1 组件关系

```
visp-config
  └─ [herdr] 配置节 ───────────────┐
                                   ▼
visp-tui
  ├─ notify.rs      （既有，终端通知）
  └─ herdr.rs       （新增）
        ├─ 探测：HERDR_ENV / HERDR_BIN_PATH / HERDR_PANE_ID
        ├─ 映射：visp 事件 → herdr state（纯函数）
        ├─ 去重 + 单飞 + seq 计数
        └─ 上报 task（有界 channel → 调 HERDR_BIN_PATH CLI，超时/失败静默）
  挂接点：event.rs 的 Enter / UserQuery / Done / Error / cancel；run() 起点与退出；on_disconnect
```

### 4.2 状态流转（TUI 视角）

```
       [启动]──unknown──┐
                       │
   用户提交输入 ────────▶ working
        ▲                 │
        │ 批准/继续生成    │  UserQuery(审批/提问)
        │                 ▼
     (idle)◀──Done/Error/cancel── blocked
        │                 │
        │  用户响应        │
        └─────────────────┘
   连接中断 ──▶ unknown ──(重连成功)──▶ idle(按重放帧再修正)
   [退出] ──▶ release-agent
```

### 4.3 上报链路（失败降级）

```
TUI 事件
  → herdr.rs 状态映射 → 变化去重 → 单飞合并 → 有界 channel
      → 后台上报 task（串行）
          → HERDR_BIN_PATH 可用？
              ├─ 否 → 标记禁用，丢弃（debug 日志）
              └─ 是 → spawn CLI（短超时）
                        ├─ 成功 → seq+=1
                        └─ 失败/超时 → 丢弃（debug 日志），主流程不受影响
```

### 4.4 会话恢复数据流

```
visp --resume-last （或 auto_resume_last=true 且 HERDR_ENV=1）
  → daemon 查询 per-project 最近 session
      ├─ 命中 → 复用 -s <short-id> 路径加载历史（既有逻辑）
      └─ 未命中 → 创建新会话（既有逻辑）
```

---

## 5. 影响范围与向后兼容

| 模块 / 进程 | 改动 | 性质 |
|---|---|---|
| `visp-tui` | 新增 `herdr.rs` 模块；在既有挂接点插入上报调用；`run()` 起点/退出加初始化与 release | 新增 |
| `visp-tui` `main.rs` | 读取配置节、构造上报引擎并传入 `run` | 新增 |
| `visp-config` | 新增 `[herdr]` 配置节与默认值 | 新增 |
| `visp`（launcher） | 退出兜底补发 `release-agent`（仅 `HERDR_ENV=1`）；`--resume-last` 透传 | 新增 |
| `visp-daemon` | per-project last-session 查询（复用既有 `list_by_project`），可能新增轻量 RPC | 扩展 |
| `visp-core` | **无改动** | — |

**向后兼容保证：**
- 未设置 `HERDR_ENV` → 上报引擎整体禁用，visp 行为与现状**逐字节不变**。
- 新增配置节缺省时取默认值，老 `daemon.toml` 无需修改。
- `--resume-last` 为可选参数，不传即无行为变化。
- 上报失败的降级路径不产生任何用户可见输出。

---

## 6. 边界情况

| 场景 | 期望行为 |
|---|---|
| `HERDR_ENV` 未设 | 整体禁用，零行为变化 |
| `HERDR_ENV=1` 但 `HERDR_BIN_PATH` 缺失 | 探测后标记禁用，静默降级 |
| `HERDR_BIN_PATH` 指向旧版 herdr（无 `report-agent`） | CLI 报错/非零退出 → 丢弃并禁用，不影响主流程 |
| `HERDR_PANE_ID` 缺失（如被手工剥离环境） | 无法上报，记 debug 并禁用 |
| pane 跨 workspace move（旧 `HERDR_PANE_ID` 变别名） | 以进程启动时快照的 `HERDR_PANE_ID` 为准；move 后旧 id 失效时上报会失败 → 降级；属于可接受限制（需实测 herdr 别名行为） |
| 多个 visp 实例 / 多 pane | 每个 TUI 进程各持独立 source+seq；source 相同但 pane 不同，herdr 按 pane 区分 |
| 同一 pane 内多 TUI（异常场景） | 后启动者 seq 基数更大，权威归后者；前者退出 release 不应影响后者（需实测） |
| TUI 崩溃 / 被 kill（未走正常退出） | 无 release；herdr 进程退出会否决上报；launcher 兜底补 release |
| daemon 与 TUI 生命周期不一致（TUI 重连） | TUI 进程存活、seq 连续；重连期间上报 `unknown`，恢复后再修正，不产生假 `working` |
| 用户 Ctrl+C 取消生成 | stale Done 不上报；显式上报 `idle` |
| 审批弹窗被 Esc 关闭 | `blocked` → `idle`，不误留 blocked |
| metadata 上报与 state 上报竞争 | state 优先；metadata 单独节流，不阻塞 state |

---

## 7. 验证方法

**herdr 侧观测：**
- `herdr agent list`：确认 visp pane 的状态与上报一致。
- `herdr agent explain --json <pane>`：查看 state 的**来源与权威归因**（区分上报 vs 进程/屏幕兜底）。
- `herdr integration status`：确认集成面可用性。

**手工场景清单：**
1. `HERDR_ENV` 未设：运行 visp，确认零新增输出、无 herdr 变更。
2. 在 herdr pane 内运行 visp：发起一次提问，观察 `working → idle` 转换。
3. 触发审批弹窗（需审批的工具）：观察 `blocked`，批准后回到 `working`，回合结束 `idle`。
4. 触发普通提问（`UserQuery` 非审批）：观察 `blocked` 与 state-label 区分。
5. Ctrl+C 取消：观察 `idle`，不残留 `working`。
6. `kill -9` TUI：观察 herdr 进程退出否决 + launcher 兜底 release。
7. 模拟 herdr 不可用（清空 `HERDR_BIN_PATH`）：确认 visp 全功能正常。
8. `--resume-last`：在项目 A 会话后重开，确认续接正确会话；在项目 B 不误继承 A。
9. 连续多个工具调用：确认不产生 working↔idle flap。

---

## 8. 分期实施建议

| 阶段 | 内容 | 验收 |
|---|---|---|
| **MVP** | 探测 + 状态上报（working/blocked/idle/unknown）+ 退出 release + 配置节 + 降级 | §7 场景 1–7 通过；未设 `HERDR_ENV` 行为不变 |
| **二期** | metadata（title / display-agent / state-label，token 可选）+ 会话恢复（`--resume-last` + daemon last-session）+ 心跳夺权 | §7 场景 8–9 通过；误恢复防护验证 |
| **三期** | herdr skill 分发 + 可选 plugin（`[[startup]]` 恢复）+ socket IPC 优化评估 | skill 安装可用；plugin 恢复路径可用 |

---

## 9. 风险与未确认项

1. **自定义 source 与屏幕检测并存（最高优先，必须实测）**：任务给定事实是「自定义 source 的 state 被采纳为权威，但屏幕检测不被跳过，屏幕 blocker 可覆盖为 Blocked」。visp 无 manifest、预期不被识别，但**必须实测**确认无屏幕信号干扰，并据此决定是否启用心跳夺权。
2. **seq 跨进程语义**：herdr「丢弃同 source 旧序号」跨进程是否持续（release 后是否重置）未确认。本设计以时间戳基数规避，但需实测验证 TUI 重启后上报不被丢弃。
3. **同一 pane 多 TUI 的权威归属**：异常场景，行为未验证。
4. **pane move 后 `HERDR_PANE_ID` 别名行为**：未确认，可能影响长驻 TUI 的上报目标。
5. **herdr 版本漂移**：`report-agent` / metadata 字段可能演进；CLI 调用失败即降级可缓冲，但需在文档中记录最低版本要求（待测）。
6. **launcher 兜底 release 的时序**：TUI 退出 → launcher release → daemon shutdown 的窗口是否足够，需实测。

---

## 10. 待讨论问题

1. **自动恢复默认值**：`auto_resume_last` 默认关闭（本设计推荐）还是默认开启？开启可减少操作但增加误恢复风险。
2. **`blocked` 的粒度**：审批弹窗与普通提问是否统一映射为 `blocked`（本设计推荐，用 state-label 区分），还是普通提问映射为 `idle`？
3. **心跳夺权的默认间隔**：是否默认开启、间隔取值（过密会持续 spawn CLI 进程）。
4. **source 命名**：`custom:visp` 是否符合用户偏好（长度/字符集均合法）。
5. **skill 分发形态**：内置安装命令 vs 仅文档指引 vs `/init-skill herdr` 模板。
6. **是否投入上游 PR**：是否推动 herdr 开放自定义 source 的 authority + 屏幕 opt-out + resume 白名单可扩展。
7. **token metadata 是否值得做**：`report-metadata` token 无守卫，收益与成本需用户判断。

---

## 11. 后续演进：通用 hook 系统落地后的重评估（注记）

> **注记性质**：本节**不改动 §3 的任何现行决策**，只登记一条演进约定——**在 visp 实现通用可配置 lifecycle hook 系统之后，需根据 hook 的设计重新评估本方案**。当前基线以「visp 尚无通用 hook、由 `visp-tui` 内置上报」为前提。

### 11.1 为什么需要重评估

一旦 visp 具备通用生命周期 hook（用户在配置中将脚本挂到 `session_start` / `prompt_submit` / `permission_request` / `turn_end` 等事件上执行），herdr 集成可从「visp 核心内置 herdr 代码」**外置**为「一条 hook 规则 + 一份 herdr hook 脚本」，与 herdr 对 Claude/Codex（shell hook）、Pi/OpenCode（plugin）的集成惯例同构。此时 §3 的多数决策会被「hook 系统设计」吸收，需要重新定夺。

**必须澄清的边界（重评估的前提，不因 hook 改变）**：herdr 的 `full_lifecycle_hook_authority` 白名单只认 `herdr:<agent>` 六个组合；`custom:visp` 无论经 CLI 还是经 hook 上报，**权威级别完全相同**。引入 hook **不改变任何封闭面**（屏幕识别 / 进程识别 / 原生 resume / authority 白名单仍需上游 PR）。

### 11.2 重评估清单（对照现行决策）

| 现行决策 | hook 落地后需重新评估的变化 | 触发条件 |
|---|---|---|
| **D1 上报源 = TUI** | 是否改为 **daemon 发射**：daemon 是 `SessionStatus` 权威、是真正阻塞 pending query 的一方（`blocked` 的原因）、headless 也可工作。仍须**单一发射源**。 | 通用 hook 以 daemon 为事件权威时 |
| **D2 组件划分 = TUI 内模块，不建 crate** | 是否抽出通用 `visp-hooks` 执行器 crate（当 hook 执行器出现 daemon + launcher 两个消费方时，原「仅 TUI 使用」的 YAGNI 前提被越过）。 | 出现第二个 hook 消费方 |
| **D3 映射挂点 = event.rs** | 挂点是否迁至 daemon（`service.rs` 帧映射 + `session_start/end` 点）；`seq` 是否由 visp 每事件发放 `VISP_HOOK_SEQ`（脚本无状态、无法自算跨重启单调值）。 | 同上 |
| **D4 上报通道 = 异步调 CLI** | 通用层是否变为 hook 执行器；herdr 是否降级为「一个内置 binding / 可选脚本覆盖」。执行语义（异步 / 超时 / 静默 / 有界串行）不变。 | 同上 |
| **D5 metadata（二期）** | 基本不变（daemon 本就是 `UsageInfo` 来源）。 | — |
| **D6 屏幕竞争** | **不变**（预期不冲突 + 心跳夺权 + 实测清单）。 | — |
| **D7 会话恢复** | **不变**：hook 无法让 herdr 记住第三方 session id，per-project last-session + `--resume-last` 仍是唯一路径；hook 仅增强可发现性（title / 短 id）。 | — |
| **D8 skill 分发** | 可顺带附带 herdr hook 参考脚本。 | — |
| **D9 配置 = 单一 `[herdr]` 节** | 是否拆为通用 `[hooks]`（规则 + `disabled_builtins`）+ 具名 `[herdr]`（集成开关）；优先级 = 用户规则 > 内置 binding。 | 引入 `[hooks]` 时 |
| **D10 能力边界** | **不变，且强化**：hook ≠ authority。 | — |

### 11.3 不受 hook 影响、必须坚持的结论

1. **能力边界不变**：屏幕/进程识别、`agent start --kind`、原生 resume `is_official_agent_source()`、`full_lifecycle_hook_authority` 白名单——hook 帮不上忙。hook 只是上报姿势。
2. **屏幕竞争边界不变**：实测清单与心跳夺权兜底仍需落实。
3. **状态语义映射本身不变**：`working/blocked/idle/unknown` 与 `Enter→working、UserQuery→blocked、Done/Error/cancel→idle` 的对应关系不变（只可能改发射位置 / 触发源）。
4. **降级安全与零侵入原则不变**：hook 失败 / 超时 / 缺失 → 静默丢弃、绝不阻断 agent 主流程；未设 `HERDR_ENV`（或未配置 hook）→ 行为与现状逐字节一致。
5. **D4 的执行约束不变**：异步 + 超时 + 失败静默 + 有界串行，本就是 hook 执行器应有的形态。

### 11.4 hook 系统需先解决、且与本集成相关的前置问题

以下问题须在 hook 设计阶段定论，否则会反噬 herdr 集成：

1. **执行语义**：仅允许异步 fire-and-forget + 强制 `timeout_ms`，不允许阻塞型 hook（否则用户脚本可卡死 agent 主循环）。
2. **安全边界（最大新增风险）**：项目级 hook 是任意代码执行面（`.visp/hooks` 意味着「克隆仓库即投毒」）。需决定是否只允许全局 `~/.config/visp` 配置 hook，或对项目级强制 trust 确认。
3. **隐私**：`prompt_submit` / `permission_request` 是否携带 prompt / message 原文——建议默认脱敏，原文需显式开启。
4. **多规则语义**：同一事件多条 hook 的串行/并发与失败隔离。
5. **内置 vs 用户覆盖**：`disabled_builtins` 与同名用户规则共存时的去重/覆盖规则。

### 11.5 参考：事件契约草案（**已被 hook 设计 v2 取代**）

> ⚠️ **本节已被取代（2026-09-28）**：`docs/design/2026-09-27-visp-hooks-design.md`（v2）已把事件契约正式定为 **PascalCase**（`SessionStart`/`UserPromptSubmit`/`ToolCallRequested`/`PreToolUse`/`PermissionRequest`/`PermissionResult`/`Stop`/`StopFailure`/`SessionEnd`…），并引入 `PreToolUse` 二分、`PermissionRequest.kind` 显式字段、`source` 收敛为 `{startup, resume}`。下方 snake_case 草案仅作历史参考，**不要**按它实现。
>
> 另：hook 设计 v2 的发射源为 **daemon 侧 broadcast 事件总线**（取代本设计 §3 决策 1 的「TUI 内置上报」），且新增只读 `GetHookStats` RPC（proto 有改动）。herdr 侧兼容**不依赖事件名**，只依赖三态映射 + `session_start_source` + seq。

| 事件 | 触发时机（visp 锚点） | 关键上下文 | 可映射的 herdr state |
|---|---|---|---|
| `session_start` | daemon 创建/加载 session | `session_id`、`short_id`、`project_path`、`model`、`resume`、`cwd` | 建立权威（idle/unknown）+ title |
| `prompt_submit` | 用户提交输入 | `session_id`、`turn_id`、`source` | `working` |
| `permission_request` | `AgentEvent::UserQuery` | `session_id`、`query_id`、`kind`(approval/question)、`message`、`options` | `blocked` |
| `permission_response` | 用户响应/取消 | `session_id`、`query_id`、`outcome` | `working` |
| `turn_end` | `AgentEvent::Done` | `session_id`、`turn_id`、`status`、`output_tokens`、`duration_ms` | `idle` |
| `error` | `AgentEvent::Error` | `session_id`、`code`、`message` | `idle`（借 state-label 标错） |
| `session_end` | daemon 关闭 / TUI 退出 | `session_id`、`exit_code`、`reason` | `release-agent` |

投递约定：标量走环境变量（`VISP_HOOK_EVENT` / `VISP_HOOK_SEQ` / `VISP_SESSION_ID` / …），全量上下文走 stdin JSON；hook 子进程继承 visp 进程环境，故 `HERDR_*` 自动可见；`VISP_HOOK_SEQ` 由 visp 发放。

### 11.6 落地顺序（供届时决策）

- **若 hook 先行**（通用 hook 本身是产品卖点）：先冻结事件契约，实现顺序 = daemon emitter → 内置 herdr handler → `[hooks]` 执行器。
- **若 herdr 先行**（采用本设计 MVP）：按 hook 形态预留接缝（事件命名 + env/JSON 契约 + seq 发放），待**第二个真实消费者**（tmux/zellij、CI 通知、OTel 桥、外部编排）出现再抽 `[hooks]` 执行器。
- **YAGNI 原则**：不为「未来可能有 hook」先造通用子系统；契约先冻结，执行器待需求坐实再做。
