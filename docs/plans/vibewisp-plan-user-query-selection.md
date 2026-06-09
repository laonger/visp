# 工作计划：UserQuery 选择式确认栏

## 概述

将工具审批的 y/N 文本输入改造为横向可选项选择栏，支持方向键导航、Enter 确认、Other 自定义输入。涉及 vbw-proto / vbw-core / vbw-daemon / vbw-cli 四个 crate。

## Wave 并行策略

```
Wave 1 (并行)        Wave 2 (并行)
┌──────────┐       ┌──────────────┐
│ vbw-proto │       │ vbw-daemon   │
├──────────┤       ├──────────────┤
│ vbw-core  │       │ vbw-cli      │
└──────────┘       └──────────────┘
     │                    │
     └── 都完成后 ────────┘
         合并测试
```

## 步骤 1：vbw-proto — 协议定义

### 1a：更新 UserQuery proto

#### 🔴 红 — 测试

proto 生成的 Rust 类型有新字段即可验证，通过 `cargo build -p vbw-proto` 检查。

| # | 测试用例 | 说明 |
|---|---------|------|
| 1.1 | UserQuery 包含 `options` 字段 | Vec<String>，默认空列表 |
| 1.2 | UserQuery 包含 `allow_other` 字段 | bool，默认 false |
| 1.3 | 空 options = 工具审批模式 | 客户端据此 fallback |

#### 🟢 绿 — 实现

在 `vibewisp.proto` 中：

- `UserQuery` 增加 `repeated string options = 4` 和 `bool allow_other = 5`
- 重新生成 Rust 代码（`cargo build -p vbw-proto`）

#### 🧪 测试

```bash
cargo build -p vbw-proto
```

#### ♻️ 重构

无

#### 📦 提交

`feat(proto): add options/allow_other to UserQuery`

---

### 1b：更新 UserResponse proto

#### 🔴 红 — 测试

| # | 测试用例 | 说明 |
|---|---------|------|
| 2.1 | UserResponse 包含 `selected_index` 字段 | int32，-1 = Other |
| 2.2 | UserResponse 包含 `text` 字段 | string，Other 时用 |
| 2.3 | `approved` 字段不再使用 | proto 中移除或标记 deprecated |

#### 🟢 绿 — 实现

在 `vibewisp.proto` 中：

- `UserResponse` 将 `approved` 替换为 `int32 selected_index = 2` + `string text = 3`
- 重新生成 Rust 代码

#### 🧪 测试

```bash
cargo build -p vbw-proto
```

#### ♻️ 重构

无

#### 📦 提交

`feat(proto): replace UserResponse.approved with selected_index/text`

---

## 步骤 2：vbw-core — 核心逻辑

### 2a：UserQueryResult 结构体 + AgentEvent 更新

#### 🔴 红 — 测试

在 `crates/vbw-core/src/agent.rs` 现有测试文件追加：

| # | 测试用例 | 说明 |
|---|---------|------|
| 3.1 | UserQueryResult 构造和字段访问 | `new(0, "")`、`new(-1, "custom")` |
| 3.2 | AgentEvent::UserQuery 的 respond 类型改为 `Sender<UserQueryResult>` | 编译检查 |
| 3.3 | UserQueryResult 的默认值 | selected_index=0, text="" |
| 3.4 | 现有 UserQuery 相关测试适配新类型 | 修改调用方 |

#### 🟢 绿 — 实现

- 新增 `UserQueryResult` 结构体（在 `agent.rs` 中，或单独 `user_query.rs`）
- `AgentEvent::UserQuery` 中 `respond` 类型从 `oneshot::Sender<bool>` 改为 `oneshot::Sender<UserQueryResult>`
- 添加 `UserQueryResult::default()` 或实现 `Default` trait

#### 🧪 测试 → 🔍 类型检查

```bash
cargo test -p vbw-core
cargo clippy -p vbw-core -- -D warnings
```

#### ♻️ 重构

无

#### 📦 提交

`feat(core): add UserQueryResult, update AgentEvent::UserQuery respond type`

---

### 2b：Session 增加 approved_tools

#### 🔴 红 — 测试

在 `crates/vbw-core/src/session.rs` 现有测试文件追加：

| # | 测试用例 | 说明 |
|---|---------|------|
| 4.1 | Session 初始化后 approved_tools 为空 | `assert!(session.approved_tools.is_empty())` |
| 4.2 | Session 的 Clone 包含 approved_tools | clone 后相等 |
| 4.3 | 添加工具名到 approved_tools | `insert` 后 `contains` 返回 true |

#### 🟢 绿 — 实现

- `Session` 结构体增加 `pub approved_tools: HashSet<String>` 字段
- 修改所有构造处（`session.rs` 中的 `create`）
- 确认 `Session` 的 derive macro（Clone, PartialEq）兼容新字段

#### 🧪 测试 → 🔍 类型检查

```bash
cargo test -p vbw-core
cargo clippy -p vbw-core -- -D warnings
```

#### ♻️ 重构

无

#### 📦 提交

`feat(core): add approved_tools HashSet to Session`

---

### 2c：Agent 循环 — 工具审批路径改造

#### 🔴 红 — 测试

在 `crates/vbw-core/src/agent.rs` 现有 `test_user_query_approved` / `test_user_query_denied` 测试基础上更新：

| # | 测试用例 | 说明 |
|---|---------|------|
| 5.1 | 审批通过（selected_index=0）→ 工具执行 | 对应原 approved 测试 |
| 5.2 | 审批拒绝（selected_index=1）→ User Denied | 对应原 denied 测试 |
| 5.3 | Always Allow（selected_index=2）→ 执行 + 记录 | 验证 `approved_tools` 包含该工具 |
| 5.4 | 已审批工具再次调用直接跳过 | mock 两次调用同工具，第二次不触发 UserQuery |
| 5.5 | approved_tools 跨 spawn task 正确写入 | 并行调用两个不同工具，一个 Allow 一个 Deny |

#### 🟢 绿 — 实现

在 `agent.rs` 的 tool 执行 task 中（`tokio::spawn`）：

1. **在 `requires_approval` 检查之前**，先查 `Session.approved_tools` 是否包含 `tc.name`
2. 如果已存在，直接执行
3. 如果不存在且需要审批：
   - 创建 `UserQuery` 事件，respond 类型改为 `Sender<UserQueryResult>`
   - 根据返回的 `selected_index`：
     - **0（Approve）**：继续执行
     - **1（Deny）**：返回 `ToolResult::error("User denied")`
     - **2（Always Allow）**：写回 `Session.approved_tools`，继续执行

**注意**：spawn task 内如何访问 Session：
- 方案 A：在 task 外先 clone `approved_tools` 为 `Arc<Mutex<HashSet<String>>>` 传入，loop 结束后写回 Session
- 方案 B：将 `session_mgr` 传入 task，通过 SessionManager 方法操作

#### 🧪 测试 → 🔍 类型检查

```bash
cargo test -p vbw-core
cargo clippy -p vbw-core -- -D warnings
```

#### ♻️ 重构

无

#### 📦 提交

`feat(core): update agent approval flow with Always Allow and approved_tools`

---

### 2d：[USER_QUERY] 后处理解析

#### 🔴 红 — 测试

| # | 测试用例 | 说明 |
|---|---------|------|
| 6.1 | text_buffer 末尾匹配标记 → 解析 message + options | 标准格式 |
| 6.2 | 标记含 `allow_other=true` → 解析该字段 | 解析指令参数 |
| 6.3 | 标记不含 allow_other → 默认 false | 兼容简洁格式 |
| 6.4 | text_buffer 不含标记 → 不触发 UserQuery | 正常结束 |
| 6.5 | 剥离标记后的 text_buffer 不含标记文字 | 剩余文本正确 |
| 6.6 | 标记中多行选项正确解析为 Vec<String> | 每行 `- ` 前缀去掉 |
| 6.7 | 标记格式异常时静默忽略 | 不做 UserQuery，继续正常流程 |

#### 🟢 绿 — 实现

在 `agent.rs` 的 LLM 事件收集循环结束后（Done 之后，决定"无 tool_call → done"之前）：

1. 正则或字符串匹配检测 `text_buffer` 末尾的 `[USER_QUERY ...]...[/USER_QUERY]`
2. 如匹配：
   - 将 `text_buffer` 剥离标记，剩余文本作为 Assistant Message 发送
   - 解析：message（第一行非 `-` 内容）、options（`- 前缀` 行列表）、allow_other（从 `[USER_QUERY allow_other=...]` 解析）
   - 创建 `UserQuery` 事件，`respond` 用新 channel
   - 等待 respond（`resp_rx.await`）
   - 根据结果：将选项文本或自定义文本作为 User Message 插入历史
   - **不 break 循环**，让外层的 for loop 继续下一轮迭代（LLM 看到用户选择后输出）
3. 不匹配：按原有逻辑

**提示**：这个功能可能需要新增一个 helper 函数 `parse_user_query_marker(text: &str) -> Option<UserQueryMarker>` 方便测试。

#### 🧪 测试 → 🔍 类型检查

```bash
cargo test -p vbw-core
cargo clippy -p vbw-core -- -D warnings
```

#### ♻️ 重构

无

#### 📦 提交

`feat(core): add [USER_QUERY] post-processing for LLM-initiated user queries`

---

### 2e：系统 prompt 新增指令

#### 🔴 红 — 测试

| # | 测试用例 | 说明 |
|---|---------|------|
| 7.1 | PromptBuilder 生成的 prompt 包含 user_query 指令 | 加一个测试验证 system prompt 内容 |

#### 🟢 绿 — 实现

在 `crates/vbw-core/src/prompt.rs` 或系统 prompt 模板中，增加一段关于 `[USER_QUERY]` 格式的指令：

```
When you need the user to make a choice, append the following at the end of your response:
[USER_QUERY]
Your question here
- Option A description
- Option B description
- Option C description
[/USER_QUERY]

Optionally use [USER_QUERY allow_other=true] to allow custom input.
```

如果 system prompt 模板使用内置默认字符串，直接在常量中追加；如果是可加载的外部模板（`.vibewisp/system-prompt.md`），则需要在项目中更新默认模板。

#### 🧪 测试 → 🔍 类型检查

```bash
cargo test -p vbw-core
cargo clippy -p vbw-core -- -D warnings
```

#### ♻️ 重构

无

#### 📦 提交

`feat(core): add [USER_QUERY] instruction to system prompt`

---

## 步骤 3：vbw-daemon — 消息桥接适配

### 3a：UserQuery 映射更新

#### 🔴 红 — 测试

| # | 测试用例 | 说明 |
|---|---------|------|
| 8.1 | AgentEvent::UserQuery → proto::UserQuery 包含 options/allow_other | 桥接完整 |
| 8.2 | UserQuery 空 options 时 proto 字段空列表 | 工具审批场景正确 |

#### 🟢 绿 — 实现

在 `service.rs` 的 UserQuery 映射处理（第 306-322 行）：

- 在 `AgentEvent::UserQuery` → `proto::UserQuery` 转换中，增加 `options` 和 `allow_other` 字段
- 注意：工具审批场景生成的 UserQuery，options 为空列表、allow_other 为 false

#### 🧪 测试 → 🔍 类型检查

```bash
cargo test -p vbw-daemon
cargo clippy -p vbw-daemon -- -D warnings
```

#### ♻️ 重构

无

#### 📦 提交

`feat(daemon): map UserQuery options/allow_other in service`

---

### 3b：UserResponse 映射更新 + approved_tools

#### 🔴 红 — 测试

| # | 测试用例 | 说明 |
|---|---------|------|
| 9.1 | proto::UserResponse → UserQueryResult 映射 | selected_index, text 正确传递 |
| 9.2 | SessionManager 新增 add_approved_tool / is_tool_approved | 供 agent 循环调用 |
| 9.3 | 同时处理多个 UserResponse 无竞态 | 原有 pending_queries HashMap 保护 |

#### 🟢 绿 — 实现

- `service.rs`：`UserResponse` 处理分支（第 386-390 行），将 `resp.selected_index` 和 `resp.text` 映射为 `UserQueryResult` 发送
- `session.rs`：`SessionManager` 新增 `add_approved_tool(&self, session_id, tool_name)` 和 `is_tool_approved(&self, session_id, tool_name)` 方法
- agent 循环中通过 `session_mgr.add_approved_tool(...)` 写入

#### 🧪 测试 → 🔍 类型检查

```bash
cargo test -p vbw-daemon && cargo test -p vbw-core
cargo clippy -p vbw-daemon -- -D warnings && cargo clippy -p vbw-core -- -D warnings
```

#### ♻️ 重构

无

#### 📦 提交

`feat(daemon): map UserResponse to UserQueryResult, wire approved_tools`

---

## 步骤 4：vbw-cli — TUI 改造

### 4a：ConfirmState 更新 + 键盘事件

#### 🔴 红 — 测试

| # | 测试用例 | 说明 |
|---|---------|------|
| 10.1 | ConfirmState 包含 options / allow_other / selected_index / other_active | 编译通过 |
| 10.2 | ←/→ 方向键移动 selected_index | 循环边界（到末尾再到开头） |
| 10.3 | Enter 确认选中项 → 发送 UserResponse | 验证 call_id 和 index |
| 10.4 | Esc 相当于 Deny（selected_index=1） | 工具审批场景 |
| 10.5 | Other 模式：Esc 退出回到导航，输入区清空 | |
| 10.6 | -1 场景：新建 ConfirmState selected_index 从 0 开始 | 默认选中第一个 |

#### 🟢 绿 — 实现

- 修改 `ConfirmState`：增加 `options: Vec<String>` / `allow_other: bool` / `selected_index: usize` / `other_active: bool` 字段
- 修改 `event.rs` 中的 `handle_key_event`：当 `app.confirm.is_some()` 时：
  - `KeyCode::Left` → `selected_index` 减 1（循环到末尾）
  - `KeyCode::Right` → `selected_index` 加 1（循环到开头）
  - `KeyCode::Enter` → 确认，发送 `UserResponse { selected_index, text: "" }`
  - `KeyCode::Esc` → 导航态：发送 Deny；Other 态：回导航、清空输入区
  - 当移动到最后一项且该项是 Other（`allow_other && selected_index == options.len()`）→ 进入 `other_active = true`
  - 从 Other 位置移走 → 退出 `other_active = false`
- 修改 `handle_grpc_message` 中的 UserQuery 处理（第 300-304 行），传入 options / allow_other
- 修改 `client.rs` 中的 `send_response`，支持 `selected_index` 和 `text`

#### 🧪 测试

```bash
cargo test -p vbw-cli
```

注意：TUI 测试需要 mock crossterm 事件，或通过测试 AppState 的方法来验证事件处理逻辑。

#### ♻️ 重构

无

#### 📦 提交

`feat(cli): update ConfirmState and event handling for selection UI`

---

### 4b：确认栏渲染

#### 🔴 红 — 测试

| # | 测试用例 | 说明 |
|---|---------|------|
| 11.1 | 确认栏渲染包含 message + 选项行 | 两行结构 |
| 11.2 | 选中项反色/高亮显示 | 当前 selected_index 选项样式不同 |
| 11.3 | 工具审批模式显示 [Approve] [Deny] [Always Allow] | options 为空时 fallback |
| 11.4 | LLM 模式显示 [A] opt1 [B] opt2 [C] opt3 | options 非空时显示 LLM 选项 |
| 11.5 | allow_other=true 时末尾显示 [D] Other | Other 按钮样式与其他一致 |

#### 🟢 绿 — 实现

重写 `ui.rs` 中的 `render_confirm_bar`：

- 第一行：`❓ {message}`（黄色 `❓` + 白色 message）
- 第二行：选项列表，格式 `[字母] 选项名`，当前选中项反色
  - 工具审批（空 options）：`[A] Approve  [B] Deny  [C] Always Allow`
  - LLM 模式：根据 options 长度逐个生成字母前缀
  - 有 Other：追加 `[字母] Other`
- 顶部边框（与现有风格一致）
- 使用 `theme::CONFIRM_*` 颜色常量

#### 🧪 测试

```bash
cargo build -p vbw-cli
```

UI 渲染通过 ratatui 的 `TestBackend` 写快照测试验证。

#### ♻️ 重构

无

#### 📦 提交

`feat(cli): rewrite render_confirm_bar with selection UI`

---

### 4c：Other 输入模式

#### 🔴 红 — 测试

| # | 测试用例 | 说明 |
|---|---------|------|
| 12.1 | other_active=true 时输入区可编辑 | 即使 generating=true |
| 12.2 | Other 模式下任意字符输入到输入区 | 正常 textarea 行为 |
| 12.3 | Other 模式下 Enter → 发送 text | UserResponse { selected_index: -1, text } |
| 12.4 | Other 模式切换时输入区内容清空 | 进入和退出都清空 |

#### 🟢 绿 — 实现

- 修改 `render_input_area`：优先检查 `confirm.other_active`，为 true 时忽略 generating 锁定
- 修改 `handle_key_event`：`other_active=true` 时，字符键转发到 textarea，Enter 确认 Other 提交
- Other 提交时 `send_response(query_id, -1, textarea_text)`

#### 🧪 测试

```bash
cargo test -p vbw-cli
```

#### ♻️ 重构

无

#### 📦 提交

`feat(cli): implement Other input mode for confirm bar`

---

### 4d：选项超宽可滚动

#### 🔴 红 — 测试

| # | 测试用例 | 说明 |
|---|---------|------|
| 13.1 | 选项总宽 > 屏幕宽时显示 `<` `>` 指示器 | |
| 13.2 | ←/→ 同步滚动可见区域 + 移动 selected_index | |
| 13.3 | 滚动到最左端时隐藏 `<`，最右端时隐藏 `>` | |
| 13.4 | 不超宽时不显示指示器 | 无需要不额外加 UI |

#### 🟢 绿 — 实现

- 在 `render_confirm_bar` 的选项行渲染中，先计算所有选项的总宽度
- 如果超出 `area.width`，计算 `scroll_offset`（与 selected_index 同步）
- 在首尾根据 `scroll_offset` 状态渲染 `<` / `>` 指示器
- `selected_index` 变化时同步调整 `scroll_offset`

#### 🧪 测试

```bash
cargo test -p vbw-cli
```

#### ♻️ 重构

无

#### 📦 提交

`feat(cli): add overflow scrolling for confirm bar options`

---

## 步骤 5：集成验证

### 5a：全量测试

```bash
cargo test
cargo clippy -- -D warnings
cargo fmt -- --check
```

### 5b：手动验证

- 启动 daemon + CLI
- 触发工具审批场景（如 bash 高危命令）
- 验证：[A] [B] [C] 渲染正确，←/→ 选择正常，Enter 确认
- 选择 Always Allow，再次触发同工具 → 自动跳过审批
- LLM 选择场景需构造包含 `[USER_QUERY]` 标记的特殊 prompt

### 📦 提交

`feat: complete selection-based confirm UI`

---

## 测试覆盖汇总

| Wave | 并行数 | 子步骤 | 测试用例数 |
|------|--------|--------|-----------|
| Wave 1a: proto | 1 | 1a, 1b | 5 |
| Wave 1b: core | 1 (与 1a 并行) | 2a, 2b, 2c, 2d, 2e | ~20 |
| Wave 2a: daemon | 1 | 3a, 3b | 5 |
| Wave 2b: cli | 1 (与 2a 并行) | 4a, 4b, 4c, 4d | ~15 |
| Wave 3: 集成 | 串行 | 5a, 5b | — |

## 备注

- **proto 兼容性**：当前项目开发阶段，不做向后兼容层，直接替换
- **SessionManager 方法**：`add_approved_tool` / `is_tool_approved` 是新增的 SessionManager API，需要加锁保护
- **spawn task 数据传递**：`approved_tools` 需要通过 Arc/Mutex 共享或通过 SessionManager 方法操作
- **UI 测试**：ratatui 提供 `TestBackend`，可辅助写渲染快照测试
- **提交顺序**：严格按照 Wave 顺序，每个子步骤一个独立 commit
