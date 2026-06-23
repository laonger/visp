# Wave 1 Step 2 — tokio::spawn 审计清单

对应工作计划：`docs/plans/visp-plan-self-observability.md` §Step 2
对应设计：`docs/design/visp-design-self-observability.md` §11.2

## 总览

| 指标 | 数值 |
|------|------|
| 总调用点数 | **36**（35 × `tokio::spawn` + 1 × `tokio::task::spawn_blocking`） |
| 关键路径 | **9**（设计预期 ~8，实际 9，详见下文差异说明） |
| 后台任务 | **12** |
| 测试 | **15** |

分布 crate：

| Crate | 关键 | 后台 | 测试 | 合计 |
|-------|------|------|------|------|
| visp-core/agent.rs | — | — | 11 | 11 |
| visp-core/agent_loop.rs | 1 | — | 2 | 3 |
| visp-agent/orchestrator.rs | 4 | 1 | 1 | 6 |
| visp-daemon/main.rs | 1 | 1 | — | 2 |
| visp-daemon/service.rs | 2 | 1 | — | 3 |
| visp-cli/client.rs | — | 6 | — | 6 |
| visp-mcp/manager.rs | — | 2 | — | 2 |
| visp-codegraph/watcher.rs | — | 1 | — | 1 |
| visp-llm/mock.rs | — | — | 1 | 1 |
| visp-tools/fetch.rs | 1* | — | — | 1 |
| **合计** | **9** | **12** | **15** | **36** |

> \* `visp-tools/fetch.rs:251` 是 `tokio::task::spawn_blocking`（其余均为 `tokio::spawn`）。

---

## 关键路径（9 处）

（设计 §11.2 预期 ~8 处，实际 9 处，差异见 § 差异说明）

| # | 路径:行号 | 函数 / 用途 | Step 3 处理动作 | parent span |
|---|-----------|-------------|----------------|-------------|
| 1 | `crates/visp-core/src/agent_loop.rs:873` | `execute_tool_calls` 内 tool 执行 spawn | `.instrument(span)` 挂载到当前迭代 span | `visp.agent.iteration`（继承调用点 current span） |
| 2 | `crates/visp-agent/src/orchestrator.rs:366` | `handle_new_session` — main session 事件转发（agent_tx → grpc_tx） | `.instrument(span)` 挂载到 session 处理 span | `visp.orchestrator.handle_session` |
| 3 | `crates/visp-agent/src/orchestrator.rs:390` | `handle_new_session` — main session `run_agent_loop` | `.instrument(span)` 挂载到 session 处理 span | `visp.orchestrator.handle_session` |
| 4 | `crates/visp-agent/src/orchestrator.rs:561` | `spawn_sub_agent` — sub-agent 事件转发（agent_tx → grpc_tx，含 parent metadata） | `.instrument(span)` 使用 spawn span | `visp.subagent.spawn` |
| 5 | `crates/visp-agent/src/orchestrator.rs:585` | `spawn_sub_agent` — sub-agent `run_agent_loop`（设计 §11.2 #1） | **`.instrument(spawn_span)`** — 这是设计文档指定的关键注入点 | `visp.subagent.spawn` |
| 6 | `crates/visp-daemon/src/main.rs:339` | 启动 orchestrator 主循环 `orchestrator.run()` | `.instrument(span)` 挂载到 daemon 启动 span | `visp.daemon.startup`（或 root span） |
| 7 | `crates/visp-daemon/src/service.rs:369` | gRPC `Chat` RPC — inbound stream handler（CLI → Orchestrator） | **`.instrument(chat_root_span)`** — 作为 trace root（设计 §11.2 #3） | `visp.grpc.chat_stream`（per-connection root） |
| 8 | `crates/visp-daemon/src/service.rs:608` | gRPC `Chat` RPC — outbound stream handler（Orchestrator → CLI） | **`.instrument(chat_root_span)`** — 同一 connection 的 companion task | `visp.grpc.chat_stream`（per-connection root） |
| 9 | `crates/visp-tools/src/fetch.rs:251` | `spawn_blocking` — HTML→Markdown 转换（CPU 密集型，tool 执行路径） | `.in_current_span()` 或 `Span::current().instrument()` 继承调用点 | `visp.tool.execute.webfetch`（继承 agent 迭代 span） |

### 关键路径差异说明（设计预期 ~8，实查 9）

设计 §11.2 列出 4 处已识别 + 4 处待审计（5-8）。审计后实际分布：

| 设计编号 | 设计描述 | 审计结果 | 说明 |
|---------|---------|---------|------|
| #1 | `spawn_sub_agent` L584 | ✅ K5 (L585) | 已确认，比设计多 1 行偏移（因代码变更） |
| #2 | orchestrator 主循环 mpsc → spawn | ✅ K2+K3 (L366+L390) | `handle_new_session` 内两个 spawn：forward + agent_loop |
| #3 | daemon gRPC Chat 流 per-stream spawn | ✅ K7+K8 (L369+L608) | inbound + outbound 两个 companion task |
| #4 | agent_loop 内部异步任务 | ✅ K1 (agent_loop.rs:873) | tool 执行 spawn |
| #5 | 待审计 | ✅ K6 (main.rs:339) | orchestrator.run() 启动 task |
| #6 | 待审计 | ✅ K9 (fetch.rs:251) | tool 执行路径 spawn_blocking |
| #7-8 | 待审计 | — | 其余视为后台/测试，非关键路径 |

**结论**：实际关键路径 9 处，比设计预期（8 处）多 1 处，原因：
- 设计未计入 `orchestrator.run()`（main.rs:339）作为一个独立 spawn 点
- 设计未单独列出 `spawn_blocking`（fetch.rs:251），但其在 tool 执行路径上，属于关键业务

---

## 后台（12 处）

| # | 路径:行号 | 函数 / 用途 | 处理动作 |
|---|-----------|-------------|---------|
| B1 | `crates/visp-agent/src/orchestrator.rs:667` | `handle_done` — SubAgentComplete 发送 backpressure fallback（inbox full 时 spawn 发送） | 不挂载 parent；可加 `Span::current()` 简单追踪 |
| B2 | `crates/visp-daemon/src/main.rs:362` | gRPC server 启动（`server::start_server`） | 独立 root span，不挂载 parent |
| B3 | `crates/visp-daemon/src/service.rs:204` | codegraph background full index build | 独立 root span |
| B4 | `crates/visp-cli/src/client.rs:112` | `ChatHandle::send_input` — fire-and-forget gRPC 消息发送 | 不挂载；保持轻量 |
| B5 | `crates/visp-cli/src/client.rs:125` | `ChatHandle::send_ack` — fire-and-forget gRPC 消息发送 | 不挂载 |
| B6 | `crates/visp-cli/src/client.rs:139` | `ChatHandle::send_response` — fire-and-forget gRPC 消息发送 | 不挂载 |
| B7 | `crates/visp-cli/src/client.rs:151` | `ChatHandle::send_cancel` — fire-and-forget gRPC 消息发送 | 不挂载 |
| B8 | `crates/visp-cli/src/client.rs:163` | `ChatHandle::send_join` — fire-and-forget gRPC 消息发送 | 不挂载 |
| B9 | `crates/visp-cli/src/client.rs:176` | `ChatHandle::send_config_update` — fire-and-forget gRPC 消息发送 | 不挂载 |
| B10 | `crates/visp-mcp/src/manager.rs:86` | `McpManager::start_all` — 启动时逐个连接 MCP 服务器 | 独立 root span |
| B11 | `crates/visp-mcp/src/manager.rs:238` | `McpManager::reconnect` — 重连 MCP 服务器 | 独立 root span |
| B12 | `crates/visp-codegraph/src/watcher.rs:58` | `start_watching` — file watcher debounce loop | 独立 root span |

---

## 测试（15 处）

全部在 `#[cfg(test)]` 模块或 `#[tokio::test]` 函数内，**不动**。

| # | 路径:行号 | 测试函数 / 上下文 |
|---|-----------|------------------|
| T1 | `crates/visp-core/src/agent.rs:691` | `run_collect` 辅助函数（tests 模块内） |
| T2 | `crates/visp-core/src/agent.rs:1068` | `test_user_query_approved` |
| T3 | `crates/visp-core/src/agent.rs:1133` | `test_user_query_denied` |
| T4 | `crates/visp-core/src/agent.rs:1206` | `test_user_query_always_allow` |
| T5 | `crates/visp-core/src/agent.rs:1300` | `test_history_appended` |
| T6 | `crates/visp-core/src/agent.rs:1440` | `test_user_query_marker_select_option` |
| T7 | `crates/visp-core/src/agent.rs:1511` | `test_user_query_marker_continue_loop` |
| T8 | `crates/visp-core/src/agent.rs:1580` | `test_user_query_marker_custom_text` |
| T9 | `crates/visp-core/src/agent.rs:1698` | `test_session_completed_status` |
| T10 | `crates/visp-core/src/agent.rs:1751` | `test_panic_does_not_leak_session_running` |
| T11 | `crates/visp-core/src/agent.rs:1837` | `test_panic_emits_error_envelope_to_global_tx` |
| T12 | `crates/visp-core/src/agent_loop.rs:1634` | `test_retry_cancellation` |
| T13 | `crates/visp-core/src/agent_loop.rs:1767` | `test_phase2_tool_spawn_and_collect` |
| T14 | `crates/visp-agent/src/orchestrator.rs:1124` | `test_orchestrator_tracks_sub_agent_join_handles` |
| T15 | `crates/visp-llm/src/mock.rs:133` | `test_chat_stream_cancel_returns_cancelled_within_50ms` |

---

## 关键路径详细分析

### K1: agent_loop.rs:873 — tool 执行 spawn
- **调用栈**：`run_agent_loop` → `execute_tool_calls` → 对每个 ToolCall 并行 spawn task
- **task 内执行**：工具执行（bash/grep/fetch 等）+ 结果收集 + 通过 global_tx 转发 AgentMessage
- **当前 parent span**：无显式 parent，继承 spawning task 的 tracing context
- **Step 3 注入后**：`.instrument(span)` 使用当前 agent 迭代的 `visp.agent.iteration` span 作为 parent，使 tool 执行 span 正确嵌套在 iteration 下
- **备注**：此 spawn 在 `exec_tasks.push(...)` 中收集，之后在 `collect_stream_events` 中 await JoinHandle

### K2-K3: orchestrator.rs:366, 390 — main session spawn
- **调用栈**：orchestrator 主循环 → `handle_new_session` → L366 forward + L390 run_agent_loop
- **task 内执行**：
  - L366: 循环转发 `AgentEvent` → gRPC `AgentEventFrame`
  - L390: `run_agent_loop` 全生命周期
- **当前 parent span**：无显式 parent
- **Step 3 注入后**：同挂载到 `visp.orchestrator.handle_session` span 下
- **备注**：这两个 task 在同一函数内连续创建且生命周期相关（L366 forward task 为 L390 agent loop 提供事件输出通道）

### K4-K5: orchestrator.rs:561, 585 — sub-agent spawn（设计 #1）
- **调用栈**：orchestrator 主循环收货 `SpawnRequest` → `spawn_sub_agent` → L561 forward + L585 run_agent_loop
- **task 内执行**：
  - L561: 循环转发 sub-agent `AgentEvent` → gRPC（含 parent_session_id/name）
  - L585: `run_agent_loop` 全生命周期（子 agent）
- **当前 parent span**：无
- **Step 3 注入后**（设计文档指定的核心注入点）：
  - 在 `spawn_sub_agent` 中先创建 `visp.subagent.spawn` span
  - L561 forward task: `.instrument(spawn_span.clone())`
  - L585 agent loop: **`.instrument(spawn_span)`** — 使子 agent 的 `visp.agent.run` span 挂载到 spawn span 下
  - 同时通过 `TraceContext` + `ParentLinkLayer` 实现 tracing-native 和 OTel 双通道 parent 重建

### K6: daemon/main.rs:339 — orchestrator.run()
- **调用栈**：`main` 函数 → 构造 orchestrator → L339 tokio::spawn(orchestrator.run())
- **task 内执行**：orchestrator 主事件循环（接收 mpsc 消息并调度 agent）
- **当前 parent span**：无
- **Step 3 注入后**：`.instrument(daemon_start_span)` 挂载到 daemon 启动 root span
- **备注**：此任务持续运行直到 daemon 退出

### K7-K8: daemon/service.rs:369, 608 — gRPC Chat 流 per-stream spawn（设计 #3）
- **调用栈**：gRPC `Chat` RPC handler → 创建 inbound (L369) + outbound (L608) 两个 companion task
- **task 内执行**：
  - L369: 循环读取 `in_stream`（CLI 发来的 ClientMessage），路由到 orchestrator / pending_queries
  - L608: 循环读取 `orchestrator_rx`（orchestrator 发来的 AgentEventFrame），转发到 gRPC response stream
- **当前 parent span**：无
- **Step 3 注入后**：两个 task 均 **`.instrument(chat_root_span)`** 挂载到 per-connection root span `visp.grpc.chat_stream`。这个 root span 在 RPC handler 入口创建，所有 connection 内的 agent 活动 span 最终追溯至此
- **备注**：inbound + outbound 是同一 gRPC 双向流的两个方向，应当共享同一个 root span。设计文档要求作为 trace root

### K9: tools/fetch.rs:251 — spawn_blocking HTML→Markdown（工具执行）
- **调用栈**：`run_agent_loop` → `execute_tool_calls` → tool `WebFetch` → `spawn_blocking(html_to_markdown)`
- **task 内执行**：CPU 密集型 HTML→Markdown 转换（同步操作卸到线程池）
- **当前 parent span**：继承 tool 执行点的 tracing context
- **Step 3 注入后**：`.in_current_span()` — 只需继承当前 span（`visp.tool.execute.webfetch`），不需要单独创建子 span
- **备注**：`spawn_blocking` 不同于 `tokio::spawn`，不会脱离 tracing context，继承即可

---

## 风险与备注

### 1. 设计预期与实际差异
- 设计 §11.2 预期 8 处关键路径，实际审计发现 **9 处**。多出的 1 处为 `daemon/main.rs:339`（orchestrator.run() spawn），设计将其隐含在 orchestrator 生命周期中未单独列出。S9 `fetch.rs:251` 是 `spawn_blocking` 而非 `tokio::spawn`，设计未单独计数。
- 不影响 Step 3 工作范围——9 处均需注入，工作量略增但可控。

### 2. 分类边界案例
- **B1 (orchestrator.rs:667)**：SubAgentComplete backpressure fallback。虽在 orchestrator 内，但仅在 try_send 失败时才触发（异常路径），且职责是发完成通知而非业务执行，归为后台。
- **B2 (main.rs:362)**：gRPC server 启动 task。它是 daemon 基础设施，不是 per-request 处理，归为后台。设计 §11.2 #3 的 "gRPC 服务 spawn" 指的是 per-Chat-stream 的 spawn（service.rs:369, 608），非 server 本身。
- **client.rs 6 处**：均为 fire-and-forget 消息发送。任务期间短暂存在，不持有业务状态，不建议加 instrument（仪器化成本和收益不成比例）。

### 3. 注入建议优先级
Step 3 注入时，按以下优先级处理：

| 优先级 | 为什么 |
|--------|--------|
| **P0** | K5 (orchestrator.rs:585 sub-agent spawn) — 设计文档核心注入点，sub-agent trace 断裂的根因 |
| **P0** | K7+K8 (service.rs:369,608 Chat stream) — trace root，整个 connection 的 trace 从此开始 |
| **P1** | K2+K3 (orchestrator.rs:366,390 main session) — main agent 入口 |
| **P1** | K1 (agent_loop.rs:873 tool execution) — 工具调用可观测性 |
| **P2** | K6 (main.rs:339 orchestrator.run()) — orchestrator 启动 |
| **P2** | K9 (fetch.rs:251 spawn_blocking) — 继承 current span 即可，改动最简单 |
| **P3** | 后台 12 处 — 独立 root span 或不动 |
| **不动** | 测试 15 处 — 不修改 |

### 4. cli/client.rs 的 6 处 spawn 是否应重分类？
这 6 处 `tokio::spawn` 在 `ChatHandle` 方法中，是 fire-and-forget 消息发送。它们不在 `#[cfg(test)]` 内，但也不在 agent/orchestrator/daemon 核心路径上。**建议保留为「后台」**。如果 Step 3 时间充裕，可考虑给它们加简单的 `.in_current_span()` 以改善 CLI 侧追踪，但并非必需。

---

## 附录：完整调用点列表

按文件分组列出所有 36 处，含分类标记。

### visp-core/src/agent.rs（11 处 — 全部测试）
| 行号 | 分类 | 函数上下文 |
|------|------|-----------|
| 691 | 测试 | `run_collect` → `run_agent_loop` |
| 1068 | 测试 | `test_user_query_approved` |
| 1133 | 测试 | `test_user_query_denied` |
| 1206 | 测试 | `test_user_query_always_allow` |
| 1300 | 测试 | `test_history_appended` |
| 1440 | 测试 | `test_user_query_marker_select_option` |
| 1511 | 测试 | `test_user_query_marker_continue_loop` |
| 1580 | 测试 | `test_user_query_marker_custom_text` |
| 1698 | 测试 | `test_session_completed_status` |
| 1751 | 测试 | `test_panic_does_not_leak_session_running` |
| 1837 | 测试 | `test_panic_emits_error_envelope_to_global_tx` |

### visp-core/src/agent_loop.rs（3 处）
| 行号 | 分类 | 函数上下文 |
|------|------|-----------|
| 873 | **关键** | `execute_tool_calls` — tool 并行执行 |
| 1634 | 测试 | `test_retry_cancellation` |
| 1767 | 测试 | `test_phase2_tool_spawn_and_collect` |

### visp-agent/src/orchestrator.rs（6 处）
| 行号 | 分类 | 函数上下文 |
|------|------|-----------|
| 366 | **关键** | `handle_new_session` — main session 事件转发 |
| 390 | **关键** | `handle_new_session` — main session run_agent_loop |
| 561 | **关键** | `spawn_sub_agent` — sub-agent 事件转发 |
| 585 | **关键** | `spawn_sub_agent` — sub-agent run_agent_loop |
| 667 | 后台 | `handle_done` — SubAgentComplete 发送 backpressure fallback |
| 1124 | 测试 | `test_orchestrator_tracks_sub_agent_join_handles` |

### visp-daemon/src/main.rs（2 处）
| 行号 | 分类 | 函数上下文 |
|------|------|-----------|
| 339 | **关键** | orchestrator.run() 主循环 |
| 362 | 后台 | gRPC server 启动 |

### visp-daemon/src/service.rs（3 处）
| 行号 | 分类 | 函数上下文 |
|------|------|-----------|
| 204 | 后台 | codegraph 后台全量索引构建 |
| 369 | **关键** | gRPC Chat stream — inbound handler（CLI→Orch） |
| 608 | **关键** | gRPC Chat stream — outbound handler（Orch→CLI） |

### visp-cli/src/client.rs（6 处 — 全部后台）
| 行号 | 分类 | 函数上下文 |
|------|------|-----------|
| 112 | 后台 | `send_input` fire-and-forget |
| 125 | 后台 | `send_ack` fire-and-forget |
| 139 | 后台 | `send_response` fire-and-forget |
| 151 | 后台 | `send_cancel` fire-and-forget |
| 163 | 后台 | `send_join` fire-and-forget |
| 176 | 后台 | `send_config_update` fire-and-forget |

### visp-mcp/src/manager.rs（2 处 — 全部后台）
| 行号 | 分类 | 函数上下文 |
|------|------|-----------|
| 86 | 后台 | `start_all` — MCP 服务器启动连接 |
| 238 | 后台 | `reconnect` — MCP 服务器重连 |

### visp-codegraph/src/watcher.rs（1 处 — 后台）
| 行号 | 分类 | 函数上下文 |
|------|------|-----------|
| 58 | 后台 | `start_watching` — file watcher debounce loop |

### visp-llm/src/mock.rs（1 处 — 测试）
| 行号 | 分类 | 函数上下文 |
|------|------|-----------|
| 133 | 测试 | `test_chat_stream_cancel_returns_cancelled_within_50ms` |

### visp-tools/src/fetch.rs（1 处 — 关键，spawn_blocking）
| 行号 | 分类 | 函数上下文 |
|------|------|-----------|
| 251 | **关键** | `spawn_blocking` — HTML→Markdown 转换 |
