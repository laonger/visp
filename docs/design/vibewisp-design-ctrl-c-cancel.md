# Ctrl+C 中断能力强化设计

## 1. 目标

无论系统处于什么状态（LLM 推理中、工具执行中、确认栏等待中），按下 Ctrl+C 都能立即中断，返回等待用户输入状态。

## 2. 现状与缺口

### 当前 Ctrl+C 路径

```
客户端按 Ctrl+C
  → 发送 Cancel（gRPC）
    → Daemon 取消 token
      → Agent 循环检查 token → 发 Error + Done
        → 客户端 generating = false，回到等待输入
```

### 三个缺口

| 缺口 | 现象 |
|------|------|
| **客户端状态残留** | `streaming_text` / `pending_usage` / `current_request_id` 没清理，Done 到达后可能创建乱序消息 |
| **join_all 阻塞** | agent 循环在 `futures::future::join_all` 上阻塞等待工具执行完成，无法检查 cancel token |
| **pending_queries 残留** | 如果有 UserQuery 正在等用户响应（工具审批或 [USER_QUERY]），Cancel 只取消 token，`resp_rx.await` 的 sender 仍在 pending_queries 中，不会收到 Err(RecvError)，agent 循环卡死 |

## 3. 改动范围

| 层次 | 改动内容 |
|------|---------|
| **vbw-core** | Agent 循环：join_all 改为可被 cancel token 中断 + abort 所有工具 task |
| **vbw-daemon** | Cancel handler：清理 pending_queries（需关联 session_id） |
| **vbw-cli** | Ctrl+C handler：统一行为，清理状态；确认模式下先发 deny 再发 cancel |
| **vbw-proto** | 无改动 |

## 4. 模块详细设计

### 4.1 客户端 — Ctrl+C Handler 统一（按优先级呈现）

当前 Ctrl+C 在两个位置处理：
- 确认栏分支内（event.rs:112-117）：只发 cancel，不清理状态
- 常规生成分支（event.rs:193-197）：只发 cancel，不清理状态

**改造后两个分支统一行为**：

1. 清理流式状态：`streaming_text.clear()`、`pending_usage = None`、`current_request_id = None`
2. 如果有确认栏正在显示：先发 deny 响应（释放 agent 循环中的 `resp_rx.await`），再发 cancel
3. 如果没有确认栏：直接发 cancel

清理状态的目的是：Cancel 后 agent 循环终止会发 Done，Done 的 `flush_streaming()` 和 `pending_usage.take()` 不应再创建任何消息。让 Done 成为一个空操作。

### 4.2 Agent 循环 — join_all 可中断 + abort 工具 task

当前代码：
```
for each tool_call:
  tokio::spawn(async move { ... 执行工具 ... })
task_results = join_all(exec_tasks).await  ← 此处阻塞，无法检查 cancel token
for each result:
  处理结果
```

**改造后**：

用 `tokio::select!` 并行等待 `join_all` 和 cancel token。`exec_tasks` 用 `Option` 包装解决所有权问题：

```rust
let mut exec_tasks = Some(exec_tasks);
let task_results = tokio::select! {
    biased;
    _ = ctx.cancel_token.cancelled() => {
        // Cancel 先到：abort 所有未完成的工具 task
        if let Some(tasks) = exec_tasks.take() {
            for h in &tasks { h.abort(); }
        }
        Vec::new()
    }
    results = futures::future::join_all(exec_tasks.take().unwrap()) => {
        // 正常完成：过滤掉被 abort 的 task
        results.into_iter().filter_map(|r| match r {
            Ok(result) => Some(result),
            Err(e) if e.is_cancelled() => None,  // abort 导致，静默忽略
            Err(e) => {
                tracing::warn!("tool task failed: {e}");
                None
            }
        }).collect()
    }
};
```

`Option::take()` 在运行时确定 `exec_tasks` 的所有权归属——cancel 分支和 `join_all` 分支只有一条会执行到 `.take()`，另一条被 select 丢弃。详见 `docs/learn/rust-ownership-select-joinall.md`。

`JoinHandle::abort()` 会真正终止 tokio task，不会让工具在后台空转。已被 abort 的 task 返回 `JoinError::Cancelled`，被 `filter_map` 过滤掉；其他 panic 等异常打印 warn 日志。

### 4.3 Daemon — Cancel 时清理 pending_queries（双重保险）

当前 daemon 的 `get_codegraph` 方法中有一个 `pending_queries: Arc<Mutex<HashMap<String, Sender<UserQueryResult>>>>`（按 query_id 索引）。当 Cancel 到达时，只做了 `session_mgr.cancel_agent(sid)`（取消 token），但没清理 pending_queries。

改造：
1. `pending_queries` 的 value 类型从 `Sender<UserQueryResult>` 改为 `(String, Sender<UserQueryResult>)`，其中 `String` 是 `session_id`
2. Cancel handler 中：从 `pending_queries` 中删除所有匹配 `session_id` 的条目
3. sender 被 drop → 对应的 `resp_rx.await` 收到 `Err(RecvError)` → `unwrap_or_default()` 返回默认值 → agent 循环不再卡死

客户端 A 方案（先发 deny 再发 cancel）已经能确保 `resp_rx` 被释放，C 方案是双重保险，防止极端时序下 deny 先被处理但 agent 循环已因其他原因退出的情况。

### 4.4 边界情况

- **Ctrl+C 时没有生成任务（idle）**：不做任何事
- **Ctrl+C 后快速按 Enter 发新消息（竞态）**：在 `AppState` 中增加 `stale_done_expected: bool` 标志位。Cancel 时设 true，Done 处理时检查：
  ```
  if stale_done_expected:
      stale_done_expected = false
      return  // 跳过这个陈旧的 Done
  ```
  新请求发起时不影响该标志，新请求的 Done 到达时标志已被重置为 false，正常处理。**
- **连续按 Ctrl+C**：幂等，第二次取消无效果（token 已取消）
- **多工具并行 + 一个已执行完**：abort 会终止所有未完成的 task，已完成的 task 结果被丢弃

## 5. 不做什么

- ❌ 不修改 Tool trait 或 ToolContext（工具本身不需要感知取消，由外层 abort 终止）
- ❌ 不改动 gRPC 协议
- ❌ 不影响 Esc 的当前行为（Esc 在确认栏中仍保留 deny + cancel + 状态清理）
- ❌ 不支持 Ctrl+C 撤销已完成的工具（已完成的结果已被追加到历史中）

## 6. 验收标准

1. **推理中 Ctrl+C**：立即中断，回到空闲状态，可输入新消息
2. **工具执行中 Ctrl+C**：工具被 abort，不继续空转，agent 终止
3. **确认栏中 Ctrl+C**：确认栏消失，回到空闲状态，可输入新消息
4. **空闲状态下 Ctrl+C**：无任何效果
5. **多工具并行中 Ctrl+C**：所有未完成工具全部 abort
6. **Done 不残留**：Cancel 后的 Done 不创建任何额外消息
7. **连续 Ctrl+C**：无 panic 或异常
8. **测试通过**：`cargo test` 全部 green
9. **Clippy 零警告**

## 7. 拆分策略

**步骤 1：Client Ctrl+C 状态清理**
- event.rs：统一 confirm 内外的 Ctrl+C handler，清理 streaming_text/pending_usage/current_request_id
- confirm 模式下先发 deny 再发 cancel

**步骤 2：Agent 循环 join_all 可中断**
- agent.rs：用 `tokio::select!` 替代 `join_all`，abort 所有 task

**步骤 3：Daemon pending_queries 清理**
- service.rs：pending_queries value 加 session_id，Cancel handler 中匹配删除
