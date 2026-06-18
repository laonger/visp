# 工作计划：并行 sub-agent 可观测性与 panic 兜底（策略 A）

对应设计：`docs/design/visp-design-parallel-subagent-observability.md`

## 总原则

- 严格遵循 TDD：红 → 绿 → 测试 → clippy → fmt → 提交
- 每个 wave 内的步骤是顺序依赖；wave 之间可串行执行
- 每个 wave 结束后单独 commit
- 提交信息格式：`fix(scope): xxx` 或 `feat(scope): xxx`，scope ∈ {agent, core}

## 测试基线

执行前先记录基线：

```bash
cargo test 2>&1 | tail -3   # 通过数 N
cargo clippy --all-targets -- -D warnings 2>&1 | tail -3   # 0 警告（除已知遗留）
```

每个 wave 结束后必须保持：测试 ≥ N + 本 wave 新增数；clippy 不引入新警告。

## Wave 1：handle_done 与 Done 路径增加 INFO 日志

**目标**：sub-agent 生命周期完整可见。最小风险改动，先打通可观测性。

### 步骤

1. **W1-S1（红）**：在 `crates/visp-agent/src/orchestrator.rs` 的 tests 模块新增单元测试
   - `test_handle_done_emits_completion_log`：用 `tracing-test` 或 `tracing-subscriber::fmt::TestWriter` 捕获 INFO 日志，调用 handle_done 后断言日志中包含 `agent completed` + session_id
   - 若引入 `tracing-test` 依赖：在 `crates/visp-agent/Cargo.toml` 的 `[dev-dependencies]` 中添加
   - 备选：不引入新依赖，改成验证函数返回值或 mock channel 行为（次选）

2. **W1-S2（绿）**：在 handle_done 入口、SubAgentComplete 投递前后、错误路径分别加 `tracing::info!`
   - 字段：session_id, parent_id (Option), agent_name, message_count（若可获取）
   - 不在循环或 token 级别加日志

3. **W1-S3（红）**：在 `crates/visp-core/src/agent_loop.rs` tests 模块或 fixture 测试中
   - `test_done_decision_logs_completion`：跑一个最简 agent loop（用 mock provider 立即返回 Done），断言 INFO 日志含 `agent loop completed`
   - 若 agent_loop.rs 已有类似的 mock provider 测试，复用

4. **W1-S4（绿）**：在 StreamDecision::Done 分支加 `tracing::info!("agent loop completed", session_id, iterations, ...)`

5. **W1-S5（验证）**：
   - `cargo test -p visp-agent && cargo test -p visp-core`
   - `cargo clippy -p visp-agent -p visp-core --all-targets -- -D warnings`
   - `cargo fmt -- --check`

6. **W1-S6（提交）**：`feat(agent,core): sub-agent 与 agent loop 完成路径增加 INFO 日志`

### 验证标准
- handle_done 路径所有出口（成功 / 错误 / 取消）均有 INFO 日志
- agent loop Done 分支有 INFO 日志
- 新增至少 2 个单元测试，全部通过
- clippy 零新增警告

## Wave 2：panic 兜底 + Phase 2 channel 关闭兜底

**目标**：sub-agent panic 或 channel 关闭时，父 agent 不会死等。

### 步骤

1. **W2-S1（设计确认）**：先扫描 `agent_loop.rs:catch_unwind` 段落与 Phase 2 收集循环
   - 用 read 工具看 1003-1118 + 1201-1364 行
   - 确定 panic 字符串化方式（已存在的 helper 还是新增）
   - 确定 OrchestratorMessage 中是否有现成的"sub-agent error"变体可复用，还是要新增

2. **W2-S2（红）**：新增集成测试 `crates/visp-agent/tests/parallel_sub_agents.rs`
   - `parallel_sub_agents_both_succeed`：使用 mock provider，两个 sub-agent 都正常返回 Done，父 agent 收到 2 个 ToolResult
   - `parallel_sub_agents_one_panics`：mock provider 让其中一个 sub-agent 在第 N 个 LLM call 时 panic，断言：
     - 父 agent 在 5 秒超时内 execute_tool_calls 返回
     - 返回的 ToolResult 中失败那个含错误标记
     - 成功那个的内容正确
   - 测试初版会失败（panic 没兜底，会死等到超时）

3. **W2-S3（绿）**：在 `agent_loop.rs::catch_unwind` 的 Err 分支
   - 在 finish_loop 后、resume_unwind 前，通过 global_tx 发送 OrchestratorMessage（表示 sub-agent panicked，含 session_id + 错误字符串）
   - Orchestrator 收到后走 handle_done 等价路径，注入 SubAgentError 到父 inbox
   - 关键：保证 send 不会因为 channel 满阻塞过久（用 try_send 或带超时；若选 try_send 失败则 fallback 到 log）

4. **W2-S4（绿）**：Phase 2 收集循环 channel 关闭兜底
   - `inbox.recv()` 返回 None 但 pending_spawns 非空时
   - 打印 warn 日志（含 session_id 列表）
   - 对每个未完成项合成一个失败 ToolResult，让 execute_tool_calls 正常返回

5. **W2-S5（验证）**：
   - `cargo test -p visp-agent --tests parallel_sub_agents`
   - `cargo test`（全量）
   - `cargo clippy --all-targets -- -D warnings`
   - 手动复现：起 2 个并行 sub-agent 真实 LLM 调用，观察日志含完整生命周期

6. **W2-S6（提交）**：`fix(core,agent): sub-agent panic 与 channel 关闭兜底，避免父 agent 死等`

### 验证标准
- 集成测试 2 个全部通过
- panic 场景父 agent 在 5 秒内返回，不会无限等待
- channel 关闭场景同样不会死等
- 不引入死锁、不破坏现有 600+ 测试

## Wave 3：Orchestrator JoinHandle 监控

**目标**：spawn 出去的 tokio task 不再"裸跑"，Orchestrator 持有 JoinHandle，便于诊断与未来 cancel 扩展。

### 步骤

1. **W3-S1（红）**：单元测试
   - `test_orchestrator_tracks_sub_agent_join_handles`：spawn 一个 sub-agent 后，断言 Orchestrator 内部的 JoinHandle map 含该 session_id
   - sub-agent 完成后，map 中对应 entry 被移除（在 handle_done 中 .remove）

2. **W3-S2（绿）**：在 Orchestrator struct 中加 `sub_agent_handles: HashMap<String, JoinHandle<()>>`
   - spawn_sub_agent 中保存 JoinHandle
   - handle_done 中 .remove(session_id)
   - 注意：JoinHandle drop 不会 abort task，所以这只是"持有"不是"控制"，符合本期范围

3. **W3-S3（验证）**：
   - 单元测试通过
   - 全量 cargo test 通过
   - clippy 零新增警告

4. **W3-S4（提交）**：`refactor(agent): Orchestrator 持有 sub-agent JoinHandle 便于诊断`

### 验证标准
- 新增单元测试通过
- spawn → done 完整生命周期 map 状态正确

## Wave 4：手动验证 + 文档更新

**目标**：在真实环境复现原始场景，确认改动效果。

### 步骤

1. **W4-S1**：清理旧 daemon 日志，启动新 visp，输入指令让主 agent 同时 spawn 2 个 sub-agent（参考用户原场景）

2. **W4-S2**：grep 日志确认所有 INFO 事件都在
   - `sub agent spawned` × 2
   - `agent loop completed` × N（含父 agent + 2 个 sub-agent）
   - 完成时间间隔合理

3. **W4-S3**（可选）：在某个 sub-agent 的 LLM 路径里临时插入 `panic!` 断言验证 panic 兜底确实生效（验证后回滚）

4. **W4-S4**：将本计划标记为完成。如果 W4-S2 暴露新的真实问题（不是日志盲区），追加一个新的"策略 B"待办

### 验证标准
- 真实双 sub-agent 场景下，日志可读、生命周期完整
- 用户主观感受"消息不及时"的判断有了客观日志依据（多久延迟可量化）

## 非目标确认

- **不实施**：task 工具增量返回（策略 B）
- **不实施**：每 sub-agent 独立 fanout channel（策略 C）
- **不优化**：体感"不及时"——这是 join_all 固有特性
- **不改**：gRPC 协议、CLI tab 渲染逻辑、daemon main.rs 启动顺序

## 完成定义

- 4 个 wave 全部完成并独立 commit
- `cargo test && cargo clippy --all-targets -- -D warnings && cargo fmt -- --check` 通过（不引入新警告）
- 至少 4 个新增测试（2 单元 + 2 集成）
- 真实双 sub-agent 场景日志验证通过
- 设计文档与本计划归档保留

## 委托策略

- W1、W2 涉及多文件 + 测试编写：完成测试编写后，可以将"实施 + clippy/fmt 修复"打包委托给 @fixer，自己专注审阅
- W3 改动小，自己执行更快
- W4 是手动验证，自己执行
- 如果 W2 测试编写陷入复杂 mock 问题（>30 分钟没进展），调用 @oracle 求助

## 风险登记

| 风险 | 缓解 |
|------|------|
| tracing-test 依赖引入与现有 tracing-subscriber 冲突 | 备选方案：改用 mock channel 验证行为而非日志文本 |
| panic 跨 await 转发涉及 Send + Unpin | catch_unwind 内已有错误字符串化，发送 String 即可 |
| 集成测试 mock LLM provider 复杂度 | 复用现有 TestProvider，仅扩展 panic 注入选项 |
| Phase 2 兜底改动可能影响现有单 sub-agent 路径 | 兜底分支只在 inbox 关闭时触发，正常路径不变 |
