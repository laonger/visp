# vibewisp TUI 工作计划：ratatui 终端界面

## 概述

用 ratatui 重写 CLI 前端，替换 `print!`/`readline` 为分区化 TUI。

---

## 步骤 1：添加依赖（Cargo.toml）

修改 `crates/vbw-cli/Cargo.toml`：

- 添加 `ratatui = "0.28"`（features: `crossterm`）
- 添加 `crossterm = "0.28"`
- 添加 `tui-textarea = "0.7"`
- 移除 `rustyline`（不再需要）

验证：`cargo build -p vbw-cli` 编译通过。

#### 📦 提交

```bash
git add crates/vbw-cli/Cargo.toml Cargo.lock && git commit -m "feat(vbw-cli): add ratatui, crossterm, tui-textarea deps; remove rustyline"
```

---

## 步骤 2：创建 app.rs（应用状态）

新建 `crates/vbw-cli/src/app.rs`。在 `main.rs` 添加 `mod app;`。

### 🔴 红 — 测试

| # | 测试用例 |
|---|---|
| 1 | `test_app_state_default` — 新建 AppState 各字段为初始值 |
| 2 | `test_chat_line_types` — LineType 枚举各变体正确 |
| 3 | `test_streaming_text_clear` — Done 后 streaming_text 清空并追加到 messages |

### 🟢 绿 — 实现

- `LineType` 枚举：`User`, `Assistant`, `ToolCall`, `ToolResult`, `Error`, `Status`
- `ChatLine` 结构体：`line_type`, `content`
- `ConfirmState` 结构体：`query_id`, `message`
- `AppState` 结构体（见 5.5 节完整字段定义）
- `AppState::new(session_id, model)` → 初始化
- `AppState::add_message(&mut self, line_type, content)` → 追加 ChatLine
- `AppState::append_streaming(&mut self, delta)` → 追加 streaming_text
- `AppState::flush_streaming(&mut self)` → streaming_text → ChatLine → 清空

#### 🧪 测试 → 🔍 clippy

```bash
cargo test -p vbw-cli && cargo clippy -p vbw-cli -- -D warnings
```

#### 📦 提交

```bash
git add crates/vbw-cli/ && git commit -m "feat(vbw-cli): AppState with ChatLine, streaming text, and scroll state"
```

---

## 步骤 3：创建 ui.rs（渲染函数）

新建 `crates/vbw-cli/src/ui.rs`。在 `main.rs` 添加 `mod ui;`。

### 🟢 绿 — 实现

- `render(app: &AppState, f: &mut Frame)`
  - 三区布局：对话区（上部）、输入区+状态栏（底部）
  - 对话区：`List` widget，每行按 LineType 染色
  - streaming_text 临时拼到 List 末尾
  - 确认区：`Paragraph`，仅在 `app.confirm.is_some()` 时显示
  - 输入区：`app.textarea` widget
  - generating 时 textarea 变灰
  - 状态栏：`Paragraph`，左侧 session_id，右侧 model + status

### 🔍 clippy

```bash
cargo clippy -p vbw-cli -- -D warnings
```

#### 📦 提交

```bash
git add crates/vbw-cli/ && git commit -m "feat(vbw-cli): TUI render with List chat area, tui-textarea input, and status bar"
```

---

## 步骤 4：创建 event.rs（事件循环）

新建 `crates/vbw-cli/src/event.rs`。在 `main.rs` 添加 `mod event;`。

### 🟢 绿 — 实现

- `pub async fn run(session_id, chat_handle) -> Result<()>`
  - 初始化 AppState
  - `crossterm::terminal::enable_raw_mode()`
  - 创建 `Terminal` + `EventStream`
  - `tokio::select!` 循环：
    - `event_stream.next()` → `handle_key(event)` / `handle_resize(event)`
    - `chat_handle.recv()` → gRPC 消息处理
  - `handle_key(event)`:
    - 优先消费：Ctrl+C/D、Enter、y/n（确认）、PageUp/PageDown
    - 其余：`textarea.input(event)`
    - generating 时忽略输入键
  - gRPC 消息映射：
    - TextDelta → `app.append_streaming(delta)` → draw
    - ToolCall → `app.add_message(ToolCall, ...)` → draw
    - Done → `app.flush_streaming()` → `app.generating = false` → draw
    - None → `app.should_quit = true` → break
  - 退出时：`disable_raw_mode()` + `terminal.clear()`

### 🔍 clippy

```bash
cargo clippy -p vbw-cli -- -D warnings
```

#### 📦 提交

```bash
git add crates/vbw-cli/ && git commit -m "feat(vbw-cli): TUI event loop with crossterm select! and gRPC message handling"
```

---

## 步骤 5：更新 main.rs + 删除旧文件

- 修改 `main.rs`：移除 `mod repl; mod display;`，添加启动/退出清理
- 删除 `display.rs`
- 删除 `repl.rs`

### 🔍 clippy

```bash
cargo clippy -p vbw-cli -- -D warnings
```

#### 📦 提交

```bash
git add crates/vbw-cli/ && git commit -m "feat(vbw-cli): wire TUI into main, remove old display and repl modules"
```

---

## 步骤 6：质量门

```bash
cargo build --workspace && cargo test --workspace && cargo clippy --workspace -- -D warnings && cargo fmt --check --all
```

---

## Wave 并行策略

Phase 仅一个 crate，依赖链路短：

### Wave 1：类型 + UI（并行）

```
Agent A: 步骤 1 → 步骤 2 (依赖 + app.rs)
Agent B: 步骤 3 (ui.rs — 仅需 app.rs 类型签名)
```

### Wave 2：事件循环 + 集成（串行）

```
Agent A: 步骤 4 (event.rs — 需 app + ui + client)
Agent B: 步骤 5 (main.rs 更新 + 清理)
```

### Wave 3：质量门

---

## 测试覆盖汇总

| Wave | Crate | 步骤 | 测试用例 |
|---|---|---|---|
| 1 | vbw-cli | 依赖 + app + ui | 3 |
| 2 | vbw-cli | event + main | 0（手动验收） |
| 3 | 全 workspace | 质量门 | — |

总计：**6 步骤，3 测试用例**。TUI 多为视觉/交互测试，自动化有限。
