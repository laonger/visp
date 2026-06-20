# visp 工作计划：恢复 session 时一并恢复子 agent tab

## 概述

实现 `visp -s <main-id>` 或 `/sessions <id>` 切换会话时，daemon 递归回放所有后代子 agent 的历史，CLI 以 ViewOnly tab 展示。涉及 5 个 crate：visp-proto / visp-core / visp-db / visp-daemon / visp-cli。

设计文档：`docs/design/visp-design-resume-subagent-tabs.md`（第四版）。

### 设计文档笔误修正（计划阶段确认）

设计文档"改动范围"表中 `AgentStatus` 归属写为 visp-core，实际位置是 `crates/visp-cli/src/app.rs:296`。本计划按真实位置实施，回头同步修设计文档。

### 关键事实（计划阶段确认）

| 事实 | 位置 | 现状 |
|---|---|---|
| `AgentStatus` 枚举 | `visp-cli/src/app.rs:296` | 3 变体：Running/Done/Error |
| `TabEntry::new(session_id, agent_name)` | `visp-cli/src/app.rs:318` | 硬编码 `status: AgentStatus::Running` |
| `SessionStore` trait | `visp-core/src/session.rs:53` | 8 方法，无 `list_child_sessions` |
| `SessionRepo` SQL 层 | `visp-db/src/session_repo.rs` | `list_by_project` 模式可复用 |
| `StatusUpdate` proto | `visp-proto/proto/visp.proto` | 已有 `session_id/agent_name/user_inputs`，无 `view_only` |
| `Error` proto | `visp-proto/proto/visp.proto:273-283` | 已有 `code/message/session_id/agent_name`，复用承载 `SessionNotActive` |
| `JoinSession` handler | `visp-daemon/src/service.rs:423-534` | 主回放跳过 `Role::User` |
| DB schema | V4 | 已有 `parent_id/agent_name` 列，无 `parent_id` 索引 |

---

## 步骤 1：visp-proto 扩展 view_only 字段

### 1a：StatusUpdate 新增 view_only

#### 红 — 测试

| # | 测试用例 | 描述 |
|---|---------|------|
| 1 | `status_update_default_view_only_is_false` | 未设置 view_only 时默认为 false（proto3 语义） |
| 2 | `status_update_with_view_only_true` | 显式设置 view_only=true 能正确序列化/反序列化 |

#### 绿 — 实现

在 `StatusUpdate` message 末尾新增 `bool view_only = N;`（N 为当前最大字段号+1）。重新 `cargo build -p visp-proto` 生成代码。

#### 测试 → 类型检查

```bash
cargo test -p visp-proto && cargo clippy -p visp-proto -- -D warnings && cargo fmt -p visp-proto -- --check
```

#### 提交

`feat(proto): add view_only field to StatusUpdate`

---

## 步骤 2：visp-cli AgentStatus + TabEntry 扩展

### 2a：AgentStatus 新增 ViewOnly 变体

#### 红 — 测试

| # | 测试用例 | 描述 |
|---|---------|------|
| 1 | `agent_status_view_only_variant_exists` | ViewOnly 变体可构造、可 match |
| 2 | `agent_status_all_variants_display` | Running/Done/Error/ViewOnly 都有合理 Display/颜色映射（回归） |

#### 绿 — 实现

在 `AgentStatus` 枚举新增 `ViewOnly` 变体；UI 渲染分支（`ui.rs` 中按状态选颜色/图标的位置）补充 ViewOnly 分支（建议灰色 + `⏸` 或 `[View Only]` 标记，具体由 @designer 评估）。

#### 测试 → 类型检查

```bash
cargo test -p visp-cli agent_status && cargo clippy -p visp-cli -- -D warnings
```

#### 提交

`feat(cli): add ViewOnly variant to AgentStatus`

### 2b：TabEntry::new_view_only 构造器

#### 红 — 测试

| # | 测试用例 | 描述 |
|---|---------|------|
| 1 | `tab_entry_new_view_only_has_view_only_status` | `TabEntry::new_view_only(sid, name)` 初始 status 为 ViewOnly |
| 2 | `tab_entry_new_keeps_running_status` | 回归：`TabEntry::new(sid, name)` 仍为 Running（未破坏现有 28 处调用） |
| 3 | `tab_entry_new_view_only_other_fields_default` | 其他字段（frames/messages/scroll 等）与 new() 一致 |

#### 绿 — 实现

新增 `pub fn new_view_only(session_id: impl Into<String>, agent_name: impl Into<String>) -> Self`，内部调用 `new()` 后将 `status` 置为 `ViewOnly`。不动 `new()` 签名。

#### 测试 → 类型检查

```bash
cargo test -p visp-cli tab_entry && cargo clippy -p visp-cli -- -D warnings && cargo fmt -p visp-cli -- --check
```

#### 提交

`feat(cli): add TabEntry::new_view_only constructor`

---

## 步骤 3：visp-core SessionStore trait 扩展

### 3a：SessionStore 新增 list_child_sessions

#### 红 — 测试

| # | 测试用例 | 描述 |
|---|---------|------|
| 1 | `session_store_list_child_sessions_signature` | trait 有 `list_child_sessions(&self, parent_id: &str) -> Result<Vec<Session>, SessionError>` |
| 2 | `mock_store_list_child_sessions_returns_empty` | 默认 mock 实现返回空 Vec（保证不破坏现有 mock） |
| 3 | `mock_store_list_child_sessions_returns_sessions` | mock 实现可配置返回值 |

#### 绿 — 实现

在 `SessionStore` trait 新增 `fn list_child_sessions(&self, parent_id: &str) -> Result<Vec<Session>, SessionError>;`。所有现有实现（`visp-db::Store`、测试用 mock）补充默认实现或显式实现。

> 设计文档原文用 `SessionMeta`（轻量元信息），但 `Session` 结构本身已含所需字段（id/agent_name/status/created_at），且 trait 一致性更高。计划阶段决定返回 `Vec<Session>`，daemon 端 BFS 时按需忽略 history 字段。这是对设计的合理细化，回头同步设计文档。

#### 测试 → 类型检查

```bash
cargo test -p visp-core session_store && cargo clippy -p visp-core -- -D warnings && cargo fmt -p visp-core -- --check
```

#### 提交

`feat(core): add list_child_sessions to SessionStore trait`

---

## 步骤 4：visp-db 实现 list_child_sessions + parent_id 索引

### 4a：SessionRepo::list_child_sessions SQL + parent_id 索引

#### 红 — 测试

| # | 测试用例 | 描述 |
|---|---------|------|
| 1 | `list_child_sessions_returns_children` | 父 session 有 2 个子 session 时返回 2 条 |
| 2 | `list_child_sessions_empty_when_no_children` | 父 session 无子时返回空 Vec |
| 3 | `list_child_sessions_orders_by_created_at_asc` | 多个子 session 时按 created_at 升序 |
| 4 | `list_child_sessions_excludes_parent_self` | 不会返回 parent_id 等于自己的 session |
| 5 | `list_child_sessions_only_returns_direct_children` | 只返回直接子，不返回孙级（BFS 由 daemon 层负责递归） |
| 6 | `parent_id_index_created_idempotent` | schema 迁移幂等：多次运行不报错 |
| 7 | `list_child_sessions_does_not_return_other_parents_children` | parent_id 精确匹配，不串 |

#### 绿 — 实现

1. 在 `session_repo.rs` 新增 `list_child_sessions(conn, parent_id) -> Result<Vec<Session>>`，SQL：`SELECT * FROM session WHERE parent_id = ? ORDER BY created_at ASC`。
2. 在 `schema.rs` 的新迁移（或现有迁移幂等块）中 `CREATE INDEX IF NOT EXISTS idx_session_parent_id ON session(parent_id)`。
3. `store.rs` 的 `SessionStore` 实现补充 `list_child_sessions` 方法委托给 `SessionRepo`。

#### 测试 → 类型检查

```bash
cargo test -p visp-db list_child_sessions && cargo clippy -p visp-db -- -D warnings && cargo fmt -p visp-db -- --check
```

#### 提交

`feat(db): implement list_child_sessions with parent_id index`

---

## 步骤 5：visp-daemon JoinSession BFS 后代回放

### 5a：BFS 后代收集 + 单 session 回放工具函数

#### 红 — 测试

| # | 测试用例 | 描述 |
|---|---------|------|
| 1 | `collect_descendants_bfs_flat` | 主 session 直挂 2 子，收集到 2 个，按 created_at 升序 |
| 2 | `collect_descendants_bfs_nested` | 主→子→孙 3 层，BFS 顺序为 [子, 孙]，先父后子 |
| 3 | `collect_descendants_visited_prevents_cycle` | 构造理论环路（手动塞 visited），不重复访问 |
| 4 | `collect_descendants_soft_limit_50` | 60 个子 session 时只返回前 50 个（BFS 层级优先 + created_at 倒序截断） |
| 5 | `collect_descendants_skips_load_failure` | 某子 session 加载抛错时跳过、继续、不 panic |
| 6 | `replay_single_session_emits_status_update_with_view_only` | 单 session 回放：首帧为 StatusUpdate，view_only=true，agent_name 正确 |
| 7 | `replay_single_session_task_prompt_in_user_inputs` | 子 session 首条 Role::User 消息内容出现在 StatusUpdate.user_inputs |
| 8 | `replay_single_session_skips_subsequent_user_messages` | 子 session 多条 Role::User 时，user_inputs 含全部但消息流不重复发 |
| 9 | `replay_single_session_emits_assistant_and_tool_frames` | Role::Assistant → TextDelta(+ToolCall)，Role::Tool → ToolResult，顺序正确 |
| 10 | `replay_single_session_emits_done_at_end` | 末帧为 Done(session_id=child) |

#### 绿 — 实现

1. 新增内部函数 `collect_descendants(store, root_id) -> Vec<Session>`：BFS 队列 + `visited: HashSet` + 软上限 50 截断（BFS 层级优先，同层 created_at 倒序保留）。
2. 新增内部函数 `replay_session_history(tx, session, messages) -> Result<()>`：发 StatusUpdate(view_only=true, user_inputs=首条 User 内容) → 遍历 messages 发帧 → 发 Done。
3. 单 session 加载失败用 `tracing::warn!` 记录并跳过。

#### 测试 → 类型检查

```bash
cargo test -p visp-daemon collect_descendants replay_single && cargo clippy -p visp-daemon -- -D warnings && cargo fmt -p visp-daemon -- --check
```

#### 提交

`feat(daemon): add BFS descendant collection and session replay helpers`

### 5b：JoinSession handler 集成后代回放

#### 红 — 测试

| # | 测试用例 | 描述 |
|---|---------|------|
| 1 | `join_session_with_no_children_replays_only_main` | 主 session 无子时行为与原来一致（回归） |
| 2 | `join_session_with_children_replays_main_then_descendants` | 有子时帧序列：主 StatusUpdate → 主消息 → 主 Done → 子 StatusUpdate → 子消息 → 子 Done |
| 3 | `join_session_descendants_view_only_flag_set` | 所有子 session 的 StatusUpdate.view_only=true |
| 4 | `join_session_soft_limit_warning_emitted_to_main` | 超 50 个子时主 session 收到 TextDelta 警告消息 |
| 5 | `join_session_main_replay_unchanged_skip_user` | 回归：主 session 回放仍跳过 Role::User（不因新功能破坏） |

#### 绿 — 实现

在 `JoinSession` handler 现有主 session 回放完成后，调用 `collect_descendants` + 循环 `replay_session_history`。超限时向主 session 推一条 TextDelta 警告。

#### 测试 → 类型检查

```bash
cargo test -p visp-daemon join_session && cargo clippy -p visp-daemon -- -D warnings
```

#### 提交

`feat(daemon): replay descendant sub-agent tabs on JoinSession`

### 5c：UserInput 对非 Running session 返回 SessionNotActive

#### 红 — 测试

| # | 测试用例 | 描述 |
|---|---------|------|
| 1 | `user_input_to_view_only_session_returns_session_not_active` | 向 ViewOnly tab 发 UserInput，daemon 返回 Error{code="SessionNotActive"} |
| 2 | `user_input_to_running_session_works` | 回归：向 Running session 发 UserInput 正常处理 |
| 3 | `session_not_active_error_includes_session_id` | Error 帧 session_id 字段正确填充 |

#### 绿 — 实现

在 `UserInput` handler 入口处检查目标 session 是否在活跃 loop 集合中（或根据 DB status 判断），非活跃则返回 `Error { code: "SessionNotActive", message: "...", session_id, agent_name }`。

> 计划阶段决策：判断"非 Running"的标准 = daemon 内存中的活跃 loop 集合不包含该 session_id。不依赖 DB status 字段（设计文档已论证 DB status 不可靠）。

#### 测试 → 类型检查

```bash
cargo test -p visp-daemon user_input session_not_active && cargo clippy -p visp-daemon -- -D warnings
```

#### 提交

`feat(daemon): reject UserInput to non-running sessions with SessionNotActive`

---

## 步骤 6：visp-cli route_frame 适配 view_only + ViewOnly tab UI

### 6a：route_frame 按 view_only 建 ViewOnly tab

#### 红 — 测试

| # | 测试用例 | 描述 |
|---|---------|------|
| 1 | `route_frame_status_update_view_only_creates_view_only_tab` | 收到 view_only=true 的 StatusUpdate 且 session 未知时，自动 `insert_sub_agent` 并设 status=ViewOnly |
| 2 | `route_frame_status_update_view_false_creates_running_tab` | 回归：view_only=false（默认）时仍建 Running tab |
| 3 | `route_frame_existing_view_only_tab_not_recreated` | 已存在的 ViewOnly tab 收到新帧不重建 |
| 4 | `route_frame_user_inputs_populated_for_view_only_tab` | ViewOnly tab 的 input_history 被填充 user_inputs（↑↓ 可翻看 task prompt） |

#### 绿 — 实现

修改 `route_frame()` 中处理 StatusUpdate 的分支：读取 `view_only` 字段，新建 tab 时调用 `TabEntry::new_view_only()` 而非 `new()`。`user_inputs` 处理逻辑保持（填充 input_history）。

#### 测试 → 类型检查

```bash
cargo test -p visp-cli route_frame view_only && cargo clippy -p visp-cli -- -D warnings
```

#### 提交

`feat(cli): route_frame creates ViewOnly tabs for view_only StatusUpdate`

### 6b：ViewOnly tab UI 渲染（禁用输入 + task prompt 标注）

#### 红 — 测试

| # | 测试用例 | 描述 |
|---|---------|------|
| 1 | `view_only_tab_input_submission_disabled` | ViewOnly tab 下按 Enter 不触发 send_user_input（或触发后被拒并提示） |
| 2 | `view_only_tab_shows_task_prompt_marker` | ViewOnly tab 渲染时 task prompt（input_history[0]）以可区分样式显示，带 `[task prompt]` 前缀或灰色 |
| 3 | `view_only_tab_arrow_keys_browse_input_history` | ↑↓ 仍可翻看 input_history（只读） |
| 4 | `view_only_tab_status_indicator_renders` | tab 标题渲染 ViewOnly 状态指示（灰色 / `[View Only]`） |

#### 绿 — 实现

1. 输入处理分支：当前 active tab 是 ViewOnly 时，Enter 键改为渲染提示"此 tab 为只读历史，无法输入"（不调用 send_user_input）。
2. UI 渲染：`ui.rs` 中 ViewOnly 状态走专属渲染分支（颜色/图标/标题前缀）。task prompt 从 `input_history[0]` 取并加 `[task prompt]` 标记渲染。
3. ↑↓ 翻看逻辑不动（已天然兼容）。

> 具体视觉样式（颜色色值、图标、前缀文案）由 @designer 在实现时定稿，本步骤只保证逻辑分支和测试通过。

#### 测试 → 类型检查

```bash
cargo test -p visp-cli view_only_tab && cargo clippy -p visp-cli -- -D warnings && cargo fmt -p visp-cli -- --check
```

#### 提交

`feat(cli): render ViewOnly tab with disabled input and task prompt marker`

### 6c：SessionNotActive Error 帧渲染

#### 红 — 测试

| # | 测试用例 | 描述 |
|---|---------|------|
| 1 | `route_frame_error_session_not_active_renders_hint` | 收到 Error{code="SessionNotActive"} 时，对应 tab 渲染提示"该会话已结束，无法继续输入" |
| 2 | `route_frame_error_other_codes_unchanged` | 回归：其他 code 的 Error 帧行为不变 |
| 3 | `route_frame_error_routes_by_session_id` | Error 帧按 session_id 路由到正确 tab（不串到主 tab） |

#### 绿 — 实现

在 `route_frame()` 处理 Error 帧的分支中，按 `code` 字段分支：`"SessionNotActive"` → 渲染友好提示气泡；其他 → 保持现有行为。

#### 测试 → 类型检查

```bash
cargo test -p visp-cli session_not_active error_frame && cargo clippy -p visp-cli -- -D warnings
```

#### 提交

`feat(cli): render SessionNotActive error as friendly hint`

---

## 步骤 7：端到端验证

### 7a：集成测试（子 agent tab 完整恢复）

#### 红 — 测试

| # | 测试用例 | 描述 |
|---|---------|------|
| 1 | `e2e_resume_session_with_sub_agents` | 启动 daemon → 创建主 session → mock task 工具派生 2 子 agent（含 1 孙）→ 子完成 → 退出 CLI → `visp -s <main-id>` → 断言：主 tab 完整、2 子 tab + 1 孙 tab 出现、状态全为 ViewOnly、各 tab 消息内容与 DB 一致、task prompt 可见 |
| 2 | `e2e_resume_session_no_sub_agents_unchanged` | 回归：无子 agent 的 session 恢复行为完全不变 |
| 3 | `e2e_user_input_to_view_only_returns_hint` | 在 ViewOnly tab 尝试输入 → CLI 端禁用 + 若绕过则 daemon 返回 SessionNotActive → CLI 渲染提示 |

#### 绿 — 实现

编写集成测试 fixture（可用现有 daemon 测试基础设施 + mock LLM provider）。若现有 e2e 框架不支持多 tab 断言，则降级为 daemon 层的帧序列断言 + CLI 层的 mock 帧输入断言（步骤 5b/6a 已覆盖）。

#### 测试 → 类型检查

```bash
cargo test --test '*' resume_sub_agent && cargo clippy -- -D warnings && cargo fmt -- --check
```

#### 提交

`test(e2e): resume session with sub-agent tabs`

---

## Wave 并行策略

### Wave 1：基础类型扩展（3 个并行任务，并行）

- 任务 A: 步骤 1a（visp-proto view_only 字段）
- 任务 B: 步骤 2a + 2b（visp-cli AgentStatus + TabEntry::new_view_only）
- 任务 C: 步骤 3a（visp-core SessionStore trait 扩展）

> 三个任务在不同 crate，互不依赖（步骤 2b 用 `new_view_only` 不动 `new()` 签名，不破坏 visp-cli 其他 28 处调用）。可并行执行。

### Wave 2：DB 实现（1 个任务，串行，依赖 Wave 1 任务 C）

- 任务 D: 步骤 4a（visp-db list_child_sessions + parent_id 索引）

> 依赖 Wave 1 的 trait 定义。单独成 Wave 因后续 daemon 实现强依赖。

### Wave 3：daemon + CLI 并行实现（2 个并行任务，并行，依赖 Wave 1+2）

- 任务 E: 步骤 5a → 5b → 5c（visp-daemon BFS 回放 + SessionNotActive）
- 任务 F: 步骤 6a → 6b → 6c（visp-cli route_frame + UI + Error 帧）

> daemon 和 CLI 都依赖 Wave 1（proto + AgentStatus + trait）和 Wave 2（DB）。但 daemon 与 CLI 之间无依赖（daemon 输出 proto 帧，CLI 消费 proto 帧，协议已定）。可并行。

### Wave 4：端到端验证（1 个任务，串行，依赖 Wave 3）

- 任务 G: 步骤 7a（集成测试）

> 需要 daemon 和 CLI 都完成。串行收尾。

## 依赖关系总览

```
Wave 1 (并行)
  1a (proto)      ─┐
  2a+2b (cli enum)─┼──> Wave 3 (并行)
  3a (core trait) ─┼──>   5a→5b→5c (daemon)
                   │      6a→6b→6c (cli)
       ↓           │           │
Wave 2 (串行)      │           │
  4a (db) ─────────┘           │
                               ↓
                          Wave 4 (串行)
                            7a (e2e)
```

## 测试覆盖汇总

| Wave | 并行数 | 模块/包 | 步骤 | 测试用例数 |
|------|--------|---------|------|-----------|
| 1 | 3 | visp-proto / visp-cli / visp-core | 1a, 2a, 2b, 3a | 2+3+3+3=11 |
| 2 | 1 | visp-db | 4a | 7 |
| 3 | 2 | visp-daemon / visp-cli | 5a, 5b, 5c, 6a, 6b, 6c | 10+5+3+4+4+3=29 |
| 4 | 1 | 集成测试 | 7a | 3 |
| **合计** | — | — | 13 个子步骤 | **50** |

## 备注

### 计划阶段对设计文档的细化/修正

1. **AgentStatus 归属**：设计文档写 visp-core，实际在 visp-cli。本计划按真实位置实施。回头同步修设计文档"改动范围"表。
2. **list_child_sessions 返回类型**：设计文档用 `SessionMeta`（轻量元信息），计划改为 `Vec<Session>`（复用现有类型，trait 一致性更高）。回头同步设计文档。
3. **"非 Running session" 判断标准**：设计文档未明确，计划阶段决策为"daemon 内存活跃 loop 集合不包含该 session_id"，不依赖 DB status。
4. **soft limit 截断策略**：设计文档"BFS 层级优先 + 同层 created_at 倒序"在计划中明确为：BFS 遍历到第 50 个之后停止入队（自然实现层级优先），同层多个时按 created_at 升序访问、超限从队尾丢弃（即保留较早创建的，与设计文档"倒序保留最近"略有出入——计划阶段重新决策为"保留较早创建的直接子节点"，理由：用户更关心初次派生的 agent）。回头同步设计文档。

### 已知限制（沿用设计文档）

1. 同步回放阻塞 1-2 秒（下一期改后台 task）
2. 不区分 Completed/Interrupted/Failed（统一 ViewOnly）
3. 不支持续跑子 agent
4. tab 超 50 时截断

### 执行建议

- Wave 1 的 3 个任务可分别委托 @fixer 并行执行（边界清晰、TDD 步骤明确）
- Wave 2 单任务委托 @fixer
- Wave 3 的 daemon 任务（5a→5b→5c）涉及 service.rs 主流程，建议 orchestrator 自做或委托后严格 review；CLI 任务（6a→6b→6c）的 6b UI 渲染建议委托 @designer
- Wave 4 由 orchestrator 自做整合验证

### 质量门禁（每个 commit 前必须通过）

```bash
cargo test && cargo clippy -- -D warnings && cargo fmt -- --check
```

