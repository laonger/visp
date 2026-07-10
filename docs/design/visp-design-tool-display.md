# 工具展示样式 - 设计文档

## 背景

当前 TUI 中工具调用和结果的显示较为粗糙：

1. **工具调用**：直接显示原始 JSON 参数字符串（如 `📖 read_file {"path":"src/main.rs","start_line":10,"end_line":50}`），`tc_display()` 函数虽存在但是死代码，从未被调用。
2. **工具结果**：`result_summary()` 只实现了 `read_file`，其他工具全部裸输出原始内容，靠 `max_lines_for_tool()` 做简单截断。

## 目标

为每个工具提供定制化的调用参数摘要和结果摘要，让 TUI 展示更简洁、更可读。

---

## 现有工具清单

### Common 工具（category: `common`）

| 工具名 | 图标 | 参数 | 返回数据 |
|--------|------|------|----------|
| `read_file` | 📖 | `path`(必填), `paths`(可选, string[]), `start_line`(可选, int), `end_line`(可选, int) | 文件内容文本，含可选行号范围信息 |
| `write_file` | 📝 | `path`(必填), `content`(必填) | 成功消息 |
| `edit_file` | ✏️ | `path`(必填), `old_string`(必填), `new_string`(必填) | 成功消息 + unified diff |
| `grep` | 🔍 | `pattern`(必填), `path`(可选), `include`(可选), `context`(可选, int), `max_matches`(可选, int) | 匹配行（含 file:line） |
| `glob` | 📂 | `pattern`(必填), `path`(可选) | 匹配文件路径列表 |
| `bash` | 💻 | `command`(必填), `timeout`(可选, int), `workdir`(可选) | stdout+stderr 输出 |

### Network 工具（category: `network`）

| 工具名 | 图标 | 参数 | 返回数据 |
|--------|------|------|----------|
| `fetch_web` | 🌐 | `url`(必填), `timeout`(可选, int) | URL 内容提取为 Markdown 文本 |

### Agent 工具（category: `agent`）

| 工具名 | 图标 | 参数 | 返回数据 |
|--------|------|------|----------|
| `task` | 🔧 | `subagent_type`(必填), `description`(必填), `task_id`(可选) | 子 agent 执行结果 |
| `skill` | 🔧 | `name`(必填) | Skill 指令文本 |

### Analyze 工具（category: `analyze`）

| 工具名 | 图标 | 参数 | 返回数据 |
|--------|------|------|----------|
| `codegraph_rebuild` | 🔎 | 无 | 成功/失败消息 |
| `codegraph_search` | 🔎 | `query`(必填), `limit`(可选, int, 默认20) | 符号搜索结果 |
| `codegraph_get_details` | 🔎 | `name`(必填) | 定义位置、源码、callers、callees |
| `codegraph_context` | 🔎 | `task`(必填), `detail`(可选, 默认"overview"), `max_nodes`(可选, 默认20) | 入口点、调用关系、源码 |
| `codegraph_trace` | 🔎 | `from`(必填), `to`(必填) | 调用链（含 file:line） |
| `codegraph_impact` | 🔎 | `symbol`(必填), `depth`(可选, int, 默认1) | callers 和 callees |

---

## 当前渲染流程

### 数据流

```
ServerMessage::ToolCall { tool_name, arguments(JSON string), call_id }
  → TabEntry::render_pending()
    → push_chat_line(LineType::ToolCall { name: tool_name }, arguments, call_id)
      → ChatLine { content: arguments }  // 原始 JSON 字符串

ServerMessage::ToolResult { call_id, content, is_error, tool_name }
  → push_chat_line(LineType::ToolResult { name }, content, call_id)
    → ChatLine { content: raw_output }
```

### 渲染逻辑（`MessageCache::from_message`）

**ToolCall 渲染** (`app.rs:777`):
- 首行: `format!("{} {} {}", icon, name, content)` — content 是原始 JSON
- 后续行: 原始 JSON 续行
- 最多 5 行，超出截断

**ToolResult 渲染** (`app.rs:810`):
- `max_lines_for_tool(name)` 决定最大行数
- `Some(0)`: 只显示摘要行 `✓ {icon} {name} {summary}`
- 其他: 首行作为状态头 `✓ {icon} {name} {first_line}`，剩余内容高亮显示
- bash 特殊处理: 不加状态头，全部内容直接高亮

### 当前辅助函数

| 函数 | 位置 | 说明 |
|------|------|------|
| `tool_icon(name)` | `app.rs:248` | 工具名→emoji 映射 |
| `max_lines_for_tool(name)` | `app.rs:263` | 结果最大行数限制 |
| `result_summary(name, content)` | `app.rs:274` | 结果摘要（**只实现了 read_file**） |
| `tc_display(tc)` | `event.rs:998` | 格式化工具调用参数（**死代码，从未调用**） |

---

## 设计方案

### 新增模块：`crates/visp-cli/src/tool_display.rs`

包含两个核心函数 + 更新现有函数。

### 1. `format_tool_call(name: &str, args_json: &str) -> String`

将原始 JSON 参数格式化为简洁摘要。解析 JSON 失败时 fallback 显示原始字符串。

| 工具 | 示例参数 | 输出 |
|------|----------|------|
| `read_file` | `{"path":"src/main.rs","start_line":10,"end_line":50}` | `src/main.rs:10-50` |
| `read_file` | `{"path":"src/main.rs"}` | `src/main.rs` |
| `read_file` | `{"paths":["a.rs","b.rs"]}` | `a.rs, b.rs` |
| `write_file` | `{"path":"src/main.rs","content":"..."}` | `src/main.rs` |
| `edit_file` | `{"path":"src/main.rs","old_string":"...","new_string":"..."}` | `src/main.rs` |
| `grep` | `{"pattern":"fn\\s+\\w+","include":"*.rs"}` | `"fn\s+\w+"` `in *.rs` |
| `glob` | `{"pattern":"**/*.rs"}` | `**/*.rs` |
| `bash` | `{"command":"echo hello","timeout":30}` | `$ echo hello` |
| `fetch_web` | `{"url":"https://example.com"}` | `https://example.com` |
| `task` | `{"subagent_type":"fixer","description":"..."}` | `fixer` |
| `skill` | `{"name":"technical-design"}` | `technical-design` |
| `codegraph_search` | `{"query":"parse"}` | `"parse"` |
| `codegraph_get_details` | `{"name":"parse"}` | `parse` |
| `codegraph_context` | `{"task":"auth module"}` | `"auth module"` |
| `codegraph_trace` | `{"from":"main","to":"parse"}` | `main → parse` |
| `codegraph_impact` | `{"symbol":"parse","depth":2}` | `parse` `(depth=2)` |
| `codegraph_rebuild` | `{}` | *(空)* |
| 其他/MCP | 任意 | 原始 JSON 的 string 值拼接 |

### 2. `format_tool_result(name: &str, content: &str, is_error: bool) -> String`

生成结果摘要行。摘要后的完整内容仍按 `max_lines_for_tool` 截断显示。

| 工具 | 成功时 | 错误时 |
|------|--------|--------|
| `read_file` | `Read 1.2KB (30 lines)` | 错误消息首行 |
| `write_file` | `Written 256B to src/main.rs` | 错误消息首行 |
| `edit_file` | `Replaced 1 occurrence in src/main.rs` | 错误消息首行 |
| `grep` | `5 matches` / `No matches` | 错误消息首行 |
| `glob` | `3 files found` / `No files found` | 错误消息首行 |
| `bash` | `exit 0, 128B output` | `exit 1, error` |
| `fetch_web` | `Fetched 5.2KB` | 错误消息首行 |
| `task` | 子 agent 结果首行 | 错误消息首行 |
| `skill` | `Loaded skill: technical-design` | 错误消息首行 |
| `codegraph_*` | 结果首行摘要 | 错误消息首行 |
| 其他/MCP | 内容前 60 字符 | 内容前 60 字符 |

### 3. 更新 `tool_icon(name)` — 补充缺失图标

当前 `task` 和 `skill` 使用默认 🔧，改为：

| 工具 | 旧图标 | 新图标 |
|------|--------|--------|
| `task` | 🔧 | 📋 |
| `skill` | 🔧 | 🎨 |

---

## 改动点

| 文件 | 变更 |
|------|------|
| `crates/visp-cli/src/tool_display.rs` | **新建**：`format_tool_call()` + `format_tool_result()` |
| `crates/visp-cli/src/main.rs` | 添加 `mod tool_display;` |
| `crates/visp-cli/src/app.rs` | `tool_icon()` 补充 task/skill 图标；`result_summary()` 替换为调用 `tool_display::format_tool_result()`；`MessageCache::from_message` 的 ToolCall 分支调用 `format_tool_call()` |
| `crates/visp-cli/src/event.rs` | 删除死代码 `tc_display()` |

### ToolCall 渲染改动（`app.rs:777`）

**Before:**
```rust
let display = if i == 0 {
    format!("{} {} {}", icon, name, content)  // content = 原始 JSON
} else {
    content
};
```

**After:**
```rust
let summary = tool_display::format_tool_call(name, &msg.content);
let display = if i == 0 {
    format!("{} {} {}", icon, name, summary)
} else {
    String::new()  // 摘要通常一行就够了
};
```

ToolCall 不再需要多行换行，摘要控制在单行内（超长由 `wrap_text` 自然折行）。

### ToolResult 渲染改动（`app.rs:810`）

**Before:**
```rust
let summary = result_summary(name, &msg.content);  // 只有 read_file 有摘要
```

**After:**
```rust
let summary = tool_display::format_tool_result(name, &msg.content, is_error);
// 所有工具都有摘要
```

---

## 验证清单

| # | 验证项 | 预期 |
|---|--------|------|
| 1 | `read_file` 调用显示 | `📖 read_file src/main.rs:10-50` |
| 2 | `read_file` 结果显示 | `✓ 📖 read_file Read 1.2KB (30 lines)` |
| 3 | `write_file` 调用显示 | `📝 write_file src/main.rs` |
| 4 | `edit_file` 调用显示 | `✏️ edit_file src/main.rs` |
| 5 | `grep` 调用显示 | `🔍 grep "fn\s+\w+"` |
| 6 | `bash` 调用显示 | `💻 bash $ echo hello` |
| 7 | `bash` 结果显示 | `✓ 💻 bash exit 0, 128B output` |
| 8 | `task` 调用显示 | `📋 task fixer` |
| 9 | `skill` 调用显示 | `🎨 skill technical-design` |
| 10 | `codegraph_trace` 调用显示 | `🔎 codegraph_trace main → parse` |
| 11 | MCP 工具调用 | 显示原始 JSON string 值拼接 |
| 12 | MCP 工具结果 | 显示内容前 60 字符摘要 |
| 13 | JSON 解析失败 | fallback 显示原始参数字符串 |
| 14 | `tc_display()` 已删除 | 编译无 warning |
