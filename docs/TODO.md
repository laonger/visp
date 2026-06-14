# TODO & 已知限制

> 最后更新：2026-06-14
> 基于真实代码状态核实，旧文档中 3 项「待实现」经确认已完成。

## Phase 完成状态

| Phase | 内容 | 状态 |
|-------|------|------|
| 1 | 项目骨架 + 核心抽象 | ✅ |
| 2 | LLM Provider + 内置工具 | ✅ |
| 3 | Agent 核心 + Daemon | ✅ |
| 4 | CLI 前端 (visp-cli) | ✅ |
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
