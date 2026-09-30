# TODO & 已知限制

> 最后更新：2026-09-30
> 基于真实代码状态核实，旧文档中 3 项「待实现」经确认已完成。

## Phase 完成状态

| Phase | 内容 | 状态 |
|-------|------|------|
| 1 | 项目骨架 + 核心抽象 | ✅ |
| 2 | LLM Provider + 内置工具 | ✅ |
| 3 | Agent 核心 + Daemon | ✅ |
| 4 | CLI 前端 (visp-tui) | ✅ |
| 5 | CodeGraph 代码智能 | ✅ |

**测试分布**：visp-codegraph 18+64 · visp-core 53 · visp-daemon 17 · visp-llm 22 · visp-tools 27 · 其余 ~480

---

## ✔ 已完成（此前被标记为「待实现」）

以下三项在旧 TODO 中列为待实现，经代码核实**均已落地**：

### Session 持久化到 SQLite ✅（visp-db / daemon）

**旧状态**：「当前使用 `InMemorySessionStore`，重启 daemon 后所有会话丢失」

**真实状态**：`crates/visp-db/src/store.rs` 已实现 `SqliteSessionStore: SessionStore`（完整的 CRUD + append_message + get_messages + list_by_project）。daemon 默认 storage driver 为 `"sqlite"`，路径 `~/.visp/data/visp.db`。见 `crates/visp-daemon/src/main.rs:181-184`。

### 对话历史 token 计数 + 上下文裁剪 ✅（visp-context / core）

**旧状态**：「Session.history 不设上限，可能导致 context window 溢出」

**真实状态**：`crates/visp-context/src/lib.rs` 实现 `DefaultContextTrimmer`（HEAD+MIDDLE+TAIL 三段裁剪策略）。Message 含 `estimated_tokens: u32` 字段。Agent 循环在每次调用前执行裁剪（`crates/visp-core/src/agent.rs:381-397`）。

### ConfigUpdate 支持 model 切换 ✅（daemon / cli）

**旧状态**：「/model 命令只改内存，LlmProvider 不重建」

**真实状态**：CLI 发送 `/model` → daemon 接收 `ConfigUpdate` 后重建 LlmProvider 并写回 RwLock（`crates/visp-daemon/src/service.rs:545-611`），Agent 循环每次从 provider_ref 读取当前 provider，切换即时生效。

---

## 🔴 待实现（按优先级排序）

### 固化skills、agent

### 继承RTK

https://github.com/rtk-ai/rtk

CLI proxy that reduces LLM token consumption by 60-90% on common dev commands. Single Rust binary, zero dependencies

### P1：gRPC TLS 支持（daemon / cli）

**问题**：daemon 监听 `[::1]:50051` 纯文本 gRPC，CLI 连接无加密。本地回环虽相对安全，但 TLS 为零信任/合规场景的必要特性。

**状态**：`DaemonSection` 无 TLS 配置字段，daemon 和 CLI 均无 TLS 握手逻辑。

**方案参考**：config 添加 `[daemon.tls]` section（cert_path/key_path 或 tls_mode），tonic 的 `ServerTlsConfig` / `ChannelBuilder::tls_config()`。

---

### P1：Agent 委派 / sub-agent 编排（core）

**问题**：当前 Agent 是单循环模式，所有工具调用在一个 loop 中串行执行。不支持多 specialist 异步协作、无 sub-task 拆分能力。

**状态**：代码中不存在 subagent、specialist、delegate task 相关实现。

**方案参考**：设计 sub-agent 编排机制——Agent 可 spawn 子 agent 独立执行子任务（如并行搜索代码、同时查多份文档），汇总结果后继续主流程。需考虑：子 agent 的 session 隔离、结果归并、错误传播、超时控制。

---

### P2：CodeGraph 模糊匹配（codegraph）

**问题**：CodeGraph 搜索仅使用 SQLite FTS5 全文搜索，LLM 生成的工具调用参数（符号名）与索引中的符号名必须精确匹配。如果 LLM 猜错符号名（如大小写不对、拼写近似），搜不到结果。

**状态**：`crates/visp-codegraph/` 中无 fuzzy/levenshtein/edit_distance/jaccard 等模糊匹配实现。`visp-plan-codegraph-highlevel-tools.md` 末尾已标记此 TODO。

**方案参考**：实现编辑距离 + token 重叠度评分，对 FTS5 结果退化为模糊匹配，按相似度排序返回 top-k。

---

### P2：Prompt 整体优化（core / tools）

**问题**：项目内各 tool 的 prompt 描述、Agent system prompt、rule 注入方式等分散在多处，缺乏统一的 prompt 工程策略。部分 tool 描述过于简略或表述不一致，导致 LLM 理解偏差、生成不准确的工具调用参数。

**状态**：需系统梳理 `visp-core/src/agent.rs` 中的 system prompt、各 Tool 的 `description()` 返回、rules 注入格式，统一风格、补充缺失说明、优化英文/中文表述。

**方案参考**：
- 统一 prompt 模板中英文使用规范（中英文混合场景 vs 全英文场景）
- 为每个 tool 的 description 增加参数说明、返回值格式、使用示例
- 优化 system prompt 的结构——职责声明、工具选择指南、输出格式要求
- 对 rules 注入做 token 预算分配，避免 rules 过长挤占正常对话空间

---

### P2：Tool 名称适配 Claude 大写调用（tools）

**问题**：Claude 模型在生成 tool call 时，倾向于将 tool 名称首字母大写（如 `ReadFile`、`Bash`、`Grep`），而当前所有 tool 名称为小写 snake_case（`read_file`、`bash`、`grep`）。这导致 tool 调用匹配失败，LLM 报 "tool not found" 错误。

**状态**：当前 tool 名称列表：
- `bash`, `read_file`, `write_file`, `edit_file`, `grep`, `glob`, `fetch_web`
- `codegraph_rebuild`, `codegraph_search`, `codegraph_get_details`, `codegraph_context`, `codegraph_trace`, `codegraph_impact`

**方案参考**：
- 将 tool 名称统一改为首字母大写或 PascalCase（如 `ReadFile`、`WriteFile`、`EditFile`、`Bash`、`Grep`、`FetchWeb`、`CodegraphSearch` 等）
- 需同步更新所有调用处：Tool trait 的 `name()` 返回值、tool registry 注册、daemon 中 tool 路由、测试中匹配 tool name 的字符串字面量
- 在 `visp-core/src/tool.rs` 的 `Tool` trait 文档中注明命名规范（PascalCase）

---

### P2：`SessionError::AlreadyExists` 错误消息错误（core）

**问题**：`crates/visp-core/src/error.rs:90-91` 的 `AlreadyExists` 变体错误消息显示 "Session not found: {0}"，与 `NotFound` 完全一样（应该是 "Session already exists: {0}" 或其他指明「已存在」的消息）。

**状态**：Review 已指出、仍未修复。

---

### P2：visp-core 包含 IO 操作（core — 架构违规）

**问题**：`crates/visp-core/src/rules.rs` 和 `crates/visp-core/src/session.rs` 中存在 `std::fs::read_to_string`、`read_dir` 等 IO 调用。`visp-core` 的设计约束是纯逻辑、不依赖任何 IO。这些 IO 应移到 `visp-tools` 或 `visp-daemon`。

**状态**：首次代码 review 已指出、仍未修复。

---

### P2：Memory 系统（core / db / tools）

**问题**：当前 Agent 是无状态的——每次对话从头开始，没有跨 session 的知识积累。Agent 无法记住用户偏好、项目约定、已解决的问题、之前发现的 bug 位置等。

**状态**：不存在任何记忆存储/检索机制。Session 历史虽持久化到 SQLite，但只作为对话回放用，Agent 不会主动写入或查询结构化记忆。

**功能设计**：
- `memory` 工具（tool）：Agent 可显式调用 `memory write` / `memory read` / `memory search`
- 记忆条目：键值对 + 自然语言内容 + 标签 + 时间戳
- 存储：独立 SQLite 表（`memory`），非 session 绑定，跨对话共享
- 检索：支持按 tag 过滤、全文搜索、最近使用排序
- 注入：每次 Agent 循环启动时，自动注入相关记忆到 system prompt（由 context trimmer 控制预算）

**后续考虑**：
- 记忆优先级/衰减：高频使用的记忆保留，低频的自动归档
- 会话级 vs 项目级 vs 用户级作用域
- LLM 自动总结旧记忆、合并重复条目

**问题**：`crates/visp-core/src/session.rs:30` 中 `created_at: Instant` 是相对时间戳，db 持久化重启后无法回溯真实的创建时间。

**状态**：应改用 `chrono::DateTime<Utc>` 或 `SystemTime`。

---

### P2：工具审批 `reject_always` 语义（core — 来自 visp-acp 设计未决 5，2026-09-23 拍板本期降级）

**问题**：审批答复 `selected_index` 仅支持 `0`（允许）/`2`（始终允许）/其他（拒绝，`agent_loop.rs:1216-1244`），无「永久拒绝某工具」的会话内存储位。ACP 适配器（`visp-acp`，见 `docs/design/visp-design-acp.md` §6.5）收到客户端 `reject_always` 只能降级为 `reject_once`（`selected_index = 1`），用户选"永久拒绝"后同类工具仍会再弹窗。

**影响**：安全方向（多弹窗不会误放行），功能保真度缺失。

**方案参考**：Session 增加 `rejected_tools: HashSet<String>`（对齐已有 `approved_tools`），审批判定逻辑优先查拒绝集合；`reject_always` 回填新语义索引或在 UserResponse 中携带 always_reject 标记。可与 ACP 适配器 M1 后迭代同批实现。

---

### P2：LLM 原生 stop reason 透出（daemon / llm — 来自 visp-acp 设计未决 5，2026-09-23 拍板本期降级）

**问题**：daemon/LLM 层未向上暴露 LLM 原生 stop reason（`max_tokens` 截断、内容过滤、工具调用终止等不可区分），错误一律以统一 `Error` 收尾。ACP 侧只能把所有结束映射为 `end_turn`/`refusal`，Zed 用户无法看到「输出因长度被截断」。

**影响**：信息保真度，无功能损害；长输出截断时用户略困惑。

**方案参考**：`visp-llm` provider 响应解析处保留 stop reason 原始值，沿 `AgentEvent`/`ServerMessage` 透传（proto 可加可选字段），TUI/ACP 按需展示。属既有缺口，非 ACP 引入。

---

## 🔶 已知限制（可优化，非阻塞）

### Phase 3 已知限制

#### 1. Agent 循环 panic 时状态无法自动恢复（daemon）

Agent 循环在等待 UserQuery 确认时 panic，mpsc sender 被 drop，daemon service 阻塞等待客户端 UserResponse，Session 永久卡在 Running 状态。

**临时缓解**：客户端超时断开后，通过 DeleteSession 手动清理。概率极低。

**后续方案**：用 `tokio::select!` 同时监听 mpsc receiver 和 UserResponse，加 heartbeat 检测。

#### ~~2. 规则热重载不打断运行中的 Agent（core — 设计行为~~ -- 不打算制作热重载

~~规则文件变更后，正在运行的 Agent 循环继续使用旧规则（快照机制）。只有下一轮对话才加载新规则。这是设计行为，但部分用户可能期望立即生效。~~

#### ~~3. Chat 流中 UserQuery 等待时无法处理其他消息（daemon — 设计限制）~~ -- 没有意义

~~当 daemon service 阻塞等待客户端回复 UserQuery 时，无法处理同一 Chat 流上的其他消息（如新的 UserInput）。当前设计下，同一会话同一时刻只有一个 UserQuery，此限制可接受。~~

### Phase 5 已知限制

#### 4. 全量构建无进度反馈（codegraph）

全量构建是 daemon 启动时的后台初始化操作，对用户透明。构建期间查询返回"索引构建中"。MVP 不做进度回调。

**后续方案**：添加 `build_status() -> Progress` 状态查询接口。

#### 5. 跨文件关系解析：A↔B 循环导入（codegraph）

两个文件互相导入对方符号时，不产生死循环（全局符号表已完成），但调用关系可能产生间接循环边。查询调用者/被调用者时 SQL JOIN 只需一步，不受影响。

**后续方案**：如需实现路径追踪/影响分析，需在查询层加环检测。

#### 6. Source 字段截取规则（codegraph）

`SymbolDetails.source` 基于 `line` 字段读文件取源码片段，MVP 截取前 500 字符。

**后续方案**：使用 tree-sitter 的 node range 精确定位完整函数源码。

---

## 📋 终端通知：v1 不做项与未来扩展（notification）

> 登记于计划 `docs/plans/visp-plan-notification.md` 步骤 6c；来源 `docs/design/notification.md` §5（不做项）与 §7（未来扩展）。

### v1 不做项（设计已拍板，本期不实现）

- **系统通知后端**（osascript / notify-rust）：v1 只走终端协议通道；未来可作为「协议不可用时的 fallback」扩展
- **kitty OSC 99 高级特性**：点击动作、关闭回调、图标、进度
- **kitty OSC 99 能力探测**（`a=q` 查询等待响应）：v1 用环境变量判定协议
- **tmux `DCS tmux;` 包裹**：tmux 用户可自行开启 allow-passthrough
- **终端聚焦检测、通知文案模板、点击跳转、通知历史**

### 未来扩展（不在本期）

- kitty OSC 99 能力探测（`a=q` 查询/响应），替代或校准环境变量探测
- 系统通知 fallback 后端（协议不可用或复用器拦截场景）
- tmux DCS 包裹、聚焦检测、通知文案带任务摘要

### 已知限制

#### 未识别终端回退 BEL 兜底（visp-tui — 已修订进设计正文，经用户拍板 2026-09-18）

**问题**：rmux / tmux 等复用器可能拦截或丢弃 OSC 9/777/99 通知序列（rmux 的 `osc_notification` 为空实现），导致通知完全静默。

**状态**：未识别终端（含 rmux / tmux 等复用器）统一回退 BEL（`0x07`）兜底——保响铃/🔔 提示，无文本横幅。**已修订进设计正文**：`docs/design/notification.md` §1「实现偏离说明（2026-09-18）」，原决策「未知终端盲发 OSC 9」作废。该兜底属事实上的复用器适配（不特殊识别某个复用器，通用「未识别 → BEL」）。已识别终端仍按 OSC 9/777/99 发送；复用器拦截 OSC 时不做绕行，是否透传由复用器自身负责（tmux 的 allow-passthrough、rmux 的透传实现）。

**后续方案**：系统通知 fallback 后端（协议不可用或复用器拦截场景）。

---

## 2026-09-30 新增：思考增量语义 + daemon 双 loop 加固 的已知边界与另立项

> 来源：`docs/design/2026-09-29-thinking-delta-and-double-loop-design.md`（**已实施**）。
> 本清单记录**本期显式接受**的边界与**建议另立项**项，避免随设计文档沉没。

### 另立项（建议单独立项）

1. **回放路径不下发思考块**（daemon）：`JoinSession` 回放把思考消息当普通 `TextDelta` 重放、`extra_blocks` 不回放。增量改造不使其恶化，但重连后思考缺失是既有行为。
2. **Anthropic `signature_delta` 未解析**（llm）：落入 `_ => Skip`，签名仅来自 `content_block_start`，真实流式下签名保真度受损。**同族**：`content_block_stop` 的「发射最终完整块」分支疑似不可达（累积键与查找键不一致）；core「只保留最后一块」导致多块丢失。→ 与 `docs/todo/thinking-history-issues.md` 的**问题 1/2/3 同族，建议合并立项**。
3. **多思考块完整性**：同上（core 只保留最后一块；Anthropic 交错思考/工具时签名丢失）。
4. **`status` 失步的常态化收敛**（core / daemon）：当前仅依赖 daemon 重启时的孤儿 `Running` 重置（设计 §12.4 B3 未定案）。
5. **取消动作移除令牌的时机**（core）：`cancel_agent` 现为「取消即移除」，造成取消后短暂窗口内守卫放行、由状态闸拒绝（设计 §12.4 B4 未定案）。
6. **ACP 纵深（原计划步骤 5a）未实施**：结束本轮前确认 daemon 终态 + 兜底超时补发 Cancel。计划中标记为**可选**，本期未做；当前仍以 10s 兜底自行收尾。

### 已知限制（本期显式接受）

7. **`ThinkingBlock.thinking` 语义静默变更为「增量」**（proto）：不加标记字段，仅以注释声明语义并指向回归测试。**发布说明须点明**；存在混合版本本地部署时，旧客户端会静默降级为只显示最后一片。
8. **思考丢帧不自愈**：增量语义下丢失的增量永久缺失；但该自愈窗口仅存在于同一连接内的后续帧，Lagged replay 与重连回放本就不含思考帧。
9. **多块时显示内容多于持久化内容**：TUI/ACP 显示「块1+块2」拼接，而落库仍只有最后一块（设计 §4.4）。
10. **跨轮追加到同一 TUI 行**：thinking-only 跨多轮时，新块首帧被追加到上一轮仍在的同一 Thinking 行（设计 §4.4）。
11. **TUI `event.rs` 层对 `SessionBusy` 仍会 `stop_generating()`**：`app.rs` 三处站点已特判（不置 `Error`、不 flush、不提前 stop），但 `event.rs` 的 `handle_grpc_message` Error 分支对任何 Error 都会释放门禁——属既有行为，符合「输入门禁不改」的决策。真实取消窗口内会因 `stale_done_expected` 提前 return，不影响主缺陷。
12. **思考/文本交替会轮换 `messageId`**（acp，既有策略）：`translate.rs` 中 `TextDelta` 带 `agent_name`、`ThinkingBlock` 固定传 `""`，二者共用 `last_agent` 做轮换。

### 工程卫生（本次实施发现）

13. **既有 flaky 测试**（core）：`crates/visp-core/src/session.rs` 中 `unsafe { std::env::set_var("HOME", ...) }` 与并行测试竞态，导致 `agent_loop::tests::test_agent_run_carries_session_id_field` **间歇失败**；因测试过滤是子串匹配，`cargo test -p visp-core session` 会把它捞进来。→ 建议加互斥，或实施时改用 `session::tests` 精确过滤。
14. **文档同步**：`docs/design/visp-design-acp.md` 已与代码过时——仍把「LLM 提问等待中取消无收尾信号」列为 V10 例外，而 `agent_loop` 已补发 `Error{Cancelled}` 并收尾（§13.10 方案 A 已落地）；V10/V11、§6.6、§13 风险表等条目需同步修订。
15. **Polyglot 提醒**：`ThinkingBlock` 语义变更的机器可检测护栏是两条 core 契约测试（「连续同块快照 → 只发增量」与「流式期间不产生快照变体」）；proto 注释已指向它们，改动语义须同步更新。
16. **G5 判据的理论窗口（可达性极低，记录备查）**：G5 以「当前状态是否为 `Running`」近似「是否已被新回合改写」。若旧回合收尾迟到到**新回合自身也已走到终态**，判据不生效，旧收尾会以错误的终态发布会话级 `Stop`，并抢先移除（此刻属于新回合的）registration。需 orchestrator 被饥饿整个新回合才可能触发。
17. **死变体待清理**：`AgentEvent::ThinkingBlock` 已**无生产者**（唯一发射点是新增的 `AgentEvent::ThinkingDelta`），其 `event_to_msg` 与 daemon 映射臂仅为维持穷尽匹配而保留。确认无其它路径后应删除该变体与其映射臂。
18. **显示层反例缺断言**：设计 §4.4 已记录两类显示层已知差异（回退时的显示残留 `ABCAB`；前缀碰撞导致块合并），但**当前没有显示层用例覆盖**；如需防回归，应在 TUI / ACP 侧补显示断言。
19. **测试代码的潜在 lint**：`crates/visp-daemon/src/service.rs` 测试模块内有一处 `while_let_loop`（约 :5946，来自忙拒绝 e2e 用例）。CI 门禁的 `cargo clippy` 不带 `--all-targets`、不检查测试代码，故**不影响门禁**；但 `cargo clippy --all-targets` 会报。建议顺手修掉。

---

## 2026-09-30 新增（二）：工具调用被 SSE 解析丢弃 —— 根因与修复

> 现象：模型思考里"说要使用工具"，但**工具从未执行**；表现为 15 轮空转、约 124 秒、15 次 LLM 调用，最终由用户手动取消，且每轮思考被追加显示，看起来像"重复推理"。

**根因（日志实证）**：provider 元数据报告 `finish_reasons = ["tool_calls"]`、`is_token_limit = false`，而 agent loop 解析到的 `tool_calls` 为空 → 被归类为 **thinking-only → `Continue`**。
证据原文（`~/.visp/logs/visp-daemon.log.<date>`）：
`"message":"LLM produced thinking-only response (no text/tool calls), continuing loop","output_tokens":169,"is_token_limit":false,"finish_reasons":"Some([\"tool_calls\"])"`

**解析层缺口（`crates/visp-llm/src/openai.rs` 的 delta 解析，均已修）**：
1. 逐字段提前 `return`：同一 chunk 同时含 `reasoning_content`/`reasoning` 与 `tool_calls` 时，工具调用被吞；
2. `tool_calls` 数组内「匹配到第一个就 `return`」：一个 chunk 含多个 tool_call 时只处理第一个；
3. 完全不认旧式 `function_call` 字段。

**已落地修复**：`db2c762d`（按 chunk 收集事件 + 处理全部 tool_call + 兼容 `function_call`）、`886723f2`（provider 声称 `tool_calls` 却解析不到时**明确失败**，不再空转续跑）。

**残余 / 未做**：
1. **未抓到原始 SSE**：因此无法确证用户那次命中的是缺口 1 还是缺口 3（两者均已修）；F4 作为兜底，遇到未知形态会给出明确错误而非空转。
2. 「provider 声称 `tool_calls`，但既无正文也无思考块」的变体仍会落到空响应分支：`output_tokens > 0` 会报错，`== 0` 仅告警后 `Done`（静默）——未处理。
3. **连续 thinking-only 续跑仍无上限**（本次靠手动取消；上限为软 50 / 硬 200）。

### 可观测性 / 运维缺口（本次排查付出代价才发现）

1. **ACP 自拉起 daemon 的日志文件恒为 0 字节**：`visp-acp` 把子进程 stdout/stderr 重定向到 `~/.visp/logs/daemon-*.log`，但 daemon 的日志实际写自己的滚动文件 `~/.visp/logs/visp-daemon.log.<date>`（`[observability] log_file`，默认 `~/.visp/logs`）。两套路径没对齐，现场排查时"看不到日志"。
2. **孤儿 daemon**：`visp-acp` 非正常退出（被 Zed 终止/超时）时不回收它自拉起的 daemon；实测 50051 / 50052 / 50053 各残留一个。
3. **端口探测疑似 TOCTOU**：Zed 日志多次记录 `starting daemon addr=[::1]:50051`，即使该端口当时已被上一轮 daemon 占用——建议复核 `find_available_addr` 的「试绑后立即释放」在 macOS 上的行为，或改为「失败即换端口」。
4. **`Incoming transport closed: session/new`（未定位）**：手工复现 `initialize + session/new` 两次均正常；Zed 日志显示 agent 只输出一行 `starting daemon` 后**静默死亡、无 stderr 错误**，且失败前后 Zed 自身在重启（`ERROR timed out waiting on app_will_quit` → `crash handler registered`）。怀疑是 Zed 侧退出/线程重建把 agent 子进程整体终止，但**缺实锤**。

### 发布说明条目（草案）

> **行为变更**：`ThinkingBlock.thinking` 的语义由「累积全文快照」改为「**增量**」（相对同一思考块上一帧的新增部分）。这是 proto 契约的**静默语义变更**——字段名与编号未变，消费方应改为「**追加**」消费（ACP 的 `agent_thought_chunk` 消费方式天然正确；TUI 已同步改为追加）。若存在与本仓库**不同批构建**的旧客户端，其思考显示会降级为只显示最后一片。


