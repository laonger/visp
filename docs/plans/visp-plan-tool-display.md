# 工具差异化显示 — 实施计划

## 影响范围

| 文件 | 变更 |
|------|------|
| `visp-proto/proto/visp.proto` | ToolResult 新增 `tool_name` 字段 |
| `visp-core/src/agent.rs` | AgentEvent::ToolCallResult 新增 `tool_name` 字段 |
| `visp-daemon/src/service.rs` | agent_event_to_server_message + 测试 |
| `visp-cli/src/app.rs` | LineType 枚举、MessageCache 截断策略、icon/显示策略函数 |
| `visp-cli/src/event.rs` | ToolResult 处理改为独立消息 |
| `visp-cli/src/theme.rs` | BlockStyle 拆分、图标映射 |

## Wave 1: 数据链路（proto → agent → daemon）

为 `ToolResult` 补充 `tool_name`，确保从 agent 循环到客户端全线传递。

### T1-1: proto 加字段
- `visp.proto`: `ToolResult` 新增 `tool_name = 5`

### T1-2: AgentEvent 加字段
- `visp-core/src/agent.rs`: `ToolCallResult { tool_name: String, .. }`
- 更新所有 `send(AgentEvent::ToolCallResult { ... })` 调用点（3 处：lines 649, 707, 744），传入 `tool_name`

### T1-3: daemon 转发
- `service.rs`: `agent_event_to_server_message` 中 `ToolCallResult` → `proto::ToolResult` 增加 `tool_name`

### T1-4: 编译通过 + 测试
- `cargo test -p visp-proto -p visp-core -p visp-daemon` 通过

## Wave 2: CLI 数据结构（可独立测试）

### T2-1: LineType 枚举改造
```rust
pub enum LineType {
    User,
    Assistant,
    Thinking,
    ToolCall { name: String },    // 含工具名
    ToolResult { name: String },
    ToolError { name: String },
    Error,
    Status,
    Usage,
}
```

### T2-2: icon 映射函数
`theme.rs` 新增：
```rust
pub fn tool_icon(name: &str) -> &'static str   // 返回 emoji
pub fn tool_label(name: &str) -> &'static str  // 返回显示名
```

### T2-3: 截断策略函数
`app.rs` 新增：
```rust
fn max_lines_for_tool(name: &str) -> Option<usize>
// None = 不截断, Some(0) = read_file 不显示结果, Some(N) = 最多 N 行
fn result_summary(name: &str, content: &str) -> String
// 返回 read_file 类的摘要行（"Read 847 bytes (42 lines)"）
```

### T2-4: MessageCache 改造
`MessageCache::from_message` 中：
- ToolCall：首行显示 `icon name: "参数摘要"`
- ToolResult：根据 `tool_icon + name` 决定是否显示内容、截断行数
- ToolError：红色显示，不截断

### T2-5: 测试
测试每种工具类型的显示截断策略

## Wave 3: 事件处理 + 渲染

### T3-1: event.rs — ToolResult 独立消息
```rust
Some(server_message::Payload::ToolResult(tr)) => {
    // 查找 tool_name：优先从 proto 取，fallback 从已有消息按 call_id 找
    let name = tr.tool_name.clone().unwrap_or_else(|| {
        app.messages.iter()
            .find(|m| matches!(m.line_type, LineType::ToolCall { .. }) && m.call_id.as_deref() == Some(&tr.call_id))
            .map(|m| /* extract name from LineType */)
            .unwrap_or_default()
    });
    if tr.is_error {
        app.add_tool_line(LineType::ToolError { name }, tr.content, &tr.call_id);
    } else {
        app.add_tool_line(LineType::ToolResult { name }, tr.content, &tr.call_id);
    }
}
```

### T3-2: theme.rs — BlockStyle 拆分
- `TOOL_CALL_STYLE`: 完整阴影，margin_vertical=1
- `TOOL_RESULT_STYLE`: 缩进 2，无阴影
- `TOOL_ERROR_STYLE`: 红色底色版

### T3-3: theme.rs — style_for 更新
```rust
pub const fn style_for(line_type: &LineType) -> BlockStyle {
    match line_type {
        LineType::ToolCall { .. } => TOOL_CALL_STYLE,
        LineType::ToolResult { .. } => TOOL_RESULT_STYLE,
        LineType::ToolError { .. } => TOOL_ERROR_STYLE,
        // ...
    }
}
```

### T3-4: 端到端测试
```bash
cargo test && cargo clippy -- -D warnings && cargo fmt -- --check
```

## 验证清单

| # | 验证项 | 预期 |
|---|--------|------|
| 1 | `read_file` 结果 | 只显示 "Read N bytes (M lines)"，无文件内容 |
| 2 | `edit_file` 结果 | 完整显示 diff，不截断 |
| 3 | `write_file` 结果 | 完整显示，不截断 |
| 4 | `bash` 结果 | 最多 30 行，超出显示 `[truncated, N more lines]` |
| 5 | `grep` 结果 | 最多 20 行 |
| 6 | `glob` 结果 | 最多 15 行 |
| 7 | `fetch_web` 结果 | 最多 20 行 |
| 8 | `codegraph_*` 结果 | 最多 20 行 |
| 9 | 错误工具调用 | 红色显示，不截断 |
| 10 | ToolCall 有阴影 | 完整框+阴影 |
| 11 | ToolResult 缩进 | 右侧缩进 2 格，无阴影 |
