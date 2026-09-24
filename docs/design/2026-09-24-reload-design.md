# 设计文档：`/reload`、自动文件监听热重载与 TUI 自动重连

- 日期：2026-09-24
- 状态：v3.1（v3 经独立评审后修订：1 项 P0 + 3 项 P1 + 6 项 P2 全部处理；待用户最终确认）
- 涉及 crate：`visp-proto`、`visp-daemon`、`visp-config`、`visp-core`、`visp-command`、`visp-tui`

**v3.1 修订要点**（相对 v3）：

- **P0（idle 看门狗重写）**：删除「连续 2 次 idle 超时 → 强制重建」规则。该规则有两处致命缺陷：其一，TUI 与 daemon 同机、unary 与 Chat 流共享同一条 gRPC HTTP/2 连接，「探测成功但流半开」的状态实际不存在，宽限没有保护对象；其二，常态空闲连接会被每约 90s 无限重建一次，与验收标准直接矛盾，且长工具执行（帧空洞）会被误判重连——而误判后果远重于漏报（daemon 端 agent loop 不因 Chat 断开而取消，生成凭空消失、审批对话框丢失）。修正为**单一探测语义：idle 45s 超时 → HealthCheck 探测（5s 超时）→ 失败/超时即重连、成功即重置计时**（自然形成周期性探测）。同步修订 §5.9、决策 14、§6.4、验收 17，并在 §8 边界表补「长工具执行」「工具审批挂起」两行。
- **P1-1（agents 变化守卫）**：由「至少覆盖 name/description/mode/model/steps」改为 **`AgentDefinition` 全 9 字段等值**（name、description、mode、model、temperature、steps、permission、allowed_sub_agents、system_prompt），并要求为 `AgentDefinition` 补充 `PartialEq` 派生（其组成类型均已实现）。消除「改权限/系统提示词 → 守卫误判无变化 → 静默不生效」的安全漏报。
- **P1-2（监听计划降级链）**：项目 `.visp` 目录（#6）纳入缺目录降级（降级监听 project 根 + `.visp` 前缀过滤），祖先目录（#1）过滤规则明确为「AGENTS.md **或** `.visp` 子树前缀」，修复全新项目「首次创建的资产不被自动重载」的断链；验收 14 补 `.visp` 整体从零创建用例。
- **P1-3（reload 核心输入依赖）**：持有清单补齐三项——`builtin_overrides`（启动时构建一次的 clone）、`agent_dirs` 的**每次重载重算**（存在性检查不可缓存）、`global_tx` clone（对账注册新 agent 工具所需）。
- **P2**：全文交叉引用统一修正（决策在 §7、边界在 §8、模块改造点在 §5）；`agent_loader.rs` 容错行号更正为 95-100；明确 macOS **固定 kqueue 后端**（workspace 显式启用该特性）及「目录补挂竞态」的两点缓解（补挂后立即领域重扫、已监听路径集合去重）；rules 守卫论证补「`files[].path` 无运行时消费者」一环；§9 补最小回滚策略；决策 11 滞留帧措辞精确化。

**v3 修订要点**（相对 v2，保留备查）：

1. 新增自动文件监听热重载（agents、AGENTS.md/rules、skills 三类，保存即生效），复用 v2 定义的重载入口。
2. TUI 重连补充 idle 心跳看门狗（借鉴 OpenCode 2.0.9）。
3. 变化守卫：重载后内容无实际变化时如实跳过，不假装成功、不发通知。
4. 非目标更新：移除「不提供文件监听式自动重载」；保留「不支持自动重启 daemon」。

---

## 1. 背景与目标

visp daemon 启动时一次性装配四类「文件系统来源」的运行时资产，此后用户修改对应文件无法生效，必须重启 daemon（进而丢失 TUI 上下文、中断所有 session）：

| 资产 | 加载时机 | 消费方式 |
|---|---|---|
| AGENTS.md / rules | daemon 启动（`RuleEngine::new`） | agent loop **每轮迭代**动态读取 |
| skills 列表 | daemon 启动（`SkillTool::new`） | 烧入 `skill` 工具的 description；skill 正文执行时实时读盘 |
| agents 定义 | daemon 启动（`load_agents`） | **每次 UserInput / 每次 spawn 动态查表**；subagent 同时注册为工具 |
| system-prompt.md 模板 | **每次 create session** 时读盘并持久化到 session | agent loop 每轮从 session 取固化模板 |

本设计提供**两条互补的重载路径**：

- **显式路径**：`/reload` 命令，覆盖全部四类资产，逐项反馈——作为手动兜底，并覆盖自动监听不含的 system-prompt.md。
- **自动路径**：daemon 内文件监听，对 agents、AGENTS.md/rules、skills 三类「保存即生效」。

此外补齐 TUI 的**自动重连 + session 恢复**能力（含 idle 心跳看门狗），使 daemon 重启或卡死后 TUI 不退出、上下文不丢失。

### 1.1 生效语义（本设计的关键前提，v2 已按代码事实修正，v3/v3.1 不变）

「哪些 session 受影响」不是一个统一答案，而是**三层不同语义**：

| 层 | 内容 | 代码读取时机 | reload 后生效范围 |
|---|---|---|---|
| **L1** | system-prompt.md **base 模板** | session 创建时一次并持久化（`session.rs:264`） | **仅新 session**（唯一真正「冻结」的项） |
| **L2** | **agent 定义、subagent 列表** | 每次主 session UserInput 重新查表（`orchestrator.rs:344-353` → `:415`）；每次 spawn 重新查表（`orchestrator.rs:630`） | **旧 session 下一次输入 / 下一次 spawn 即生效** |
| **L3** | **rules（AGENTS.md）、工具定义与 tool guide** | 每轮迭代（`agent_loop.rs`） | **下一轮迭代，全部 session 即时生效** |

> **L2/L3 交叉提示（agent 工具）**：agent 工具的名称可见性走 **L3**（`render_tool_guide` 每轮迭代渲染）；Task Delegation 的 subagent 描述段走 **L2**（`build_subagent_prompt` append 进模板，且无去重）。因此删除某 agent 后，旧 session 内会出现短暂不一致：下一轮 tool guide 已无该工具，但模板里旧的 Task Delegation 段落仍列着它，直到该 session 下次输入才追加新段落。这是既有架构的自然结果，非正确性问题。

该分层对**自动与显式两条路径同等成立**——自动重载只是改变了「谁来触发」，不改变「何时生效」。

### 1.2 目标

1. `/reload` 一条命令重载四类文件系统资产，结果逐项反馈给用户（v2 目标，保留）。
2. **agents、AGENTS.md/rules、skills 三类资产在文件保存后自动热重载，保存即生效，无需手动命令**（v3 新增）。
3. rules 重载后对**所有 session 下一轮迭代**即时生效（L3）。
4. agent 定义 / subagent 列表重载后对**后续输入与后续 spawn** 生效（L2）。
5. system-prompt.md base 模板重载后对**新 session** 生效（L1，仅显式路径）。
6. **重载内容无实际变化时，如实反馈/静默跳过，不产生虚假成功反馈**（v3 新增）。
7. daemon 断线/重启/卡死后 TUI 自动重连并恢复当前 session，历史完整回放；**空闲连接的异常（卡死、半开）也能被检出并恢复，且不误伤正常空闲与长工具执行**（v3 新增、v3.1 修正判定语义）。

### 1.3 非目标（明确排除）

- **daemon.toml 不在热重载范围**（自动监听与显式 `/reload` 均不含）：providers、MCP、tool 参数在启动时构建，热替换代价高且收益低。daemon.toml 变更仍走「重启 daemon」，由 TUI 重连机制兜底体验。
- **system-prompt.md 不在自动监听范围**：模板变更低频、且只影响新 session（L1），保存即生效的收益低；仍走显式 `/reload`。
- **不追溯改写已存在 session 的持久化 base 模板**：子 agent spawn 会向 session 模板追加内容并持久化，覆盖式重写会与追加语义冲突。
- **不引入「旧 session 冻结 agent 定义」机制**：L2 的自然语义是「下次交互即用新定义」，与现有「每次输入动态查表」架构一致（见 §7 决策 7）。
- **TUI 本地配置（notification）不重读**：TUI 仅有的本地配置是通知引擎参数，构造时值语义烧入。通知配置变更需重启 TUI，在 `/help` 与 reload 结果文案中说明。
- **不恢复 subagent tab 的历史**：多 tab 共享单条 Chat 流，daemon 端 JoinSession 只回放单个 session；subagent tab 历史为纯内存态。重连后按 §5.9 语义处理。
- **不做逐文件增量更新**：自动重载对受影响领域执行「全量重扫 + build-then-swap + 变化守卫」，不做「某文件变了只更新该文件」的增量路径。资产规模小（数十文件）、重扫毫秒级，增量路径的复杂度不划算（见 §7 决策 10）。
- **不支持自动重启 daemon**；不提供「watcher 运行时动态增删监听路径」的配置面（监听计划随 daemon 启动一次性确定，见 §5.6）。

---

## 2. 架构概览

变更分四块：

1. **协议层**：新增一个 unary RPC `ReloadConfig`（显式路径）；**通知复用既有 `ServerMessage.StatusUpdate`**，oneof 不动。
2. **daemon 层**：把 v2 的「service 层 reload 处理器」上提为 daemon 内**共享 reload 核心**（单一互斥量 + 三类资产重载入口 + 逐项结果），显式与自动两条触发路径共同复用；新增**文件监听模块**（基于 `notify` crate，workspace 已有依赖）驱动自动路径。
3. **TUI 层**：`/reload` 作为「Daemon RPC 命令」接入既有命令管线；重连状态机接管现「断线即退出」行为，并新增 **idle 心跳看门狗**。
4. **通知**：自动重载完成后经既有 Chat 下行通道推送一条汇总 `StatusUpdate`（可观测性），无变化则静默。

```
┌──────────────────── TUI (visp-tui) ────────────────────┐
│ 输入框 → command::handle                                │
│   /reload → pending_reload 标志                         │
│ event loop select!:                                     │
│   recv()=None ──────┐                                   │
│   idle 45s 超时 →   │ 进入 Reconnecting（同一状态机）    │
│   health 探测失败 ──┘                                   │
│     → health_check → 新 Chat 流 → send_join 回放 →      │
│       清理 subagent tab → get_session 刷新模型          │
│                                                         │
│   recv()=StatusUpdate(session_id="") → 主 tab 状态行    │
└────────────────────────┬───────────────────────────────┘
                         │ unary RPC / Chat 流
                         ▼
┌────────────────── daemon (visp-daemon) ─────────────────┐
│ ReloadConfig handler（显式）      共享 reload 核心      │
│                                  ┌──────────────────┐  │
│ 文件监听模块（自动）─────────────►│ reload 互斥量    │  │
│   notify → 路径→领域过滤        │ 1. rules build→swap│ │
│   → debounce(200ms) 按领域聚合  │ 2. skills 重建+update│
│   → 逐领域: 重扫→build→swap    │ 3. agents swap+对账│  │
│      →变化守卫→通知            │ 4. 逐项结果        │  │
│                                  └──────────────────┘  │
│ system-prompt.md：不在监听范围（L1，仅显式确认回执）    │
└─────────────────────────────────────────────────────────┘
```

核心原则：**两条触发路径，一套重载核心**。自动路径不引入任何新的状态替换机制，只是「替用户按 /reload」并按领域过滤。

---

## 3. 协议变更

新增（归入 proto 文件的「Daemon 控制」段落，与 HealthCheck/Shutdown 同组）：

- `ReloadConfigRequest`：当前为空消息。
- `ReloadConfigResponse`：逐条目结果列表。接口规范如下（说明性）：
  - 每个条目包含：**类别**（`rules` / `skills` / `agents` / `system_prompt`）、**成功与否**、**消息**（成功时为统计摘要，如「3 个规则文件」；失败时为原因）、**变更数**（新增/修改/删除/跳过 的条目数）。
- 服务新增 RPC：`ReloadConfig(ReloadConfigRequest) returns (ReloadConfigResponse)`。

**通知复用既有协议（v3 新增，proto 零改动）**：

- 自动重载通知使用既有 `ServerMessage.StatusUpdate`（oneof 变体 4，字段含 `message`、`session_id`、`view_only`），不新增变体。
- daemon 端经既有 `orchestrator_grpc_tx` 下行通道发送（`main.rs:554` 创建的 `AgentEventFrame` 通道，Chat 流下行帧的唯一来源，`service.rs:1412` 将 `AgentEvent::StatusUpdate` 映射为 proto `StatusUpdate`）。
- **`session_id` 置空**：TUI 路由逻辑（`app.rs:1815`）将空 session_id 的帧路由到主 tab（index 0），渲染为对话区 Status 行（`app.rs:680-683` 的 `push_chat_line`），不改变输入框状态、不触碰 generating 状态——正是「不打断」的通知语义。**不得携带非空且未知的 session_id**，否则会落入子 agent 帧路由分支创建 hidden/view_only tab（`app.rs:1832-1853`）。

设计要点（v2 保留）：

- `system_prompt` 条目**恒为成功**（无操作，仅作为「L1 只对新 session 生效」的确认回执）。
- 不在 `ClientMessage`/`ServerMessage` 上加变体，对老客户端零影响。
- 失败不使用 gRPC status error，而是正常响应中带逐项失败信息——「部分成功」是常态（一个非法 agent 文件不应让 rules 重载作废），gRPC 错误语义表达不了部分成功。仅当 daemon 内部异常（互斥锁中毒等不可恢复情况）才返回 gRPC 错误。
- 空请求消息不做「未来选择性重载」的前瞻字段设计（YAGNI）。

---

## 4. 范围划分：显式路径 vs 自动路径

| 维度 | 显式 `/reload` | 自动文件监听 |
|---|---|---|
| 触发方 | 用户命令 | 文件事件（保存/创建/删除/重命名） |
| 覆盖资产 | rules + skills + agents + system_prompt（回执） | rules + skills + agents 三类 |
| 结果表达 | RPC 响应，TUI 逐项渲染 | 一条汇总 `StatusUpdate`（仅实际变更/失败时） |
| 无变化时 | 逐项报「无变更」统计（如实反馈） | 静默（不发通知，仅 debug 日志） |
| 失败粒度 | 逐项失败原因（用户可读） | 保留旧状态 + warn 日志 + 一条失败提示 |
| 并发控制 | 与自动路径**共用同一把 reload 互斥量** | 同左 |
| 生效语义 | L1/L2/L3 分层（§1.1） | 同左（不含 L1） |

---

## 5. 模块改造点（逐个）

### 5.1 `visp-config`：RuleEngine

现状：`rules: Arc<RwLock<RuleSet>>` 锁已存在但无写入入口；`new(project_path)` 扫描后未保存路径。

改造：

- 构造时**补存 `project_path`** 为私有字段。
- 新增 `reload` 能力：基于保存的 project_path 重新执行与构造时完全相同的扫描逻辑（AGENTS.md 向上发现 → 全局 AGENTS.md → 项目 rules → 全局 rules，`rules.rs:22-70`），在新变量中组装出新 `RuleSet`，**成功后才取写锁整体替换**。
- 失败语义：构建阶段任何 IO 错误（如 rules 目录中某文件暂时不可读）→ **放弃本次写回，保留旧 RuleSet**，将该失败上报。绝不让重载把「好状态」变成「空状态」。
- **变化守卫素材**：`RuleSet.content` 是全部文件内容的拼接串（`rules.rs:61-65`）——新 `RuleSet` 构建完成后与旧 `content` 做整体比对，相等即为「无变化」（见 §7 决策 13）。

并发影响（v2 已核）：`get_active_rules()` 是读锁 + clone String，写锁替换整体原子——读侧要么拿到完整旧内容、要么拿到完整新内容，不存在撕裂。`Orchestrator` 与 `CoderDaemonService` 持有同一 `Arc<RuleEngine>` 的 clone，reload 全局生效。

### 5.2 `visp-core`：ToolRegistry + SkillTool（skills 列表）

现状：`SkillTool` 的 description 字段在构造时烧入 skills 列表；`ToolRegistry` 已具备 `update`（同名替换）能力。

改造：**重建实例 + `ToolRegistry::update` 替换**，不改 SkillTool 内部结构。

- 重载时重新调用 skills 扫描（复用 daemon 启动时保存的 project root，即 `main.rs:322` 传入的 cwd，而非临时取 cwd）构造新 SkillTool 实例，经 ToolRegistry 同名替换。
- 不选「SkillTool 内部加 `RwLock<String>`」的原因：`Tool` trait 的 `description()` 返回 `&str`，内部锁会迫使重构 trait 签名，侵入所有工具实现。ToolRegistry 写锁替换提供完全相同的原子性。
- **变化守卫素材**：skills 列表即 `load_skills` 的返回字符串（`skills.rs:49`）——重扫得到的新 listing 与旧 listing 比对，相等即「无变化」。

并发影响（v2 已核）：`definitions()` 读锁与替换写锁互斥；运行中 loop 每轮迭代开头取快照，本轮用旧列表、下一轮拿新列表（L3）。替换瞬间正在执行的 skill 调用持有旧实例 Arc，不受影响。

### 5.3 `visp-agent` / `visp-daemon`：AgentRegistry

现状：`Arc<AgentRegistry>`（不可变），内部 HashMap 无锁；subagent 集合在启动时以 name+description 烧入注册为工具。

改造：**`ArcSwap<AgentRegistry>` 整体快照替换**（新增 `arc-swap` 依赖）。

- daemon 持有 `ArcSwap<AgentRegistry>`；Orchestrator 字段类型随之调整。
- 重载流程：重新执行 `load_agents`（内置 → daemon.toml 的 `[[agent.builtin]]` overrides → 全局目录 → 项目目录，顺序不变）构建全新 registry → 校验后 `store` 替换。
- 失败语义：单个 agent 文件解析失败沿用现有「warn + 跳过」容错（`agent_loader.rs:95-100` 已内建；注意「目录不存在则跳过」是另一处更早的宽容分支，位于 `agent_loader.rs:60-62`），计入条目「跳过数」；整体构建失败不可能发生（最坏退化为仅内置 agent）。
- **变化守卫素材**：对替换前后的 registry 做**定义集合等值比较——`AgentDefinition` 全部 9 个字段逐一等值**：name、description、mode、model、temperature、steps、permission、allowed_sub_agents、system_prompt。集合相等即「无变化」，工具对账自然为零操作。**不做「至少覆盖关键字段」的留口**：漏掉 permission 或 system_prompt 会导致权限规则/系统提示词的变更被守卫误判为无变化、静默不生效，属安全相关漏报（详见 §7 决策 13）。实现前提：`AgentDefinition` 目前仅 derive `Debug, Clone`（`agent_definition.rs:31`），需补充 `PartialEq` 派生——其组成类型 `AgentMode`、`PermissionRule`、`PermissionAction` 均已实现 `PartialEq`（`agent_definition.rs:2/20/14`），可直接组合，成本一次 derive。
- 备选方案（若不愿引入 arc-swap 依赖）：手写「`RwLock<Arc<AgentRegistry>>` + clone Arc」包装，语义等价。

**读侧消费点（v2 已核，如实评估）**：`orchestrator.rs` 有 6 处消费点（约 395/405/415/439/630/654）。改造方式是**单次流程入口处取一次 Arc 快照并复用**（arc-swap 用 `load_full()` 得到 `Arc<AgentRegistry>`；`load()` 返回的 Guard 非 Send，不可跨 await 持有）。跨 await 的消费仅限从快照 clone 出来的定义值（现有模式即如此），不持借用或 Guard 跨 await。

> **实施订正（2b 落地时核实）**：6 处消费点经 grep 核实确为 6 处，但本节原「`start_main_agent` 内部 395 与 415 必须共享同一次 load」的举例不准确——改造前 L395（vision 查表）与 L415（agent_name 查表）位于 `if has_images` 的**互斥分支**，同一流程内不会同时执行；真正会连续执行的是无图片路径下的 L415（agent_name）与 L439（`build_subagent_prompt` 的 subagent 列表）。实施改为「**每个流程入口**（`start_main_agent` / `spawn_sub_agent`）**各取一次 `load_full()` 快照并全程复用**」，覆盖全部 6 处的任意组合，语义比原举例更严。已实现。

**关键关联点——已注册 agent 工具的对账同步**：重载 registry 后必须同步 ToolRegistry 中的 agent 工具。对账算法（说明性，v2 保留）：

1. 取替换前、替换后的 subagent 快照（name → description）。
2. 仅存在于旧集合 → 从 ToolRegistry 移除。
3. 仅存在于新集合（含 mode 从 Primary 改为 Subagent/All）→ 注册新工具；与现存工具重名（含大小写不敏感撞名）时该单条失败记入结果，不阻断其余对账。
4. 两边都有但 description 变化 → 同名替换。
5. mode 从 Subagent/All 改为 Primary → 表现为「旧有新无」→ 移除。

**并发影响（v2 已核）**：`AgentTool::execute` 只发送 subagent 名字，不持有定义快照；查表发生在 Orchestrator 处理 SpawnRequest 时取当前快照。替换后后续 spawn 立即用新定义；删除的 agent 后续 spawn 立即返回「Unknown subagent type」——预期行为。

### 5.4 system-prompt.md 模板（L1）：无改造，仅确认语义

`SessionManager::create` 每次 create 实时读盘并持久化到 session（`session.rs:262-266`），「reload 后**新** session 用新模板」天然成立。**该资产不在自动监听范围**——自动路径的逐项结果中不含 system_prompt 条目；仅显式 `/reload` 含确认回执。

> **既有缺陷记录（本次不修，另行处理）**：`append_system_prompt_template`（`session.rs:417-426`）为无条件 `push_str`、无去重，同一 session 内每次 UserInput 都会重复追加 subagent 列表。本设计不顺带修复，但实施计划中应记录为已知问题。

### 5.5 daemon：共享 reload 核心（v3 重构，取代 v2 的「service 层处理器」；v3.1 补齐输入依赖清单）

v2 将 reload 处理器放在 service 层。v3 因自动路径需要从 watcher 任务调用同一套逻辑，将其上提为 **daemon 内独立模块（reload 核心）**：

- **职责**：对外提供两个入口：
  - **显式入口**：由 ReloadConfig gRPC handler 调用，重载全部四类（system_prompt 仅回执），返回逐项结果供 RPC 响应。
  - **自动入口**：由文件监听任务调用，入参为**待重载领域集合**（rules / skills / agents 的任意子集），只重载命中的领域，返回逐项结果。
- **持有与输入依赖清单（v3.1 补齐，缺一不可）**：
  1. 三个重载目标的引用：`Arc<RuleEngine>`、`Arc<ToolRegistry>`、`ArcSwap<AgentRegistry>`。
  2. **`builtin_overrides` 的 clone**：启动时构建一次（含 `[[agent.builtin]]` 配置，以及 `llm.image_generation_model`/`vision_model` 到 painter/vision 内置 agent 的 wire 产物，`main.rs:504-532`），是 `load_agents` 的必要参数（`agent_loader.rs:28`）。daemon.toml 不热重载，故其内容在 daemon 生命周期内静态，持有 clone 即可。
  3. **agent_dirs 每次重载时重算**：全局/项目 agents 目录的存在性检查（`main.rs:534-544`）**不可缓存启动时结果**——缓存会导致「运行中创建 agents 目录」的场景（§5.6 的补挂链所覆盖）不被拾取。每次重载重新收集目录列表。
  4. **`global_tx` 的 clone**：作为「多 agent 模式」的**门控**（`main.rs:552` 创建、`main.rs:559` 启动注册时传入）。**实施核实（3c）**：启动路径的 `register_agent_tools` 仅在 `global_tx.is_some()` 时**直接调用 `ToolRegistry::register`**，并不真正发送通道消息；对账因此沿用同一路径（`global_tx` 仅作门控、注册为直接调用，与启动逐字一致）。设计原文「经 `global_tx` 通道注册」措辞不准确，以此为准。
- **内部流程（两入口共享）**：依序执行 5.1 → 5.2 → 5.3 的重载入口，逐项收集结果；单项失败不阻断后续项；各步骤打 tracing 日志（含变更统计）。
- **变化守卫在核心内部执行**（见 §7 决策 13）：无变化的领域跳过替换（避免无谓的 ArcSwap store / 写锁获取），结果中标注「无变更」。
- 显式入口额外产出 system_prompt 回执；自动入口不触碰 L1。
- gRPC handler 退化为薄封装：调核心显式入口 → 组装 ReloadConfigResponse；同时真正使用 `CoderDaemonService.rule_engine` 字段并移除其 `#[allow(dead_code)]` 标记（v2 遗留事项）。
- **通知发送也在自动入口完成后执行**（见 §7 决策 11）：核心持有 `orchestrator_grpc_tx` 的 clone，按汇总结果决定是否推送 StatusUpdate。
- **并发控制**：核心持有**一把异步互斥量**（显式与自动共用，见 §7 决策 9）。

### 5.6 daemon：文件监听模块（v3 新增；v3.1 修正降级链与 kqueue 竞态）

**归属**：`visp-daemon` crate 内新模块，**不复用** `visp-codegraph` 的 Watcher 实现但借鉴其模式。理由：codegraph Watcher 与 `Indexer` 耦合、单 project 递归监听、按扩展名过滤，其形态与「多路径 → 多领域映射」不符；但其成熟骨架值得照搬——`notify::recommended_watcher` 同步回调 → unbounded mpsc → 后台 tokio 任务做**按路径合并的 debounce 批处理**（`watcher.rs:59-93`：首事件开窗，窗口内每来事件重置计时，HashMap 按路径保留最新事件）。

**依赖**：`notify` v7 已是 workspace 依赖（根 `Cargo.toml:47`，显式启用了 macOS kqueue 特性），`visp-daemon` 的 Cargo.toml 增加 workspace 引用即可，无新增第三方依赖。

**生命周期**：

- **创建**：daemon 启动序列中，在 service 组装完成后、gRPC server 启动前（main.rs 步骤 9 与 10 之间）。此时 reload 核心、共享引用均已就绪。构造入参：daemon cwd（project root）、reload 核心句柄、通知发送句柄。
- **停止**：daemon 的 Ctrl+C 关闭流程（`main.rs:615-624`）中，在 MCP shutdown 之前**显式停止**（abort 后台任务 + drop notify watcher），保证关闭路径上不再有 reload 触发。watcher 自身停止动作失败不阻断 shutdown（尽力而为，日志记录）。

**监听计划（启动时一次性构建，daemon 生命周期内不变）**：

路径全集与「路径 → 领域」映射如下（目录函数均来自 `visp_config::path`）：

| # | 监听对象 | 监听模式 | 路径过滤 | 命中领域 |
|---|---|---|---|---|
| 1 | project 向上每个祖先目录（`discover_agents_md` 的遍历范围，`rules.rs:79-97`） | 非递归 | 文件名 = AGENTS.md；**或 `.visp` 子树前缀**（仅 project 根这一层需要，服务 #6 的缺目录降级补挂链） | rules（`.visp` 前缀事件按 #6 分流） |
| 2 | 全局配置根（`global_config_dir()`，默认 `~/.config/visp`） | 非递归 | 顶层 AGENTS.md；以及 `rules`/`agents`/`skills` 三个子目录自身的创建/删除事件 | rules / agents / skills（按子目录名分流） |
| 3 | 全局 rules 目录（`rules_dir_global()`） | 递归 | `.md` 文件 | rules |
| 4 | 全局 agents 目录（`agents_dir_global()`） | 递归 | `.md` 文件 | agents |
| 5 | 全局 skills 目录（`skills_dir_global()`） | 递归 | 子目录下任意事件（目标形态 `skills/<name>/SKILL.md`） | skills |
| 6 | 项目 `.visp` 目录（`visp_dir(project)`） | 非递归；**不存在时（全新项目）降级监听 project 根（非递归）+ `.visp` 子树前缀过滤**，目录树创建后补挂 | `rules`/`agents`/`skills` 子目录的创建/删除事件 | 同 #2 分流 |
| 7 | 项目 rules 目录（`rules_dir_project()`） | 递归 | `.md` 文件 | rules |
| 8 | 项目 agents 目录（`agents_dir_project()`） | 递归 | `.md` 文件 | agents |
| 9 | 项目 skills 目录（`skills_dir_project()`） | 递归 | 子目录下任意事件 | skills |

关键设计规则：

- **缺目录降级（v3.1 修正覆盖范围）**：目标目录不存在时不监听它，改为监听**最近的已存在祖先目录**（非递归）并以路径前缀过滤事件，捕获该目录树被创建的时机；目录创建事件命中领域的同时**动态补挂**对该目录的递归监听。此策略覆盖 **#3、#4、#5、#6、#7、#8、#9 全部「启动时可能不存在」的目录**——包括全新项目尚无 `.visp` 的情形（#6 降级到 project 根，`.visp` 创建事件经 #1 的前缀过滤规则命中，再补挂 `.visp` 及其子目录）；#1/#2 的目录本身必然存在，无降级需求。此修正修复 v3 草案的断链：若 #6 不降级，全新项目首次创建的任何资产（rules/agents/skills）都不会被自动重载。
- **明确排除**：全局/项目的 system-prompt.md、daemon.toml、webfetch.toml、logs、codegraph.db 及其余 `.visp` 内容不在过滤集合内，其事件直接丢弃。此排除是**硬编码的领域映射**，不给用户提供通配配置（YAGNI）。
- **祖先 watch 失败降级**：祖先链直达文件系统根，个别层（如 `/`）watch 可能因平台权限失败——降级为跳过该层并 log warn，代价是该层 AGENTS.md 变化不触发自动重载，显式 `/reload` 兜底。非致命，不阻断启动。
- **监听计划固定性**：project root 即 daemon 启动 cwd，进程生命周期内不变；`VISP_CONFIG_DIR` 是进程级环境变量，同样启动时固定。故监听计划的**路径全集**无需运行时重算（动态补挂只是对既定全集的执行层落实）。

**事件处理管线**（单 watcher feed，借鉴 OpenCode 的一致性设计）：

1. **过滤**：回调线程内按监听计划做「路径 → 领域集合」匹配，未命中的事件丢弃（如 `.visp` 下无关文件）。
2. **debounce（200ms）**：命中事件进入后台任务，按**领域**聚合（同一领域窗口内的多个事件合并为一次待重载标记），窗口内新事件重置计时。窗口值依据见 §7 决策 9。
3. **串行化**：debounce 到期后，将脏领域集合交给 reload 核心的自动入口——互斥量保证与显式 `/reload` 串行。
4. **守卫与通知**：核心完成重载后返回逐项结果（含「无变化」标注），通知策略见 §7 决策 11。

**kqueue 后端与目录补挂竞态（v3.1 新增）**：

- **后端事实**：根 `Cargo.toml:47` 显式启用了 macOS kqueue 特性——notify 7 的 `RecommendedWatcher` 在该配置下**固定走 kqueue 后端**（默认的 FSEvents 后端被该特性排除；已核对 notify 7.0.0 源码的条件编译）。Linux 走 inotify。
- **竞态**：kqueue 的递归监听是**逐目录挂载的模拟实现**——「子目录创建事件到达 → 对新目录完成递归补挂」之间存在时间窗口，窗口内写入新目录的后续事件会丢失（如补挂完成前编辑器已写入 SKILL.md）。
- **缓解一（兜底）**：补挂完成后**立即执行一次该领域重扫**。全量重扫直接读盘，不依赖事件，天然拾取窗口内丢失的写入——这与 §7 决策 10 的「领域级重扫」同构，零额外机制。
- **缓解二（去重）**：维护**已监听路径集合**，补挂前先查重——防止 git checkout 等目录风暴（大量目录同帧创建/删除）下对同一路径重复挂载（句柄泄漏与事件重复放大）。

**CodeGraph watcher 的共存**：`visp-codegraph` 已有独立 watcher 监听同一 project（用于索引），与本模块互不感知、互不干扰（各自独立的 notify watcher 实例与事件通道）。两者监听范围有重叠（project 树），但事件处理完全隔离，无一致性要求。

### 5.7 `visp-command`：命令解析（v2 保留）

- `Command` enum 新增 `Reload` 变体（无参数）；`parse` 增加 `/reload` 精确匹配分支（遵循现有 `/init` 与 `/init-agent` 的边界写法）。
- `resolve` 返回既有「非 daemon 文本命令」语义（实际执行走 TUI 的 unary RPC）。
- daemon 侧 `service.rs` 的 UserInput 斜杠命令拦截无需改动。**防御性降级**：旧客户端把 `/reload` 原文发来，落入 `CommandAction::None` → 被当作普通 prompt 转发给 LLM（`service.rs:544-553`）。可接受，文档明示。

### 5.8 TUI：命令接入（v2 保留）

- `command.rs::handle` 识别 `/reload` → 置 `pending_reload` 标志 + 状态行「Reloading...」（与 `/new` 的 pending 模式一致）。
- `event.rs` 主循环新增 pending 处理块：调用 unary RPC → 逐项结果渲染进当前激活 tab（成功用 Status 样式、失败用 Error 样式）→ 清除标志。
- **同步三处硬编码命令清单**（已知技术债，一并补 `/reload`）：`event.rs` Tab 补全列表、`ui.rs` 输入提示列表、`ui.rs` 帮助弹窗（说明文案注明：「AGENTS.md/rules、skills、agents 另有保存自动生效；daemon.toml 变更需重启 daemon」）。

### 5.9 TUI：自动重连、session 恢复与 idle 看门狗（v2 §4.8 基础上 v3 新增看门狗、v3.1 重写判定语义）

现状：`chat_handle.recv()` 返回 `None` → `app.should_quit = true` 直接退出（`event.rs:161`）。

**多 tab 事实（v2 已核）**：TUI 存在多 tab（主 tab + subagent tab），但共享唯一一条 Chat 流；subagent 的 `ServerMessage` 按 session_id 路由。不存在「每 tab 一条流」。

改造：引入**连接状态机**（状态存于 AppState，逻辑集中在 event loop）：

```
stateDiagram-v2
    [*] --> Connected
    Connected --> Reconnecting: recv() 返回 None（流干净关闭）
    Connected --> Reconnecting: idle 超时 + health 探测失败/超时（v3 新增、v3.1 修正语义）
    Reconnecting --> Reconnecting: 重试失败（指数退避 1s→2s→…→30s 封顶）
    Reconnecting --> Connected: health_check 成功 → 重建 Chat 流 → send_join 回放主 session → 清理 subagent tab → get_session 刷新模型
    Connected --> [*]: Ctrl+D
    Reconnecting --> [*]: Ctrl+D（随时可退出）
```

**断线检测——两个入口汇聚同一状态机**：

1. **`recv()` 返回 `None`**（流干净关闭，覆盖 daemon 正常重启）：v2 语义保留，立即进入 `Reconnecting`。
2. **idle 心跳看门狗**（v3 新增；v3.1 修正为单一探测语义），覆盖 daemon 卡死、网络半开——这两种情况下 gRPC 流**不会**干净关闭，`recv()` 永远挂起，仅靠入口 1 完全失效：
   - **机制**：event loop 的 `select!` 中增加 idle 计时分支（与现有 `spinner_tick` interval 分支并列）。**收到任何 Chat 流帧即重置计时**。
   - **判定（v3.1 修正）**：空闲超过阈值（45s，借鉴 OpenCode）→ 通过独立的 unary `HealthCheck` 探测（不经 Chat 流，5s 超时），**恰好两个结局**：
     - **失败或超时** → 连接失效，立即进入 `Reconnecting`。
     - **成功** → 重置 idle 计时，继续等待（自然形成「每 45s 一次周期性探测」的稳态，直到流恢复出帧或 daemon 真死）。
   - **没有第三种「强制重建」路径**。v3 草案的「连续 2 次超时强制重建」已删除，完整论证见 §7 决策 14——要点：TUI 与 daemon 同机、unary 与 Chat 流共享同一条 gRPC HTTP/2 连接，**TCP 半开时 unary 探测必然同样失败**，「探测成功但流半开」需要被宽限的状态实际不存在；而误判重连的代价极重（见下）。
   - **帧空洞的正确处理**：agent loop 中存在三类无帧区间——工具执行中（`agent_loop.rs:1310-1313`，执行区间无任何帧）、工具执行后的 LLM 网络等待、以及**工具审批对话框挂起**（UserQuery 发出后等待用户操作，`agent_loop.rs:1186-1215`，最长约 120s 自动超时）。这些区间内 idle 必然超时 → 探测 → daemon 存活（工具执行与审批等待都不阻塞 gRPC server）→ **成功即重置，不误判**。「生成期间帧密集」仅对 LLM 流式阶段成立，设计不依赖该假设。为什么不按 generating 状态豁免看门狗：帧空洞本身就发生在 generating 期间，TUI 的 generating 标记恰好在空洞中无法区分「忙」与「卡死」，豁免会让 daemon 卡死检测出现盲区。
   - **误判重连的代价（为什么「失败才动」是硬约束）**：daemon 端 agent loop **仅在收到显式 Cancel 时取消，Chat 流断开本身不触发取消**。若在生成期间被误判重连：TUI 丢弃旧流，但 daemon 端 loop 继续执行并烧 token；其后续帧（工具结果、Done）发进已弃流而丢失；重连回放不含未完成的生成；session 停留 Running，用户后续输入被 SessionBusy 拒绝——用户看到「生成凭空消失」。审批挂起场景更糟：UserQuery 帧不可重放（daemon 无重放队列），审批对话框 UI 丢失，120s 后落入自动 deny。因此看门狗必须是「探测失败才动」，宁可多探测、绝不瞎猜。
   - **参数**：idle 45s、探测超时 5s，以常量集中定义（便于回滚调整，见 §9）。daemon 侧零改动（HealthCheck RPC 已存在，`service.rs:1040`）、proto 零改动。

**重试与退避**：指数退避 1s 起步、倍增、30s 封顶，无限重试（用户随时 Ctrl+D 退出）。两个检测入口共用同一退避循环；每次重试状态栏显示「Daemon disconnected — reconnecting (attempt N)…」。

**重建与恢复**（复用 `/sessions <id>` 既有路径，`event.rs:272-291` 同构，v2 保留）：

1. 重新 connect 构造新 client + 新 Chat 流（旧 Channel 不可复用）；
2. `health_check` 确认存活；
3. 向新流 `send_join` 回放当前主 session 历史；
4. `get_session` 顺带刷新 available_models / model_keys（仅创建/查询时下发，JoinSession 不携带）；
5. 清理本地 streaming 状态。

**多 tab 恢复语义（v2 保留）**：

- **仅回放主 session**；**subagent tab 直接丢弃、不入 `closed_tabs` 回收站**（历史纯内存态不可恢复），实现复刻关闭逻辑但跳过回收站写入。对话区提示「连接已恢复；子 agent 标签页已关闭（其历史不可恢复）」。
- **per-tab 状态必须遍历重置**（完整字段清单见 `app.rs:335-352`）：`frames`、`rendered_up_to`、`streaming_text`、`stream_started_at`、`stream_output_tokens`、`pending_usage`、`last_stream_tps`、`last_stream_elapsed`、`generating`。`frames`/`rendered_up_to` 是未渲染队列与游标不变量（`render_pending` 依赖），只清 messages/generating 会残留错乱。

**重连期间的用户交互（v2 保留）**：输入框保持可编辑，Enter 提交在 Reconnecting 状态下提示「未连接到 daemon，稍后自动重连」，不发送不清空；断线时正在 generating → 显示「连接中断，生成已终止」（daemon 重启场景下该 loop 确实已不存在——daemon 启动时的孤儿 Running 重置逻辑保证 session 回 Idle）；重连 select! 分支保留 exit 通道监听。

**与 `/reload` 及热重载通知的交互**：

- `/reload` 在断线期间调用 → unary 失败 → 正常报错，不重试（用户显式动作，重试语义由用户决定）。
- **热重载通知帧（session_id 为空的 StatusUpdate）在重连后不会自动补发**——它是瞬时通知，daemon 端无重放队列。断线期间发生的热重载在重连后已生效于 daemon 内存态（L2/L3 按各自时机消费），无需补发。

实现层面注意（v2 保留）：现 `event::run` 以 `&mut client` 借用持有 client（`event.rs:85`），重连需整体替换——event loop 改为持有 client 所有权（或封装为连接管理职责），机械改动留给实现计划。idle 看门狗的计时与判定逻辑抽为纯函数/可注入时钟以便单测（见 §10.4）。

---

## 6. 数据流与交互时序

### 6.1 `/reload` 正常时序（v2 保留）

```
用户            TUI                    daemon
 │ 输入 /reload  │                       │
 │──────────────►│                       │
 │               │ command::handle:      │
 │               │   置 pending_reload   │
 │               │   状态行 "Reloading…" │
 │               │──ReloadConfig(RPC)───►│
 │               │                       │ reload 互斥量（与自动路径共用）
 │               │                       │ 1. rules: 重扫→守卫→build→swap
 │               │                       │ 2. skills: 重扫→守卫→重建→update
 │               │                       │ 3. agents: 重扫→守卫→swap→工具对账
 │               │                       │ 4. system_prompt: 确认回执
 │               │◄─ReloadConfigResponse─│
 │               │ 当前 tab 渲染逐项结果 │
```

### 6.2 自动热重载时序（v3 新增）

```
编辑器            notify(feeds)        监听任务                reload 核心
 │ 保存 AGENTS.md │                     │                        │
 │──write/rename──►│ 路径过滤: 命中 rules│                        │
 │                │────────────────────►│ debounce 200ms 开窗    │
 │（编辑器原子写： │                     │ 合并 tmp+rename 双事件 │
 │  tmp 写+rename │                     │                        │
 │  = 多个事件）  │                     │ 窗口到期                │
 │                │                     │──reload([rules])──────►│ 取互斥量
 │                │                     │                        │ 重扫 → 守卫: content 变了
 │                │                     │                        │ build→swap（写锁替换）
 │                │                     │                        │◄─结果: rules=有变更
 │                │                     │                        │ 推 StatusUpdate("已热重载…")
 │                │                     │◄─完成──────────────────│ 释放互斥量
 │                │                     │                        │
 │ 保存未改动的文件│                     │                        │
 │──modify────────►│ 命中               │ debounce 到期          │
 │                │                     │──reload([rules])──────►│ 重扫 → 守卫: 无变化
 │                │                     │                        │ 跳过替换、不通知
 │                │                     │◄─结果: 无变更──────────│
```

用户侧体感：保存后约 200ms（debounce）+ 毫秒级重扫 ≈ **亚秒内生效**（L3 下一轮迭代 / L2 下次输入或 spawn）。

### 6.3 reload 与运行中 agent loop 的并发（v2 保留，两路径通用）

- **L3（rules / 工具定义）**：loop 第 N 轮迭代开头取快照 → 第 N 轮全程旧快照 → 第 N+1 轮新快照。
- **L2（agent 定义）**：查表发生在「每次 UserInput 进入 start_main_agent」与「每次 spawn」，替换后下一次该动作即用新值。
- **L1（base 模板）**：只在新 session 创建时读取，运行中 loop 不受影响（自动路径不触碰 L1）。
- 结果：**正在进行的生成不受影响、不中断**；自动重载无需等待、无需取消、无需特殊协调——「本轮旧快照、下一轮生效」由既有的快照读取模式天然保证。

### 6.4 daemon 重启恢复时序（v2 保留；末段 v3.1 修正）

```
daemon 重启             TUI
 │ 流断开                │ recv()=None → 进入 Reconnecting
 │                        │ 退避重试 health_check…
 │ 启动完成              │ health_check 成功
 │                        │ 新建 Chat 流 → send_join（主 session）
 │◄─JoinSession──────────│
 │──主 session 历史回放──►│
 │                        │ 关闭/清空 subagent tab + 重置 per-tab 状态
 │                        │ get_session → 刷新模型列表
 │                        │ 状态行 "Reconnected, session restored"
```

idle 看门狗触发的重连走同一时序，唯一差异是入口：`recv()` 不返回 None，而是 idle 超时 + health 探测失败/超时后主动丢弃旧流再进入 Reconnecting。

---

## 7. 关键设计决策与权衡

### 决策 1：显式触发通道——独立 unary RPC（v2 保留）

| 维度 | unary RPC `ReloadConfig` | Chat 流新增 ClientMessage 变体 |
|---|---|---|
| 语义归属 | reload 是 **daemon 全局状态操作**，与 session 无关，unary 语义正确 | 绑定在「某个连接的 session 流」上，语义错位 |
| 结果表达 | 结构化逐项成败，天然表达「部分成功」 | 只能挤进 StatusUpdate 单条文本 |
| 通道依赖 | **不依赖 chat 连接** | 必须有活跃 chat 流 |
| proto 变更 | +2 message +1 rpc，oneof 不动 | +1 变体 +1 message，oneof 动 |

决定性理由：语义归属、部分成功的结构化表达、不依赖 chat 连接生命周期的鲁棒性。

### 决策 2：Running session 期间允许 reload（v2 保留）

三类状态均满足「原子替换 + 运行中按各自时机读取」模型。拒绝 Running 时的 reload 会强迫用户先 Cancel，体验差且无安全必要。文档化语义：按 L1/L2/L3 分层生效。

### 决策 3：AgentRegistry 并发原语——ArcSwap（v2 保留）

读侧在流程入口 `load_full()` 一次拿快照；备选手写 `RwLock<Arc<…>>` 已记录，两者成本差异小。

### 决策 4：SkillTool 刷新方式——重建实例 + Registry 同名替换（v2 保留）

不改 trait 签名、零新增锁，复用 ToolRegistry 既有能力。

### 决策 5：重连无限重试 + 不自动重发输入（v2 保留）

daemon 重启耗时不可预期，封顶退出会背离「TUI 不退出」目标；断线时正在生成的请求**不自动重发**（重发语义不成立，且可能造成意外的重复执行）——明确告知用户并要求重新输入。

### 决策 6：TUI 本地通知配置不热重载、重连后刷新模型列表（v2 保留）

代价收益判断；重连流程一次 get_session 顺带保证 `/model` 选择器不显示过期列表。

### 决策 7：接受 L2 的「下次交互生效」语义，不强制冻结旧 session 的 agent 定义（v2 保留）

代码现状是「每次输入/每次 spawn 动态查表」。强制冻结需按 session 快照 agent 定义，与架构相悖、收益不明。**对用户原始诉求「agents 只对新 session 生效」的语义更正**，用户已确认接受。

### 决策 8（v3）：watcher 落在 visp-daemon 新模块，参考 codegraph 模式而不复用其类型

- **为什么不复用 `visp-codegraph::Watcher`**：它面向「单 project 递归 + 扩展名过滤 + Indexer 消费」，与「多路径 → 多领域映射 + reload 核心消费」形态不同；强行参数化会让两个场景共享一套不匹配的过滤配置。**借鉴的是模式**（recommended_watcher + unbounded mpsc + 窗口重置式 debounce 批处理，`watcher.rs:59-93` 已验证可靠），照抄骨架、替换过滤与消费端。
- **为什么放 visp-daemon 而非独立 crate**：消费端（reload 核心）在 daemon 内，监听计划依赖 daemon 的 cwd 与配置；独立 crate 无第二个消费者（YAGNI）。
- **依赖与后端（v3.1 精确化）**：`notify` v7 已在 workspace（`Cargo.toml:47`，显式启用 macOS kqueue 特性），无新增第三方依赖。**macOS 上固定走 kqueue 后端**（该特性排除了默认的 FSEvents 后端，见 §5.6 的竞态分析），Linux 走 inotify。不做 OpenCode 式的双实现（`@parcel/watcher` + `node:fs.watch` 兜底）——`notify::RecommendedWatcher` 本身已封装平台差异。

### 决策 9（v3）：debounce 200ms、按领域聚合、与显式路径共用一把互斥量

- **debounce 值**：OpenCode 用 100ms、visp-codegraph 用 500ms。取 **200ms**：足以合并编辑器原子写的多事件（tmp 创建 + rename 落盘）与「Save All」的多文件风暴（通常 100ms 内完成），同时仍低于人类「保存→生效」的感知阈值（约 250ms），比 codegraph 的 500ms 更跟手。单值硬编码，不做配置（无场景需要调）。
- **按领域而非按路径聚合**：重载的最小单位是「领域」（重扫全目录），同领域的多个文件事件合并为一次重扫。跨领域事件（如同时改了 rules 和 agents）合并为一次 reload 调用、领域子集执行。
- **共用同一把互斥量**：自动与显式 `/reload` 竞争同一批可变状态（RuleEngine 写锁、ToolRegistry 写锁、ArcSwap），若用两把锁则需在它们之间维护顺序约定，复杂且无收益。一把异步互斥量串行化所有 reload，最坏等待 = 一次毫秒级重扫。**debounce 不在互斥量内进行**（开窗聚合不碰共享状态），互斥量只覆盖真正的重载执行。
- **进行中的 reload 期间又来事件**：debounce 窗口到期恰逢互斥量被占 → 排队等待后执行；其重扫结果已是最新盘上状态，天然覆盖之前未处理的事件，无需事件回放。

### 决策 10（v3）：自动重载是「领域级重扫 + 守卫」，不做逐文件增量

资产规模小（rules 数个 .md、agents/skills 各数十目录）、全量重扫毫秒级；逐文件增量需要维护「路径 → 文件内状态」映射与增删改分支，复杂度显著高于重扫而收益仅是节省毫秒。**全量重扫 + build-then-swap + 变化守卫**与显式路径完全同构，复用度最高、推理负担最低。OpenCode 的领域 State（「可重放 transform、读时懒重建、reload 只标脏」）在本场景的等价简化即为此形态——visp 的读取端已是动态查表（L2/L3），无需引入懒重建层。该决策同时是 §5.6 kqueue 补挂竞态的兜底机制（重扫读盘、不依赖事件）。

### 决策 11（v3）：自动重载通知——推送但仅在实际变更/失败时，单条汇总，best-effort

- **推送 vs 静默**：选推送。「保存即生效」若完全静默，用户无法区分「生效了」与「watcher 没工作」，可观测性损失大于刷屏风险；但**只在有实际变更或失败时推送**（变化守卫保证无变化不产生通知），正常编辑节奏下通知频率 ≈ 实际修改频率，无刷屏。
- **形式**：一次 reload 执行产生**至多一条**汇总 StatusUpdate（列出有变更/有失败的领域及统计，如「已热重载：agents（2 变更）」；无变更领域不提及）。debounce 已把文件风暴合并为单次 reload，git checkout 等批量触碰场景也只产生一条。
- **通道**：经 `orchestrator_grpc_tx` → proto `StatusUpdate{session_id: "", view_only: false}` → TUI 主 tab Status 行（§3 已述路由依据）。`view_only:false` 在此无副作用——StatusUpdate 的处理路径不触碰输入框/generating 状态（`app.rs:680-683`、`event.rs:1029-1039` 已核）。
- **best-effort 语义（v3.1 措辞精确化）**：无活跃 Chat 连接时存在两种情形——(a) Chat 流已建立又断开：下行接收端已随流关闭销毁，发送即失败，静默丢弃；(b) daemon 启动后从未建立 Chat 连接：帧**滞留在下行通道的缓冲区内**，由**首个建立的 Chat 连接**读出并渲染（出现在该连接的回放内容之后）。两种情形均帧极小、语义无害，接受。通知失败不重试、不影响 reload 结果本身。
- **失败提示**：自动重载失败（领域整体失败）同样用该通道推送一条错误性质的 Status 消息，让用户知道「这次保存没有生效，需要 /reload 或修正文件」。

### 决策 12（v3）：开关放 daemon.toml 的 daemon 段，默认开启，重启生效

- 配置项：`DaemonSection`（`config.rs:149`，现仅 listen_addr、log_level）新增一个布尔字段，默认 `true`。关闭后 daemon 不创建 watcher，自动热重载整体失效，`/reload` 仍是手动兜底。
- **重启生效的必然性**：daemon.toml 本身不在热重载范围（非目标 §1.3），该开关自然遵循同一约束——修改后需重启 daemon。文档与 `/help` 文案明示。
- **不做运行时开关**：watcher 的启停涉及监听计划重建与后台任务生命周期管理，而「临时关掉自动重载」的真实场景（如批量脚本操作期间）已被 debounce + 守卫覆盖（批量操作只产生一次有效重载），运行时开关无真实需求。

### 决策 13（v3；v3.1 修正 agents 字段清单并补 rules 论证）：变化守卫——按领域各自等值比较，无变化则跳过替换且不通知

| 领域 | 比较对象 | 无变化判定 |
|---|---|---|
| rules | 新旧 `RuleSet.content`（全部文件内容按序拼接的整体串） | 字符串相等 |
| skills | 新旧 skills listing 字符串（`load_skills` 返回值，含内置 + 全局 + 项目） | 字符串相等 |
| agents | 新旧 registry 定义集合（name → 完整 `AgentDefinition`，**全部 9 个字段逐一等值**：name、description、mode、model、temperature、steps、permission、allowed_sub_agents、system_prompt；需为 `AgentDefinition` 补充 `PartialEq` 派生，其组成类型均已实现） | 集合相等 |

- **为什么 rules/skills 用拼接串而非文件清单**：拼接串是消费端实际使用的完整形态（L3 注入 prompt 的就是它），串相等 ⇒ 消费端行为不变，判定充分且实现最简。rules 文件增加/删除必然改变拼接内容，无需单独比对清单。
- **rules 守卫的论证补一环（v3.1）**：`RuleSet.files[].path` 当前**无任何运行时消费者**（读取接口只返回拼接 content，`rules.rs:72-74`），因此「文件被重命名但内容不变 → 判定无变化」是行为等价的，不构成漏报。同时注意拼接串的构成差异：rules 目录文件不带路径 header，仅 AGENTS.md 条目带 `Instructions from: {path}` header（`rules.rs:28/41`）——header 含路径且参与拼接，故 AGENTS.md 的移动/重命名会正确反映为「有变化」（prompt 中的来源标注确实变了），而 rules 目录文件的重命名则静默判等。两者均与消费端实际行为严格对应。
- **为什么 agents 必须全 9 字段等值（v3.1 修正）**：L2 动态消费完整定义，任一字段变化都改变后续 spawn 行为。v3 草案的「至少覆盖 name/description/mode/model/steps」是危险的留口：漏掉 `permission` 会导致权限规则变更被守卫误判为无变化、**静默不生效**（权限是安全边界）；漏掉 `system_prompt` 会导致子 agent 系统提示词变更同样静默失效。守卫的价值在于「如实」，留口比不守卫更糟（用户以为改了、实际没生效、且无任何反馈）。因此直接全字段等值，实现成本仅为一次 `PartialEq` derive（组成类型 `AgentMode`/`PermissionRule`/`PermissionAction` 均已实现，`agent_definition.rs:2/14-28`）。
- **守卫位置**：在 reload 核心内部、替换动作之前。无变化 ⇒ 不获取写锁、不 store ArcSwap、不做工具对账、不计入通知。
- **守卫的边界**：守卫判定的是「结果等价」，不是「没有文件事件」——文件被 touch 但内容最终未变 ⇒ 重扫结果相同 ⇒ 守卫拦下。这正是对「无关文件事件触发无效重建」的回答。
- 显式 `/reload` 的「如实反馈」：响应条目对无变化领域标注「无变更」（v2 的统计语义自然涵盖），用户得到真实状态而非笼统的「成功」。

### 决策 14（v3；v3.1 重写）：idle 看门狗——45s idle + health 探测，失败即重连、成功即重置，与 recv=None 共用状态机

- **为什么必须有看门狗**：TCP 半开/daemon 进程 hang 时，gRPC 流不产生任何事件，`recv()` 永远挂起——v2 的唯一检测点在这两类故障下完全失效，TUI 表现为「永远转圈的假死」。OpenCode 的 45s idle + 周期健康检查是经过验证的成熟方案。
- **判定语义（v3.1 修正，采纳评审结论）**：**单一探测规则，无宽限、无累计**——idle 45s 超时 → HealthCheck 探测（5s 超时）→ **失败/超时即重连；成功即重置 idle 计时**。成功后自然进入下一轮 45s 等待，形成周期性探测稳态，直至流恢复出帧或 daemon 真死。
- **为什么「连续 N 次超时强制重建」是错的（v3 草案缺陷，记录以防回潮）**：
  1. **宽限没有保护对象**：TUI 与 daemon 同机部署， unary RPC 与 Chat 流共享同一条 gRPC HTTP/2 连接——TCP 半开时 unary 探测**必然同样失败**（请求发出后无任何响应，5s 超时），「探测成功但流已半开」的状态在此部署形态下不存在。SIGSTOP/进程 hang 时 unary 被一同冻结 → 5s 超时 → 正确检出。即：探测结果是可信的连通性判据，无需对「探测成功」再设怀疑机制。
  2. **常态空闲被无限重建**：空闲是 TUI 的常态（无帧是正常状态）。在「成功即重置 + 连续 2 次强制重建」规则下，空闲连接每约 90s 必被重建一次并无限循环，与「正常空闲不产生错误重连」的验收目标直接矛盾。
  3. **长工具执行被误判，后果极重**：agent loop 的工具执行区间无帧（`agent_loop.rs:1310-1313`），一个 3 分钟的 bash 构建会在 t≈45s、90s 连续触发两次超时 → 被强制重建。而 daemon 端 agent loop **仅在收到显式 Cancel 时取消，Chat 断开不取消**——重建后旧 loop 继续烧 token，其后续帧发进已弃流丢失，session 停留 Running、输入被 SessionBusy 拒绝，用户看到「生成凭空消失」；工具审批挂起（`agent_loop.rs:1186-1215`）同理，且 UserQuery 帧不可重放、审批 UI 丢失、120s 后自动 deny。「消除打断」的收益远小于「打断生成」的伤害。
- **用 HealthCheck 而非应用层心跳帧**：daemon 端 HealthCheck unary RPC 已存在（`service.rs:1040`），语义就是「进程存活探测」；新增应用层 ping 需要动 proto 与 daemon，收益仅是省一次 unary 连接复用（同一条 gRPC channel 本就复用）。**daemon 零改动、proto 零改动**。
- **不按 generating 状态豁免**：帧空洞（工具执行、LLM 网络等待、审批等待）本身就是 generating 的一部分，TUI 的 generating 标记在空洞中无法区分「忙」与「卡死」；豁免会让看门狗在「daemon 卡死 + 正在生成」的最需要它的场景下失效。统一「失败即重连、成功即重置」天然覆盖所有状态。
- **不优化探测间隔（YAGNI）**：长工具执行期间每 45s 一次探测成功即重置，探测轻量、对 UI 与生成零影响；「generating 期间缩短下次探测间隔」的优化无实际收益，v1 不做。
- **与退避的联动**：看门狗触发的重连进入与 recv=None 相同的退避循环；重连成功后 idle 计时重置。
- **参数集中**：idle 45s（借鉴 OpenCode，远大于正常 RPC 往返）、探测超时 5s，以常量集中定义，便于紧急调整（§9 回滚策略）。

---

## 8. 边界情况与错误处理

| 场景 | 行为 |
|---|---|
| reload（任一路径）时有 session 正在生成 | 允许执行；按 L1/L2/L3 分层生效（§6.3），无锁冲突 |
| 自动与显式 reload 同时触发 | 共用互斥量串行执行，各自产出结果；后执行者大概率被变化守卫拦为「无变更」 |
| debounce 窗口内快速连续保存 | 合并为窗口到期后的一次重扫，只见最终状态 |
| **编辑器原子写（临时文件 + rename）** | 产生 Create/Modify/Rename 多个事件 → debounce 合并 + 全量重扫幂等 + 变化守卫兜底 ⇒ 单次有效重载。rename 后旧路径的 Remove 事件同样映射到领域重扫，无需区分 |
| 监听目录启动时不存在 | 监听最近存在祖先 + 前缀过滤；目录树创建事件触发补挂递归监听 + 领域重载（§5.6，含 #6 全新项目无 `.visp` 的降级链） |
| kqueue 补挂竞态窗口内的写入 | 补挂完成后立即领域重扫兜底（§5.6 缓解一），丢失事件由重扫读盘拾取 |
| 监听目录运行中被整体删除（如 rm -rf .visp/rules） | 父目录监听捕获 Remove → 领域重载 → 重扫结果为空/缺省（扫描逻辑对缺失目录宽容）→ 守卫判定内容变化则生效；后续重建目录同理自动恢复监听 |
| 保存到一半的文件（写入超过 debounce 窗口） | reload 可能读到截断内容：rules 触发 IO/解析错误 → 放弃写回、保留旧状态、warn 日志；agents 单文件解析失败 → 跳过该文件。**不主动重试**——文件写完的后续事件会再次触发重载，最终收敛（最后一次事件后必有一次成功重载）；用户也可显式 `/reload` |
| 单个 rules 文件读取失败 | 本次 rules 重载整体放弃、保留旧内容（宁可旧不可空）；显式路径报失败原因，自动路径 warn 日志 + 一条失败提示 |
| 单个 agent 文件非法 | 沿用「跳过 + 警告」（`agent_loader.rs:95-100`），条目报「成功，跳过 N 个非法文件」；自动路径汇总提示 |
| agent 名与现有核心/MCP 工具重名（含大小写不敏感） | 仅该 agent 的工具注册失败并计入结果，registry 本身已生效；其余对账继续 |
| system-prompt.md 非法/缺失 | 不在自动监听范围；显式 `/reload` 沿用现有加载行为（缺省模板），条目照常回执 |
| 文件事件命中但内容无实际变化（touch、无改动保存） | 变化守卫拦下：不替换状态、不通知，仅 debug 日志 |
| 无活跃 Chat 连接时发生自动重载 | 重载照常执行（互斥量与通知通道解耦）；通知发送失败/滞留（§7 决策 11），不影响重载结果 |
| daemon 关闭时 watcher 清理 | Ctrl+C 流程中先停 watcher（abort 任务 + drop notify watcher），再执行 MCP shutdown 与 server abort；清理失败仅记日志不阻断 |
| 自动重载与 `/new`、session 删除并发 | 无交集：重载核心不触碰任何 session 状态（仅 RuleEngine/ToolRegistry/ArcSwap 三个全局资产）；通知帧 session_id 为空、路由主 tab，与 session 生命周期无关 |
| **单次工具执行超过 idle 阈值（帧空洞，如长 bash 构建；`agent_loop.rs:1310-1313` 执行区间无帧）** | idle 超时 → HealthCheck 成功（工具执行不阻塞 gRPC server）→ 重置计时，周期性探测直至工具结果帧恢复到达；**不重连、生成不受影响**（v3.1 修正后行为；v3 草案此处会误判重连） |
| **工具审批对话框挂起（UserQuery 发出后等待用户，无帧；`agent_loop.rs:1186-1215`，最长约 120s 自动超时）** | 同上：探测成功即重置，不误判。此处「不误判」是硬约束——UserQuery 帧不可重放，误判会导致审批 UI 丢失并落入 120s 自动 deny |
| 重连时原 session 已被删除 | 提示「session 不存在」，引导 `/list` 选择，不自动新建 |
| `/new` 后未发消息即 daemon 重启 | 空 session 被启动清理删除（`main.rs:420-435`），重连落入「session 不存在」分支——常规场景，文案说明「空会话在 daemon 重启时会被自动清理」 |
| 重连后主 session 仍为 Running（网络闪断、daemon 未重启） | JoinSession 回放照常；后续输入被 SessionBusy 拒绝，原样展示——与现状一致。注意此场景仅在探测失败（连接真断）后发生，看门狗不制造该场景 |
| 重连期间 Ctrl+D | 立即退出（重连循环保留 exit 通道） |
| 祖先目录 watch 失败（平台权限） | 跳过该层 + warn 日志；该层 AGENTS.md 依赖显式 `/reload`，不阻断启动 |
| 旧客户端把 `/reload` 原文当 UserInput 发送 | 落入 `CommandAction::None` → 被当作普通 prompt 转发给 LLM（防御性降级，可接受） |
| 用户误以为 /reload 或自动热重载能改 providers | 响应与 /help 文案明确：「daemon.toml（providers/MCP/工具参数）不在热重载范围，需重启 daemon」 |

---

## 9. 影响范围与风险

| 模块 | 变更 | 风险 |
|---|---|---|
| visp-proto | +2 message、+1 rpc（ReloadConfig）；通知复用 StatusUpdate，oneof 不动 | 低 |
| visp-daemon / service | ReloadConfig handler 退化为薄封装；启用 `rule_engine` 字段并移除 dead_code | 低 |
| visp-daemon / **reload 核心模块（新）** | 互斥量 + 三类资产重载 + 守卫 + 逐项结果；显式/自动两入口；持有 overrides/agent_dirs/global_tx（§5.5） | **中**：v2 §4.5 的逻辑整体迁入，需保证与三个资产模块的引用关系正确；agent_dirs 重算与 overrides 静态的区分要在实现时守住 |
| visp-daemon / **文件监听模块（新）** | notify 多路径监听、领域映射、debounce、缺目录补挂 + 补挂后重扫 + 去重集合、生命周期管理 | **中**：平台行为差异（事件序列）、路径过滤遗漏会导致「不生效」或「误触发」；有守卫与重扫兜底，最坏是多/少一次重载，无正确性风险 |
| visp-config / RuleEngine | 补存 project_path、新增重载写回 | 低；锁已存在 |
| visp-config / DaemonSection | +1 布尔配置字段（默认开启） | 低 |
| visp-core / ToolRegistry | 无改动（复用 update） | 无 |
| visp-core / AgentRegistry | 无内部改动（整体替换）；`AgentDefinition` 补 `PartialEq` 派生 | 低 |
| visp-daemon / Orchestrator | Arc→ArcSwap；6 个消费点改为入口 load 一次并复用 | **中**：需逐点核对，避免同一流程内多次 load 造成快照撕裂 |
| visp-daemon / main.rs | registry 包装 ArcSwap；启动序列挂载 watcher；关闭序列停 watcher | 低-中：插入点明确（步骤 9/10 之间、Ctrl+C 分支） |
| visp-command | +1 变体 + parse 分支 + 测试 | 低 |
| visp-tui / command+event+ui | `/reload` 接入；三处命令清单 | 低 |
| visp-tui / **event.rs 重连状态机 + idle 看门狗** | recv=None 分支改造、select! 新增 idle 分支、client 所有权调整、多 tab 清理 | **高**：`event.rs`（约 1200 行）主 select! 循环重构仍是最大改动面；看门狗与 spinner/exit 分支并存需仔细验证 |
| 新依赖 | arc-swap（成熟、轻量）；notify 已在 workspace | 低 |

整体风险排序：TUI 重连状态机 + 看门狗 > agent 工具对账 ≈ ArcSwap 消费点改造 > 文件监听模块 > reload 核心 > 其余低风险增量。

**回滚策略（最小方案，v3.1 新增）**：

- **自动热重载整体回退**：使用决策 12 的开关（daemon.toml 布尔字段，默认开启）关闭自动路径并重启 daemon——watcher 不创建，系统退回 v2 的纯显式 `/reload` 形态，显式路径不受任何影响。这是自动路径的唯一回滚开关，改动面为零。
- **TUI 看门狗参数回退**：idle 阈值（45s）与探测超时（5s）以常量集中定义（§7 决策 14），紧急情况下可在一处调整（如临时调大阈值规避特定环境的探测抖动），无需触碰状态机逻辑。
- 看门狗机制本身不设开关：其判定为「探测失败才动」，误触发概率与误伤面已被设计消除；若极端环境确有问题，调大 idle 阈值即等效于近似关闭。

---

## 10. 验收标准

### 10.1 显式 `/reload`（v2 保留）

1. **rules 即时生效（L3）**：修改项目 AGENTS.md 后执行 `/reload`，正在进行的旧 session 的**下一轮**迭代 prompt 中包含新内容；生成中的本轮不受影响。
2. **skills 列表生效（L3）**：新增 skill 后 `/reload`，LLM 可见工具列表中 `skill` 的 description 包含新 skill；旧 skill 移除后同步消失。
3. **agents 生效与对账一致（L2）**：新增/删除/修改 subagent 定义后 `/reload`，ToolRegistry 中的 agent 工具集合与 registry 的 subagent 集合严格一致（含 description 同步）；**旧 session 下一次输入/下一次 spawn 即使用新定义**；被删除 agent 的后续 spawn 返回「Unknown subagent type」。
4. **base 模板只对新 session 生效（L1）**：修改 system-prompt.md 后 `/reload`，新创建 session 使用新模板；既有 session 的持久化 base 模板不受影响。验证方法：reload 流程无任何 session 写路径，单测层构造 session → 调 reload 核心显式入口 → 断言模板逐字节不变。
5. **部分失败隔离**：agents 目录放入非法定义后 `/reload`，agents 条目报告跳过信息，其余条目正常成功；daemon 不崩溃、旧 agent 集合不丢。
6. **结果反馈**：`/reload` 在当前 tab 逐项展示四类结果（成功含统计、失败含原因、**无变化领域如实标注**）；`/help`、Tab 补全、输入提示三处均列出 `/reload`，文案说明 daemon.toml 需重启。

### 10.2 自动热重载（v3 新增）

7. **保存即生效（rules）**：修改项目 AGENTS.md 保存，**不执行任何命令**，约 1s 内 rules 重载完成；对旧 session 的下一轮迭代生效；TUI 主 tab 出现一条「已热重载」汇总提示。
8. **保存即生效（skills）**：在项目 skills 目录新建/删除一个 skill 目录并保存，自动重载后 `skill` 工具 description 同步变化。
9. **保存即生效（agents）**：新增/删除/修改 subagent 文件，自动重载后工具集合与 registry 一致（对账正确执行）；被删除 agent 的后续 spawn 返回「Unknown subagent type」。
10. **变化守卫**：对已保存且内容未变的文件再次保存（或 touch）→ daemon 不替换状态、TUI 无通知（或验证响应中标注无变更）；单测覆盖三个领域的「无变化判定」，其中 agents 守卫验证 9 字段全覆盖（单独修改 permission 或 system_prompt 字段必须判定为「有变化」）。
11. **自动失败不伤状态**：保存一个非法 agent 文件 → daemon 不崩溃、既有 agent 集合保留、出现一条失败提示与 warn 日志；随后修正文件保存 → 自动恢复正确状态。
12. **开关**：daemon.toml 关闭自动监听（重启 daemon 生效）→ 文件修改不再触发自动重载，`/reload` 仍正常工作。
13. **原子写收敛**：模拟编辑器原子写（写临时文件后 rename 覆盖目标）→ 单次有效重载（以通知条数/守卫日志验证）。
14. **缺目录场景（v3.1 扩充）**：(a) 启动时项目 agents 目录不存在 → 运行中创建该目录并放入定义文件 → 自动重载生效（监听补挂验证）；(b) **全新项目启动时 `.visp` 整体不存在 → 运行中首次创建 `.visp/rules`（或 agents/skills）并写入文件 → 首次创建的资产被自动重载**（降级链 #6 → project 根 → `.visp` 补挂验证）。
15. **9 字段守卫（v3.1 新增）**：修改某 agent 的 permission 规则或 system_prompt（不动 name/description）并保存 → 自动重载判定为「有变化」并生效（修复前该场景会被静默忽略）。

### 10.3 重连与 idle 看门狗（v2 保留 + v3.1 修正）

16. **自动重连**：daemon 重启期间 TUI 不退出，状态栏显示重连尝试；恢复后自动回放主 session 全部历史，subagent tab 被关闭清理，`/model` 列表为重启后最新值。
17. **重连期间交互**：输入在断线期间不被发送也不丢失；Ctrl+D 在重连等待中可随时退出。
18. **idle 看门狗（v3.1 修正措辞）**：(a) daemon 进程被挂起（如 SIGSTOP）或连接半开后，TUI 在 idle 超时（45s）+ 探测超时（5s）量级内检出并自动重连恢复；(b) **正常空闲时，周期性探测均成功，不产生任何重连**（观察多个探测周期无状态迁移）；(c) **长工具执行（超过 45s 的 bash 命令）与工具审批对话框挂起期间，探测成功、无重连，生成与审批流程不受影响**。
19. **看门狗与干净断开共用状态机**：两种检测入口进入同一 Reconnecting 状态，退避与恢复路径一致（状态机单测覆盖：idle 超时 → 探测失败 → 重连；idle 超时 → 探测成功 → 重置计时并继续周期性探测）。

### 10.4 质量门（v2 保留 + 扩充）

20. **新增逻辑均有单测**：RuleEngine 重载写回与 content 守卫、SkillTool 替换后 description 与 listing 守卫、agents 定义集合守卫（**全 9 字段等值**，含 `AgentDefinition` 的 `PartialEq` 派生）与对账算法、parse 分支、监听计划构建（路径→领域映射、缺目录降级含 `.visp` 从零创建）、事件→领域过滤、已监听路径集合去重。
21. **TUI 重连状态机与看门狗需可测**：状态迁移判定、per-tab 清理、idle 超时判定抽为**纯函数/可注入时钟与连接抽象**，单测覆盖 Connected→Reconnecting→Connected、exit 通道保留、per-tab 状态重置、subagent tab 关闭、以及看门狗两条路径（idle 超时 → 探测失败 → 重连；idle 超时 → 探测成功 → 重置继续）。
22. **文件监听集成测试**：临时目录中执行「写文件 → 等待 debounce → 断言领域重载被调用（可用可注入的 reload 核心假体）」；守卫拦截路径断言 reload 核心未被实际替换；补挂场景断言补挂后立即触发一次领域重扫。
23. `cargo test`、`cargo clippy -- -D warnings`、`cargo fmt -- --check` 全绿；既有测试不回归。

---

*文档遵循 design-doc-no-code 规则：全文仅含架构、流程、职责与决策描述；§3 的响应结构与 §7 决策 13 的比较对象为接口规范意义上的字段/语义说明，非实现代码。v2/v3 中已按代码核实的事实（行号引用）在 v3.1 修订时未重新验证的部分沿用原标注；v3.1 新核实的事实：`AgentDefinition` 9 字段与 derive 现状（`agent_definition.rs:31-47`，组成类型 `PartialEq` 见 `:2/:14-28`）、agent 解析失败容错行号（`agent_loader.rs:95-100`）、notify 7.0.0 的 macOS 后端条件编译（启用 kqueue 特性后 `RecommendedWatcher` 固定为 kqueue 后端）。*
