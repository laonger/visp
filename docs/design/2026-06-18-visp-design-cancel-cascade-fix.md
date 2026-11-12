# 设计文档：Ctrl-C 取消彻底级联到 sub-agent

**状态**：草案，待审核
**日期**：2026-06-18
**关联 bug**：用户按 Ctrl-C，主 agent 停止，但 sub-agent 仍在跑（继续调 LLM、消耗 token）

---

## 1. 背景与问题

### 1.1 现状

CLI Ctrl-C → daemon → orchestrator `cancel_agent(root_session_id)` 已实现"递归取消所有子孙"的逻辑：

- 取消 root 自己的 `ActiveAgent.cancel_token` 和 `session_mgr` 的 token
- 通过 `ActiveAgentRegistry::descendants_of` BFS 遍历，依次取消每个子孙的两类 token

**这层逻辑已经正确，bug 不在这里。**

### 1.2 真实根因（来自 oracle 诊断）

子 agent 的 cancel_token 确实被 cancel 了，但实际"继续运行"的原因有三层：

#### 根因 #1：`call_llm_with_retry` 重试循环不检查 cancel_token

`crates/visp-core/src/agent_loop.rs` 的 `call_llm_with_retry`（约 237-299 行）处理 LLM 的 RateLimit / Network 错误重试。当前实现：

- 失败 → `tokio::time::sleep(Duration::from_millis(delay))` → 重新调用 `provider.chat_stream`
- 整个循环**不检查 `ctx.cancel_token`**

后果：子 agent cancel 后正在 retry sleep 中，sleep 完后照常发起新 HTTP 请求，再次消耗 token。这是用户感知最强的"还在跑"。

#### 根因 #2：Phase 2 父 agent `inbox.recv().await` 不能被 cancel 打断

父 agent 调 task 工具后进入 Phase 2 收集循环（agent_loop.rs:1003-1118），等待 sub-agent 完成。其中一条分支是纯 `rx.recv().await`，cancel 检查只在循环顶部 `is_cancelled()`，但 `recv().await` 不会因为 cancel 返回。后果：

- 父 cancel 后，仍卡在 `recv()`
- 必须等子 agent 完成（或更晚）发送一条消息才能跳回循环顶部检查 cancel
- TUI 视角：父"卡住没响应"

#### 根因 #3：`LlmProvider::chat_stream` HTTP `.send()` 不接受 cancel

`LlmProvider` trait 签名（`crates/visp-core/src/provider.rs:102-107`）不接受 cancel token。Anthropic / OpenAI 实现里 `.send().await` 等待 HTTP 首响应是不可中断的。

后果：即便 cancel_token 已 cancel，正在等首字节的 HTTP 请求会跑到底，stream 拿到后才被外层 select 打断。延长 cancel 响应延迟（约 1-N 秒，看网络）。

#### 补充观察：当前 Phase 2 cancel 路径缺少 pending_spawns drain

`agent_loop.rs:1009-1014` 的 `is_cancelled()` 分支当前只 `break` 跳出收集循环（最多 abort `exec_tasks`），**未合成 pending_spawns 的 error ToolResult**。后果：history 中存在 tool_use 但缺对应 tool_result。如果 cancel 后该 session 被恢复继续运行，下一次 LLM 调用会触发 HTTP 400。

修复 #2 需顺带修这个缺陷——共用同一个 drain helper（见 4.2 节）。

#### 已有 cancel 检查点（不动）

agent_loop.rs 中已有的 cancel 检查点：入口 1244、setup_iteration 131、collect_stream_events 331-343 (select! biased)、execute_tool_calls 非多 agent 1160-1167、Phase 2 顶部 1009、tool spawn 837。本次修复补齐余下盲区（retry 循环、Phase 2 inbox.recv、HTTP send）。

---

## 2. 目标

- 用户 cancel 信号发出后 ≤2 秒内，**所有 active agent 的 tokio 任务停止调度新的异步工作**（含 sleep、recv、HTTP send）
- 已发出的 HTTP 请求**通过 TCP/HTTP2 abort 尽早终止**，不消耗后续 token（已收到首字节的部分 token 是网络硬限制）
- 父 agent 在 Phase 2 收集时**立即**响应 cancel，不死等
- 不破坏现有正常路径（Done / SubAgentComplete / SubAgentError）的语义

> 测试目标说明：单元测试通过 mock 验证 <200ms（mock 环境）；2 秒是包含真实网络延迟的集成环境目标。

---

## 3. 修复策略概览

三处独立修复，互相加强：

| 编号 | 位置 | 改动范围 | 关键效果 |
|---|---|---|---|
| 修复 #1 | agent_loop.rs::call_llm_with_retry | 函数内 ~10 行 | retry sleep 与下次调用前检查 cancel，cancel 后立即返回错误 |
| 修复 #2 | agent_loop.rs Phase 2 收集循环（多处 recv().await） | 包 select! | inbox.recv 与 cancel_token 并行 await，cancel 立即跳出 |
| 修复 #3 | LlmProvider::chat_stream trait 签名 + 两个 provider 实现 | trait 加 cancel 参数，实现里 select! 包 HTTP send | HTTP 阶段可中断，drop request → reqwest 主动断连 |

---

## 4. 设计细节

### 4.1 修复 #1：retry 循环加 cancel 检查

**职责**：`call_llm_with_retry` 在每次重试前检查 cancel_token。

**流程**：

1. 进入循环前，记录 ctx.cancel_token 引用
2. 每次 retry 等待前先检查 `is_cancelled()` → 已 cancel 则返回 `LlmError::Cancelled`（新增 variant）
3. retry sleep 改为 `tokio::select!`，sleep 完成 → 继续；cancel 触发 → 返回 Cancelled
4. provider.chat_stream 调用前再检查一次 cancel，避免 sleep 完恰好 cancel 仍发请求

**返回类型**：`LlmError` 枚举新增 `Cancelled` variant，语义清晰，区别于 RateLimit / Network。

**调用方处理**：在 agent_loop 层统一处理 `LlmError::Cancelled` 为取消退出路径——跳到 finish_loop，**不重复发出 `AgentEvent::Error` envelope**（cancel 是用户主动行为，且 `collect_stream_events` 等已有检查点会发一次 Cancelled 事件，避免重复）。

### 4.2 修复 #2：Phase 2 inbox.recv() 加 select!

**职责**：父 agent 在 Phase 2 等子 agent 时，`recv()` 与 cancel 并行 await。

**两处需改**（agent_loop.rs 多 agent Phase 2 收集循环）：

a) `select! { join_fut, recv_fut }` 已在 select 中——把 `recv_fut` 改为同时监听 cancel_token，cancel 触发时跳出外 loop
b) `else if has_tasks` / `else if let Some(ref mut rx) = inbox` 的纯 `recv().await` 分支——把 `recv` 包成 `tokio::select!`，监听 `ctx.cancel_token.cancelled()`

**关键约束**：cancel 跳出时，pending_spawns 必须**合成 `ToolResult::error("agent cancelled")` 加入 collected**，避免 LLM 下次调用因 tool_use_id 缺失而 HTTP 400。

实现层面：把 W2-S4 已添加的"inbox 关闭兜底" drain 代码提取成内部 helper，cancel 路径和 inbox 关闭路径共用。

### 4.3 修复 #3：trait 加 cancel_token

**trait 签名变更**（`crates/visp-core/src/provider.rs`）：

`chat_stream` 增加 `cancel: &CancellationToken` 参数（在 config 之后）。

**实现方改动**：

- `crates/visp-llm/src/anthropic.rs`、`openai.rs` 在 `.send().await` 处包 `tokio::select!`，cancel 触发时返回 `LlmError::Cancelled`
- 流体已经返回后，stream 本身被 reqwest 持有；cancel 时 drop 未完成的 `send()` future 或未消费完的 `bytes_stream()` 都会触发 reqwest 0.12（hyper 1.x）abort 底层连接（HTTP/1.1 关闭 TCP socket，HTTP/2 触发 cancellation）
- agent_loop 调用方（`call_llm_with_retry`、`collect_stream_events` 入口）传 `&ctx.cancel_token`

**测试 / mock 影响**：

- visp-core 的 `TestProvider`、`PanicProvider` 等 mock 都需要更新签名
- 实测约 6 个实现需要跟进（机械改动，签名扩展，body 不变）

---

## 5. 影响范围

### 直接修改

- `crates/visp-core/src/agent_loop.rs`：retry 循环、Phase 2 select、调用 chat_stream 的地方加 cancel 参数
- `crates/visp-core/src/provider.rs`：trait 签名
- `crates/visp-core/src/error.rs`（或 LlmError 所在位置）：新增 `Cancelled` variant
- `crates/visp-llm/src/anthropic.rs` 和 `openai.rs`：实现签名 + select 包 HTTP send
- `crates/visp-core/src/agent.rs` 测试中的 PanicProvider / TestProvider：跟随更新

### 不动

- orchestrator `cancel_agent` 逻辑（已正确，递归取消）
- ActiveAgentRegistry / descendants_of / compute_depth
- session_mgr.cancel_agent
- proto 定义

---

## 6. 风险与缓解

| 风险 | 缓解 |
|---|---|
| trait 签名变更导致大量测试 mock 失败 | 一次性集中改完；mock 都很简单（直接转发或返回 mock stream） |
| cancel 后 reqwest drop 是否真的 abort 连接 | reqwest 0.12（本项目使用版本，基于 hyper 1.x）在 drop 未完成的 `send()` future 或未消费完的 `bytes_stream()` 时均会 abort 底层 TCP/HTTP2 连接，已确认 |
| Phase 2 cancel 跳出时丢失部分子 agent 真实结果 | 已 cancel 即等同放弃，合成 error ToolResult 符合用户语义 |
| 新增 LlmError::Cancelled 需要所有上层 match 处理 | 在 agent_loop 的 LLM 错误分类点统一映射为"取消退出"路径，不发 Error envelope |
| 并发 cancel + 正常完成竞态（finish_loop 双调） | cancel_token 是幂等的；但 `SessionManager::finish_loop` 是否对并发调用幂等需在实现前验证（可能需要 AtomicBool / CAS 保护或状态机），如未实现需补保护 |
| 已发出但 cancel 时已收到首字节的 HTTP 请求 | TCP 层 abort 仅中断未来字节传输；服务端可能已经计费部分 token，这是网络硬限制，无代码层修复办法 |

---

## 7. 验证方法

### 单元测试（新增）

1. **#1 retry cancel**：模拟 provider 反复返回 RateLimit，主动 cancel，断言 `call_llm_with_retry` 在 ≤sleep 间隔内返回 `LlmError::Cancelled`
2. **#2 Phase 2 cancel**：spawn 一个永不完成的 sub-agent（`std::future::pending()`），父进入 Phase 2 等待，cancel root，断言父 ≤100ms 内退出收集循环并合成 error ToolResult
3. **#3 chat_stream cancel**：mock provider 在 `chat_stream` 方法体内 `tokio::time::sleep(N秒)` 后才返回，cancel 触发后 chat_stream 应在 ≤50ms 内返回 `LlmError::Cancelled`（验证 sleep 期间 cancel 即时生效）
4. **#4 cancel 时 pending_spawns drain**：进入 Phase 2，spawn 3 个永不完成的 sub-agent，cancel root，断言 3 个 error ToolResult 都被合成并加入 collected（防止 history 不一致）
5. **#5 多 sub-agent cancel 竞态**：同时 cancel 3 个活跃子 agent + 父 agent，断言所有 join_handle 都能在 timeout 内完成，无 panic（验证 finish_loop 幂等性）

### 集成回归

- `cargo test`：当前基线测试全部通过 + 新增 5 个测试
- `cargo clippy -- -D warnings`：零新增警告
- `cargo fmt -- --check`：通过

### 手动验证

- 启动 visp，使用一个会启动 sub-agent 的 prompt（如"用 task 工具并行做两件事"）
- 在 sub-agent 正在跑时按 Ctrl-C
- 观察 daemon 日志：≤2 秒内出现所有 active agent 的 cancel 日志，无后续 LLM HTTP 请求

---

## 8. 不做什么（YAGNI）

- 不引入 child_token() 衍生关系。当前 cancel_agent 的显式遍历已足够，trait 改动后这层级联无关紧要
- 不实现 `OrchestratorMessage::Cancelled`。当前 cancel_token 已经能直接打断 await，不需要新消息
- 不做 graceful degradation（"cancel 后允许当前 LLM 流式输出完再退出"）。用户预期 cancel = 立即停
- 不在 trait 加 timeout 参数。timeout 由 LlmConfig 单独承载，与 cancel 正交

