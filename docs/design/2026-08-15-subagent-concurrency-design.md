# Subagent 并发限制设计

> 日期：2026-08-15
> 状态：待用户审核
> 范围：visp-agent（orchestrator）、visp-config（配置）、visp-core（AgentConfig）、visp-daemon（接线）

## 1. 需求概述

限制**全局同时最多 N 个 subagent 并行运行**（主 agent 不计入），默认 N=3，可通过配置调整。达到上限时新请求**排队等待**，有空位再执行。

**已确认的决策**（用户选定）：

| 决策点 | 方案 |
|---|---|
| 限制范围 | 全局最多 3 个 subagent（主 agent 不计入） |
| 超限行为 | 排队等待（不丢请求，有空位再执行） |
| 配置方式 | daemon.toml `[agent] max_concurrent`（默认 3，可配置） |

## 2. 架构概览

```
主 agent 回复 → N 个 agent 工具调用
  → execute_tool_calls → 每个 tokio::spawn(agent 工具)
  → agent.rs: agent 工具发 SpawnRequest + await response_rx（阻塞等结果）
  → orchestrator handle_agent_message → spawn_sub_agent
      ├─ 当前 subagent 数 < max_concurrent → 立即 spawn（现状）
      └─ 达到上限 → 请求入队（queued_spawns），返回（父 agent 继续阻塞）
                                                    │
subagent 完成 → handle_done ──► 从队列取出一个等待的 spawn 请求执行
                                                    │
                              （有空位 → 新 subagent 启动 → 父 agent 的
                                response_rx 收到结果，继续）
```

## 3. 模块职责

| 模块 | 职责 |
|---|---|
| visp-config / config.rs | `AgentSection` 新增 `max_concurrent_subagents: u32`（`#[serde(default)]`，默认 3） |
| visp-core / agent.rs | `AgentConfig` 新增 `max_concurrent_subagents: u32`（与 max_depth 对称） |
| visp-agent / orchestrator.rs | `spawn_sub_agent` 加入并发检查 + 队列；新增 `queued_spawns: VecDeque<QueuedSpawn>`；`handle_done` 完成后消费队列；新增 `active_subagent_count()` 计数 |
| visp-daemon / main.rs | AgentConfig 构建时传入 `max_concurrent_subagents` |

## 4. 核心设计

### 4.1 计数逻辑

```rust
// active_agents 中 parent_session_id.is_some() 的个数 = 当前 subagent 数
fn active_subagent_count(&self) -> usize {
    self.active_agents
        .agents_cloned()
        .iter()
        .filter(|a| a.parent_session_id.is_some())
        .count()
}
```

### 4.2 排队机制（orchestrator.rs）

```rust
// 新增状态
queued_spawns: VecDeque<QueuedSpawn>,   // 等待中的 spawn 请求

struct QueuedSpawn {
    parent_session_id: String,
    call_id: String,
    subagent_type: String,
    description: String,
    prompt: String,
    task_id: Option<String>,
    trace_context: Option<visp_core::TraceContext>,
    response_tx: Option<oneshot::Sender<String>>,
}
```

`spawn_sub_agent` 开头（在 depth check 之后）：
```rust
// 并发检查：达到上限 → 入队等待（父 agent 继续阻塞在 response_rx）
if self.active_subagent_count() >= self.agent_config.max_concurrent_subagents {
    tracing::info!(count = self.active_subagent_count(), max = ..., "subagent concurrency limit reached, queuing");
    self.queued_spawns.push_back(QueuedSpawn { ... });
    return;
}
```

`handle_done` 末尾（subagent 移除后）：
```rust
// 有空位：从队列取出下一个等待的 spawn 请求
if let Some(next) = self.queued_spawns.pop_front() {
    let QueuedSpawn { parent_session_id, call_id, subagent_type, description, prompt, task_id, trace_context, response_tx } = next;
    self.spawn_sub_agent(&parent_session_id, &call_id, &subagent_type, &description, &prompt, task_id.as_deref(), trace_context, response_tx).await;
}
```

### 4.3 关键语义说明

- **父 agent 不感知排队**：父 agent 的 agent 工具一直阻塞在 `response_rx`，直到 subagent 真正启动并完成。排队期间父 agent 保持"等待工具结果"状态（现有行为），无超时变化。
- **主 agent 不计入**：主 agent 通过 `start_main_agent` 启动（parent 为 None），计数只看 `parent_session_id.is_some()`。
- **取消兼容**：排队中的请求若父 agent 被取消，父 agent 的 response_rx 会因通道关闭而收到 Err——排队请求自然失效（无需额外清理，但需确认取消时不会重复执行）。
- **死锁规避**：spawn 检查在入队后立即 return，不阻塞 orchestrator 单线程 run 循环；队列消费发生在 handle_done（异步点），不引入额外等待。

### 4.4 配置

```toml
# daemon.toml [agent]
max_concurrent_subagents = 3   # 可选，默认 3
```

- `config.rs`：`AgentSection` 加字段 `#[serde(default = "default_max_concurrent_subagents")]`，`fn default_max_concurrent_subagents() -> u32 { 3 }`
- `agent.rs`：`AgentConfig` 加 `pub max_concurrent_subagents: u32`
- `main.rs`：构建 AgentConfig 时 `max_concurrent_subagents: config.agent.max_concurrent_subagents`

## 5. 边界情况

| 场景 | 处理 |
|---|---|
| 达到 3 上限，第 4 个请求 | 入队，不 spawn |
| 多个请求排队 | VecDeque FIFO 顺序 |
| subagent 完成释放空位 | handle_done 消费队列，启动下一个 |
| 嵌套 subagent（sub 再 spawn sub） | 计数为全局，嵌套也受同一上限约束 |
| 父 agent 被取消，排队请求未执行 | response_rx 通道关闭 → 父 agent 收到 Err，自然处理 |
| max_concurrent = 0 | 视为 1（至少允许 1 个 subagent），或按配置原样（0 则全部排队——需文档说明） |
| 主 agent 同时多个 | 不计数（只看 parent_session_id.is_some()） |

## 6. 影响范围

| 文件 | 改动 |
|---|---|
| `crates/visp-config/src/config.rs` | AgentSection 字段 + 默认函数 |
| `crates/visp-core/src/agent.rs` | AgentConfig 字段 |
| `crates/visp-agent/src/orchestrator.rs` | 计数 + 队列 + spawn 检查 + handle_done 消费 |
| `crates/visp-daemon/src/main.rs` | AgentConfig 接线 |
| 测试 | orchestrator 并发测试（mock）；config 解析测试 |

## 7. 测试策略（TDD 重点）

1. **config**：`[agent] max_concurrent_subagents = 5` 解析正确；缺省默认 3。
2. **orchestrator 计数**：`active_subagent_count` 对 parent_id Some/None 的过滤正确。
3. **排队行为**（mock agent registry + session_mgr）：
   - 并发达到上限 → 第 N+1 个请求入队（不 spawn）
   - 一个 subagent done → 队列中的请求被取出执行
   - 队列 FIFO 顺序
4. **AgentConfig 传递**：daemon main.rs 接线后 max_concurrent_subagents 生效。

## 8. 验证标准

- `cargo test`（orchestrator/config 相关全过）
- `cargo clippy --workspace -- -D warnings`（0 错误）
- `cargo fmt --check`（通过）
- 手动验证（可选）：让 LLM 一次生成 5 个 agent 调用，观察只有 3 个并行，其余排队。
