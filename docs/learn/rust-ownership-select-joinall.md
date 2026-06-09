# Rust 所有权与 `tokio::select!`：`join_all` 的可取消改造

## 问题上下文

在异步 Rust 中，有时需要同时等待多个任务完成，同时又要能响应用户的取消请求。

### 典型场景

Agent 循环在执行工具时，会并行启动多个工具任务，然后等待全部完成：

```rust
// 启动多个工具任务
let mut exec_tasks = Vec::new();
for tc in tool_calls {
    exec_tasks.push(tokio::spawn(async move {
        // ... 执行工具 ...
        result
    }));
}

// 等待全部完成
let task_results = futures::future::join_all(exec_tasks).await;
// ↑ 此处阻塞，无法响应 Ctrl+C
```

**问题**：`join_all(exec_tasks).await` 会阻塞当前 async 函数，直到所有任务完成。如果用户在这期间按 Ctrl+C，希望立即中断，但这段代码无法响应取消信号。

### 目标

把 `join_all` 改造成可被 `CancellationToken` 打断的版本：

```
Ctrl+C 时 → 终止所有正在运行的工具 → 跳过结果处理 → agent 立即退出
```

---

## 问题的症结：所有权

### 直观的想法

`tokio::select!` 可以同时等待多个 future，哪个先完成就处理哪个：

```rust
tokio::select! {
    _ = cancel_token.cancelled() => {
        // 取消：abort 所有任务
        for h in &exec_tasks { h.abort(); }
        Vec::new()
    }
    results = futures::future::join_all(exec_tasks) => {
        // 正常完成
        results.into_iter().filter_map(|r| r.ok()).collect()
    }
}
```

### 编译错误

这段代码**无法编译**。原因是 Rust 的所有权规则：

```
error[E0382]: use of moved value: `exec_tasks`
  │
  │     _ = cancel_token.cancelled() => { ... &exec_tasks ... }
  │                                              --------- borrow occurs here
  │     results = futures::future::join_all(exec_tasks) => { ... }
  │                                         ^^^^^^^^^^ value moved here
```

```
                  ┌─ exec_tasks: Vec<JoinHandle>
                  │
        tokio::select! {
                       │
         ┌─────────────┴─────────────┐
         ▼                           ▼
    cancel 分支                  join_all 分支
         │                           │
         │ 借用 &exec_tasks           │ 需要 exec_tasks 的所有权
         │ （读，不消费）              │ （消费，move）
         │                           │
         └──────────┬────────────────┘
                    ▼
           Rust 编译器：
           一个值不能同时被借用和移动！
```

### 为什么会有这个矛盾

- **cancel 分支**只需要遍历任务列表来 abort 它们，不消费（consume）列表本身——借用即可
- **join_all 分支**需要消费 `Vec<JoinHandle>`，因为 `join_all` 要 take 所有权来 await 每个 JoinHandle
- 但 `tokio::select!` 在编译时并不知道哪个分支会执行——两个分支都必须能编译
- 所以编译器要求：两个分支对 `exec_tasks` 的访问方式必须不冲突

---

## 解决方案：用 `Option` 包装

### 思路

用 `Option<Vec<JoinHandle>>` 包装 `exec_tasks`，利用 `Option::take()` 在运行时确定所有权归属。

```
初始状态:
  exec_tasks: Some(Vec<JoinHandle>)   ← 用 Option 包装

运行时只会有一种情况发生:
  ├─ cancel 先触发:
  │     exec_tasks.take() → Some(handles)
  │     遍历 abort (借用)
  │     join_all 分支被 drop
  │
  └─ join_all 先完成:
        exec_tasks.take() → Some(handles)
        传给 join_all (move)
        cancel 分支被 drop

take() 之后 exec_tasks 变成 None。
另一条分支看到的是 None，不会访问已被取走的数据。
```

### 解决后的代码

```rust
// 1. 用 Option 包装
let mut exec_tasks = Some(exec_tasks);

// 2. select! 中通过 take() 获取所有权
let task_results = tokio::select! {
    biased;
    _ = ctx.cancel_token.cancelled() => {
        // Cancel 先到：abort 所有未完成任务
        if let Some(tasks) = exec_tasks.take() {
            for h in &tasks {
                h.abort();
            }
        }
        // 注意：这里 for h in &tasks 是借用（abort 只需要 &Handle）
        // tasks 在循环结束后被 drop（因为 take() 拿走了所有权）
        Vec::new()
    }
    results = futures::future::join_all(
        exec_tasks.take().unwrap()
    ) => {
        // join_all 先完成：正常处理结果
        results.into_iter().filter_map(|r| r.ok()).collect()
    }
};

// 3. 后续处理结果
for tr in task_results {
    // 如果是 cancel 路径：task_results 为空 Vec，跳过
    // 如果是正常路径：task_results 包含所有结果
}
```

### 为什么这样可行

| 步骤 | cancel 分支 | join_all 分支 |
|------|------------|---------------|
| `exec_tasks` 初始 | `Some(vec)` | `Some(vec)` |
| `.take()` 后 | `Some(vec)` → `None`，本地获得 `vec` | `Some(vec)` → `None`，`vec` 传给 `join_all` |
| 另一条分支看到 | 该分支不会执行（被 select drop） | 该分支不会执行 |
| `exec_tasks` 最终 | `None` | `None` |

- `Option::take()` 是运行时操作，编译时两个分支都能通过
- 两个分支各自拿到 `exec_tasks` 的独立所有权，没有冲突
- 未执行的分支被 `tokio::select!` 自动 drop，不会访问已取走的数据

### 对比：Rust 编译时 vs 运行时所有权

```
编译时（编译器检查）        运行时（实际执行）
─────────────────          ────────────────
两个分支都要能编译          只有一条分支会执行
所以两个分支的代码          take() 确保拿到所有权
都必须符合所有权规则        的分支独占数据
                           另一条分支甚至不会运行
┌──── 编译时 ────┐         ┌──── 运行时 ────┐
│                │         │                │
│  ✅ cancel 分支 │         │  ✅ 一条分支执行  │
│    可行        │         │  ✅ 拿到所有权   │
│                │         │                │
│  ✅ join_all 分 │         │  ❌ 另一条被 drop │
│     支可行     │         │     无需访问数据 │
│                │         │                │
└────────────────┘         └────────────────┘
```

### 其他注意事项

**`biased` 的作用**：
`tokio::select!` 默认是随机选择（公平调度）。加上 `biased;` 后，按书写顺序优先检查——先检查 cancel token，再检查 `join_all`。这样在取消时能更快响应。

**`JoinHandle::abort()` 的行为**：
- 调用 `abort()` 后，tokio 会终止对应的 async task
- 被 abort 的 task 返回 `JoinError::Cancelled`
- 它不会等 task 执行完毕，而是立即取消（类似 Unix `SIGKILL`）
- **注意**：abort 不会等待 task 内部的 drop 或清理代码

**为什么用 `filter_map(|r| r.ok())`**：
- 正常完成的 task：返回 `Ok(result)`，被保留
- 被 abort 的 task：返回 `Err(JoinError::Cancelled)`，被过滤掉
- 因 panic 失败的 task：返回 `Err(JoinError::Panic(...))`，也被过滤掉（建议额外打印日志）

---

## 总结

| 概念 | 说明 |
|------|------|
| **问题** | `join_all` 消费所有权，`select!` 需要两个分支共享 |
| **方案** | `Option::take()` 运行时确定所有权归属 |
| **关键** | 编译时两个分支都能编译，运行时只有一条执行 |
| **优点** | 不改变现有接口，不引入 `unsafe`，纯 safe Rust 实现 |
| **适用场景** | 任何需要在异步等待期间响应取消信号的场合 |
