# Refactor: 拆分 `run_agent_loop`

## 问题

`run_agent_loop` 在 `crates/visp-core/src/agent.rs:337`，约 1000 行。单函数承载了整个 Agent 执行循环的所有逻辑，包括：

- 会话操作（load、append、finish）
- Prompt 构建 + tool guide 渲染
- LLM 流式调用 + 重试
- Stream 事件收集（thinking/text/tool_calls）
- [USER_QUERY] 标记检测与处理
- 工具调用执行（含权限审批、sub-agent 调度）
- Doom loop 检测
- 循环控制（soft/hard limit）

含 9 个标号段落（a-i），内部嵌套多个循环分支和维护大量局部变量。

## 目标

在不改变行为的前提下，将 `run_agent_loop` 拆分为 5-6 个职责清晰的子函数，每个 50-150 行。同时保证：
- 不增加运行时开销
- 不破坏 panic 安全（catch_unwind）
- 不改变测试
- 保持 `visp-core` 零 IO 约束

## 设计方案

### 整体架构

```
run_agent_loop()                    ← 入口，catch_unwind + 循环骨架
├── setup_iteration()               ← a. 取消检查 b. 限制检查 c. prompt 构建
├── call_llm_with_retry()           ← d. LLM 调用（含重试）
├── collect_stream_events()         ← e. 流式事件收集
├── handle_stream_result()          ← f. 决策：USER_QUERY / done / 继续
└── execute_tool_calls()            ← g. 权限审批 + 工具执行
    └── (internal) append_results() ← h. 结果合并回 history
```

### 具体拆分方案

#### 1. 新建 `crates/visp-core/src/agent_loop.rs`（~450 行）

把 `run_agent_loop` 及其子函数移入新文件，原 `agent.rs` 纯导出让渡。

**为什么用新文件而不是 `agent/mod.rs` 模块目录？** `agent.rs` 已有 2926 行，拆出 `agent_loop.rs` 比拆成多个小文件更经济，后续如果需要可以进一步拆。

#### 2. 提取的子函数签名

以下所有函数都定义为 `fn` 而不是 `pub fn`（模块内可见），通过 `run_agent_loop` 唯一公开入口。

```rust
// ── a/b/c: 构建 prompt 并检查边界 ──

struct IterationContext<'a> {
    session: &'a SessionData,
    enriched_template: String,
    messages: Vec<Message>,
    tools: Vec<ToolDefinition>,
}

async fn setup_iteration(
    iteration: u32,
    ctx: &mut AgentLoopContext,
    sm: &SessionManager,
    tool_registry: &ToolRegistry,
    rule_engine: &RuleEngine,
    cfg: &AgentConfig,
    sid: &str,
    tx: &mpsc::Sender<AgentEvent>,
) -> Result<IterationContext, ()>;
```

```rust
// ── d: LLM 调用（含重试）──

async fn call_llm_with_retry(
    provider: &dyn LlmProvider,
    messages: &[Message],
    tools: &[ToolDefinition],
    ctx: &AgentLoopContext,
    cfg: &AgentConfig,
    sid: &str,
    tx: &mpsc::Sender<AgentEvent>,
    sm: &SessionManager,
) -> Result<Pin<Box<dyn Stream<Item = Result<ChatEvent, LlmError>> + Send>>, ()>;
```

```rust
// ── e: 收集流式事件 ──
// 此函数保持同步风格，因为 stream 处理已经是异步的

struct StreamOutput {
    text_buffer: String,
    thinking_blocks: Vec<serde_json::Value>,
    tool_calls: Vec<ToolCallRequest>,
    input_tokens: u32,
    output_tokens: u32,
    cache_creation_input_tokens: u32,
    cache_read_input_tokens: u32,
}

async fn collect_stream_events(
    stream: Pin<Box<dyn Stream<Item = Result<ChatEvent, LlmError>> + Send>>,
    sid: &str,
    tx: &mpsc::Sender<AgentEvent>,
    sm: &SessionManager,
    ctx: &mut AgentLoopContext,
) -> Option<StreamOutput>;  // None = error/abort
```

```rust
// ── f: 处理 USER_QUERY ──

enum StreamDecision {
    Done,
    UserQuery { response_rx: oneshot::Receiver<UserQueryResult> },
    Continue,  // 有 tool_calls
}

async fn handle_stream_result(
    output: &StreamOutput,
    ctx: &mut AgentLoopContext,
    sm: &SessionManager,
    sid: &str,
    tx: &mpsc::Sender<AgentEvent>,
) -> StreamDecision;
```

```rust
// ── g: 执行工具调用 ──

async fn execute_tool_calls(
    tool_calls: &[ToolCallRequest],
    ctx: &mut AgentLoopContext,
    sm: &SessionManager,
    tool_registry: &ToolRegistry,
    sid: &str,
    tx: &mpsc::Sender<AgentEvent>,
    cfg: &AgentConfig,
) -> Vec<ToolExecResult>;
```

#### 3. 主循环骨架

```rust
pub async fn run_agent_loop(/* ... */) {
    // 外层 catch_unwind 不变
    let result = AssertUnwindSafe(async move {
        let (try_send, forward_global) = setup_helpers(&ctx, &tx, &sm, &sid);

        // Pre-loop: append user message
        // ...

        // Initialize doom loop detection
        let mut iteration = 0u32;

        loop {
            // a/b/c: 构建 prompt + 边界检查
            let ic = match setup_iteration(/* ... */).await {
                Ok(ic) => ic,
                Err(()) => return,
            };

            // d: LLM 调用
            let stream = match call_llm_with_retry(/* ... */).await {
                Ok(s) => s,
                Err(()) => return,
            };

            // e: 收集流
            let output = match collect_stream_events(stream, /* ... */).await {
                Some(o) => o,
                None => return,
            };

            // f: 决策
            match handle_stream_result(&output, /* ... */).await {
                StreamDecision::Done => { break; }
                StreamDecision::UserQuery { response_rx } => {
                    // 等待用户响应，继续下一轮
                    // ...
                    continue;
                }
                StreamDecision::Continue => {}
            }

            // g: 执行工具调用
            let results = execute_tool_calls(&output.tool_calls, /* ... */).await;

            // h: 排序 + 追加结果到 history
            append_results(results, ctx, sm, sid);

            iteration += 1;
        }
    });
}
```

#### 4. 复用当前 `try_send!` / `forward_global!` 宏

这两个宏捕获了 `ctx`/`tx`/`sm`/`sid` 四个变量。提取后子函数需要同样的能力。

**方案**：将宏定义提升为外层辅助函数，或直接在每个子函数参数中传入这四个变量。

推荐：每个子函数签名明确接收 `tx: &mpsc::Sender<AgentEvent>` + `sm: &SessionManager` + `sid: &str`。宏只留在 `run_agent_loop` 外层，内部函数用 `.await.map_err(|_| ())?` 或 `?` 传播。

实际上，子函数用 `Result<_, ()>` 返回更方便，调用处检查：

```rust
fn try_send_inner(tx: &mpsc::Sender<AgentEvent>, sm: &SessionManager, sid: &str, event: AgentEvent) -> Result<(), ()> {
    if tx.send(event).await.is_err() {
        let _ = sm.finish_loop(sid, SessionStatus::Error);
        return Err(());
    }
    Ok(())
}
```

#### 5. `event_to_msg` 内联化

`event_to_msg` 只用于 `try_send!` 宏中的 global_tx 转发。提取后可在 `setup_helpers` 中作为闭包维护，或转为文件级私有函数。

### 保留在 `agent.rs` 中的内容

- `AgentEvent` / `AgentMessage` / `OrchestratorMessage` / `Envelope` 类型定义
- `AgentLoopContext` / `AgentConfig` struct 定义
- `AgentExecResult` / `PendingSpawn` 内部结构
- `llm_error_to_code` / `format_tool_args` / `parse_user_query_marker` / `strip_user_query_marker` / `extract_thinking_text` 等辅助函数
- `cleanup_orphan_tool_uses` / `render_tool_guide` / `dump_prompt_to_file`
- 所有测试（`#[cfg(test)] mod tests`）

### 关键边界：变量可见性

当前 `run_agent_loop` 内的局部变量（约 30+ 个）分布在各段落之间共享。提取时需要：

| 变量 | 作用域 | 提取方案 |
|------|--------|---------|
| `sid`, `sm`, `cfg`, `tx` | 整个函数 | 每个子函数参数传入 |
| `iteration` | 循环控制 | 外层 loop 维护，传入 `setup_iteration` |
| `text_buffer`, `tool_calls`, `thinking_blocks` | e 段 | 归入 `StreamOutput` |
| `tool_calls` (继续) | g 段 | 从 `StreamOutput.tool_calls` 获取 |
| `exec_tasks`, `pending_spawns`, `inbox` | g 段 | 归入 `execute_tool_calls` |
| `collected` | g→h 段 | `execute_tool_calls` 返回值 |

无全局或跨轮次状态需要额外处理。

### Doom loop 检测不变

Doom loop 检测逻辑在 g 段末尾，基于 `session_mgr.get_history` 检查。提取时保持这部分逻辑位置不变（在 `execute_tool_calls` 之后、`iteration += 1` 之前）。

## 验证标准

1. **编译通过**：`cargo build -p visp-core`
2. **测试通过**：`cargo test -p visp-core`（全部 ~650+ 测试不变）
3. **逻辑等效**：所有子函数提取后，主循环执行顺序与当前完全一致
4. **行数目标**：`run_agent_loop` 从 ~1000 行降至 ~100 行骨架
5. **无 Clippy 警告**：`cargo clippy -p visp-core -- -D warnings`
6. **格式检查**：`cargo fmt -- --check`

## 实施步骤

| 步骤 | 内容 | 预估行数 |
|------|------|---------|
| 1 | 新建 `agent_loop.rs`，将 `run_agent_loop` 整体移入并确保编译 | +5 / -0 |
| 2 | 提取 `setup_iteration`（a/b/c 段） | ~80 行 |
| 3 | 提取 `call_llm_with_retry`（d 段） | ~60 行 |
| 4 | 提取 `collect_stream_events`（e 段） | ~90 行 |
| 5 | 提取 `handle_stream_result`（f 段） | ~70 行 |
| 6 | 提取 `execute_tool_calls`（g + h 段） | ~200 行 |
| 7 | 主循环骨架瘦身、参数清理、测试 | ~30 行 |
| 8 | 最终清理：`agent.rs` 结构调整、检查编译+测试 | ~10 行 |

每步均为纯提取（Extract Function 重构），不改变逻辑。
