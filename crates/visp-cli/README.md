# visp-cli — TUI 客户端

基于 ratatui 的终端界面客户端，通过 gRPC 连接 daemon 提供交互式对话体验。

## 关键文件

- `client.rs` — gRPC 客户端封装（VbwClient / ChatHandle）
- `app.rs` — 应用状态与 Markdown 渲染
- `event.rs` — 事件循环（键盘 + gRPC 消息）
- `ui.rs` — ratatui 渲染（对话区 / 输入区 / 状态栏）

## 依赖

- `visp-proto`（gRPC 客户端）

## 测试

```bash
cargo test -p visp-cli
```
