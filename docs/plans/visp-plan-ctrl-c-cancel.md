# 工作计划：Ctrl+C 中断能力强化

## 概述

三步独立改造，可全并行。每个步骤修改不同的 crate，互不依赖。

## Wave 并行策略

```
Wave 1 (全并行)        Wave 2 (串行)
┌──────────────┐      ┌──────────────┐
│ 1. Client     │      │ 4. 集成验证   │
│ (event.rs)    │      │              │
├──────────────┤      └──────────────┘
│ 2. Core       │
│ (agent.rs)    │
├──────────────┤
│ 3. Daemon     │
│ (service.rs)  │
└──────────────┘
       │
       └── 全部完成 → 集成验证
```

## 步骤 1：Client — Ctrl+C / Esc 统一 + stale_done_expected

### 1a：AppState 新增字段 + Esc handler 更新

#### 🔴 红 — 测试

| # | 测试用例 | 说明 |
|---|---------|------|
| 1.1 | AppState 初始化后 `stale_done_expected` 为 false | 默认值 |
| 1.2 | 新建 ConfirmState 不影响 stale_done_expected | |

#### 🟢 绿 — 实现

在 `crates/visp-cli/src/app.rs` 中：

- `AppState` 增加 `pub stale_done_expected: bool` 字段
- 在 `AppState::new()` 中初始化 `stale_done_expected: false`

在 `crates/visp-cli/src/event.rs` 中修改 **Esc handler**（`else` 分支，非 Other 模式）：

```rust
} else {
    let q = app.confirm.take().unwrap();
    chat_handle.send_response(&q.query_id, 1, "");
    if app.generating {
        app.stale_done_expected = true;        // 新增
        app.streaming_text.clear();
        app.pending_usage = None;
        app.current_request_id = None;
        chat_handle.send_cancel();
    }
}
```

#### 🧪 测试 → 🔍 类型检查

```bash
cargo test -p visp-cli
cargo clippy -p visp-cli -- -D warnings
```

#### 📦 提交

`feat(cli): add stale_done_expected flag, set on Esc cancel`

---

### 1b：Ctrl+C handler 统一 + Done handler 防竞态

#### 🔴 红 — 测试

| # | 测试用例 | 说明 |
|---|---------|------|
| 2.1 | Ctrl+C 在 confirm 模式下设 stale_done_expected + 清理状态 + 发 deny + 发 cancel | 覆盖所有操作 |
| 2.2 | Ctrl+C 在非 confirm 模式下设 stale_done_expected + 清理状态 + 发 cancel | 不含 deny |
| 2.3 | Ctrl+C 在 idle 状态下不做任何事 | |
| 2.4 | Done handler 最先检查 stale_done_expected，为 true 时跳过并重置 | |
| 2.5 | stale_done_expected 为 true 时 Done 不创建任何消息 | 回归验证 |

#### 🟢 绿 — 实现

**修改两个 Ctrl+C handler**（event.rs），统一行为：

1. 设置 `stale_done_expected = true`
2. `streaming_text.clear()`
3. `pending_usage = None`
4. `current_request_id = None`
5. 如果 confirm 存在：`send_response(deny)` + `send_cancel()`
6. 如果 confirm 不存在且 generating：`send_cancel()`
7. 如果 idle（!generating）：不做任何事

**修改 Done handler**，在最开头插入 stale_done_expected 检查：

```rust
Some(server_message::Payload::Done(_)) => {
    if app.stale_done_expected {
        app.stale_done_expected = false;
        return;
    }
    // ...原有逻辑...
}
```

#### 🧪 测试 → 🔍 类型检查

```bash
cargo test -p visp-cli
cargo clippy -p visp-cli -- -D warnings
```

#### 📦 提交

`feat(cli): unify Ctrl+C handler, skip stale Done via stale_done_expected`

---

## 步骤 2：Core — Agent 循环两个阻塞点可中断

### 2a：LLM stream 改为 tokio::select!

#### 🔴 红 — 测试

| # | 测试用例 | 说明 |
|---|---------|------|
| 3.1 | LLM stream 正常迭代时收到 cancel token → 立即终止并发 Error + Done | 现有取消测试扩展 |
| 3.2 | LLM stream 正常完成 → 行为不变 | 回归 |
| 3.3 | cancel token 在 stream 迭代间隙被触发 → 正确退出 | |

#### 🟢 绿 — 实现

在 `crates/visp-core/src/agent.rs` 中，将 LLM 事件收集循环（约 384-422 行）从 `while let` 改为 `tokio::select!`：

```rust
let mut pin_stream = Box::pin(stream);
loop {
    tokio::select! {
        biased;
        _ = ctx.cancel_token.cancelled() => {
            try_send!(AgentEvent::Error {
                code: AgentErrorCode::Cancelled,
                message: "Agent loop cancelled".into(),
            });
            let _ = session_mgr.finish_loop(&ctx.session_id, SessionStatus::Error);
            return;
        }
        event = pin_stream.next() => {
            match event {
                Some(Ok(ChatEvent::TextDelta(delta))) => { ... }
                Some(Ok(ChatEvent::Done)) => break,
                Some(Err(e)) => { ... }
                None => break,
                // ...其他事件...
            }
        }
    }
}
```

注意：原有 match 逻辑不变，只是外层从 `while let` 改成 `loop + tokio::select!`。

#### 🧪 测试 → 🔍 类型检查

```bash
cargo test -p visp-core
cargo clippy -p visp-core -- -D warnings
```

#### 📦 提交

`feat(core): make LLM stream interruptible via tokio::select! + cancel token`

---

### 2b：join_all 改为 tokio::select! + Option + abort

#### 🔴 红 — 测试

| # | 测试用例 | 说明 |
|---|---------|------|
| 4.1 | 工具正常执行完成 → join_all 结果正常处理 | 回归 |
| 4.2 | 工具执行中 cancel → abort 所有 task → 返回空结果 | |
| 4.3 | 多工具并行 + cancel → abort 所有 | |
| 4.4 | 工具正常完成 + 另一个 panic → panic 打印 warn 日志 | |

#### 🟢 绿 — 实现

在 `crates/visp-core/src/agent.rs` 中，修改约 670-671 行：

```rust
// 用 Option 包装 exec_tasks
let mut exec_tasks = Some(exec_tasks);

let task_results = tokio::select! {
    biased;
    _ = ctx.cancel_token.cancelled() => {
        if let Some(tasks) = exec_tasks.take() {
            for h in &tasks { h.abort(); }
        }
        Vec::new()
    }
    results = futures::future::join_all(
        exec_tasks.take().unwrap()
    ) => {
        results.into_iter().filter_map(|r| match r {
            Ok(result) => Some(result),
            Err(e) if e.is_cancelled() => None,
            Err(e) => {
                tracing::warn!("tool task failed: {e}");
                None
            }
        }).collect()
    }
};

// 后续的 sorted_results / for tr in sorted_results 不变
```

注意：原有结果处理逻辑（sorted_results、append to history）保持不变。

#### 🧪 测试 → 🔍 类型检查

```bash
cargo test -p visp-core
cargo clippy -p visp-core -- -D warnings
```

#### 📦 提交

`feat(core): make join_all cancellable via select! + abort, use Option for ownership`

---

## 步骤 3：Daemon — Cancel 时清理 pending_queries

### 3a：pending_queries value 类型扩展

#### 🔴 红 — 测试

| # | 测试用例 | 说明 |
|---|---------|------|
| 5.1 | pending_queries insert 携带 session_id | 编译检查 |
| 5.2 | Cancel handler 删除匹配 session_id 的条目 | |
| 5.3 | 非匹配 session_id 的条目不受影响 | |
| 5.4 | UserResponse 处理时正确取出 sender（忽略 session_id） | |

#### 🟢 绿 — 实现

在 `crates/visp-daemon/src/service.rs` 中：

**修改 pending_queries 类型声明**（约 174 行）：

```rust
// 旧: Arc<Mutex<HashMap<String, oneshot::Sender<UserQueryResult>>>>
// 新: Arc<Mutex<HashMap<String, (String, oneshot::Sender<UserQueryResult>)>>>
// 其中 (session_id, sender)
```

**修改 insert 点**（约 311 行）：

```rust
pq.lock().await.insert(query_id.clone(), (sid2.clone(), respond));
```

**修改 remove 点**（约 387 行）：

```rust
let sender = pending_queries.lock().await.remove(&resp.query_id);
if let Some((_sid, sender)) = sender {
    let _ = sender.send(UserQueryResult {
        selected_index: resp.selected_index,
        text: resp.text,
    });
}
```

**Cancel handler 增加清理**（约 411-421 行）：

```rust
Some(proto::client_message::Payload::Cancel(cancel)) => {
    let sid = &cancel.session_id;
    match session_mgr.get(sid) {
        Ok(s) if s.status == SessionStatus::Running => {
            session_mgr.cancel_agent(sid);
            running_sessions.retain(|id| id != sid);
            // 清理该会话的所有 pending queries
            let mut pq = pending_queries.lock().await;
            pq.retain(|_, (sess_id, _)| sess_id != sid);
        }
        _ => {}
    }
}
```

#### 🧪 测试 → 🔍 类型检查

```bash
cargo test -p visp-daemon
cargo clippy -p visp-daemon -- -D warnings
```

#### 📦 提交

`feat(daemon): clean up pending_queries on Cancel, store session_id in value`

---

## 步骤 4：集成验证

```bash
cargo test
cargo clippy -- -D warnings
cargo fmt -- --check
```

无额外提交。

---

## 测试覆盖汇总

| Wave | 并行数 | 步骤 | 测试用例数 | 涉及文件 |
|------|--------|------|-----------|---------|
| Wave 1 | 3 | 1a-1b | ~9 | app.rs, event.rs |
| Wave 1 | 3 | 2a-2b | ~7 | agent.rs |
| Wave 1 | 3 | 3a | ~4 | service.rs |
| Wave 2 | 串行 | 4 | — | — |

## 备注

- **步骤 1 和 2 的测试主要以现有测试的回归为主**，新增的 Ctrl+C 功能难以在单元测试中完全模拟（涉及 gRPC 流交互）。人工验证为主。
- **`Option::take()` 所有权模式** 详见 `docs/learn/rust-ownership-select-joinall.md`。
- **Bash 子进程可能变孤儿**：abort 的已知限制，见设计文档 4.4。
- **步骤 1b 的 Done handler 修改需要小心**：检查逻辑必须放在 `pending_usage.take()` 之前，否则会误操作新请求的状态。
