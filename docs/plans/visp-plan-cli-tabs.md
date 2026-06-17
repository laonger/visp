# CLI Tab 视图工作计划（v2）

## 前置条件

- 设计文档已 4 轮 oracle 审核通过：`docs/design/visp-design-cli-tabs.md`（405 行）
- 当前分支：`agents`
- proto 已含 `agent_name` 字段（5 处消息）；本计划**不改 proto**
- 主 session_id 通过 `ChatHandle.session_id`（`crates/visp-cli/src/client.rs:19`）获得
- `visp-agent` / `visp-daemon` / `visp-core` / `visp-proto` 全部**不改动**——sub-agent Done 已通过 `spawn_sub_agent` 中的 forwarding task 天然到达 CLI

## TDD 步骤总览

按照 红 → 绿 → 测试 → clippy → fmt → 提交 的循环。共 12 个步骤。每步独立可验证、可单独 commit。

**关键约束**：
- UI 渲染必须使用 `ratatui::widgets::Tabs` widget，不允许自绘 tab bar
- 所有事件路由必须按 `session_id`，不靠 `agent_name`（Done / UserQuery / UsageInfo 三种消息无 agent_name）
- 仅 visp-cli 4 个文件改动（app.rs / event.rs / ui.rs / main.rs）
- 状态守卫：`Running → Done` 是 Done 唯一允许的状态覆盖

---

## Step 1：定义 AgentStatus / TabEntry / TabBar 数据结构

### 测试（红）

在 `crates/visp-cli/src/app.rs` 内 `#[cfg(test)] mod tests`：

- `test_agent_status_default_is_running` — TabEntry 默认状态为 Running
- `test_tab_entry_new_with_session_and_name` — 构造函数正确设置 session_id 和 agent_name
- `test_tab_entry_initial_empty` — frames、messages、streaming_text 初始为空，rendered_up_to == 0
- `test_tab_entry_default_per_tab_state` — generating == false，pending_usage == None，scroll == 0
- `test_tabbar_new_creates_default_tab` — TabBar::new(session_id) 创建一个 tab，agent_name == "default"，active == 0
- `test_tabbar_insert_sub_agent_at_index_1` — 插入子 agent 后位于 tabs[1]
- `test_tabbar_insert_two_sub_agents_newer_first` — 插入两个子 agent 后，最新在 tabs[1]，旧的在 tabs[2]
- `test_tabbar_insert_does_not_change_active` — active==0 时插入不改 active
- `test_tabbar_insert_when_active_geq_1_shifts_active_plus_1` — active>=1 时插入，active 自动 +1（保持指向同一 tab）
- `test_tabbar_find_index_by_session` — find_index_by_session(sid) 返回正确索引
- `test_tabbar_find_or_insert_creates_when_missing` — find_or_insert(sid, name) 创建并返回新索引
- `test_tabbar_find_or_insert_returns_existing` — 已存在 session_id 时返回原索引

### 实现（绿）

- `AgentStatus` 枚举：`Running` / `Done` / `Error`
- `TabEntry` 结构（含 per-tab 状态字段，注意类型与现有代码一致）：
  - `session_id: String`
  - `agent_name: String`
  - `status: AgentStatus`
  - `frames: Vec<ServerMessage>`（CLI 接收的 proto 类型）
  - `messages: Vec<ChatLine>`（**注意：是 ChatLine 不是 MessageLine**）
  - `rendered_up_to: usize`
  - `streaming_text: String`（**注意：与 app.rs:601 现有命名一致**）
  - `generating: bool`
  - `pending_usage: Option<(u32, u32, u32, u32, u32)>`（5 元组，含 tool_calls；与 app.rs:627 现有类型一致）
  - `scroll: usize`
- `TabBar` 结构：
  - `tabs: Vec<TabEntry>`
  - `active: usize`
  - `tab_page: usize`
  - 方法：`new(main_session_id)` / `insert_sub_agent(sid, name)` / `find_index_by_session(sid)` / `find_or_insert(sid, name)`

### 验证

`cargo test -p visp-cli` → `cargo clippy -p visp-cli -- -D warnings` → `cargo fmt -- --check`

### 提交

`feat(cli): add TabEntry/AgentStatus/TabBar data structures`

---

## Step 2：增量渲染（TabEntry::render_pending）+ Done/Error 状态守卫

### 测试（红）

- `test_render_pending_empty_frames_noop`
- `test_render_pending_text_delta_appends_streaming`
- `test_render_pending_tool_call_flushes_streaming`
- `test_render_pending_tool_result_finds_tool_name_within_tab`
- `test_render_pending_idempotent`
- `test_render_pending_increments_rendered_up_to`
- `test_render_pending_done_running_to_done`
- `test_render_pending_done_does_not_overwrite_error` — **状态守卫**
- `test_render_pending_done_does_not_overwrite_done` — **重复 Done 幂等**
- `test_render_pending_error_event_updates_status_to_error`
- `test_render_pending_error_then_done_status_remains_error` — cancel 场景
- `test_render_pending_done_clears_generating` — 独立于守卫，generating 总是清零

### 实现（绿）

在 `TabEntry` 上实现 `render_pending(&mut self)`：

- 遍历 `frames[rendered_up_to..]`，逐个 dispatch
- 流式累积/flush：操作 `self.streaming_text` 和 `self.messages`（沿用现有 event.rs 的逻辑）
- ToolResult 的 tool_name 查找限定在 `self.messages` 内
- **Done 状态守卫**：仅 `status == Running` 时才设为 Done；其他状态保持不变。`generating = false` 总是执行（独立于守卫）
- Error 处理：`status = Error`；`generating = false`
- pending_usage flush 显示 L1（详细 token 三层路由交给 Step 8）

### 验证

`cargo test -p visp-cli` → clippy → fmt

### 提交

`feat(cli): implement TabEntry::render_pending with Done/Error status guard`

---

## Step 3：移除 [sub:] 前缀机制

### 测试（红）

- `test_sub_agent_text_does_not_get_prefix` — sub agent 的 TextDelta 内容不含 `[sub: ...]`
- 删除原有 `test_*_sub_prefix_*` 系列测试（如有）

### 实现（绿）

- 删除 `AppState::sub_prefix_shown` 字段
- 删除 `AppState::maybe_add_sub_agent_prefix` 方法
- 删除 event.rs 中所有调用点
- 删除/改造相关旧测试

### 验证

`cargo test -p visp-cli` → clippy → fmt

### 提交

`refactor(cli): remove [sub:] prefix mechanism in favor of tabs`

---

## Step 4：AppState 改造（messages → tabs，per-tab 状态迁移）

最大改动 step。AppState 从单 messages 流改为 TabBar 结构；`generating` 和 `pending_usage` 从全局迁移到 TabEntry；新增 `current_request_usage` 全局字段。

### 测试（红）

- `test_appstate_new_has_one_default_tab` — 新建 AppState 后 tab_bar.tabs.len() == 1，agent_name == "default"
- `test_appstate_main_session_id_matches_default_tab` — main_session_id == tab_bar.tabs[0].session_id
- `test_appstate_active_messages_returns_default_initially`
- `test_appstate_active_streaming_text_returns_default_initially`
- `test_appstate_keeps_global_current_request_id`
- `test_appstate_keeps_global_stale_done_expected`
- `test_appstate_keeps_global_total_tokens` — 4 个 `total_*_tokens`
- `test_appstate_has_global_current_request_usage` — 新字段，初值 (0,0,0,0)
- `test_appstate_active_tab_generating_initially_false`
- `test_appstate_set_default_tab_generating` — 通过 `active_tab_mut` 设置，能从 active_tab 读到

### 实现（绿）

- AppState 字段改造：
  - **移除**：`messages: Vec<ChatLine>`、`streaming_text: String`、`generating: bool`、`pending_usage: Option<(u32,u32,u32,u32,u32)>`
  - **新增**：`tab_bar: TabBar`、`main_session_id: String`、`current_request_usage: (u32, u32, u32, u32)`
  - **保留**：`current_request_id`、`stale_done_expected`、4 个 `total_*_tokens`
- 新增方法：
  - `active_tab(&self) -> &TabEntry`
  - `active_tab_mut(&mut self) -> &mut TabEntry`
  - `active_messages(&self) -> &[ChatLine]`
- `AppState::new(main_session_id)` 用 `TabBar::new(main_session_id)` 初始化
- `main.rs` 中 AppState 构造点接入 `ChatHandle.session_id`
- 修复所有下游 caller 编译错误：把 `app.messages` / `app.streaming_text` / `app.generating` / `app.pending_usage` 改为代理到 active tab 的访问

### 验证

`cargo test -p visp-cli` → clippy → fmt

### 提交

`refactor(cli): replace AppState.messages with TabBar; move generating/pending_usage per-tab`

---

## Step 5：消息添加 API 重构（add_message_to_session 等）

按设计文档约定，区分**类型 A**（用户输入/本地反馈，永远写 default）和**类型 B**（agent 事件，按 session_id 路由）。

### 测试（红）

- `test_add_message_writes_to_default_tab` — `add_message(...)` 永远写 default tab
- `test_add_message_to_session_routes_to_correct_tab` — `add_message_to_session(sid, ...)` 按 sid 路由
- `test_add_message_to_session_unknown_falls_back_to_default` — 未知 sid 时回退 default
- `test_add_tool_line_routes_by_session_id` — `add_tool_line(sid, ...)` 按 sid 路由
- `test_update_thinking_routes_by_session_id`
- `test_append_streaming_routes_by_session_id`
- `test_flush_streaming_routes_by_session_id`

### 实现（绿）

- `add_message(...)` 保持原签名 → 内部调 `tab_bar.tabs[0]`（永远 default）
- 新增 `add_message_to_session(session_id, ...)`
- `add_tool_line(session_id, ...)` / `update_thinking(session_id, ...)` / `append_streaming(session_id, ...)` / `flush_streaming(session_id, ...)` 均加 `session_id: &str` 参数
- `insert_tool_result` 标记为待删除（路由逻辑收敛到 render_pending 后此方法不再需要）
- 修复所有 caller：本地反馈用 `add_message`；agent 事件用 `add_message_to_session(session_id, ...)`

### 验证

`cargo test -p visp-cli` → clippy → fmt

### 提交

`refactor(cli): split add_message API into default vs session-routed variants`

---

## Step 6：handle_grpc_message 改造为 route_frame（按 session_id 路由）

### 测试（红）

- `test_route_frame_text_delta_to_correct_tab` — 不同 session_id 的 TextDelta 进入不同 tab.frames
- `test_route_frame_tool_call_to_correct_tab`
- `test_route_frame_tool_result_to_correct_tab`
- `test_route_frame_done_to_correct_tab` — Done 仅靠 session_id 路由（无 agent_name）
- `test_route_frame_user_query_to_default` — UserQuery 路由到 default（main_session_id 匹配）
- `test_route_frame_unknown_session_creates_new_tab` — 新 session_id 触发 find_or_insert，新 tab 默认状态 Running
- `test_route_frame_active_tab_renders_immediately` — 路由到 active tab 后立即调用 render_pending
- `test_route_frame_inactive_tab_accumulates_only` — 路由到非 active tab 仅累积 frames，messages 不变
- `test_route_frame_status_update_routes_by_session_id` — StatusUpdate 按 session_id 路由

### 实现（绿）

- `handle_grpc_message` 改造为：
  1. 从 ServerMessage payload 提取 session_id（每种 payload 的字段位置在 proto 中已确定）
  2. agent_name 仅作为 tab 标题来源（首次创建 tab 时使用）
  3. `app.route_frame(frame)`：
     - `let idx = tab_bar.find_or_insert(session_id, agent_name_or_default)`
     - `tab_bar.tabs[idx].frames.push(frame)`
     - 若 `idx == active`，立即调 `tabs[idx].render_pending()`
- 删除原本直接操作 `app.messages` / `app.append_streaming` 等的代码（逻辑已迁移到 `TabEntry::render_pending`）

### 验证

`cargo test -p visp-cli` → clippy → fmt

### 提交

`feat(cli): route grpc events through TabBar by session_id`

---

## Step 7：Tab 切换快捷键（Alt+←/→ 循环）

### 测试（红）

- `test_alt_right_advances_active_tab`
- `test_alt_right_at_last_wraps_to_zero` — 末尾 → 跳回 0（循环）
- `test_alt_left_at_zero_wraps_to_last` — 第 0 个 ← 跳到末尾（循环）
- `test_activate_tab_renders_pending_frames` — 切换到有累积 frames 的 tab 后，messages 立即渲染完毕
- `test_activate_tab_calls_ensure_active_visible` — 激活非可见 tab 时自动翻页（依赖 Step 9 的方法，此 step 可只声明 stub）
- `test_alt_arrow_only_when_no_modifier_conflict` — Shift 同按时不应触发普通切换

### 实现（绿）

- `TabBar::activate_next()` / `activate_prev()`（循环）
- `TabBar::activate(index)`：调 `tabs[index].render_pending()` 并预留对 `ensure_active_visible()` 的调用（Step 9 实现）
- `event.rs::handle_key_event` 处理 `Alt+Left` / `Alt+Right`（KeyCode::Left/Right + KeyModifiers::ALT）
- 切换 tab 时全局 scroll_following = true（设计约定）

### 验证

`cargo test -p visp-cli` → clippy → fmt

### 提交

`feat(cli): add Alt+arrow tab switching with circular navigation`

---

## Step 8：Token 三层路由（L1 / L2 / L3）

### 测试（红）

- `test_usage_routed_to_tab_pending_usage` — UsageInfo 按 session_id 路由，写入 `tab.pending_usage`（L1）
- `test_usage_accumulates_to_current_request_usage` — UsageInfo 同时累加到 AppState.current_request_usage（L2）
- `test_done_default_tab_displays_l2_total` — default tab Done 时显示 L2（current_request_usage 累计）
- `test_done_default_tab_accumulates_to_total_tokens` — default tab Done 后，4 个 total_*_tokens 累加 L2 数值（L3）
- `test_done_default_tab_clears_current_request_usage` — default Done 后 current_request_usage 清零
- `test_done_sub_tab_displays_l1_only` — sub tab Done 仅显示 L1（pending_usage），不累加到 L2/L3
- `test_done_sub_tab_does_not_clear_current_request_usage` — sub Done 不影响 L2
- `test_user_input_clears_current_request_usage` — 用户新输入时清零 L2
- `test_done_status_guard_does_not_apply_token_when_overwritten` — 状态守卫拒绝 Done 覆盖时（如已 Error），不再做 token 显示和累加

### 实现（绿）

- 在 `route_frame` 中处理 `UsageInfo` payload：
  - 找到对应 tab（按 session_id），更新 `tab.pending_usage`
  - 同时累加到 `app.current_request_usage`（L2）
- 在 `TabEntry::render_pending` 处理 Done 时（仅当状态守卫允许 Running→Done）：
  - 若是 default tab（caller 传入 `is_default: bool`）：
    - 显示 token 行用 L2（app.current_request_usage）
    - 累加到 L3（4 个 total_*_tokens）
    - 清零 L2
  - 否则（sub tab）：
    - 显示 token 行用 L1（self.pending_usage）
    - 不动 L2/L3
- 因 `render_pending` 现需访问 AppState 全局字段，重构方案：
  - 不让 TabEntry 持有 AppState 引用
  - 改为 AppState 上的 `render_active_tab(&mut self)` 方法，在其中协调 tab 渲染 + token 路由
  - 或：`render_pending` 接受 `(is_default, current_request_usage: &mut (...), totals: &mut (...))` 参数
  - 推荐第二种：保持 TabEntry 数据中心特性，AppState 作 orchestrator
- 在 `event.rs` 用户提交输入处：清零 `current_request_usage`

### 验证

`cargo test -p visp-cli` → clippy → fmt

### 提交

`feat(cli): implement three-layer token tracking (L1 tab / L2 request / L3 session)`

---

## Step 9：Tab Bar 渲染（ratatui Tabs widget + 状态符号 + 颜色）

### 测试（红）

- `test_tab_label_running_shows_yellow_arrow` — Running 返回的 Line 第一个 Span 内容是 `▶ ` 且 style 含黄色
- `test_tab_label_done_shows_green_check` — Done 返回 Line 第一个 Span 是 `✓ ` 绿色
- `test_tab_label_error_shows_red_cross` — Error 返回 Line 第一个 Span 是 `✗ ` 红色
- `test_tab_label_contains_agent_name` — Line 第二个 Span 是 agent_name 文本
- `test_default_tab_also_shows_status` — default tab 也带状态符号

注：测试只覆盖纯函数（Line 生成），不测试 ratatui 实际 paint。激活高亮交给 ratatui 默认 `highlight_style`（reversed）。

### 实现（绿）

**强制使用 `ratatui::widgets::Tabs`**：

- `ui.rs::tab_label_line(tab: &TabEntry) -> Line<'static>` — 纯函数，返回 `Line` 含 status span + name span
- `ui.rs::render_tab_bar(f, area, tab_bar, term_width)`：
  - `Layout::horizontal([Fill(1), Length(8)])` 把 area 分成 tabs 区和页码区
  - 暂时显示所有 sub（翻页交给 Step 10）
  - 构造 titles：`vec![tab_label_line(&tabs[0]), ...subs.map(tab_label_line)]`
  - `Tabs::new(titles).select(active).highlight_style(Style::new().reversed()).divider("|").padding(" ", " ")`
  - 页码 Paragraph 渲染（仅当 pages > 1）
- 主 UI layout 顶部预留 1 行给 tab bar
- chat area 数据源改为 `tab_bar.active_tab().messages`
- input box 仅 active==0 时启用；sub tab 上禁用并显示提示

### 验证

`cargo test -p visp-cli` → clippy → fmt

视觉验证：启动 CLI，单 default tab 显示 `▶ default`（黄色），状态符号 + 颜色正确，激活反色。

### 提交

`feat(cli): render tab bar via ratatui Tabs widget with status colors`

---

## Step 10：横向翻页（ratatui Tabs widget 外层切片）

### 测试（红）

- `test_layout_pages_default_always_first` — default tab 永远在第 0 页且固定占第一位
- `test_layout_pages_single_page_when_fits`
- `test_layout_pages_multi_page_when_overflow` — 超过宽度时分多页，正确分割索引
- `test_layout_pages_each_page_includes_default` — 每页可见区域 = default + 当前页 sub 范围
- `test_alt_shift_right_advances_page`
- `test_alt_shift_left_at_zero_stops` — 边界停止
- `test_alt_shift_right_at_last_stops`
- `test_alt_shift_does_not_change_active_tab` — 翻页不切换激活
- `test_select_idx_in_visible_when_active_in_page` — 激活 tab 在当前页时，传给 Tabs::select 的索引正确（default=0，sub=1+offset）
- `test_active_tab_change_auto_scrolls_to_visible_page` — 激活非可见 tab 时，tab_page 自动调到含 active 的那一页

### 实现（绿）

**ratatui Tabs widget 翻页通过外层切片 + select 调整**：

- `TabBar::layout_pages(&self, term_width: u16) -> Vec<Range<usize>>` —— 返回每页的 sub tab 索引范围（不含 default）
- `TabBar::current_page_subs(&self) -> Range<usize>` —— 当前页对应的 sub 范围
- `TabBar::select_idx_for_current_page(&self) -> Option<usize>` —— 激活 tab 在当前页可见 titles 中的索引
- `TabBar::next_page()` / `prev_page()` —— 边界停止
- `TabBar::ensure_active_visible()` —— 切激活时调用，确保 tab_page 包含 active
- `event.rs` Alt+Shift+Left/Right 处理
- `render_tab_bar` 改造：
  - 用 `current_page_subs()` 切片 sub tabs
  - 拼出 visible_titles
  - `Tabs::new(visible_titles).select(select_idx_for_current_page())`
  - 页码 `[N/M]` Paragraph 渲染在右侧（仅当总页数 > 1）

### 验证

`cargo test -p visp-cli` → clippy → fmt

视觉验证：手动 spawn 多个 subagent 触发分页，Alt+Shift+→ 翻页，default 始终可见。

### 提交

`feat(cli): paginate tab bar via Tabs widget slicing`

---

## Step 11：Ctrl+W 关闭子 agent tab（受状态限制）

### 测试（红）

- `test_ctrl_w_on_default_is_noop` — active == 0 时 Ctrl+W 不变
- `test_ctrl_w_on_running_sub_is_noop` — sub tab 但 status == Running 时 Ctrl+W 不关（仅 Done/Error 允许关闭）
- `test_ctrl_w_on_done_sub_removes_tab` — Done 状态可关
- `test_ctrl_w_on_error_sub_removes_tab` — Error 状态可关
- `test_ctrl_w_activates_previous_tab` — 关闭后激活索引变为 active - 1
- `test_ctrl_w_at_last_sub_falls_back_to_default` — 关闭最后一个 sub 后激活回 default
- `test_ctrl_w_renders_pending_for_new_active`
- `test_ctrl_w_adjusts_tab_page` — 关闭后 ensure_active_visible
- `test_ctrl_w_closed_session_can_reopen_on_new_event` — 关掉的 sub 后续若再发事件，会重新创建 tab

### 实现（绿）

- `TabBar::close_active(&mut self) -> bool`：
  1. 若 `active == 0`：return false
  2. 若 `tabs[active].status == Running`：return false（**状态限制**）
  3. `tabs.remove(active)`
  4. `active = active - 1`
  5. `tabs[active].render_pending()`
  6. `ensure_active_visible()`
  7. return true
- `event.rs::handle_key_event` 处理 Ctrl+W → 调 `app.tab_bar.close_active()` → 触发 `app.needs_render = true`
- 不影响 daemon：close 仅是 UI 移除，sub-agent 仍在 daemon 端运行

### 验证

`cargo test -p visp-cli` → clippy → fmt

视觉验证：spawn sub-agent → 等其 Done → 切到 sub tab → Ctrl+W → tab 消失，回到 default；Running 状态下 Ctrl+W 无反应；default 上 Ctrl+W 无反应。

### 提交

`feat(cli): close sub-agent tab with Ctrl+W (only when Done/Error)`

---

## Step 12：端到端联调与回归

### 测试（红）

- `test_e2e_spawn_subagent_creates_tab` — 集成：模拟 grpc 流，spawn 后第二个 tab 出现
- `test_e2e_subagent_done_changes_status_color` — 模拟 Done 后 status 变 Done
- `test_e2e_subagent_error_changes_status_color` — 模拟 Error 后 status 变 Error
- `test_e2e_subagent_inactive_does_not_pollute_default` — sub agent 内容不进入 default tab
- `test_e2e_switch_to_sub_renders_accumulated` — 切到 sub 看到累积内容
- `test_e2e_token_l1_displayed_per_sub_done`
- `test_e2e_token_l2_l3_only_on_default_done`
- `test_e2e_user_input_disabled_on_sub_tab`

### 实现（绿）

无新代码，仅补充集成测试覆盖 Step 1-11 已实现的行为。

### 验证

`cargo test` → `cargo clippy -- -D warnings` → `cargo fmt -- --check`

全量回归：现有 650+ 测试全部通过；新增的 tab 测试也全部通过。

### 提交

`test(cli): add end-to-end tab integration tests`

---

## 总验证标准

完成所有 12 步后做端到端验收：

1. ✅ 启动 CLI，只有 default 时 tab bar 显示 `▶ default`（基于 ratatui Tabs widget）
2. ✅ 触发 spawn_subagent 后，tab bar 在 index=1 出现新 tab，不自动激活
3. ✅ 不切换到子 agent tab 时，子 agent 内容不出现在主聊天区
4. ✅ 切换到子 agent tab，能看到所有累积内容（lazy 渲染生效，rendered_up_to 正确）
5. ✅ 再切回 default，default 内容完整保留；scroll_following 重置为 true
6. ✅ 子 agent 完成后状态变绿（✓）；root agent Done 也正确显示
7. ✅ 子 agent 出错后状态变红（✗）；cancel 场景下 Error 不被后续 Done 覆盖
8. ✅ 创建多个子 agent 触发分页后，Alt+Shift+方向键能翻页（边界停止）
9. ✅ Alt+方向键能循环切激活 tab（首尾循环）
10. ✅ 不同 tab 的 frames 独立累积，messages 独立缓存；scroll 全局
11. ✅ 不再出现 `[sub: {agent_name}]` 前缀
12. ✅ Ctrl+W 仅在 sub tab 且状态为 Done/Error 时关闭；其他情况 noop
13. ✅ Token 三层显示正确：sub tab Done 显示 L1；default Done 显示 L2 + 累加 L3 + 清零 L2；新 user input 清零 L2
14. ✅ Tab bar 实现确实使用 `ratatui::widgets::Tabs`（代码 review 确认）
15. ✅ Input box 仅在 active==0 启用；sub tab 上禁用并提示
16. ✅ 全量测试通过：`cargo test`
17. ✅ Clippy 零警告：`cargo clippy -- -D warnings`
18. ✅ 格式正确：`cargo fmt -- --check`

## 风险与回退

每个 step 独立 commit，出现问题可回退到上一个绿色状态。

**最大风险点**：

- **Step 4（AppState 改造）**：影响面最广，所有 caller 需同步改。先编译过再修测试。
- **Step 6（route_frame）**：proto 字段提取需对每种 payload case-by-case 处理。重点测试 Done/UserQuery/UsageInfo 三种无 agent_name 的消息。
- **Step 8（token 三层）**：render_pending 与 AppState 全局字段交互复杂；推荐参数传入方式而非引用持有，避免借用冲突。

## 委托与执行节奏

按 cost-aware-delegation 规则，所有 step 委托 @fixer 执行；orchestrator 仅做编排、合并、验证。

### 依赖图（用于决策何处可并行）

```
Step 1 ─┬─ Step 2 ─┬─ Step 4 ─ Step 5 ─ Step 6 ─┬─ Step 7 ─┐
        │          │                              │           ├─ Step 8 ─┐
        │          │                              │           │           ├─ Step 11 ─ Step 12
        └─ Step 3 ─┘                              └─ Step 9 ──┴─ Step 10 ─┘
```

强依赖：编译类型/方法/字段 → 后置 step 必须等前置完成。

### 执行节奏（顺序为主 + 局部并行）

**Wave A — Step 1 ‖ Step 3（并行）**
- 两个 fixer 同时启动
- Step 1：在 app.rs 新增 `AgentStatus` / `TabEntry` / `TabBar` 结构（纯新增，不动现有逻辑）
- Step 3：删除 `sub_prefix_shown` 字段、`maybe_add_sub_agent_prefix` 方法及其调用点（纯删除）
- 二者无重叠：Step 1 是 append 新结构，Step 3 是删除旧字段，git 自动合并干净
- orchestrator：等两个 fixer 都完成 → 合并 → 跑 `cargo test/clippy/fmt` → 双 commit

**Wave B — Step 2（顺序）**
- 单 fixer，依赖 Step 1 的 TabEntry
- 实现 `render_pending` + 状态守卫
- 编译 + 测试通过后 commit

**Wave C — Step 4 → Step 5 → Step 6（顺序，每步单 fixer）**
- 这是最复杂的链路，强依赖：Step 4 改 AppState 字段 → Step 5 用 active_tab_mut → Step 6 调 add_message_to_session
- 不能并行：同时改 app.rs / event.rs 会撞冲突
- 每步独立 commit，方便回退

**Wave D — Step 7 ‖ Step 9（并行）**
- 两个 fixer 同时启动
- Step 7：event.rs 加 Alt+Left/Right 处理 + TabBar::activate_next/prev（动 event.rs + app.rs）
- Step 9：ui.rs 新增 `tab_label_line` + `render_tab_bar`（仅动 ui.rs + main UI layout 1 行）
- 文件重叠仅在 app.rs 上：Step 7 加方法，Step 9 不动 app.rs；冲突可控
- orchestrator 合并后双 commit

**Wave E — Step 8（顺序）**
- 单 fixer，依赖 Step 6（route_frame）+ Step 7（activate）
- Token 三层路由：UsageInfo 写 tab + 累加 L2；Done 时按 default/sub 分流
- 涉及 render_pending 签名修改 + 全局字段 → 借用冲突需谨慎，单 fixer 处理更稳

**Wave F — Step 10（顺序）**
- 单 fixer，依赖 Step 9（render_tab_bar）
- 翻页逻辑（layout_pages / current_page_subs / select_idx）+ Alt+Shift 处理

**Wave G — Step 11 ‖ Step 12 部分（并行）**
- Step 11：单 fixer，依赖 Step 10 的 ensure_active_visible
- Step 12：写端到端测试，可与 Step 11 同时启动（测试只读 Step 1-10 已实现的 API，与 Step 11 互不干扰）
- 两个 fixer 同时跑：Step 11 改 close_active 逻辑，Step 12 加 e2e 测试文件

### 并行收益预估

|  | 顺序串行 | 顺序+局部并行 | 节省 |
|---|---|---|---|
| Wave A | 2 单元 | 1 单元 | 1 |
| Wave D | 2 单元 | 1 单元 | 1 |
| Wave G | 2 单元 | 1 单元 | 1 |
| **合计** | **12 单元** | **9 单元** | **~25%** |

并行收益有上限——12 个 step 中只有 3 对真正独立。强行并行其它 step 会因文件冲突产生返工成本，净损失。

### Orchestrator 编排职责

每个 wave：
1. 同时 launch 多个 @fixer（带完整 step 规格）
2. 等所有返回
3. 合并（如有冲突，先看 fixer 是否各自动了独立部分；若撞同一函数则 oracle 评审一次）
4. 跑 `cargo test -p visp-cli && cargo clippy -p visp-cli -- -D warnings && cargo fmt -- --check`
5. 不通过 → 让对应 fixer 修复
6. 通过 → 各 step 独立 commit（保持回退粒度）

每组完成后 orchestrator 验证（cargo test/clippy/fmt），不通过让 @fixer 修复。
