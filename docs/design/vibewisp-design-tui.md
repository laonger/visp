# vibewisp TUI 设计：ratatui 终端界面

## 1. 目标

用 ratatui 框架替换当前 `print!`/`readline` 的简单终端交互，提供分区化的、流式更新的 TUI 体验。

## 2. 布局

```
┌─────────────────────────────────────────────────┐
│              对话区 (ChatArea)                    │
│                                                  │
│  User: 请帮我看下项目                             │
│  Assistant: 好的，让我先看看...                    │
│  🔧 bash(ls)                                     │
│  📄 [output]                                     │
│  Assistant: 项目包含以下文件...                    │
│                                                  │
├─────────────────────────────────────────────────┤
│  ❓ 是否允许执行: rm -rf? [y/N] _                 │  ← 确认区（UserQuery 时显示）
├─────────────────────────────────────────────────┤
│  > 输入消息...                                   │  ← 输入区
├─────────────────────────────────────────────────┤
│  Session: abc123  |  Model: deepseek-v4-flash    │  ← 状态栏
└─────────────────────────────────────────────────┘
```

三区 + 条件确认区：
- **对话区**（上半屏，可滚动）：显示用户消息、助手回复、工具调用/结果/错误。使用 `List` widget 逐行渲染，每行可独立染色。
- **确认区**（UserQuery 时显示）：提示文本 + 直接按键 y/n（不需要 Enter）
- **输入区**（底部固定一行）：`tui-textarea` 组件，Enter 发送，↑↓ 浏览历史
- **状态栏**（底部固定一行）：session id、模型名、运行状态（Idle/Generating）

## 3. 模块变更

| 模块 | 变更 |
|---|---|
| `display.rs` | **删除** |
| `repl.rs` | **删除** |
| `event.rs` | **新增**，事件循环（tokio::select!）和事件分发 |
| `app.rs` | **新增**，应用状态管理 |
| `ui.rs` | **新增**，ratatui 渲染函数（ChatArea/InputArea/ConfirmBar/StatusBar） |
| `main.rs` | 微小改动 |
| `client.rs` | **不变** |

新增依赖：
- `ratatui`：TUI 框架
- `crossterm`：终端后端
- `tui-textarea`：输入区组件

## 4. 核心数据流

```
crossterm EventStream (keyboard, resize)
    │
    ▼
ratatui App 事件循环 (tokio::select!):
    ├─ crossterm event → handle_key(key) / handle_resize(size)
    │   输入区文字编辑 / Enter发送 / 特殊命令
    │   Resize 事件 → 重新计算三区布局

    ├─ gRPC message → chat_handle.recv()
    │   追加到对话历史 / 触发 UI 刷新
    │   · Some(msg) → 更新 AppState → draw
    │   · None → 状态栏显示 "Daemon disconnected" → should_quit = true → break
    │   每个 TextDelta token 到达时立即 draw，帧率由 LLM 输出速度决定
    │   ratatui 仅重绘变化区域，高频 token 场景性能可接受

    └─ Ctrl+C / Ctrl+D → 键盘事件中处理（crossterm raw mode）
```

- 启动时启用 crossterm `raw mode`，Ctrl+C/Ctrl+D 统一在键盘事件中处理
- Ctrl+C：若 Agent 运行中 → `send_cancel()`；空闲 → 忽略
- Ctrl+D：任意状态 → 设置 `should_quit = true`，事件循环 break
- 退出时调用 `disable_raw_mode()` + `terminal.clear()` 清理终端

## 5. 组件设计

### 5.1 对话区 (ChatArea)

- 使用 `List` widget，逐行渲染，支持不同颜色
- 数据结构：`Vec<ChatLine>`

```rust
enum LineType { User, Assistant, ToolCall, ToolResult, Error, Status }
struct ChatLine { line_type: LineType, content: String }
```

**每行颜色方案**：

| 类型 | 颜色 |
|---|---|
| User | Cyan |
| Assistant | White |
| ToolCall | Yellow |
| ToolResult | DarkGray |
| Error | Red |
| Status | Gray |

**流式文本缓冲**：App 维护 `streaming_text: String`（当前 assistant 的未完成文本）
- TextDelta → `streaming_text += delta`，渲染时临时拼到 List 末尾
- Done → `ChatLine(Assistant, streaming_text)` 追加到 messages → `streaming_text` 清空

**滚动**：
- 自动滚动：新行追加时若用户在底部（`scroll_following: true`）则滚到底部
- 手动滚动：PageUp/PageDown 浏览历史，向上滚动后 `scroll_following = false`
- 恢复跟随：用户手动滚回底部或发送新消息时 `scroll_following = true`

### 5.2 输入区 (InputArea)

- 使用 `tui-textarea` crate

**按键消费分工**：
- **App 优先**：Enter（发送/命令）、Ctrl+C/D、y/n（确认）、PageUp/PageDown（滚动）
- **Textarea 消费**：字母、退格、Delete、光标移动、↑↓（历史）等其余所有键
- 使用 `textarea.input(event)` 判断是否消费

- Enter → 发送 UserInput → 清空输入区
- 特殊命令（`/temp`、`/model`、`/clear`、`/help` 等）
- `/clear` → `messages.clear()`（不是 ANSI escape）
- Ctrl+D 直接退出，无需 `/quit` 命令

**↑↓ 历史浏览**（App 自行实现，非 textarea 内置）：
- 维护 `input_history: Vec<String>`（最近 100 条）
- Enter 发送时将当前输入 push 到 history
- ↑：从 history 取上一条 → 写入 textarea
- ↓：从 history 取下一条 → 写入 textarea，到底则清空
- 维护 `history_index: Option<usize>` 跟踪位置

### 5.3 确认区 (ConfirmBar)

- UserQuery 到达时显示，平时隐藏
- **直接按键 y/n，不需要 Enter**
- App 维护 `confirm_state: Option<ConfirmState>`，handle_key 优先消费
- y → 发送 approved=true，n / 其他 → 发送 approved=false
- 确认完成后清空 confirm_state

**Agent 运行中锁定输入**：
- App 维护 `generating: bool` 状态
- `generating = true` 时，handle_key 忽略所有输入键（仅 Ctrl+C / Ctrl+D 仍可用）
- textarea 文字变灰（`Style::fg(Color::DarkGray)`），状态栏显示 `[Generating]`
- Done / Error 到达后设回 false，恢复 textarea 颜色和状态栏

### 5.4 状态栏 (StatusBar)

- 左侧：session id（前 8 位）
- 右侧：模型名、运行状态（Idle/Generating）

### 5.5 AppState 汇总

```rust
struct AppState {
    // 对话
    messages: Vec<ChatLine>,
    streaming_text: String,            // Done 前累积，渲染时临时拼到 List 末尾
    scroll_offset: usize,
    scroll_following: bool,

    // 输入
    textarea: tui_textarea::TextArea<'static>,
    input_history: Vec<String>,
    history_index: Option<usize>,      // ↑↓ 历史浏览位置

    // 状态
    generating: bool,
    confirm: Option<ConfirmState>,
    model: String,
    session_id: String,
    should_quit: bool,
}
```

## 6. 不做什么

- ❌ 多标签会话
- ❌ 语法高亮
- ❌ 文件树/侧边栏
- ❌ 复杂快捷键（仅 Enter/Ctrl+C/Ctrl+D/PageUp/PageDown/↑↓）

## 7. 验收标准

- TUI 启动正常，三个区域可见
- 输入文本后流式显示在对话区
- 工具调用/结果显示在对话区
- UserQuery 弹出确认区，直接按键 y/n
- Ctrl+C 发送取消（Agent 运行中）；空闲时忽略
- Ctrl+D 任意状态退出
- 对话区可滚动查看历史（PageUp/PageDown）
- ↑↓ 浏览输入历史
- 退出时终端正常恢复
