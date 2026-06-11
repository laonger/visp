# CLI 数据流：从 gRPC 接收到终端渲染

## 文件结构

CLI 接收和渲染涉及三个主要文件：

| 文件 | 职责 |
|------|------|
| `event.rs` | 主事件循环、gRPC 消息处理、键盘事件处理 |
| `app.rs` | `AppState` 状态管理（含 `streaming_text`、消息列表） |
| `ui.rs` | 最终的 ratatui 渲染（`render()` 函数） |

## 主循环结构

`event.rs` 第 133 行，`tokio::select!` 监听三个来源：

```
loop {
    tokio::select! {
        event = key_rx.recv() => { handle_key_event(...) }  // 键盘事件
        msg = chat_handle.recv() => { handle_grpc_message(...) }  // ← gRPC 消息
        _ = exit_rx.changed() => { ... }  // Ctrl+D 退出
    }
    // ↓ 每次 select 结束后，检查是否需要渲染
    if app.needs_render {
        ...
        let _ = terminal.draw(|f| render(&mut app, f));
        app.needs_render = false;
    }
}
```

## 完整渲染链路

```
daemon 推送 ServerMessage (gRPC)
        │
        ▼
chat_handle.recv() 收到消息
        │
        ▼
handle_grpc_message(msg, app, chat_handle)
  ├─ app.needs_render = true          ← 标记"需要渲染"
  ├─ TextDelta   → app.append_streaming(&delta.delta)
  ├─ ToolCall    → app.flush_streaming(); app.add_tool_line(...)
  ├─ ToolResult  → app.insert_tool_result(...)
  ├─ Thinking    → app.flush_streaming(); app.add_message(...)
  ├─ UsageInfo   → app.pending_usage = Some(...)
  └─ Done        → app.finish_streaming()
        │
        ▼
tokio::select! 本轮结束，进入渲染检查
        │
        ▼
流式节流检查（for TextDelta only）
  try_begin_stream_render() 时间窗口控制
  ├─ 未到时间 → app.needs_render = false，跳过本次
  └─ 通过     → 继续
        │
        ▼
terminal.draw(|f| render(&mut app, f))
        │
        ▼
render() 读取 AppState 中所有数据
  ├─ app.messages        → 历史消息列表
  ├─ app.streaming_text  → 当前正在流式输出的文本
  ├─ app.textarea        → 用户输入区
  ├─ app.confirm         → 确认栏状态
  └─ app.pending_usage   → token 用量
        │
        ▼
ratatui Frame::render_widget 绘制到终端
```

## 关键细节

### 1. gRPC 消息只更新状态，不直接渲染

`TextDelta` 等消息**仅**调用 `app.append_streaming()` 将增量文本追加到 `app.streaming_text` 缓冲区，**不会触发任何绘制**。实际的像素级绘制在后续的 `terminal.draw()` 统一完成。

### 2. 流式渲染有节流控制

```rust
// event.rs 第 177 行
if app.generating && app.confirm.is_none() && !app.try_begin_stream_render() {
    app.needs_render = false;  // 跳过本次渲染
}
```

`try_begin_stream_render()` 用时间窗口控制帧率（~30-60ms 一帧），避免每收到一个字符就重绘一次终端。

### 3. 确认状态不受流节流影响

```rust
// 确认状态始终需要渲染，不受流节流影响
if app.generating && app.confirm.is_none() && !app.try_begin_stream_render() {
    // ^^^^^^^^^^^^^^^^  confirm.is_none() 时才会节流
```

当有 `confirm` 弹窗时，每次 gRPC 消息都会触发渲染，保证用户操作响应及时。

### 4. 渲染区域划分

`ui.rs` 的 `render()` 将终端分为三个区域：

```
┌──────────────────────┐
│   对话区（消息列表）   │  ← 包含 streaming_text，经 syntect 语法高亮
├──────────────────────┤
│     ─── 分隔线 ───    │
├──────────────────────┤
│  确认栏 / 输入区      │
│  状态栏（token 用量）  │
└──────────────────────┘
```

### 5. 输入框的实现

用户输入使用 `ratatui_textarea::TextArea`，粘贴事件（bracketed paste）通过 `paste_text()` 函数逐字符模拟输入（`\n` 映射为 `Key::Enter` 以正确处理换行）。

## 一句话总结

**gRPC 消息 → handle_grpc_message 更新 AppState → 主循环检查 needs_render → 节流控制 → terminal.draw() → render() 将 AppState 绘制到屏幕。**
