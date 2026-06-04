# vibewisp 前置工作设计：Cancel 消息协议扩展

## 1. 目标

为 gRPC Chat 双向流添加 `Cancel` 消息类型，使 CLI 前端能在用户按 Ctrl+C 时通知 daemon 取消当前 Agent 循环。

## 2. 变更范围

| 组件 | 改动 |
|---|---|
| `vibewisp.proto` | 新增 `Cancel` 消息 + `ClientMessage` oneof 新变体 |
| `vbw-daemon service.rs` | Chat handler 中每轮 UserInput 独立 spawn agent task 和转发 task；新增 Cancel 消息处理 |
| `vbw-proto` | 重新编译生成 Rust 代码（自动） |

## 3. Proto 变更

```
ClientMessage oneof 新增字段 4:
  Cancel cancel = 4;

新增消息:
  message Cancel { string session_id = 1; }
```

## 4. Daemon Service 变更

Chat handler 当前在 while 循环中串行接收 ClientMessage。每轮 UserInput spawn 独立的 agent task。

核心变更：Agent 事件转发改为每轮独立 spawn 的转发 task，Chat handler 循环退化为简单的 while let。

```
while let Some(msg) = client_stream.next().await {
    match msg.payload {
        UserInput →
            session_mgr.start_loop(sid) → AgentLoopContext
            创建 mpsc channel
            spawn agent loop task（持有 mpsc sender）
            spawn 转发 task：
                while let Some(event) = agent_rx.recv().await {
                    转 proto ServerMessage → client_tx.send().await
                    Done/Error → finish_loop → break
                }

        Cancel →
            若 session 存在且 Running → 触发 CancellationToken
            （Agent 检测到取消后发 Error(Cancelled)，转发 task 自动 finish_loop）
            若 session 不存在或非 Running → 静默忽略

        ConfigUpdate → 正常处理
        UserResponse → oneshot 回传
    }
}
```

**响应流构建**：

Chat handler 入口处创建 mpsc channel 作为 gRPC 响应流的数据源：

```
let (client_tx, client_rx) = mpsc::channel(16);
// client_tx clone 后传给各转发 task
// client_rx 通过 ReceiverStream 返回给 tonic 作为 gRPC 响应流
Ok(Response::new(ReceiverStream::new(client_rx)))
```

每个转发 task 持有 `client_tx` 的 clone，发送时 `client_tx.send(ServerMessage).await`。所有转发 task 共享同一个 mpsc sender，并发安全（mpsc 支持多 sender）。

**关键变化**：
- Agent 事件转发改为每轮独立 spawn 的转发 task，不与客户端消息循环耦合
- 每轮 UserInput 独立 spawn agent + 转发 task，解决 mpsc 复用问题
- Cancel 无效 session 时静默忽略，不报错
- Agent 循环逻辑无需修改（已有 CancellationToken 支持）
