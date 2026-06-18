# 工作计划：Ctrl-C 取消彻底级联到 sub-agent

**关联设计文档**：`docs/design/visp-design-cancel-cascade-fix.md`
**日期**：2026-06-18
**状态**：草案，待审核

---

## 0. 预先验证（W1 之前的事实确认）

### 0.1 finish_loop 幂等性 ✅ 已验证

`SessionManager::finish_loop`（`crates/visp-core/src/session.rs:567-576`）：

- `store.get(id)` → 取出 session（已 Idle 也成功）
- 设 `status = Idle` → `store.update`（幂等）
- `running_tokens.lock().remove(id)`（缺失 key 也无副作用）

**结论**：天然幂等。无需新增 AtomicBool / CAS / 状态机保护。设计文档第 6 节风险表的"finish_loop 双调"风险**降级为已缓解**，仅以测试 #5 作回归网。

### 0.2 collect_stream_events cancel 分支已发 Cancelled ✅ 已验证

`agent_loop.rs:331-343` cancel 分支已经：

- 发送 `AgentEvent::Error{ Cancelled, "Agent loop cancelled" }`
- 调用 `sm.finish_loop(sid, SessionStatus::Error)`
- 返回 `None`

**结论**：上层在 `call_llm_with_retry` 收到 `LlmError::Cancelled` 后**不重复**发 Error envelope，让 collect_stream_events 已有的 cancel 检查点接管。

### 0.3 LlmError 现状

`crates/visp-core/src/error.rs` 当前 5 个 variant：Network、RateLimit、Auth、Api、Stream。新增 `Cancelled` 后所有 match 点会被编译器强制要求补分支：

- `agent.rs:229 llm_error_to_code` 主映射点
- 其它 match 点用 `cargo check` 一次性发现

### 0.4 6 个 LlmProvider 实现位置

| # | 路径 | 角色 |
|---|------|------|
| 1 | `crates/visp-llm/src/anthropic.rs:444` | 生产 |
| 2 | `crates/visp-llm/src/openai.rs:641` | 生产 |
| 3 | `crates/visp-llm/src/mock.rs:20` | 测试 mock |
| 4 | `crates/visp-core/src/agent.rs:610` TestProvider | 测试 |
| 5 | `crates/visp-core/src/agent.rs:1718` PanicProvider | 测试 |
| 6 | `crates/visp-core/src/agent.rs:1783` PanicProvider | 测试 |

---

## 1. Wave 划分

按 oracle 复审建议：**W1（基础设施）→ W2（retry）→ W3（Phase 2）→ W4（集成与并发）**。

| Wave | 内容 | 依赖 | 测试增量 |
|---|---|---|---|
| W1 | trait 加 cancel 参数 + LlmError::Cancelled + 6 个 impl + Anthropic/OpenAI 的 send select! | 无 | +1（测试 #3） |
| W2 | call_llm_with_retry 加 cancel + 上层吞 Cancelled | W1 | +1（测试 #1） |
| W3 | Phase 2 inbox.recv 加 select + drain helper | W2 | +2（测试 #2、#4） |
| W4 | 多 sub-agent 并发 cancel 竞态 + 手动验证 | W1-W3 | +1（测试 #5） |

每个 Wave 独立提交，TDD 红→绿→测试→clippy/fmt→重构→commit。

---

## 2. W1：trait + LlmError::Cancelled + 实现层 cancel

### 2.1 目标

- `LlmProvider::chat_stream` 接收 `cancel: &CancellationToken`
- `LlmError` 新增 `Cancelled` variant
- Anthropic / OpenAI 在 `.send().await` 处用 `tokio::select!` 包裹
- 6 个 impl 全部跟新签名
- `cargo test` + `cargo clippy -- -D warnings` + `cargo fmt -- --check` 通过

### 2.2 TDD 步骤

#### W1-S1：红 — 测试 #3（chat_stream cancel）

**位置**：`crates/visp-llm/src/mock.rs` 同文件 `#[cfg(test)] mod tests`

**场景**：

- SlowProvider mock：`chat_stream` 体内 `tokio::time::sleep(5s)` 后才返回 stream
- 创建 `CancellationToken`，spawn chat_stream future
- 50ms 后 `cancel()`
- 断言：≤50ms 内返回 `Err(LlmError::Cancelled)`

**预期**：编译失败（Cancelled 不存在 + 签名缺 cancel 参数）→ 红达成。

#### W1-S2：绿（基础设施）

按顺序：

1. `crates/visp-core/src/error.rs` `LlmError` 加 `Cancelled` variant，thiserror 消息 "LLM call cancelled"
2. `crates/visp-core/src/provider.rs` `chat_stream` 签名加 `cancel: &tokio_util::sync::CancellationToken`（参数列表末尾，config 之后）
3. `crates/visp-core/src/agent.rs:229 llm_error_to_code` 补 `LlmError::Cancelled` 分支
4. 6 个 impl 跟新签名（W1-S3）
5. `crates/visp-core/src/agent_loop.rs` 调用 chat_stream 处传 `&ctx.cancel_token`（grep 确认所有调用点）

#### W1-S3：6 个 impl 跟新

按 0.4 表顺序：

1. **anthropic.rs:444**：签名加 cancel；`.send().await` 包成 `tokio::select!`，cancel 分支返回 `LlmError::Cancelled`
2. **openai.rs:641**：同上
3. **mock.rs:20**：签名加 `_cancel`，body 不变
4. **agent.rs:610 TestProvider**：签名加 `_cancel`，body 不变
5. **agent.rs:1718 PanicProvider**：签名加 `_cancel`，body 不变
6. **agent.rs:1783 PanicProvider**：签名加 `_cancel`，body 不变

> 注：mock / Test / Panic provider 不需真听 cancel，签名扩展 + `_cancel` 抑制 unused 即可。SlowProvider（W1-S1 新增）才需真用。

#### W1-S4：测试 + 类型检查

- `cargo test -p visp-llm`：测试 #3 通过
- `cargo test`：全量基线 855 + 1 = 856
- `cargo clippy -- -D warnings`、`cargo fmt -- --check`

#### W1-S5：重构

- anthropic / openai 两处 select! **不提取 helper**（YAGNI）
- thiserror 消息风格与其他 variant 一致

#### W1-S6：提交

```
feat(llm): add cancel token to chat_stream + LlmError::Cancelled
```

---

## 3. W2：call_llm_with_retry 加 cancel

### 3.1 目标

- retry 循环顶部检查 `is_cancelled()`
- retry sleep 用 `tokio::select!` 监听 cancel
- chat_stream 调用前再次检查 cancel
- 上层收到 `LlmError::Cancelled` 后吞掉（不重复发 Error envelope）

### 3.2 TDD 步骤

#### W2-S1：红 — 测试 #1（retry cancel）

**位置**：`agent_loop.rs` 同文件 `#[cfg(test)] mod tests`

**场景**：

- RateLimitProvider mock：前 3 次返回 `Err(LlmError::RateLimit{ retry_after_secs: 10 })`，第 4 次成功
- 用 `Arc<AtomicUsize>` 计数 chat_stream 调用次数
- 启动 `call_llm_with_retry`，100ms 后 cancel
- 断言：≤1s 内返回 `Err(LlmError::Cancelled)`，且调用计数 ≤3（未达第 4 次）

#### W2-S2：绿

修改 `call_llm_with_retry`（agent_loop.rs:237-299）：

1. 循环顶部：`if ctx.cancel_token.is_cancelled() { return Err(LlmError::Cancelled); }`
2. retry sleep 改 `tokio::select!`：sleep → continue；cancel → return `Err(Cancelled)`
3. provider.chat_stream 调用前再查 cancel

#### W2-S3：上层错误处理

grep 所有调用 `call_llm_with_retry` 的位置，对 `Err(LlmError::Cancelled)`：

- **不发 Error envelope**
- return None / break，由 collect_stream_events 已有的 cancel 检查点（331-343）发事件 + finish_loop

#### W2-S4：测试 + 检查

- `cargo test -p visp-core`：测试 #1 通过
- 全量 `cargo test` 856 → 857
- `cargo clippy -- -D warnings`、`cargo fmt -- --check`

#### W2-S5：提交

```
fix(agent_loop): respect cancel_token in call_llm_with_retry
```

---

## 4. W3：Phase 2 inbox.recv 加 select + drain helper

### 4.1 目标

- Phase 2 收集循环两条 `inbox.recv().await` 路径包成 `tokio::select!`
- cancel 触发时合成 `ToolResult::error("agent cancelled")` 给 pending tool_use_id（保持 Anthropic tool_use ↔ tool_result 对齐）
- 提取 drain helper：cancel 路径与 inbox 关闭路径（已存在的兜底）共用此逻辑

### 4.2 TDD 步骤

#### W3-S1：红 — 测试 #2 + 测试 #4

**测试 #2（Phase 2 cancel）位置**：`agent_loop.rs` 同文件 tests

**场景**：

- TestProvider 返回带 1 个 tool_use 的 stream
- 注册 SlowTool（执行 5s）
- 启动 agent loop，进入 Phase 2 后 100ms cancel
- 断言：≤200ms 跳出 Phase 2；pending tool_use_id 收到 ToolResult error；最终 Error{Cancelled} envelope 被发送

**测试 #4（drain helper）位置**：同文件 tests

**场景**：

- 直接对 helper 单元测试：传入 3 个 pending tool_use_id 与一段 reason
- 断言：返回 3 个 `ToolResult::error(reason)`，is_error=true

#### W3-S2：绿

修改 agent_loop.rs Phase 2（约 1003-1154）：

1. 提取 `fn drain_pending_results(pending: Vec<PendingUse>, reason: &str) -> Vec<ToolResult>`
2. Phase 2 顶部已有 cancel 检查（1009）保留；新增 inbox 主循环 `tokio::select!`：
   - `_ = ctx.cancel_token.cancelled()` 分支：abort 所有 exec_tasks → drain_pending_results → break
   - `msg = inbox.recv()` 分支：原逻辑
3. 复用 drain helper 替换原 inbox 关闭兜底（如有）

#### W3-S3：测试 + 检查

- `cargo test -p visp-core`：测试 #2、#4 通过
- 全量 857 → 859
- `cargo clippy -- -D warnings`、`cargo fmt -- --check`

#### W3-S4：提交

```
fix(agent_loop): cancel-aware Phase 2 inbox loop + drain helper
```

---

## 5. W4：多 sub-agent 并发 cancel 竞态测试 + 手动验证

### 5.1 目标

- 模拟父 agent 启动 3 个 sub-agent，所有 sub 同时在 LLM 重试 sleep / Phase 2 inbox.recv 等待
- 父 cancel 后，确认 ≤2s 内全部 sub 退出，无 finish_loop 双调、无 hang
- 手动操作 visp CLI 验证真实场景

### 5.2 TDD 步骤

#### W4-S1：红 — 测试 #5（并发 cancel 竞态）

**位置**：`crates/visp-agent/src/orchestrator.rs` 或 `crates/visp-core/src/agent_loop.rs` 集成测试

**场景**：

- 父 session 启动，spawn 3 个 sub-agent，每个 sub 用 SlowProvider（5s sleep）
- 100ms 后父 cancel
- 断言：≤2s 内 4 个 session 全部 status=Error，running_tokens 全部清理；finish_loop 每 session 仅调用一次

#### W4-S2：绿

预期 W1-W3 已修复根因，本步骤主要验证：

- 若测试失败，定位是 finish_loop 重入还是 token 残留 → 局部修复
- 若一切顺畅则无需新代码

#### W4-S3：手动验证

按设计文档第 7 节"验证方法"操作：

1. 启动 visp CLI，触发让 LLM 调用 Task 工具的提问
2. sub-agent 启动后立即 Ctrl-C
3. 观察 daemon 日志：
   - sub-agent loop 退出（CancelHit / LoopExit）
   - HTTP 请求被中断（HTTP_CANCEL）
   - finish_loop 每 session 仅一次
4. 验证 ≤2s 内所有 active token 清空

#### W4-S4：提交

```
test(agent_loop): concurrent sub-agent cancel race test + manual verify
```

---

## 6. 提交 Gate（每 Wave 结束前）

每个 Wave 提交前必须通过：

```
cargo test                  # 全量绿
cargo clippy -- -D warnings # 零警告
cargo fmt -- --check        # 格式正确
```

---

## 7. 风险与回退

- 若 trait 签名变更导致下游 crate 编译错误数量超预期，**先 W1 单独 PR**，绿后再开 W2
- 若 W3 drain helper 提取触发回归，回退到内联 cancel 处理（YAGNI 优先）
- 若 finish_loop 在并发竞态下出现意料外行为（0.1 已验证幂等，理论上不会），追加 AtomicBool guard

---

## 8. 完成标准

- [ ] 5 个新测试全部绿（#1 retry / #2 Phase2 / #3 chat_stream / #4 drain / #5 并发）
- [ ] 现有 855 测试无回归
- [ ] clippy / fmt 通过
- [ ] 手动验证 Ctrl-C ≤2s 子 agent 停 LLM 请求
- [ ] 设计文档的 3 层根因均有对应测试覆盖

