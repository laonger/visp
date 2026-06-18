# 设计：并行 sub-agent 可观测性与 panic 兜底（策略 A）

## 背景

用户在主 agent 同一轮里调用 2 次 task 工具并行启动 2 个 sub-agent，期望两个 sub-agent 都流式把消息送到主 agent UI、最终都返回结果给主 agent。

实际表现：
1. 流式消息感觉"不及时"
2. 其中一个 sub-agent 看起来"自己终止了"

## 根因（@oracle 诊断结论）

经过对 `crates/visp-agent/src/orchestrator.rs` 与 `crates/visp-core/src/agent_loop.rs` 的源码与日志交叉验证：

### 真相 1：sub-agent 没消失，是日志盲区

- 父 session 最终 `messages=7`（系统 1 + 用户 1 + 助手 1 + 用户 1 + 助手含 2 个 task call 1 + 2 个 ToolResult 2 = 7），证明两个 sub-agent 都成功 ToolResult 回到了父 agent
- `Orchestrator::handle_done`（orchestrator.rs:601-668）整段没有任何 `tracing::info!`
- `agent_loop.rs` 发送 `AgentEvent::Done` 的路径也没有 info 日志
- 结果：sub-agent 正常完成时，日志里根本看不到完成事件，造成"消失了"的错觉

### 真相 2：task 工具是 join_all 语义

- `agent_loop.rs::execute_tool_calls` Phase 2（行 1003-1118）收集 `pending_spawns: Vec<PendingSpawn>`，循环条件是 `regular_done && pending_spawns.is_empty()`
- 必须等所有 sub-agent 都完成才退出循环、返回 ToolResult、继续父 agent 下一轮 LLM
- 结果：A 跑 1 秒 / B 跑 60 秒时，父 agent 阻塞 60 秒，造成"不及时"的感觉

### 真相 3：panic 兜底缺失

- `agent_loop.rs:1201-1364` `catch_unwind` 的 Err 分支调用 `finish_loop` 后 `resume_unwind`
- 没有通过 `global_tx` 把 panic 事件转发给 Orchestrator
- 结果：sub-agent 一旦 panic，父 agent 的 inbox 永远收不到这个 sub-agent 的 `SubAgentComplete`，`pending_spawns` 永不清空，父 agent **死等**

## 设计目标（策略 A 范围）

**只解决"看不见"和"死等"，不改变 task 工具的并发语义。** 让下次再遇到并行 sub-agent 问题时能从日志直接定位根因，且 panic 不会让父 agent 卡死。

具体目标：

1. **sub-agent 生命周期可观测**：spawn → 进度 → done/error 都有 INFO 日志，含 session_id 和 parent_id
2. **panic 兜底**：sub-agent 任意位置 panic 时，父 agent 必定收到 ToolResult（错误内容），不会死等
3. **结构化测试基线**：补一个并行 sub-agent 的集成测试，覆盖：
   - 两个 sub-agent 都正常完成
   - 一个正常完成、一个 panic
4. 不改 task 工具的 join_all 语义、不改 Orchestrator 的 channel 拓扑

## 非目标（明确不做）

- 不实施策略 B（task 工具增量返回）
- 不实施策略 C（独立 fanout channel）
- 不优化"不及时"的体感（这是 join_all 的固有特性，需要策略 B 才能改善）
- 不改 gRPC 协议
- 不改 CLI tab 渲染逻辑

## 改动范围

### 1. orchestrator.rs：handle_done 增加 INFO 日志
- spawn_sub_agent 成功时已有 info（行 124-125 的日志即来自这里），保留
- handle_done 进入时记录：session_id, parent_id（若有）, agent_name, message_count
- 把 SubAgentComplete 投递到 inbox 时记录
- 失败/取消路径同样记录

### 2. agent_loop.rs：Done 路径增加 INFO 日志
- StreamDecision::Done 分支记录：session_id, total_iterations, total_tokens
- collect_stream_events 中 stream 异常结束（None 而非 Done）时已有 warn，保留并补 session_id

### 3. agent_loop.rs：catch_unwind Err 分支转发 panic
- 在 resume_unwind 之前，通过 global_tx 发送一个表示"agent panicked"的 OrchestratorMessage / AgentEvent::Error
- Orchestrator 收到后调用 handle_done 等价路径，把 SubAgentError 注入父 agent 的 inbox
- 父 agent 的 Phase 2 收集循环看到 SubAgentComplete（含错误），从 pending_spawns 移除该项，避免死等

### 4. agent_loop.rs：Phase 2 收集循环 channel 关闭兜底
- 当 `inbox.recv()` 返回 None 但 pending_spawns 非空时，当前是静默 break
- 改为：打印 warn 日志（含未完成的 sub-agent 列表），并对每个未完成的 sub-agent 合成一个错误 ToolResult，让父 agent 不会拿不到结果

### 5. orchestrator.rs：spawn 的 tokio task 加 JoinHandle 监控
- 当前 spawn 后丢弃 JoinHandle
- 改为：保存 JoinHandle 到 Orchestrator 内部 map（key = session_id）
- handle_done 时 .remove() 即可
- 后续在 cancel_agent / drop 时可以选择性 abort（本设计不强制）

### 6. 新增测试

#### 单元测试（orchestrator.rs）
- `test_handle_done_logs_sub_agent_completion`：验证 handle_done 调用后日志或返回值含完成标记
- `test_panic_in_sub_agent_propagates_error_to_parent`：mock provider panic，验证父 inbox 收到 SubAgentError

#### 集成测试（新建 `crates/visp-agent/tests/parallel_sub_agents.rs`）
- `parallel_sub_agents_both_succeed`：两个 sub-agent 都成功，父 agent messages 计数正确，两个 ToolResult 都被消费
- `parallel_sub_agents_one_panics`：A 正常 / B panic，父 agent 在合理时间内收到两个 ToolResult，B 的内容是错误信息

## 数据流（无变化）

```
sub-agent agent_loop
   ↓ AgentEvent (Done/TextDelta/ToolCall/...)
agent_tx (per-agent mpsc)
   ↓
orchestrator forwarding task (orchestrator.rs:554-570)
   ↓ AgentEventFrame
grpc_tx (shared mpsc, capacity 256)
   ↓
gRPC StreamId(5) → CLI tab 路由
```

并行执行视图：
```
父 agent execute_tool_calls Phase 2
  pending_spawns: [child_A, child_B]
  loop {
    inbox.recv() → SubAgentComplete(A) → pending_spawns.remove(A)
    inbox.recv() → SubAgentComplete(B) → pending_spawns.remove(B)  // panic 时由兜底路径合成
    if regular_done && pending_spawns.is_empty() { break }
  }
  return ToolResults [A_result, B_result]
```

策略 A 不修改这个拓扑，只让每一步都有日志、panic 时也有 SubAgentComplete。

## 风险与边界

| 风险 | 处理 |
|------|------|
| 加日志增加 IO，影响吞吐 | 用 `tracing::info!` 仅记录关键生命周期点，不在每个 token 上打印 |
| panic 转发涉及跨 await 点的 unwind 安全 | 使用 catch_unwind 内现有的字符串化错误，发送 String 而非 panic payload |
| JoinHandle map 增加一处共享状态 | 仅 Orchestrator 内部 HashMap，无锁（Orchestrator 是单 actor），改动局部 |
| 集成测试需要 mock LLM provider 的 panic | 使用现有 TestProvider 模式，新增 PanickyProvider |

## 验收标准

- 复现"两个并行 sub-agent" 场景时，daemon 日志能从 INFO 级别看到：
  - `sub agent spawned` × 2
  - `sub agent completed`（或 `sub agent error`） × 2
  - `agent loop completed` × 2 + 父 agent 1 次
- 强制让一个 sub-agent panic 时，父 agent 在 < 5 秒内拿到 ToolResult（不会死等）
- 新增集成测试 2 个全部通过
- `cargo test && cargo clippy -- -D warnings && cargo fmt -- --check` 通过
- 不引入新的 clippy 警告
