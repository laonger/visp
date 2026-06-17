# CLI Tab 视图设计

## 目标

在 CLI 顶部增加一排 tab，每个 tab 对应一个 agent（主 agent 或子 agent）的对话内容。不同 agent 的内容相互隔离，用户通过 tab 切换查看。

## 核心需求

1. **懒渲染**：未激活的 tab 不进行事件 → Line 的转换；切换到该 tab 时增量渲染（已渲染过的 Line 不重复渲染）
2. **顺序**：tabs[0] = default（主 agent，永远第一），新创建的子 agent 插入到 index = 1（越新越靠近 default）
3. **横向翻页**：tab 总宽度超过屏幕时分页显示，default 永远固定在最左
4. **状态标记**：每个 tab 显示运行状态（运行中 / Done / Error），用颜色 + 符号
5. **per-tab 状态**：每个 tab 独立维护原始事件、已渲染 Line、滚动位置

## 移除项

引入 tab 后，原有的 `[sub: {agent_name}]` 前缀机制不再需要。删除：
- `AppState::sub_prefix_shown`
- `AppState::maybe_add_sub_agent_prefix`
- `event.rs::handle_grpc_message` 中所有调用点

## 数据结构

### TabEntry

每个 tab 对应一个 agent，封装：

- **身份**：`session_id`、`agent_name`
- **原始事件缓存**：`frames: Vec<ServerMessage>`（永远累积，不丢弃）
  - 注意：CLI 通过 gRPC 接收的是 `visp_proto::visp::ServerMessage`（proto 类型），不是 `AgentEventFrame`。`AgentEventFrame` 是 daemon/orchestrator 进程内部类型，不跨进程。如果将来需要避免在 CLI 直接持有 proto 类型，可定义一个 CLI 内部事件枚举包装 ServerMessage。本次实现直接用 `ServerMessage`。
- **已渲染 Line**：`messages: Vec<ChatLine>`（沿用现有 `ChatLine` 结构，含 line_type / call_id）
- **渲染游标**：`rendered_up_to: usize`（frames 中已渲染到的索引）
- **流式累积**：`streaming_text: String`（流式 TextDelta 暂存，flush 时合并为一条 message。沿用 AppState 中现有字段名 `streaming_text`）
- **状态**：`status: AgentStatus`（Running / Done / Error）

注意：`scroll_state` 和 `scroll_following` 不放在 TabEntry 中，仍然全局唯一（保留在 AppState 上），切换 tab 时不保留之前的滚动位置——切换后默认滚到新 tab 的底部（`scroll_following = true`）。

注意：streaming_text 是 per-tab 的——子 agent 的流式输出不会和主 agent 的混淆。

#### 原 AppState 流式方法的迁移

当前 `AppState` 上的 `streaming_text: String`、`append_streaming(...)`、`flush_streaming(...)` 全部按以下策略迁移：

- **字段**：从 `AppState` 移除，迁入 `TabEntry`
- **方法签名扩展**：`AppState::append_streaming` / `flush_streaming` 改为接收 `session_id` 参数，根据 session_id 路由到对应的 TabEntry，再调用其同名方法
- **路由失败**：如果 session_id 无法匹配任何 tab（理论不应发生，因为 frame 到达前 route_frame 会先 find_or_insert），按错误处理，不静默写到 active_tab

**理由**：事件路由应该是 session-aware 而非 active-tab-aware。子 agent 后台输出 TextDelta 时，用户可能正停留在 default tab——必须按 frame 的 session_id 写入正确的 sub-agent tab，而不是当前激活 tab。

#### 其他消息修改方法的迁移

`handle_grpc_message` 中除流式三件套外，还会调用 `add_message` / `add_tool_line` / `update_thinking` 等方法修改消息流。这些方法分两类处理：

**类型 A：本地 UI 反馈**（命令回显、错误提示、用户输入回显、`/list` 输出等）

- `add_message(line_type, content)` 签名**保持不变**，行为改为始终写入 `tabs[0]`（default tab）
- 理由：用户始终在 default tab 输入命令；这些消息属于"主对话流"，本来就归 default
- 调用点（event.rs 中各 `/sessions`、`/list`、`/new` 等命令处理处，以及 user 输入回显处）不需要修改

**类型 B：来自 agent 事件的消息修改**（StatusUpdate、Error、ToolCall、ToolResult、ThinkingBlock）

- 新增方法 `add_message_to_session(session_id, line_type, content)`：按 session_id 路由
- `add_tool_line(...)` 改签名加 `session_id`：路由到对应 tab
- `update_thinking(...)` 改签名加 `session_id`：路由到对应 tab
- 路由失败时返回错误，不静默写到 active_tab

**`insert_tool_result` 不迁移**：当前代码无调用点，迁移阶段标注为待删除。

**统一原则**：所有"由 agent 事件触发的消息流修改"都走 session-aware 路由；"由本地 UI 触发的消息"继续走 default tab。

### AgentStatus

三态枚举：
- `Running`：黄色 ▶
- `Done`：绿色 ✓
- `Error`：红色 ✗

### TabBar

`AppState` 中替换原有 `messages` 字段，新增：
- `tabs: Vec<TabEntry>`（tabs[0] 永远是 default）
- `active_tab: usize`
- `tab_page: usize`（横向翻页索引）

### 请求生命周期状态字段的归属

当前 `AppState` 上的请求生命周期相关字段，按以下规则分配：

| 字段 | 归属 | 理由 |
|------|------|------|
| `generating: bool` | **per-tab**（移入 `TabEntry`） | 每个 agent 各自有 generating 状态；UI spinner 渲染时只看 `tabs[active_tab].generating` |
| `pending_usage: Option<(u32, u32, u32, u32, u32)>` | **per-tab**（移入 `TabEntry`） | 每个 agent 各自暂存自己的 UsageInfo，等自己的 Done 时追加到自己 tab 的最后一行 |
| `current_request_id: Option<String>` | **保留全局** | 仅 default tab 的 cancel 流程使用；用户的 cancel 请求只对主 agent 发起，sub-agent 无 cancel 概念 |
| `stale_done_expected: bool` | **保留全局** | 同 `current_request_id`，仅 default 的 cancel 流程用 |
| `current_request_usage: (u32, u32, u32, u32)` | **新增全局**（仅 default 语义有效） | 记录"本次用户请求"从开始到结束累积的 token 用量（default + 所有 sub-agent）。语义为"该次请求的全部 token 用量"。4 元组对应 input / output / cache_creation_input / cache_read_input；tool_calls 计数不累加（无聚合意义）。 |
| `total_input_tokens` / `total_output_tokens` / `total_cache_creation_input_tokens` / `total_cache_read_input_tokens` | **保留全局** | session 级累积，从 session 开始算起；状态栏显示。 |

### Daemon 端不需要改动：sub-agent Done 已天然到达 CLI

**关键事实**：当前 `crates/visp-agent/src/orchestrator.rs::spawn_sub_agent`（第 544-569 行）已经为每个 sub-agent 创建了 forwarding task：

```
tokio::spawn(async move {
    while let Some(event) = agent_rx.recv().await {
        grpc_tx.send(AgentEventFrame {
            event,
            session_id: <sub_session_id>,
            agent_name: <sub_agent_name>,
            parent_session_id, parent_session_name,
        }).await
    }
});
```

`agent_tx` 收到的**所有 `AgentEvent`（含 Done）都会被转发**到 grpc_tx → CLI。也就是说，sub-agent 的 Done 事件早已到达 CLI；当前 CLI"看不到效果"的原因是 Done 处理器（`event.rs::handle_grpc_message` 的 Done 分支，第 809 行附近）操作的是 AppState 的全局 `generating`/`pending_usage`，而非按 session_id 路由到对应 tab。

**因此**：
- daemon / proto / visp-core / visp-agent 全部**不需要改动**
- CLI 端只需把 Done 处理器改为 session-aware 路由即可

**已知 quirk**：`handle_done` 在 root agent 分支（第 656-666 行）会**再发一次** Done 到 grpc_tx，与 forwarding task 的 Done 形成重复。当前 CLI 处理无害（重复 Done 在第二次时 `pending_usage` 已被消耗，无操作）。本设计**沿用**这一容忍策略：CLI 收到 Done 时，若 tab 已是 `Done` 状态，直接忽略（幂等）。

### Done 与 Error 的状态覆盖守卫（cancel 场景）

**问题**：用户 Cancel 时，agent loop 发 `Error { Cancelled }` → orchestrator 转 `handle_done` → forwarding task 又发一次 `Done`。CLI 收到顺序：

1. Error → `tab.status = Error`（红 ✗）
2. Done → 若无守卫，会覆盖为 `Done`（绿 ✓） ← **bug**

**守卫规则**：CLI 处理 Done 时，`tab.status` 一旦已是 `Done` 或 `Error` 就**不再覆盖**：

```
fn on_done(tab):
    if tab.status == Running:
        tab.status = Done
    # else: 保持现状（Error 不会被 Done 覆盖；Done 重复不变）
    tab.generating = false
    flush tab.pending_usage 显示 L1
    # default tab 额外处理 L2/L3（见 token 三层）
```

`stale_done_expected` 全局字段（仅 default cancel 用）保留原语义；sub-agent 的 cancel 不需要等效字段，因为状态守卫已覆盖该场景。

### token 用量的三层处理

收到 `UsageInfo` 时（按 session_id 路由）：

1. **per-tab 暂存**：写入对应 TabEntry 的 `pending_usage`
2. **同时累加到 default 的本次请求总用量**：把 4 个 token 数加到 AppState 的 `current_request_usage`（无论 UsageInfo 来自哪个 agent，都汇聚到 default 的"本次请求"语义下）
3. **不立即累加 session 级**：等 Done 时再累加

收到 `Done` 时（按 session_id 路由）：

1. 找到对应 tab
2. 若 `tab.pending_usage.is_some()`：取出，在 `tab.messages` 最后一行追加该 agent 本次 token 显示（限定在该 tab 内）
3. 标 `tab.status = Done`、`tab.generating = false`、清空 `tab.pending_usage`
4. **若是 default tab 的 Done**（即 root agent 完成，本次用户请求结束）：
   - 在 default tab 最后一行追加"本次请求总用量"（用 `current_request_usage`，含所有 sub-agent 累加值）
   - 把 `current_request_usage` 累加到 `total_*_tokens` 全局字段
   - 清零 `current_request_usage`，准备下一次请求

收到用户新输入（发出新请求）时：

- 清零 `current_request_usage`
- default tab 的 `generating = true`、`status = Running`、`pending_usage = None`

**三层语义**：

| 层级 | 含义 | 显示位置 |
|------|------|---------|
| L1 单 agent 本次 | 该 agent 这一轮 Done 的 token | 该 agent tab 的最后一行 |
| L2 本次请求总用量 | 用户这次提问触发的所有 agent 累计 | default tab Done 时的最后一行 |
| L3 session 级累积 | 整个 session 所有请求的总和 | 状态栏（沿用现有 `total_*_tokens`） |

**结果**：default tab 的 token 行表达"该次请求的全部 token 用量"（包含所有 sub-agent）；每个 sub-agent tab 内只显示自己的；状态栏始终是 session 级累积。

## 事件处理流程

### 接收事件（handle_grpc_message）

针对 TextDelta / ToolCall / ToolResult / Error / Done / StatusUpdate / ThinkingBlock：

1. 从 frame 中提取 `session_id`（必有）和 `agent_name`（部分消息有，详见下文）
2. 查找对应的 TabEntry：
   - 主 session（agent_name == "default" 或匹配主 session_id）→ tabs[0]
   - 已存在的子 agent → 找到现有 tab
   - 新子 agent → 在 index = 1 处插入新 TabEntry，状态初始为 Running；**同步调整 active_tab**：若插入前 active_tab ≥ 1，则 active_tab += 1（保持用户原本激活的 tab 不变，焦点不被新 spawn 的子 agent 打断）。新 tab 不自动激活，用户通过黄色 Running 标记自行决定是否切过去查看。
3. 把当前 frame push 到 `tab.frames`
4. **如果是 active tab**：立即调用 `render_pending(tab)`，把 `frames[rendered_up_to..]` 转成 messages，并 append 到 `tab.messages`
5. **如果不是 active tab**：仅累积 frames，不渲染
6. 状态更新：
   - `AgentEvent::Done { error: None }` → status = Done
   - `AgentEvent::Done { error: Some(_) }` 或 `Error` → status = Error
   - StatusUpdate 作为辅助信号

### proto 字段约束（路由能力）

不同 ServerMessage 的字段可用性：

| Message | session_id | agent_name | 路由方式 |
|---------|------------|------------|----------|
| TextDelta | ✓ | ✓ | 双信号，优先 agent_name 表意；session_id 用于路由 |
| ToolCall | ✓ | ✓ | 同上 |
| ToolResult | ✓ | ✓ | 同上 |
| StatusUpdate | ✓ | ✓ | 同上 |
| Error | ✓ | ✓ | 同上 |
| **Done** | ✓ | ✗ | **仅靠 session_id 路由**。sub-agent Done 已通过 `spawn_sub_agent` 的 forwarding task 天然到达 CLI（含 root 自身的 Done 重复发送 quirk）。CLI 收到时若找不到对应 tab（理论不应发生，因 spawn_sub_agent 的 ToolCall 必先到），则丢弃并打 warn 日志。 |
| **UserQuery** | ✓ | ✗ | 仅靠 session_id 路由；UserQuery 处理逻辑不在本次范围 |
| ThinkingBlock | ✓ | ✓ | 同 TextDelta |
| UsageInfo | ✓ | ✗ | 仅靠 session_id 路由（per-tab pending_usage 由 session_id 定位） |

**实现原则**：所有事件都按 `session_id` 在 `tabs` 中查找；`agent_name` 仅用于"未知 session_id 时初始化 TabEntry 的标题"。session_id 是唯一标识符。

### 切换 tab（activate_tab）

1. 更新 `active_tab = new_index`
2. 调用 `render_pending(tabs[active_tab])` 把累积但未渲染的 frames 一次性转换
3. UI 重绘时只画 `tabs[active_tab].messages`，使用 `tabs[active_tab].scroll`

### 增量渲染（render_pending）

**职责**：把 `frames[rendered_up_to..]` 转换为 `ChatLine`，append 到 `tab.messages`，更新 `rendered_up_to`。

**关键**：
- 流式 TextDelta 的合并逻辑（原 `append_streaming` / `flush_streaming`）需要改造为 per-tab：操作目标 tab 的 `streaming_text` 和 `messages`
- ToolResult 查找 tool_name 时，**只在同一 tab 的 messages 中回查**（不是全局 messages），保证 tab 间隔离

## UI 渲染

### 布局

垂直分三段：
1. **Tab Bar**（顶部，固定高度 1 行）
2. **Chat Area**（中间，可滚动）
3. **Input + Status**（底部，沿用现有布局）

Tab Bar 始终显示（即使只有 default）。

### 输入框归属

**全局唯一输入框**。理由：

- 用户始终只和主 agent 对话；子 agent 是主 agent 通过 spawn_subagent 工具在后台 spawn 的，子 agent 不接受用户消息
- 输入框只在 active_tab == 0（default）时启用
- active_tab > 0（子 agent tab）时输入框**禁用**：
  - 视觉：输入框置灰、光标隐藏，显示占位提示"按 Alt+← 切回 default 输入"
  - 行为：忽略字符输入和回车
  - /命令、Tab 补全等也禁用（避免误操作）
- 输入历史仍然全局（不 per-tab）
- 切换 tab 时输入框内容**不清空**（因为只在 default tab 上才能编辑）

### Tab Bar 渲染（基于 ratatui Tabs widget）

**强制使用 `ratatui::widgets::Tabs`**，不自绘。

显示形式：`▶ default | ✓ fixer | ▶ explorer  [1/2]`

- 每个 tab title 是一个 `Line<'static>`，由两个 Span 组成：
  - 状态 Span（带颜色）：`▶ ` 黄 / `✓ ` 绿 / `✗ ` 红
  - 名字 Span：agent_name（无样式或与激活态联动）
- 通过 `Tabs::new(titles).select(active_index_in_current_page).highlight_style(reversed)` 表达激活态
- Padding 用 `.padding(" ", " ")`，分隔符用 `.divider(symbols::DOT)` 或 `|`
- 页码 `[当前页/总页数]` 在 tab bar 右侧独立 Paragraph 渲染（不属于 Tabs widget 内部）

### ratatui Tabs widget 与翻页的衔接

**关键约束**：Tabs widget 没有内置翻页能力，它会一次性渲染所有传入的 titles。

为支持翻页，外层做切片：

1. 计算当前页可见的 sub tab 索引范围 `visible_subs: Range<usize>`（见"翻页布局算法"）
2. 构造 `visible_titles = [default 的 Line, ...tabs[visible_subs] 的 Line]`
3. 计算激活 tab 在 visible_titles 中的相对索引 `select_idx`：
   - 如果 active_tab == 0（default）→ select_idx = 0
   - 如果 active_tab 在当前页 visible_subs 内 → select_idx = 1 + (active_tab - visible_subs.start)
   - 如果 active_tab 不在当前页（理论不发生，因为切换激活会自动调页）→ 不传 select 或传 None
4. 渲染：`Tabs::new(visible_titles).select(select_idx).highlight_style(...)`

页码指示符 `[N/M]` 用一个独立的 `Paragraph` 渲染在 tab bar 行的右端，通过 layout 把这一行分成左右两个 chunk：左 chunk 给 Tabs widget，右 chunk 给页码（固定宽度，如 6 列）。

### 翻页布局算法

输入：`tabs: &[TabEntry]`、`term_width: u16`、`current_page: usize`
输出：当前页应显示的 sub tab 索引范围

步骤：
1. default 占固定宽度（例如 12 列：`[▶ default] `）
2. 页码指示符占固定宽度（例如 6 列）
3. 剩余宽度 = term_width - default_width - page_indicator_width
4. 计算每个 sub tab 的实际宽度（基于 agent_name 长度）
5. 贪心分页：从第一个 sub 开始填充，超过剩余宽度则进入下一页
6. 返回当前页的索引范围

### Chat Area 渲染

- 渲染源：`tabs[active_tab].messages` + `tabs[active_tab].streaming_text`
- 滚动状态：使用 AppState 上的全局 `scroll_state` / `scroll_following`（不 per-tab）
- 切换 tab 时滚动位置不保留：自动重置 `scroll_following = true`，让新激活的 tab 显示在底部
- 当前 tab 收到新事件并触发 render_pending 后：若 `scroll_following == true` 自动滚到底部（沿用现有行为）
- 非当前 tab 收到事件：frames 累积，messages 不变，不影响滚动状态
- 与现有 ChatPanel 渲染逻辑保持一致，仅消息数据源变化

## 快捷键

| 键 | 行为 |
|----|------|
| `Alt+←` | 激活上一个 tab（循环：第 0 个时跳到末尾） |
| `Alt+→` | 激活下一个 tab（循环：最后一个时跳回第 0 个） |
| `Alt+Shift+←` | tab bar 翻上一页（仅滚动视野，不改激活；边界停止） |
| `Alt+Shift+→` | tab bar 翻下一页（仅滚动视野，不改激活；边界停止） |
| `Ctrl+W` | 关闭当前激活的子 agent tab；仅 Done/Error 状态可关，Running 状态无操作；default 不可关 |

激活 tab 切换时，自动调整 `tab_page` 使激活 tab 可见。

## 边界情况

### 子 agent 完成后

- tab 保留，状态变为 Done（绿色 ✓）
- 不自动关闭，用户可继续查看
- 主 session 关闭时整体清理

### 关闭子 agent tab（Ctrl+W）

- 仅在当前激活的是子 agent tab（active > 0）时生效
- active == 0（default）时按 Ctrl+W 无操作
- **仅允许关闭 Done / Error 状态的 tab**：Running 状态的 tab 按 Ctrl+W 无操作。理由：避免"关闭即复活"的体验问题（关掉后下一帧事件来了会重建 tab），同时也鼓励用户等子 agent 完成后再清理
- 关闭逻辑：
  1. 从 `tabs` 中移除 `tabs[active]`
  2. 激活索引调整为 `active - 1`（保证不会越界，至少回到 default）
  3. 调用 `tabs[active].render_pending()`（确保切换后 tab 已渲染）
  4. 调整 `tab_page` 使新激活 tab 可见
- 关闭只影响 UI，不影响 daemon 端 sub-agent 的运行
- 已关闭 tab 的 frames 数据丢弃；由于 Running 不可关，被关闭的 tab 必然已 Done/Error，理论上 daemon 不会再发该 session 的事件，所以"复活"问题在正常情况下不会发生
- 边缘情况：Done 之后又有迟到事件（不应发生，但如果发生）会按 session_id 重新 `find_or_insert` 创建新 tab——这是无副作用的退化行为，不做特殊处理

### 首次切换到子 agent tab

- 一次性渲染所有累积 frames（可能是全部对话历史）
- 渲染量大时可能有短暂延迟，可接受（仅发生一次）
- 渲染完成后 `rendered_up_to == frames.len()`，后续切换零开销

### tabs[0] 的 session_id 识别

- 主 session_id 在 `ChatHandle::session_id` 中已知
- handle_grpc_message 收到 frame 时，session_id == 主 session_id 即归属 default
- agent_name 字段对主 agent 是 "default"（或空字符串，需确认 daemon 端约定）

### 主 session 切换（/sessions 命令）

- 切换主 session 时，整个 TabBar 清空重建（tabs 只剩新的 default）
- 历史 frames 不跨 session 持久化（与现有行为一致）

### 鼠标交互

首版**不支持鼠标点击 tab**。所有 tab 切换通过键盘快捷键（Alt+方向键）。

未来扩展（不在本次范围）：可在 ui.rs 渲染时记录每个可见 tab 的屏幕 x 范围，handler 中处理 `MouseEventKind::Down(Left)` 命中测试切换激活。前提条件：用户已用 `/mouse` 命令开启鼠标捕获。

## 影响范围

| 模块 | 改动 |
|------|------|
| `crates/visp-cli/src/app.rs` | AppState 结构大改：messages → tabs；新增 TabEntry / AgentStatus / TabBar 相关方法；新增 `current_request_usage` 全局字段；`generating` / `pending_usage` 移入 TabEntry |
| `crates/visp-cli/src/event.rs` | handle_grpc_message 改为按 session_id 路由到 tab；Done 处理加状态守卫；新增 Alt+方向键处理；移除 prefix 调用 |
| `crates/visp-cli/src/ui.rs` | 新增 render_tab_bar；chat 区域数据源改为 active tab |
| `crates/visp-cli/src/main.rs` | 初始化 AppState 时构建第一个 default tab（用 ChatHandle.session_id） |

`visp-agent` / `visp-daemon` / `visp-core` / `visp-proto` 全部**不改动**。sub-agent Done 已通过 `spawn_sub_agent` 中的 forwarding task 天然到达 CLI；CLI 按 session_id 路由 + Done 状态守卫即可。

## 风险与权衡

### 大量 frames 首次渲染延迟

- 子 agent 跑了很久后用户才切过去，可能有几百个 frames 一次性渲染
- 缓解：渲染逻辑本身是纯计算，无 IO，应在毫秒级
- 如果实测有问题，未来可改为分批渲染（每帧渲染若干个）

### 内存占用

- 每个 tab 同时持有 frames 和 messages，约 2x 内存
- 子 agent 通常生命周期短，可接受
- 长 session 主 agent 的历史会持续累积——这与现有行为一致，未恶化

### tool_name 查找局部化

- 原本全局 messages 查找，现改为 tab 内查找
- 同一 tool_call_id 必定在同一 agent 内，不会跨 tab 查找
- 不会引入功能回归

## 验证标准

1. 启动 CLI，只有 default 时 tab bar 显示 `[▶ default]`，状态符号正确
2. 触发 spawn_subagent 后，tab bar 出现新 tab 在 index=1
3. 不切换到子 agent tab，子 agent 内容不出现在主聊天区
4. 切换到子 agent tab，能看到所有累积内容
5. 再切换回 default，default 内容完整保留
6. 子 agent 完成后状态变绿（依赖现有 forwarding task 已转发的 Done 事件 + CLI 按 session_id 路由）
7. 创建多个子 agent 直到超出屏幕宽度，触发分页，Alt+Shift+方向键能翻页
8. Alt+方向键能循环切激活 tab
9. 子 agent tab 上显示自己本次的 token 行（L1）；default tab 在 Done 时显示"本次请求总用量"（L2，含所有 sub-agent 累加）；状态栏始终显示 session 级累积（L3）
10. Ctrl+W 在 sub-agent tab Done 后能关闭该 tab；Running 状态时不能关
11. Cancel 主请求时，sub-agent tab 状态变红 ✗（Error），不被后续 Done 覆盖回绿（状态守卫验证）
12. 切换 tab 时新激活 tab 默认显示在底部（不保留之前的滚动位置）
13. 不再出现 `[sub: {agent_name}]` 前缀
