# visp 工作计划：拆分 `run_agent_loop`

## 概述

将 `crates/visp-core/src/agent.rs` 中约 979 行的 `run_agent_loop` 函数拆分为 5-6 个职责清晰的子函数，移入新建的 `agent_loop.rs`。

**约束**：
- 不改变行为（纯 Extract Function 重构）
- 不增加运行时开销
- 不破坏 panic 安全（catch_unwind）
- 不修改测试（~1432 行测试原封不动）
- `visp-core` 零 IO 约束不变

## 步骤

### 步骤 0：新建 `agent_loop.rs`，移入 `run_agent_loop`

#### 🔴 红 — 测试

| # | 测试用例 | 描述 |
|---|---------|------|
| 0 | 现有测试全部通过 | 移动前 `cargo test -p visp-core` 通过，作为基准线 |

无新测试。纯文件移动，行为完全不变。

#### 🟢 绿 — 实现

1. 新建 `crates/visp-core/src/agent_loop.rs`
2. 将 `agent.rs:256-1313`（`run_agent_loop` + 所有辅助函数 + 变量定义到 `UserQueryMarker`/`parse_user_query_marker`/`strip_user_query_marker`/`extract_thinking_text`）整体移入 `agent_loop.rs`
3. 在 `agent_loop.rs` 文件头添加 `use` 声明（从 `agent.rs` 的现有 import 中挑选需要的）
4. 在 `agent.rs` 末尾添加 `mod agent_loop;`（该 mod 内的函数对 `agent.rs` 不可见——需要额外处理）
5. **关键问题**：`run_agent_loop` 需要访问 `agent.rs` 中的类型（`AgentEvent`、`AgentLoopContext`、`AgentConfig`、`ToolExecResult`、`PendingSpawn` 等）和函数（`cleanup_orphan_tool_uses`、`render_tool_guide`、`dump_prompt_to_file`）。方案：
   - `agent_loop.rs` 中 `use super::*` 或逐个 `use crate::agent::XXX` 引用
   - 或保持类型定义在 `agent.rs`，`agent_loop.rs` 通过 `crate::agent::XXX` 访问
   - **推荐**：`agent_loop.rs` 以 `use super::*` + `use crate::error::*` + `use crate::{message, prompt, session, tool, tool_registry}` 方式导入，所有类型保持在 `agent.rs` 中 `pub`/`pub(crate)`
6. 调整可见性：`cleanup_orphan_tool_uses`、`render_tool_guide`、`dump_prompt_to_file`、`parse_user_query_marker`、`strip_user_query_marker`、`extract_thinking_text` 需要 `pub(crate)` 或移动到 `agent_loop.rs`
7. 在 `lib.rs` 中添加 `pub mod agent_loop;`

**方案选择**：
- **选项 A（推荐）**：将辅助函数（`cleanup_orphan_tool_uses` 等）也移入 `agent_loop.rs`，因为它们只被 `run_agent_loop` 使用。类型定义（`AgentEvent`、`AgentLoopContext` 等）留在 `agent.rs`，设为 `pub(crate)`。
- **选项 B**：辅助函数留在 `agent.rs`，设为 `pub(crate)`，`agent_loop.rs` 通过 `super::` 引用。

**推荐 A**，因为辅助函数本质上也是 agent 循环的一部分，留在 `agent.rs` 只是为了减少移动量。但选项 A 更干净。

#### 🧪 测试 → 🔍 类型检查

```bash
cargo test -p visp-core       # 全部通过 = 移动成功
cargo clippy -p visp-core -- -D warnings  # 零警告
cargo fmt -- --check          # 格式正确
```

#### ♻️ 重构

检查 `agent_loop.rs` 中 `use` 声明是否有冗余项（移入后可能某些 import 不再需要）。

#### 📦 提交

```
refactor(core): move run_agent_loop to new agent_loop.rs
```

---

### 步骤 1：提取 `setup_iteration`（原 a/b/c 段）

#### 🔴 红 — 测试

| # | 测试用例 | 描述 |
|---|---------|------|
| 1 | 现有测试全部通过 | 回归验证，纯提取不改变行为 |

无新测试。现有 ~650 个测试保证正确性。

#### 🟢 绿 — 实现

从 `run_agent_loop` 循环体开头提取以下逻辑到 `setup_iteration` 函数：

1. **a 段**（cancel check）：`if ctx.cancel_token.is_cancelled() { ... }`
2. **b 段**（limits check）：`if iteration >= cfg.hard_limit { ... }` + soft limit 注入
3. **c 段**（prompt build）：
   - `sm.get(sid)` 获取 session
   - `render_tool_guide` + 拼接 `enriched_template`
   - `PromptBuilder::build` 构建 messages
   - 获取 `tool_registry.definitions()`

函数签名（与设计文档一致）：

```rust
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

注意：`SessionData` 类型在 `session.rs` 中定义，需要确认其 `pub` 可见性。如果是 `pub(crate)` 则可直接使用引用。

**边界处理**：
- 取消时：发送 Cancel 事件，finish_loop，返回 `Err(())`
- 超限时：发送 MaxIterations 事件，finish_loop，返回 `Err(())`
- 获取 session 失败时：发送 Internal 错误，finish_loop，返回 `Err(())`
- 正常：返回 `Ok(IterationContext)`

#### 🧪 测试 → 🔍 类型检查

```bash
cargo test -p visp-core
cargo clippy -p visp-core -- -D warnings
```

#### ♻️ 重构

- 检查 `IterationContext` 是否可以被简化（考虑直接用 tuple 或省略不需要的字段）
- 检查调用处错误处理是否简洁

#### 📦 提交

```
refactor(core): extract setup_iteration from run_agent_loop
```

---

### 步骤 2：提取 `call_llm_with_retry`（原 d 段）

#### 🔴 红 — 测试

| # | 测试用例 | 描述 |
|---|---------|------|
| 2 | 现有测试全部通过 | 回归验证 |

#### 🟢 绿 — 实现

提取 LLM 调用逻辑（含重试循环）到独立函数：

```rust
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

**提取范围**：从 `let stream = { let mut attempt = 0u32; loop { ... } };` 整体提取。
**边界处理**：
- 所有错误路径（RateLimit 重试耗尽、Network 重试耗尽、Auth/Api/Stream 不可重试错误）→ 发送错误事件，`Err(())`
- 成功 → `Ok(stream)`

#### 🧪 测试 → 🔍 类型检查

```bash
cargo test -p visp-core
cargo clippy -p visp-core -- -D warnings
```

#### ♻️ 重构

- 考虑重试逻辑是否可以用 `retry` crate 简化？**不需要**——当前逻辑简单清晰，且为 `visp-core` 零 IO 约束
- 检查 `Pin<Box<dyn Stream<...>>>` 是否可以用 type alias 简化

#### 📦 提交

```
refactor(core): extract call_llm_with_retry from run_agent_loop
```

---

### 步骤 3：提取 `collect_stream_events`（原 e 段）

#### 🔴 红 — 测试

| # | 测试用例 | 描述 |
|---|---------|------|
| 3 | 现有测试全部通过 | 回归验证 |

#### 🟢 绿 — 实现

提取流式事件收集逻辑：

```rust
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
) -> Option<StreamOutput>;
```

**提取范围**：从 `let mut text_buffer = String::new();` 到 `}`（stream 消费循环结束）。
**边界处理**：
- `cancel_token.cancelled()` 抢占 → 发送 Cancelled 事件，`None`
- Stream 正常结束（收到 Done）→ `Some(StreamOutput)`
- Stream 意外中断（None 未收到 Done）→ 发送 Internal 错误，`None`
- Stream 错误 → 发送错误事件，`None`

#### 🧪 测试 → 🔍 类型检查

```bash
cargo test -p visp-core
cargo clippy -p visp-core -- -D warnings
```

#### ♻️ 重构

- 检查 `StreamOutput` 字段命名是否与现有代码一致

#### 📦 提交

```
refactor(core): extract collect_stream_events from run_agent_loop
```

---

### 步骤 4：提取 `handle_stream_result`（原 f 段）

#### 🔴 红 — 测试

| # | 测试用例 | 描述 |
|---|---------|------|
| 4 | 现有测试全部通过 | 回归验证 |

#### 🟢 绿 — 实现

提取 [USER_QUERY] 检测 + "done" 决策逻辑：

```rust
enum StreamDecision {
    Done,
    UserQuery { response_rx: oneshot::Receiver<UserQueryResult> },
    Continue,
}

async fn handle_stream_result(
    output: &StreamOutput,
    ctx: &mut AgentLoopContext,
    sm: &SessionManager,
    sid: &str,
    tx: &mpsc::Sender<AgentEvent>,
) -> StreamDecision;
```

**提取范围**：从 `if tool_calls.is_empty()` 到 `return;`/`continue;` 之前（不含 append assistant message 部分）。
**这个函数体最大**，包含：
- [USER_QUERY] 检测 → 保存 thinking/text → 发送 UserQuery → 等待响应 → 构建 user message → `continue`
- Done 分支（无 tool_calls 无 marker）→ 保存 thinking/text → 发送 UsageInfo + Done → `return`
- Continue 分支（有 tool_calls）→ 不在此函数中处理，返回 `StreamDecision::Continue`

**边界处理**：
- 空响应但消耗了 output_tokens → Error（thinking 被 redact 等）
- 空响应且 output_tokens == 0 → 警告，正常 Done
- 有 tool_calls 但无 text → 直接 Continue
- USER_QUERY 响应等待（oneshot channel）超时 → unwrap_or_default

#### 🧪 测试 → 🔍 类型检查

```bash
cargo test -p visp-core
cargo clippy -p visp-core -- -D warnings
```

#### ♻️ 重构

- 这个函数处理了多个分支（USER_QUERY / Done / error），检查是否可以用更简洁的结构表达

#### 📦 提交

```
refactor(core): extract handle_stream_result from run_agent_loop
```

---

### 步骤 5：提取 `execute_tool_calls` + `append_results`（原 g + h 段）

#### 🔴 红 — 测试

| # | 测试用例 | 描述 |
|---|---------|------|
| 5 | 现有测试全部通过 | 回归验证 |

#### 🟢 绿 — 实现

提取工具执行逻辑（这是最复杂的部分，~400 行）：

```rust
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

**提取范围**：从 `total_tool_calls += tool_calls.len() as u32;` 到 `}`（append results 结束）。
具体包含：
1. Append assistant message with tool_calls（~30 行）
2. Doom loop detection（~40 行）
3. Phase 1: Dispatch tools（~250 行，含 multi-agent task 拦截、权限审批、forward_global!、执行）
4. Phase 2: Collect results（~160 行，select! multi-agent/single-agent 模式）
5. Append tool results（~15 行）

然后主循环中调用：
```rust
let results = execute_tool_calls(...).await;
iteration += 1;
```

**注意**：doom loop 检测逻辑保持原位（放在 execute_tool_calls 内部），但 doom_loop_window 和 doom_loop_warned 变量现在在 execute_tool_calls 内部维护——需要确认它们是否是跨迭代的状态。

**重要发现**：`doom_loop_window` 和 `doom_loop_warned` 是跨迭代的局部变量，定义在 `run_agent_loop` 的循环外部。如果将它们移入 `execute_tool_calls`，则每次调用会重置状态。

**解决方案**：将 `doom_loop_window` 和 `doom_loop_warned` 作为参数传入 `execute_tool_calls`，或将其封装在一个小结构体中。设计文档提到 "Doom loop 检测不变"，所以保持这个状态的传递。

```rust
// 在 run_agent_loop 中保留
let mut doom_loop_window: Vec<Vec<(String, serde_json::Value)>> = Vec::new();
let mut doom_loop_warned = false;

// 传给 execute_tool_calls
execute_tool_calls(..., &mut doom_loop_window, &mut doom_loop_warned).await;
```

**关于 `forward_global!` 宏**：该宏在 `tokio::spawn` 闭包内部定义，捕获了 `global_tx`、`sid2`。提取后，需要在 `execute_tool_calls` 中处理这部分逻辑。由于 `tokio::spawn` 内部无法直接访问 `ctx`，我们需要确保引用生命周期正确。

**更具体的方案**：`execute_tool_calls` 不直接包含 `tokio::spawn` 中的闭包逻辑，而是保留 `dispatch_one_tool` 辅助函数在 `execute_tool_calls` 内部。

**append_results**：排序 + 追加到 history → 保留在 `execute_tool_calls` 末尾，或在主循环中调用一个 `append_results` 辅助函数。设计文档建议内部化，我倾向于保留在主循环中更清晰：

```rust
let results = execute_tool_calls(...).await;
append_results(results, ctx, sm, sid);  // 简单排序 + push
iteration += 1;
```

但设计文档提到将 append_results 作为 execute_tool_calls 的内部函数。从职责单一原则看，append 是独立逻辑。但为减少参数传递，可以保持在内。**按设计文档方案**：append_results 作为 execute_tool_calls 内部步骤。

#### 🧪 测试 → 🔍 类型检查

```bash
cargo test -p visp-core
cargo clippy -p visp-core -- -D warnings
```

#### ♻️ 重构

- 检查 doom loop 状态变量是否可以用一个 `DoomLoopDetector` 结构封装
- 检查 `exec_tasks` / `pending_spawns` / `inbox` 等局部变量的作用域是否足够清晰
- 考虑将 `tokio::spawn` 中的工具执行闭包提取为一个独立函数（但闭包捕获变量太多，提取可能不如内联清晰）

#### 📦 提交

```
refactor(core): extract execute_tool_calls from run_agent_loop
```

---

### 步骤 6：主循环骨架瘦身

#### 🔴 红 — 测试

| # | 测试用例 | 描述 |
|---|---------|------|
| 6 | 现有测试全部通过 | 回归验证 |

#### 🟢 绿 — 实现

此时 `run_agent_loop` 的主体应该已经是清晰的循环骨架：

```rust
pub async fn run_agent_loop(...) {
    let result = AssertUnwindSafe(async move {
        // Pre-loop setup
        cleanup_orphan_tool_uses(&mut ctx.history);
        // ... append user message ...

        let mut iteration = 0u32;
        let mut doom_loop_window = Vec::new();
        let mut doom_loop_warned = false;

        loop {
            let ic = setup_iteration(...).await?;
            let stream = call_llm_with_retry(...).await?;
            let output = collect_stream_events(stream, ...).await?;

            match handle_stream_result(&output, ...).await {
                StreamDecision::Done => break,
                StreamDecision::UserQuery { response_rx } => {
                    // wait for response
                    continue;
                }
                StreamDecision::Continue => {}
            }

            execute_tool_calls(&output.tool_calls, ...).await;
            iteration += 1;
        }
    }).catch_unwind().await;

    if let Err(panic) = result {
        // reset session
    }
}
```

这一步骤的目标：
1. 确保所有提取后的调用语法正确
2. 检查参数传递是否完整（不遗漏任何所需参数）
3. 验证所有 `Result` 的 `?` 传播是否正确
4. 确认 `try_send!` 宏是否还在使用，或已被 `try_send_inner` 函数替代

**关于 try_send! 宏**：设计文档建议将宏转换为普通函数。在步骤 6 中实施：
- 将 `try_send!` 宏从 `run_agent_loop` 内部提升为文件级（或模块级）函数 `try_send_inner`
- 所有子函数中需要发送事件的地方，改为调用 `try_send_inner(tx, sm, sid, event).await`
- 这样每个子函数不再需要宏展开的上下文

#### 🧪 测试 → 🔍 类型检查

```bash
cargo test -p visp-core
cargo clippy -p visp-core -- -D warnings
```

#### ♻️ 重构

- 检查每个子函数的参数列表是否过长（设计文档已标注 `#[allow(clippy::too_many_arguments)]`）
- 考虑是否将 `(tx, sm, sid)` 三元组封装为一个 `EventSender` 结构体以减少重复参数
- 检查 `catch_unwind` 后的 session reset 逻辑是否仍然正确

#### 📦 提交

```
refactor(core): slim run_agent_loop to skeleton with extracted helpers
```

---

### 步骤 7：最终清理

#### 🔴 红 — 测试

| # | 测试用例 | 描述 |
|---|---------|------|
| 7 | 现有测试全部通过 | 最终回归验证 |

#### 🟢 绿 — 实现

1. 检查 `agent.rs` 中不再需要的 `use` 声明，清理
2. 检查 `agent_loop.rs` 中的 `use` 声明，确保无冗余
3. 检查 `lib.rs` 是否需要添加 `pub mod agent_loop;`
4. 运行全量质量门禁

#### 🧪 测试 → 🔍 类型检查

```bash
cargo test -p visp-core
cargo clippy -p visp-core -- -D warnings
cargo fmt -- --check
```

#### ♻️ 重构

- 检查 `agent_loop.rs` 的行数是否在 ~450 行内
- 检查 `agent.rs` 是否从 2926 行减少到 ~2000 行

#### 📦 提交

```
refactor(core): final cleanup after run_agent_loop extraction
```

---

## Wave 并行策略

由于本次重构是**对同一文件（`agent_loop.rs` 和修改 `agent.rs` 的可见性/导入）** 的连续修改，且每个步骤修改主循环函数，**步骤间存在强顺序依赖**，无法并行。

| Wave | 步骤 | 说明 | 并行度 |
|------|------|------|--------|
| 1 | 步骤 0 | 新建文件 + 移动函数 | 串行 |
| 2 | 步骤 1-6 | 依次提取 5 个子函数 + 瘦身 | 串行（共享文件修改） |
| 3 | 步骤 7 | 最终清理 | 串行 |

**优化措施**：
- 在每个步骤的验证阶段，`cargo test` 和 `cargo clippy` 可同时运行（同一 bash 中用 `&&` 链接）
- 每次提交前可并行运行 `cargo test -p visp-core`（后台）+ 写 commit message

## 依赖关系总览

```
步骤 0 (move to agent_loop.rs)
  └── 步骤 1 (extract setup_iteration)     ← 依赖步骤 0 的文件存在
        └── 步骤 2 (extract call_llm_with_retry)  ← 依赖步骤 1 后的骨架
              └── 步骤 3 (extract collect_stream_events)
                    └── 步骤 4 (extract handle_stream_result)
                          └── 步骤 5 (extract execute_tool_calls)
                                └── 步骤 6 (skeleton cleanup)
                                      └── 步骤 7 (final cleanup)
```

所有步骤严格串行，无交叉依赖。

## 测试覆盖汇总

| Wave | 步骤 | 测试策略 | 说明 |
|------|------|---------|------|
| 1 | 0 | 回归测试（现有 ~650 个） | 验证移动不破坏编译 |
| 2 | 1-6 | 回归测试（现有 ~650 个） | 每次提取后确认测试通过 |
| 3 | 7 | 回归测试 + Clippy + fmt | 最终质量门禁 |

**不新增测试**：纯 Extract Function 重构，行为不变，现有测试即验收标准。

## 可用检查清单

每个步骤完成后，验证：
- [ ] `cargo build -p visp-core` 编译通过
- [ ] `cargo test -p visp-core` 通过（全部 ~650 个）
- [ ] `cargo clippy -p visp-core -- -D warnings` 零警告
- [ ] `cargo fmt -- --check` 格式正确
- [ ] 提取后函数边界与原代码段对应（通过 diff 确认）

## 已知风险与缓解

| 风险 | 可能性 | 影响 | 缓解 |
|------|--------|------|------|
| 类型可见性错误（`pub(crate)` 缺失） | 中 | 编译失败 | 在每个步骤的 🟢 阶段立即编译验证 |
| `try_send!` 宏提取后作用域变化 | 中 | 子函数中无法使用宏 | 步骤 6 中统一替换为函数调用 |
| `catch_unwind` 中闭包生命周期冲突 | 低 | 编译失败 | AssertUnwindSafe 包装保持不变 |
| `doom_loop_window` 跨迭代状态丢失 | 低 | Doom loop 检测失效 | 保持状态在 run_agent_loop 中，传入 execute_tool_calls |
| `forward_global!` 宏在 `tokio::spawn` 闭包中的引用 | 中 | 闭包内无法访问 ctx | 保持闭包内各自捕获所需的变量 |
| LSP 报 proto/tonic 相关错误 | 低 | 无影响 | 先 `cargo build` 再检查 |

## 备注

1. **类型可见性**：`agent_loop.rs` 中需要访问的类型（`AgentEvent`、`AgentLoopContext`、`AgentConfig`、`ToolExecResult`、`PendingSpawn`、`UserQueryMarker`、`ToolExecResult` 等）目前均为 `pub` 或文件内私有。提取后需要确保 `agent.rs` 中所有被 `agent_loop.rs` 引用的类型为 `pub(crate)`。

2. **`chrono` 依赖**：`agent.rs` 使用了 `chrono::Local::now()` 来生成日期字符串。`agent_loop.rs` 不需要额外添加依赖，因为 `chrono` 已是 `visp-core` 的依赖。

3. **关于体积**：当前 `agent.rs` 2926 行（含 ~1432 行测试）。提取后 `agent_loop.rs` 约 450 行 + `agent.rs` 约 2000 行（类型/辅助函数 + 测试）= 约 2450 行，总体减少 ~476 行（主要来自 `run_agent_loop` 函数体提取到新文件后，`agent.rs` 中减少了这一大段代码）。

4. **`mod agent_loop` 的位置**：放在 `agent.rs` 底部（`mod agent_loop;`），因为 `agent_loop.rs` 需要引用 `agent.rs` 中的类型。或者放在 `lib.rs` 中作为同级模块。**推荐**：在 `lib.rs` 中添加 `pub mod agent_loop;`，在 `agent.rs` 中不添加 `mod agent_loop;`（避免循环引用）。

5. **实际范围估算**：
   - 步骤 0：~50 行（文件创建 + use 声明调整 + 可见性调整）
   - 步骤 1-5：每次约 20-50 行（函数定义 + 原代码段移动 + 调用处替换）
   - 步骤 6：~30 行（主循环简化 + 宏替换）
   - 步骤 7：~10 行（清理）
   - **总计约 200-300 行修改**（主要是新建文件和修改 import/可见性，而非新增代码）
