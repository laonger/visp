# visp TUI 设计：ratatui 终端界面

## 1. 目标

用 ratatui 框架提供分区化的、流式更新的 TUI 体验，支持 markdown 渲染、代码语法高亮、工具调用可视化、token 用量统计。

## 2. 布局

```
┌─────────────────────────────────────────────────┐
│              对话区 (ChatArea)                    │
│                                                  │
│  User: 请帮我看下项目                             │
│  Assistant: 好的，让我先看看...                    │
│  ┌─ [Tool] ────────────────────────────┐          │
│  │  bash: "ls -a"                       │ ← 黄色  │
│  │  Output: file1 file2                  │ ← 灰色  │
│  └──────────────────────────────────────┘          │
│  Assistant: 项目包含以下文件...                    │
│  代码块：                                        │
│  ┌─────────────────────────────────────────┐      │
│  │ fn hello() { ← 语法高亮（syntect）       │      │
│  │     println!("hi");                     │      │
│  │ }                                        │      │
│  └─────────────────────────────────────────┘      │
│  [14:32:05 | Tokens: 105 in / 125 out | Tools: 2]│ ← 灰色
│                                                  │
├─────────────────────────────────────────────────┤ ← 分隔线 ─
│  ❓ 是否允许执行: rm -rf? [y/N] _                 │ ← 确认区
├─────────────────────────────────────────────────┤
│  > 输入消息...                                   │ ← 输入区
├─────────────────────────────────────────────────┤
│  Session: abc123  |  Model: deepseek-v4-flash    │ ← 状态栏
└─────────────────────────────────────────────────┘
```

三区 + 条件确认区 + 分隔线：
- **对话区**（上半屏，可滚动）：显示用户消息、助手回复、工具调用/结果/错误、token 用量
- **分隔线**：对话区与底部区域之间的 `─` 线条
- **确认区**（UserQuery 时显示）：提示文本 + 直接按键 y/n
- **输入区**（底部）：`tui-textarea` 组件，Enter 发送，↑↓ 浏览历史
- **状态栏**（底部一行）：session id、模型名、运行状态（Idle/Generating）

## 3. BlockStyle 统一渲染系统

所有消息块使用 `BlockStyle` 结构和统一的 `render_block()` 函数渲染。

### 3.1 BlockStyle 定义

```rust
struct BlockStyle {
    margin_vertical: u16,   // 垂直两端留白（字符数）
    margin_horizontal: u16,  // 水平两端留白（字符数）
    bg_fill: Option<Color>, // 底色；None → 无底色，Some → 填底色
    shadow: bool,           // 是否绘制右侧+底部 drop shadow
    bottom_pad: u16,        // 内容下方行数（底色或分隔线）
}
```

### 3.2 各消息类型样式

| 类型 | margin_vertical | margin_horizontal | bg_fill | shadow | bottom_pad | 前景色 |
|------|:---:|:---:|:---:|:---:|:---:|:---:|
| **User** | 1 | 1 | `#1A3A5E` | ✓ | 2 | Cyan |
| **Assistant** | 1 | 1 | `#222A3E` | ✓ | 2 | White |
| **Thinking** | 1 | 1 | None | ✗ | 1 | Green |
| **ToolCall / ToolResult** | 1 | 1 | `#222222` | ✓ | 0 | Yellow / DarkGray |
| **Usage (token 统计)** | 0 | 1 | `#222222` | ✗ | 1 | DarkGray |

### 3.3 render_block 渲染流程

```
渲染顺序：
  1. 底色填充（如有 bg_fill）
  2. 内容 Paragraph（按 margin 缩进）
  3. drop shadow（右侧列 + 底部行）
```

内容区域宽度：`content_w_adj = area_width - 1(阴影列) - margin_horizontal * 2`

### 3.4 滚动与视窗裁剪

使用 `viewport_intersect()` 判断 block 可见性。

计算 `hidden_top = scroll_y - y`（被滚出屏幕上方的行数），从 `hidden_top` 位置开始取 lines slice，只渲染可见部分。解决滚动时 block 前后重叠的问题。

## 4. 消息类型

### 4.1 LineType 枚举

```rust
enum LineType {
    User,        // 用户消息
    Assistant,   // AI 回复（支持 markdown）
    Thinking,    // 思考块（如 Anthropic 的 extended thinking）
    ToolCall,    // 工具调用 + 结果（合并为同一 block）
    Error,       // 错误消息
    Status,      // 状态消息
    Usage,       // token 用量统计（已废弃，并入 Assistant 末尾）
}
```

### 4.2 工具调用显示

调用行和结果行合并为同一个 `ToolCall` 消息块：
- **首行**（调用行）：`tool_name: "args"`，**黄色**
- **后续行**（结果行）：`Output: ...` 或 `Error: ...`，**灰色**
- 超过 5 行时截断，显示 `[truncated, N bytes]`
- 使用 `TOOL_RESULT_STYLE`（深色底色 `#222222`）

### 4.3 Token 用量统计

AI 回复完成后，在助理消息末尾追加一行统计：

```
[14:32:05 | Tokens: 105 in / 125 out | Tools: 2]
```

- 时间使用 `chrono::Local`（系统时区）
- 文字灰色（`Color::DarkGray`）
- 数据来源：Anthropic API `message_start.input_tokens` + `message_delta.output_tokens`
- Agent 循环累加 `total_tool_calls`

## 5. Markdown 渲染

### 5.1 架构

助理消息（`LineType::Assistant`）经过两阶段渲染：

```
原始 Markdown
  ↓
阶段 1: regex 提取代码块 → syntect 语法高亮 → 替换为 \x00CODEBLOCK_N\x00 标记
  ↓
阶段 2: ratatui-markdown 渲染（标题、列表、表格、粗体/斜体等）
  ↓
阶段 3: 标记替换 → syntect 高亮行嵌入最终输出
```

### 5.2 所用库

| 库 | 用途 |
|---|---|
| `ratatui-markdown` | 通用 markdown 渲染（标题、列表、表格、粗体/斜体等） |
| `syntect` | 代码语法高亮（`base16-ocean.dark` 主题），纯 Rust 实现 |
| `regex` | 提取 fenced code block（````lang\n...`````） |

### 5.3 代码块高亮

- 使用 `syntect::easy::HighlightLines` 逐行高亮
- 每个 token 转换为独立的 `ratatui::Span`，带 RGB 颜色
- 语言识别：`find_syntax_by_token()` + `find_syntax_by_name()`，回退到纯文本
- 支持 30+ 语言（通过 syntect 的 Sublime Syntax 定义）

## 6. 流式文本

- AppState 维护 `streaming_text: String`，累积 TextDelta
- Done 到达时 `flush_streaming()` 创建最终 `LineType::Assistant` 消息
- Usage 统计在 `flush_streaming` 前追加到 `streaming_text`
- 流式渲染使用 `Paragraph` widget，白色前景

## 7. 键盘事件与退出

### 7.1 键盘线程

独立线程使用 `crossterm::event::poll(100ms)` 轮询，而非阻塞 `read()`：
```rust
loop {
    if key_tx.is_closed() { break; }
    if crossterm::event::poll(100ms) {
        if let Ok(event) = crossterm::event::read() {
            if Ctrl+D -> 通过 watch channel 通知主循环退出
            else -> 发送到 key_tx channel
        }
    }
}
```

### 7.2 Ctrl+D 退出

- 键盘线程检测到 Ctrl+D → 通过 `watch::Sender` 发送退出信号
- 主循环 `select!` 中 `exit_rx.changed()` 分支触发退出
- 退出前先发送 Cancel 给 daemon（优雅停止 agent loop）
- 再 `drop(key_tx)` 让键盘线程退出
- `TerminalGuard` Drop guard 保证终端恢复（raw mode + mouse capture）

### 7.3 Ctrl+C

仅在 `generating` 状态时发送 cancel，否则忽略

## 8. 输入区

- `tui-textarea` crate
- Enter 发送 / 命令执行
- ↑↓ 浏览输入历史（App 自行维护 `input_history`）
- `/clear`、`/temp`、`/model`、`/help` 等命令
- 生成中文字变灰，输入被锁定

## 9. 颜色系统

### 9.1 背景色

```rust
const COLOR_BG: Color = #1A1A2E;           // 全局背景
const COLOR_INPUT_BG: Color = #111111;       // 输入区背景
const COLOR_USER_BG: Color = #1A3A5E;       // 用户消息底色
const COLOR_ASSISTANT_BG: Color = #222A3E;   // 助理消息底色
const COLOR_TOOL_RESULT_BG: Color = #222222; // 工具调用/结果底色
const COLOR_SHADOW: Color = #0D0D17;        // block 阴影色
```

### 9.2 前景色

| 类型 | 颜色 |
|---|---|
| User | Cyan |
| Assistant | White |
| Thinking | Green |
| ToolCall 首行 | Yellow |
| ToolCall 后续行 | DarkGray |
| Error | Red |
| Status | Gray |
| Usage / 时间戳 | DarkGray |
| 分隔线 | DarkGray |

## 10. 配置

### 10.1 daemon.toml

```toml
[llm]
model = "claude-sonnet-4-6"
temperature = 0.5
thinking_budget_tokens = 2048  # Claude thinking 模式预算
```

### 10.2 CLI 参数

```bash
visp-cli --model claude-sonnet-4-6 --thinking-budget 2048
```

`thinking_budget_tokens` 通过 `LlmConfig.extra` 传到 Anthropic API 请求体。

## 11. 不做什么

- ❌ 多标签会话
- ❌ 文件树/侧边栏
- ❌ 复杂快捷键（仅 Enter/Ctrl+C/Ctrl+D/PageUp/PageDown/↑↓）
- ❌ 图片渲染（Mermaid 图等）

## 12. 验收标准

- TUI 启动正常，三个区域 + 分隔线可见
- Markdown 渲染：标题、列表、表格、粗体/斜体
- 代码块语法高亮（syntect base16-ocean.dark 主题）
- 工具调用和结果显示在同一 block，颜色区分
- AI 回复末尾显示 token 用量 + 时间戳
- 流式文本实时更新
- 对话区可滚动，无重叠
- Ctrl+D 优雅退出（daemon 不受影响，终端正常恢复）
- Ctrl+C 取消生成
- ↑↓ 浏览输入历史
- Tab 补齐 / 文本选择等高级输入功能
