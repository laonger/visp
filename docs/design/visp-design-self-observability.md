# visp 自观测平台设计文档（tracing + OTel 方案）

> 范围：visp 内建自观测能力，基于 Rust `tracing` ecosystem
> 关联：`docs/eval.md` · `docs/review/visp-eval-observability-feasibility.md`

## 文档约定

本文档遵循 `design-doc-no-code` 规则：仅描述架构、流程、职责、接口约定，不含具体代码实现。代码细节留给工作计划与实施阶段。

---

# 1. 设计目标

## 1.1 核心目标

为 visp 提供**进程内可观测性**：

1. agent loop / LLM 调用 / 工具调用 / sub-agent 编排过程具备结构化可追踪能力
2. 数据通道使用 Rust `tracing` 标准生态，零侵入业务逻辑
3. 默认零外部依赖：Wave 1 仅 stdout/file subscriber + session 汇总日志
4. 可平滑升级：Wave 2 接入 `tracing-opentelemetry`，对接任意 OTLP backend

## 1.2 非目标

- 不构建 visp 内部的存储/聚合/查询系统
- 不实现 `/stats` / `/replay` 等 CLI 指标查询命令
- 不引入 event 表 / metrics_snapshot 表 / observability 专用 schema
- 不照搬 eval.md 的 Skill / Handoff / feature completion 自动评分
- 不做决策黑盒的"为什么"自动化推理

## 1.3 设计原则

| 原则 | 含义 |
|---|---|
| **零业务侵入** | 仅靠 `#[instrument]` 与 `tracing` 宏表达观测点，不引入新 trait/中间层 |
| **零运行依赖** | Wave 1 不要求用户启动任何外部服务 |
| **职责分离** | tracing 记录行为；mpsc AgentEvent 驱动 CLI UI；message 表持久化对话 |
| **可选启用** | 通过 RUST_LOG / 配置控制 subscriber，关闭时零开销 |
| **OTel 原生兼容** | span/field 直接采用 OTel GenAI Semantic Conventions |
| **visp-core IO-free** | core 仅持 `TraceContext` 纯数据 + tracing facade，IO 由 daemon 注入 |

---

# 2. 现状回顾

## 2.1 visp 已有的"观测半成品"

| 已存在 | 状态 |
|---|---|
| `tracing` crate 已是工作区基础依赖 | 各 crate 已大量使用 `tracing::info!/warn!/error!` |
| `~/.visp/logs/daemon-*.log` 日志 | 已落地，但是非结构化文本 |
| `AgentEvent` mpsc 通道（daemon → CLI） | 驱动 CLI 实时 UI 更新 |
| `Message::tool()` 含 `tool_result_duration_ms` 列 | **bug**：列已建但未填充 |
| message 表 `provider_metadata` 列 | **bug**：列已建但 agent_loop 始终填 None |
| LLM 端到端 latency | 完全未记录 |

## 2.2 当前观测能力的缺陷

- 日志是散点文本，无结构化字段、无 span 上下文关联
- 一次 agent 执行内的多次 LLM/工具/sub-agent 调用缺乏父子关系
- 无法回答"这个 session LLM 平均耗时多少 / 哪个工具最慢 / 哪次调用触发了 sub-agent"
- 多 agent 编排时主/子 agent 执行轨迹无法关联

## 2.3 visp 多 Agent 通路的关键事实

经代码调研确认：

| 事实 | 影响 |
|---|---|
| `visp-tools/src/task.rs::execute()` 是 `unreachable!()` | sub-agent 创建**不在** task 工具中 |
| 真正 spawn 点：`visp-agent/src/orchestrator.rs::spawn_sub_agent`（L584 tokio::spawn） | spawn span 必须在此处注入 |
| 触发链：`agent_loop.rs:809` 拦截 task tool → `global_tx` mpsc 发 `AgentMessage::SpawnRequest` → orchestrator 接收并 spawn 子 agent | 跨 mpsc 通信，tracing thread-local context **不能自动传递** |
| 子 agent ↔ 父 agent 通过 mpsc inbox + `OrchestratorMessage::SubAgentComplete` 通信 | 子 agent 完成回调也跨 mpsc |
| 关键路径上 8 个 `tokio::spawn` 点 | 全部必须 `.instrument()` 显式挂载 |

## 2.4 数据正确性修复（独立于本方案，但纳入 Wave 1）

| 项 | 修复内容 |
|---|---|
| `Message::tool().tool_result_duration_ms` | agent_loop 调用 tool 时记录耗时并填入 |
| `Message::assistant().provider_metadata` | LLM provider 返回的 usage/model/finish_reason 等元数据填入 |

---

# 3. 整体架构

## 3.1 三条相互独立的数据通道

```text
visp-daemon / visp-agent

   agent_loop ──┐    orchestrator ──┐    llm/tools ──┐
                │                    │                │
                │  (1) tracing span/event             │
                ▼                    ▼                ▼
   ┌──────────────────────────────────────────────────┐
   │       tracing-subscriber Layers                  │
   │  ┌────────────────────────────────────────────┐  │
   │  │ EnvFilter  →  fmt(file/stdout)             │  │
   │  │ ParentLinkLayer (Wave 1，跨 mpsc 字段补全)  │  │
   │  │ MetricsLayer (按 session 累加+汇总日志)     │  │
   │  │ [OTLP Layer] (Wave 2，可选)                 │  │
   │  └────────────────────────────────────────────┘  │
   └──────────────────────────────────────────────────┘

   (2) mpsc AgentEvent ────────► CLI UI 更新
   (3) visp-db message 表 ─────► 对话内容持久化
```

| 通道 | 用途 | 是否落地 |
|---|---|---|
| (1) tracing | 行为记录/性能/调试 | Wave 1 stdout+file+MetricsLayer 汇总；Wave 2 加 OTLP |
| (2) mpsc AgentEvent | CLI 实时 UI | 不落地，进程内瞬态 |
| (3) message 表 | 对话内容 | SQLite 持久化 |

本方案**只新增/规范化通道 (1)**，不动 (2)(3) 现有架构。

## 3.2 visp 进程不存任何 observability 数据

- Wave 1：trace → `~/.visp/logs/daemon-*.log`；session 结束输出一条汇总日志（见 §9.5）
- Wave 2：trace 通过 OTLP exporter 发往用户配置的 collector
- visp-db 不新增任何 observability 相关表
- 没有 `/stats` `/replay` CLI 命令

## 3.3 与 message 表的关系

- tracing 记录"发生了什么 / 耗时 / 父子关系"
- message 表记录"User/Assistant/Tool 说了什么"
- 两者**不要求一致性**

---

# 4. 数据正确性修复（前置）

## 4.1 修复 `tool_result_duration_ms`

- **位置**：`crates/visp-core/src/agent_loop.rs` 工具执行点
- **职责**：调用 Tool 前记录 `Instant::now()`，完成后计算 ms 并填入 `Message::tool()`
- **影响**：`Message::tool()` 构造接口需要支持传入 duration

## 4.2 修复 `provider_metadata`

- **位置**：`crates/visp-llm/src/*` provider 实现 + `crates/visp-core/src/agent_loop.rs`
- **职责**：LLM 响应返回时，把 provider 提供的 usage/model/finish_reason/cache_read/cache_write 等组装为 JSON 填入 `Message::assistant().provider_metadata`
- **影响**：明确 ProviderMetadata 数据结构（哪些字段是公共契约）

## 4.3 与 tracing 方案的关系

修复完成后，**同一份数据同时出现在两个地方**：

- message 表 `provider_metadata` 列（持久化，供 session 恢复后查阅）
- tracing span 的 fields（瞬态/导出 OTel）

不重复计算：从 provider 响应一次性提取，注入 message + 注入当前 tracing span。

---

# 5. Span / Field 设计（对齐 OTel GenAI Semantic Conventions）

## 5.1 Span 层级

`tracing` 的 parent 链在**进程内同一调用栈**自动传递；跨 mpsc 边界由 `TraceContext`（见 §7）显式重建。

```text
visp.agent.run                              (一次 run_agent_loop 调用)
├── visp.agent.iteration                    (loop 内一次迭代)
│   ├── gen_ai.client.operation             (一次 LLM 调用)
│   │   ├── event: gen_ai.client.retry
│   │   ├── event: gen_ai.client.first_token
│   │   └── event: gen_ai.client.completed
│   └── visp.tool.execute                   (一次工具调用)
└── event: visp.agent.completed / failed / cancelled

跨 mpsc 重建后（orchestrator 侧）：
visp.subagent.spawn                         (orchestrator::spawn_sub_agent 中创建)
└── visp.agent.run                          (子 agent，TraceContext 重建 parent 链)
    └── ...（递归）
```

**设计要点**：

- 不设置长生命周期的 `visp.session` span，改为把 `session.id` 作为公共 field 注入所有子 span。理由：tracing span 设计上不适合跨数小时存活
- `visp.subagent.spawn` 在 orchestrator `spawn_sub_agent` 中创建，包裹整个子 agent 生命周期；通过 `.instrument()` 将子 `run_agent_loop` task 挂载到 spawn span
- 子 agent 的 `visp.agent.run` 通过 `TraceContext` 携带的 `parent_span_id` 在 subscriber 层重建与 `visp.subagent.spawn` 的父子关系

## 5.2 Span 命名约定

| Span 名 | 创建位置 | 语义来源 |
|---|---|---|
| `visp.agent.run` | `run_agent_loop()` 入口 | visp 自定义（无 OTel 对应） |
| `visp.agent.iteration` | loop 体每次迭代 | visp 自定义 |
| `gen_ai.client.operation` | `call_llm_with_retry` 实际请求点 | **OTel GenAI 标准 span 名** |
| `visp.tool.execute` | `execute_tool_calls` 内每个 tool | visp 自定义 |
| `visp.subagent.spawn` | `orchestrator::spawn_sub_agent` | visp 自定义 |

LLM 相关用 OTel 标准命名 `gen_ai.client.operation`，agent loop / tool / subagent 用 visp 前缀（OTel 尚无对应规范，Wave 2 可平滑迁移）。

## 5.3 关键 Fields（OTel GenAI Semantic Conventions）

参考：[OpenTelemetry Semantic Conventions for Generative AI](https://opentelemetry.io/docs/specs/semconv/gen-ai/)

### 5.3.1 公共 fields（所有 visp.* span）

| Field | 类型 | 说明 |
|---|---|---|
| `session.id` | String | UUID |
| `session.short_id` | String | 8 字符前缀，便于 CLI/日志显示 |
| `session.parent_id` | String? | 父 session id（仅 sub-session 有） |
| `visp.agent.kind` | String | "primary" / "subagent:<name>" |
| `visp.agent.depth` | u32 | 0=主 agent，递增 |

### 5.3.2 LLM 调用 fields（`gen_ai.client.operation` span）

| Field | 类型 | 何时 record | 说明 |
|---|---|---|---|
| `gen_ai.system` | String | span 创建时 | "anthropic" / "openai"（OTel 标准） |
| `gen_ai.request.model` | String | span 创建时 | "claude-sonnet-4-..." |
| `gen_ai.operation.name` | String | span 创建时 | "chat" / "completion" |
| `gen_ai.request.max_tokens` | u32 | span 创建时 | 对齐 Anthropic API 命名；若 provider 使用 `max_output_tokens` 可同时记录两个字段 |
| `gen_ai.request.temperature` | f64 | span 创建时 | 请求配置 |
| `visp.llm.attempt` | u32 | span 创建时 | 重试次数（0=首次） |
| `gen_ai.usage.input_tokens` | u64 | 完成时 | OTel 标准 |
| `gen_ai.usage.output_tokens` | u64 | 完成时 | OTel 标准 |
| `gen_ai.usage.cache_read_input_tokens` | u64 | 完成时 | Anthropic 扩展（非标准，与未来 OTel 标准缓存字段不同 key，可并存） |
| `gen_ai.usage.cache_creation_input_tokens` | u64 | 完成时 | Anthropic 扩展（同上） |
| `gen_ai.response.finish_reasons` | String（逗号分隔） | 完成时 | OTel 标准为数组，tracing field 不支持 Vec 类型，存储为逗号分隔字符串 |
| `gen_ai.response.model` | String | 完成时 | 实际响应模型版本 |
| `visp.llm.cost_usd` | f64 | 完成时 | visp 计算 |

### 5.3.3 工具调用 fields（`visp.tool.execute` span）

| Field | 类型 | 说明 |
|---|---|---|
| `gen_ai.tool.name` | String | "bash" / "file_edit" 等（OTel 标准） |
| `gen_ai.tool.call.id` | String | 工具调用 id |
| `gen_ai.tool.type` | String | "function"（OTel 标准） |
| `visp.tool.is_error` | bool | 工具是否返回错误 |
| `visp.tool.duration_ms` | u64 | **权威值**：工具执行前后 `Instant::now()` 差值；消费者应优先使用此字段，而非 span 自身 duration（后者包含 instrument 开销，为近似值） |

### 5.3.4 Sub-Agent fields（`visp.subagent.spawn` span）

| Field | 类型 | 说明 |
|---|---|---|
| `visp.subagent.name` | String | "coding-agent" 等 |
| `visp.subagent.session_id` | String | 子 session 的 UUID |
| `visp.subagent.call_id` | String | 父 tool_call_id（用于回调匹配） |
| `visp.subagent.task_id` | String? | 可选跟踪 ID |
| `visp.subagent.depth` | u32 | 当前递归深度 |

### 5.3.5 错误 fields（所有失败 span）

| Field | 类型 | 说明 |
|---|---|---|
| `error.type` | String | "llm_timeout" / "tool_error" / "cancelled"（OTel 标准） |
| `error.message` | String | 错误描述 |
| `otel.status_code` | String | "OK" / "ERROR"（OTel 标准） |

## 5.4 Events（span 内一次性事件）

| 事件名 | 所在 span | 用途 |
|---|---|---|
| `gen_ai.client.first_token` | `gen_ai.client.operation` | 流式响应首 token 到达 |
| `gen_ai.client.retry` | `gen_ai.client.operation` | 重试触发（含原因） |
| `gen_ai.client.completed` | `gen_ai.client.operation` | 完成时携带 usage |
| `visp.tool.retry` | `visp.tool.execute` | 工具内部重试 |
| `visp.agent.cancelled` | `visp.agent.run` | 用户取消 |
| `visp.agent.iteration_limit` | `visp.agent.run` | 达到最大迭代数 |
| `visp.agent.completed` | `visp.agent.run` | 正常完成（含 session 汇总） |

## 5.5 不发 span/event 的场景

- 每个 TextDelta token：流量太大，且 first_token + 总耗时已足够
- 每个 Thinking block：内容已在 message 表
- message 内容本身：tracing 不是用来存对话的

## 5.6 命名策略说明

| 前缀 | 适用范围 | 兼容性 |
|---|---|---|
| `gen_ai.*` | LLM 调用、工具调用相关 | OTel 标准，Wave 2 接入零改动 |
| `visp.*` | agent loop / iteration / subagent / cost 等 visp 自定义概念 | OTel 后续若发布对应规范可平滑改名 |
| `session.*` | 公共上下文 | visp 业务概念 |
| `error.*` / `otel.*` | 错误标记 | OTel 通用标准 |

**Wave 1 即采用此命名，不允许出现 `llm.input_tokens`、`model.name` 等非标准前缀。**

---

# 6. 指标聚合策略

## 6.1 Wave 1：不在 visp 进程内做指标聚合

visp 进程**只发射 span 与 fields**，不维护任何计数器/直方图。

理由：
- 引入指标聚合层（如 `metrics` crate / OTel Metrics）会增加 visp-core 依赖与运行时开销
- Wave 1 用户需求是"看一次 session 用了多少 token / 多少钱"，无需跨 session 趋势
- 跨 session 趋势分析交给 Wave 2 OTel backend

## 6.2 Wave 1 唯一的进程内汇总：session 结束日志（MetricsLayer）

在 `tracing-subscriber` 注册一个**轻量 MetricsLayer**：

| 职责 | 说明 |
|---|---|
| 监听 `gen_ai.client.completed` event | 累加 input/output/cache tokens 与 cost |
| 监听 `visp.tool.execute` span close | 计数 + 累加 duration |
| 监听 `visp.agent.completed` event | 触发**单条汇总日志**输出（仅 primary agent） |

### 6.2.1 Session 隔离（强约束）

visp daemon 通过 gRPC Chat 流**并发服务多个 CLI 客户端**，多个 primary agent 可同时运行。MetricsLayer 必须按 `session.id` 隔离累加器，**绝不允许全局共享**。

| 设计要点 | 说明 |
|---|---|
| 累加器结构 | `DashMap<session.id, SessionMetricsBucket>`（并发安全 HashMap） |
| Bucket 创建时机 | 首次见到该 session.id 的 event 时惰性创建 |
| Bucket 销毁时机 | 对应 `visp.agent.completed`（primary，depth=0）event 触发汇总后，**立即移除**该 bucket |
| 取值来源 | 所有 event 必须携带 `session.id` field，MetricsLayer 从 event field 提取，不从 span context 推断 |
| 异常 session 清理 | 若 session 因 daemon 重启等原因未发 `agent.completed`，bucket 会驻留内存；通过**软上限**（如 64 个并发 session）+ LRU 淘汰防止泄漏 |
| 跨 session 不可串扰 | 单元测试断言：并发 2 个 session，各自 token/cost 数据互不影响 |

### 6.2.2 输出形态

INFO 级日志，进 fmt layer 与 OTLP layer：

```text
[session=a3f2..] visp.agent.completed total_tokens=4521/1283 cache_read=120 cost_usd=0.0234 llm_calls=8 tool_calls=12 duration_ms=14200 iterations=4 subagents=1
```

**特点**：
- 单条结构化 event，复用 tracing 通道（不引入新 IO）
- 字段命名为汇总派生字段（如 `total_input_tokens`），与 §5.3.2 原子字段（`gen_ai.usage.input_tokens`）来源相同但聚合粒度不同
- Sub-agent 完成 event 不触发汇总输出，其消耗已被 primary agent bucket 累加
- 关闭 observability 时不创建该 Layer，零开销

## 6.3 12 项指标 → 附录化

原 12 项指标降级为**附录 A**（OTel backend 查询模板），不是 Wave 1 产物。Wave 1 只保证 span fields 完整且命名合规，让用户在 backend 中自由聚合。


---

# 7. 架构方案：TraceContext + 跨 mpsc parent 重建

## 7.1 核心问题

`tracing` 在**进程内同一调用栈**（同一 tokio task 或显式 `.instrument()`）自动传递 parent。但 visp 的 sub-agent 流程跨越 mpsc channel：

```text
agent_loop (父 task)
  ├─ 拦截 task tool
  └─ global_tx.send(SpawnRequest)        ◀── 通过 mpsc 跨越，span context 丢失
                        │
                        ▼
orchestrator (另一 task)
  └─ spawn_sub_agent
       └─ tokio::spawn(run_agent_loop)   ◀── 子 task 默认无 parent
```

mpsc 通道不携带 `tracing::Span` 类型，且子 agent 在不同 tokio task 中执行，parent 链**必须显式重建**。

## 7.2 解决方案：`TraceContext` 纯数据类型

在 `visp-core` 中新增轻量数据结构 `TraceContext`，采用 **W3C Trace Context 规范**：

| 字段 | 类型 | 说明 |
|---|---|---|
| `trace_id` | String (32 hex chars / 16 bytes) | 整条 trace 的根 id（W3C 标准） |
| `parent_span_id` | String (16 hex chars / 8 bytes) | 调用方 span id（W3C 标准） |
| `trace_flags` | u8 | W3C trace flags（如 sampled bit） |
| `trace_state` | String? | W3C tracestate（vendor-specific propagation） |

**特性**：
- 纯数据类型，无任何 IO 依赖，**完全符合 visp-core IO-free 原则**
- 直接使用 W3C 标准格式（16-byte trace_id / 8-byte span_id），Wave 2 接入 OTel 时**零格式转换**
- 序列化友好，未来若 sub-agent 跨进程（如 daemon 分布式）也兼容
- 不依赖 `tracing::SpanContext` 或 `opentelemetry::Context` 类型（避免 visp-core 引入这两个 crate）

## 7.3 TraceContext 传播路径

```text
agent_loop                                  orchestrator
─────────────────────────────────────       ─────────────────────────────
visp.agent.iteration span 内：              收到 SpawnRequest 后：
  - 提取当前 span 的 trace_id/span_id        - 读取 envelope.trace_context
  - 写入 TraceContext                        - 在 spawn_sub_agent 中创建
  - 放入 AgentMessage::SpawnRequest            visp.subagent.spawn span
                                               (parent 由 ParentLinkLayer
  global_tx.send(Envelope {                     根据 TraceContext 重建)
    session_id,
    trace_context: Some(tc),                 - tokio::spawn(
    message: SpawnRequest { ... }                run_agent_loop(...)
  })                                               .instrument(spawn_span)
                                               )
```

**重建方式**（subscriber 层）：

#### Wave 1：ParentLinkLayer + JSON 字段补全（受 tracing API 限制）

**重要限制**：`tracing` 的 `Layer::on_new_span` 钩子**不能修改已创建 span 的 parent**，parent 在 `info_span!(parent: ...)` 调用时即已固化。因此 Wave 1 的方案是：

| 能做的 | 做法 |
|---|---|
| 在 JSON 输出中正确标注跨进程父子关系 | ParentLinkLayer 在 `on_new_span` 时把 `TraceContext.parent_span_id` 写入 span 的 extension，fmt JSON formatter 输出时附加 `trace_id` / `parent_span_id` 字段 |
| 维护 W3C ID ↔ tracing span Id 双向映射 | 供后处理工具或 Wave 2 OTel layer 查询使用 |
| 集成测试验证字段正确性 | 通过 TestLayer 断言 sub-agent span 携带正确的 parent_span_id |

| 做不到的 | 说明 |
|---|---|
| 修复原生 tracing tree 中的 parent 链 | 受 tracing crate API 限制。`visp.subagent.spawn` 在原生 tree 中的 parent 是 orchestrator 主循环 span，不是发起方的 `visp.agent.iteration` |
| 让 fmt pretty 输出自动正确缩进父子关系 | 仅 JSON 字段语义正确，pretty 文本无法体现 |

**用户视角影响**：
- Wave 1 本地查看：log 中 trace_id / parent_span_id 字段正确，可用 jq 或脚本重建 tree（附录 A 提供模板）
- Wave 1 程序消费：JSON 字段已完整，自动化测试可断言
- Wave 1 心理预期：不要期待 fmt pretty 输出"看起来像 tree"

#### Wave 2：tracing-opentelemetry 真正重建 parent 链

`tracing-opentelemetry` 桥接 layer 提供 `OpenTelemetrySpanExt::set_parent`，可在 OTel context 层面真正设置跨进程 parent，输出到 OTLP backend（Jaeger/Tempo 等）后 trace tree 完全正确。ParentLinkLayer 在 Wave 2 退役。

## 7.4 Instrumentation 注入点一览

| 注入位置 | 注入方式 | 创建的 span / event |
|---|---|---|
| `crates/visp-core/src/agent_loop.rs::run_agent_loop` | `#[instrument]` | `visp.agent.run` |
| `agent_loop.rs` loop 体每次迭代 | 手动 `info_span!` | `visp.agent.iteration` |
| `agent_loop.rs:809` task tool 拦截处 | 从当前 span 提取 TraceContext，写入 SpawnRequest | （无新 span，只传播） |
| `agent_loop.rs::execute_tool_calls` 内每个 tool | 手动 `info_span!` | `visp.tool.execute` |
| `crates/visp-llm/src/*` 各 provider 调用入口 | `#[instrument]` | `gen_ai.client.operation` |
| LLM 重试逻辑 | `tracing::warn!` event | `gen_ai.client.retry` |
| LLM 流式首 token | `tracing::info!` event | `gen_ai.client.first_token` |
| LLM 完成 | `tracing::info!` event + `Span::current().record()` | `gen_ai.client.completed` + 填 usage fields |
| `crates/visp-agent/src/orchestrator.rs::spawn_sub_agent` | 手动 `info_span!` + `parent` 重建 | `visp.subagent.spawn` |
| `orchestrator.rs:584` 子 agent `tokio::spawn` | `.instrument(spawn_span)` | 子 run_agent_loop 挂在 spawn span 下 |

## 7.5 visp-core IO-free 兼容性

| 新增 | 是否 IO | 备注 |
|---|---|---|
| `TraceContext` struct | ❌ | 纯数据，4 个字段 |
| `AgentMessage::SpawnRequest` 增加 `trace_context: Option<TraceContext>` | ❌ | 数据传递 |
| `Envelope` 增加 `trace_context: Option<TraceContext>` | ❌ | 同上 |
| `tracing::info_span!` 等宏调用 | ❌ | facade，subscriber 在 daemon 注册 |
| `tracing-subscriber` 作为 dev-dependency | ❌ | 仅测试期使用，不进入生产二进制 |

**完全符合**现有 visp-core 架构约束。

## 7.6 失败容忍

- `TraceContext` 是 `Option`，缺失时子 agent 的 span 成为独立 root（不影响子 agent 正常运行）
- subscriber 错误静默（tracing 设计原则）
- 工作计划阶段需在 §11.2 spawn 审计中确认所有关键路径都正确传播


---

# 8. Subscriber 配置策略

## 8.1 Wave 1：本地 fmt + MetricsLayer + ParentLinkLayer

在 `crates/visp-daemon/src/main.rs` 启动时初始化：

| Layer | 默认状态 | 说明 |
|---|---|---|
| stdout fmt | 关闭 | 避免污染 daemon 输出，`VISP_LOG_STDOUT=1` 开启 |
| file fmt | 开启 | 写入 `~/.visp/logs/daemon-<timestamp>.log`，JSON 格式 |
| MetricsLayer | 开启 | §6.2 定义，输出 session 汇总日志 |
| ParentLinkLayer | 开启 | §7.3 重建跨 mpsc 的 parent 链 |
| EnvFilter | 默认 `info,visp=debug` | 可通过 `RUST_LOG` 覆盖 |

**配置项**（`~/.config/visp/daemon.toml`）：

```toml
[observability]
enabled = true                  # 是否启用结构化 tracing
format = "json"                 # 文件日志格式：json | pretty
stdout = false                  # 是否同时输出到 stdout
file_retention = 7              # 文件日志保留份数
```

`enabled = false` 时只注册 EnvFilter 控制下的 fmt layer（不挂 MetricsLayer / ParentLinkLayer），关闭后端到端零开销。

## 8.2 Wave 2：tracing-opentelemetry bridge

新增 dependency：
- `tracing-opentelemetry`
- `opentelemetry`
- `opentelemetry-otlp`
- `opentelemetry_sdk`

新增 OTLP layer：

| 配置项 | 默认值 | 说明 |
|---|---|---|
| `[observability.otlp]` 段 | 不启用 | 用户显式配置才开启 |
| endpoint | - | 如 `http://localhost:4317` |
| protocol | "grpc" | "grpc" / "http" |
| sample_rate | 1.0 | 全采样 |
| resource attributes | 自动注入 | service.name=visp, service.version=... |

启用后 fmt layer 与 OTLP layer **并存**，本地仍有日志可查。Wave 2 时 ParentLinkLayer 可被 `tracing-opentelemetry` 的内置 W3C 解析替代。

## 8.3 Subscriber 初始化时机

- 必须在任何 `tokio::spawn` / 业务逻辑启动**之前**完成
- 位于 `visp-daemon/src/main.rs` 的 `#[tokio::main]` 入口最前段
- 失败时降级到 stderr fmt-only（不阻塞 daemon 启动）


---

# 9. 实施分波

## 9.1 Wave 0：前置数据正确性修复（独立 PR）

- 修复 `tool_result_duration_ms`（§4.1）
- 修复 `provider_metadata`（§4.2）
- 验收：现有测试通过 + 新增单测覆盖

## 9.2 Wave 1：tracing 基础设施 + 本地观测能力

### 范围

1. visp-core 新增 `TraceContext` 数据类型（§7.2）
2. `AgentMessage::SpawnRequest` / `Envelope` 增加 `trace_context` 字段
3. 全部 §5.2 列出的 span / event 注入到对应位置
4. ParentLinkLayer 实现（重建跨 mpsc parent 链）
5. MetricsLayer 实现（§6.2 session 汇总日志）
6. daemon 初始化 fmt + MetricsLayer + ParentLinkLayer
7. 配置项 `[observability]` 段
8. §11.2 spawn 审计前置完成

### 不做

- 不引入 OTel 任何 crate
- 不实现 OTLP exporter
- 不引入指标聚合
- 不实现 CLI 命令

### 验收

- daemon log 中能看到完整 span 树（含 sub-agent）
- session 结束时输出汇总日志（含 token / cost / 调用次数）
- 数据正确性：tool_result_duration_ms / provider_metadata 双写一致
- 单元测试：用 `tracing-subscriber::TestLayer` 捕获 span / event 断言

## 9.3 Wave 2：OTLP exporter

### 范围

1. 引入 `tracing-opentelemetry` + OTel SDK + OTLP exporter
2. 注册 OTel layer 到 subscriber
3. 配置项 `[observability.otlp]` 段
4. ParentLinkLayer 可退役（由 OTel 内置机制接管）
5. 文档：用户接入 Jaeger / Tempo / Grafana / Honeycomb 的最小示例

### 验收

- 本地 Jaeger 能看到完整 trace tree
- 子 agent span 正确挂载在 spawn span 下
- gen_ai.* fields 符合 OTel Semantic Conventions

## 9.4 Wave 3（可选 / 视用户反馈）

- 采样策略（高频 LLM 调用降采样）
- TUI 内查看最近一次 session 的 trace 摘要
- Metrics（OTel Metrics）补充

## 9.5 Session 汇总日志样例

```
[session=a3f2bd9c] visp.agent.completed
  total_input_tokens=4521
  total_output_tokens=1283
  cache_read_tokens=120
  cost_usd=0.0234
  llm_calls=8
  tool_calls=12
  duration_ms=14200
  iterations=4
  subagents=1
```

字段不依赖外部聚合，由 MetricsLayer 在进程内累加。**仅 primary agent 完成时输出**（depth=0）。


---

# 10. 测试策略

## 10.1 单元测试：自定义 Layer 捕获

Wave 1 起即用 `tracing::subscriber::with_default` + 自定义 `tracing_subscriber::Layer` 捕获 span / event（或引入 `tracing-test` crate 简化）：

| 测试维度 | 验证点 |
|---|---|
| Span 命名 | `visp.agent.run` / `gen_ai.client.operation` / `visp.tool.execute` / `visp.subagent.spawn` 均被创建 |
| Field 命名 | 严格匹配 §5.3 表（如 `gen_ai.usage.input_tokens` 必须存在且为 u64） |
| Field 取值 | LLM 完成后 input_tokens / output_tokens 与 provider 返回一致 |
| Parent 链 | `visp.tool.execute.parent_id == visp.agent.iteration.id` |
| 跨 mpsc parent 重建 | sub-agent 的 `visp.agent.run.trace_id == 父 visp.agent.iteration.trace_id` |
| TraceContext 缺失 | sub-agent 仍能正常启动，span 成为独立 root |
| Error fields | 工具失败时 `otel.status_code=ERROR` / `error.type` 存在 |
| MetricsLayer 汇总 | 模拟完整 session，断言汇总日志字段值正确 |

## 10.2 集成测试

| 场景 | 验证 |
|---|---|
| 一次完整 agent run（含 2 次 LLM + 3 次 tool） | log 中 span 数与字段完整 |
| 启动 1 个 sub-agent | parent 链跨 mpsc 重建成功 |
| 关闭 observability（`enabled=false`） | 无 MetricsLayer 输出，fmt 仅按 EnvFilter 走 |
| 用户取消 | `visp.agent.cancelled` event 存在 |
| 达到 iteration 上限 | `visp.agent.iteration_limit` event 存在 |

## 10.3 性能与开销

| 维度 | 期望 |
|---|---|
| Subscriber 关闭时 | 几乎零开销（tracing 宏在 disabled 时编译为 no-op） |
| Wave 1 启用时 | 单次 LLM 调用 < 100µs 额外开销 |
| 日志体积 | 一次中等 session（10 iterations）< 1MB JSON |

性能基准放入工作计划阶段，不在设计文档强制。


---

# 11. 风险与缓解

## 11.1 跨 mpsc parent 链断裂（核心风险）

- **风险**：任何一处 `tokio::spawn` 漏掉 `.instrument()`，或 mpsc message 漏带 TraceContext，会导致子 span 成为独立 root，trace tree 断裂
- **缓解**：
  1. §11.2 spawn 点审计前置完成（不在末期），覆盖工程中所有 8 个关键 spawn 点
  2. 集成测试用 TestLayer 断言 parent 链
  3. ParentLinkLayer 内部记录失败匹配数（缺失 TraceContext 但有 parent_span_id 不存在的情况），通过 metric 暴露

## 11.2 关键路径 spawn 点审计清单（Wave 1 前置）

工作计划阶段需对全工程 `tokio::spawn`（约 35 处，含测试）完成分类审计，其中关键路径约 8 处：

| 类别 | 处理策略 |
|---|---|
| 关键业务路径（agent 执行、子 agent spawn、orchestrator 调度） | 必须 `.instrument()` + TraceContext 传播 |
| 后台任务（健康检查、日志轮转） | 用独立 root span，不需要 parent |
| 测试代码 | 不要求 |

**已识别关键路径**（来自调研，全工程 `tokio::spawn` 含测试约 35 处，关键业务路径约 8 处）：

1. `visp-agent/src/orchestrator.rs::spawn_sub_agent` L584 — 子 agent run_loop（**必须**）
2. orchestrator 主循环 spawn（接收 mpsc message 后） — 视实现
3. daemon gRPC 服务 spawn — 每个 Chat 流（**必须**，作为 trace root）
4. agent_loop 内部异步任务（如有） — 视实现
5-8. 其他需在工作计划阶段逐一审计

## 11.3 OTel Rust crate API 稳定性

- **风险**：`opentelemetry` / `opentelemetry-otlp` / `tracing-opentelemetry` 在 0.18→0.24 多次 breaking change，pin 版本后升级成本高
- **缓解**：
  1. Wave 2 启动前重新评估当前最新稳定版本
  2. 把 OTel 相关依赖隔离到 `visp-daemon`，不污染其他 crate
  3. visp-core 的 `TraceContext` 使用 W3C 原生格式，与 OTel crate 版本解耦

## 11.4 性能开销

- **风险**：每个 LLM token 流如果创建 span / event 会显著拖慢
- **缓解**：§5.5 明确不为每个 TextDelta 创建 event，只在 first_token / completed 时 record

## 11.5 日志体积爆炸

- **风险**：长 session（数小时）日志文件膨胀
- **缓解**：
  1. file_retention 配置项
  2. EnvFilter 默认 info 级别，debug 仅 visp 自身
  3. Wave 2 OTLP 后用户可自由控制采样

## 11.6 visp-core 引入 tracing 宏使用范围扩大的隐忧

- **风险**：未来若 visp-core 测试需要断言 tracing 行为，可能引入 tracing-subscriber test util 作为 dev-dependency
- **缓解**：tracing-subscriber 仅作为 dev-dependency 引入，不污染生产依赖

## 11.7 与 OTel GenAI Semantic Conventions 演进的兼容性

- **风险**：OTel GenAI 规范仍在演进，未来字段命名可能变化
- **缓解**：Wave 1 即采用当前稳定字段（如 `gen_ai.usage.input_tokens`），规范变更时通过 alias 兼容旧名


---

# 12. 与现有架构的关系

## 12.1 visp-core 改动

| 改动 | 类型 |
|---|---|
| 新增 `TraceContext` struct | 纯数据 |
| `AgentMessage::SpawnRequest` 字段扩展 | 向后兼容（Option） |
| `Envelope` 字段扩展 | 向后兼容（Option） |
| `Message::tool()` 接受 duration 参数 | 既有 schema，构造接口扩展 |
| `Message::assistant()` 接受 provider_metadata | 既有 schema，构造接口扩展 |
| `run_agent_loop` 加 `#[instrument]` | 行为不变 |
| `execute_tool_calls` 内手动 span | 行为不变 |

**visp-core 仍保持 IO-free**：仅持 tracing facade，subscriber 由 daemon 注入。

## 12.2 visp-llm 改动

| 改动 | 类型 |
|---|---|
| provider 调用入口 `#[instrument(name="gen_ai.client.operation", ...)]` | 行为不变 |
| 重试逻辑发 `gen_ai.client.retry` event | 行为不变 |
| 完成时 `Span::current().record()` 填 usage fields | 行为不变 |
| provider 返回数据组装 `ProviderMetadata` 供 message 使用 | 与 §4.2 修复合并 |

## 12.3 visp-tools 改动

| 改动 | 类型 |
|---|---|
| `task.rs` 不动 | execute() 仍是 unreachable! |
| 其他工具不需要修改 | span 由 agent_loop 在 `execute_tool_calls` 中创建 |

## 12.4 visp-agent 改动（重点）

| 改动 | 类型 |
|---|---|
| `orchestrator.rs::spawn_sub_agent` 创建 `visp.subagent.spawn` span | 新增 |
| `tokio::spawn(run_agent_loop)` 加 `.instrument(spawn_span)` | 关键，重建 parent 链 |
| 接收 SpawnRequest 时读取 `envelope.trace_context` 并应用 | 新增 |

## 12.5 visp-daemon 改动

| 改动 | 类型 |
|---|---|
| 初始化 tracing-subscriber stack | 新增 |
| 实现 ParentLinkLayer | 新增 |
| 实现 MetricsLayer | 新增 |
| 读取 `[observability]` 配置段 | 新增 |
| gRPC Chat 流的 spawn 加 root span instrument | 新增 |

## 12.6 visp-cli 改动

**无改动**。CLI 仍通过 AgentEvent mpsc 接收 UI 事件，tracing 与 CLI 完全解耦。

## 12.7 visp-db 改动

**无改动**。observability 数据不入库。Wave 0 仅修复既有列填充逻辑，schema 不变。

## 12.8 proto 改动

**无改动**。gRPC 协议层不涉及 observability。


---

# 13. 决策记录

## 13.1 为什么不照搬 eval.md 的方案

eval.md 提案构建了独立的 evaluation pipeline（Skill / Handoff / feature completion 自动评分），与 visp 当前定位（开发期 coding agent）不匹配，且需要大量 LLM-as-judge 推理，成本高、价值密度低。本方案聚焦"行为可观测性"而非"质量自动评估"。

## 13.2 为什么用 tracing + OTel 而非自建 event 表

- tracing 是 Rust 生态标准，零学习成本
- OTel Semantic Conventions 已为 GenAI 场景提供字段规范
- 用户接入任意 backend（Jaeger / Tempo / Grafana / Honeycomb / Datadog）无需 visp 适配
- 自建 event 表会重复造轮子，且 schema 演进维护成本高

## 13.3 为什么 Wave 1 不引入 OTel crate

- OTel Rust crate 历史多次 breaking change，过早引入升级成本高
- Wave 1 用户需求是本地 debug / 单次 session 汇总，fmt + MetricsLayer 足够
- 隔离风险：Wave 1 稳定后 Wave 2 再补 OTLP，破坏面更小

## 13.4 为什么用 TraceContext 而非同步化 sub-agent

考虑过的方案：
- **方案 A**：保留 mpsc，仅传 OTel/W3C context 字符串 — 接近本方案，但与 visp-core IO-free 原则边界模糊
- **方案 B**：把 sub-agent 改为同步 await — 破坏现有 orchestrator 编排架构，影响面大
- **方案 C（采纳）**：visp-core 自定义 `TraceContext` 纯数据 + subscriber 层重建 parent — 架构边界清晰，IO-free 100% 合规，Wave 2 平滑迁移到 OTel

## 13.5 为什么删除 `visp.session` span

tracing span 设计为短生命周期（典型 ms~s）。Session 可能跨数小时，把 session 作为 span 会：
- 长时间占用 span registry
- 与 OTel backend 假设不符（多数 backend 默认 trace 超时丢弃）
- 多次跨进程恢复后无法连续

改为把 `session.id` 作为公共 field 注入所有子 span，跨 session 关联在 backend 用 `session.id` 过滤即可。

## 13.6 为什么 12 项指标放在附录

- Wave 1 不引入指标聚合（§6.1）
- visp 不存储 observability 数据，无法在进程内做趋势分析
- 用户在 OTel backend（如 Grafana）用 PromQL/查询语言聚合 span fields 更灵活
- 附录提供查询模板，让用户开箱即用，但不阻塞 Wave 1 落地


---

# 14. 不在本方案范围内

明确划界，避免范围蔓延：

- ❌ `/stats` / `/replay` / `/cost` CLI 命令
- ❌ visp-db 新增 event / metrics_snapshot 表
- ❌ Skill / Handoff / feature completion 自动评分
- ❌ LLM-as-judge 评估
- ❌ Web UI / 内置 trace viewer
- ❌ 跨 session 趋势分析（交给 OTel backend）
- ❌ 实时告警 / 阈值监控
- ❌ 用户行为追踪 / 隐私数据采集

---

# 附录 A：OTel Backend 查询模板（参考）

> 此附录为用户在 Wave 2 接入 OTLP backend 后的查询参考，不是 visp 进程内实现。

字段命名遵循 §5.3。

## A.1 Token 与成本类

| 指标 | 查询逻辑 |
|---|---|
| 单 session 总 input tokens | `sum(gen_ai.usage.input_tokens) where session.id = X` |
| 单 session 总 output tokens | `sum(gen_ai.usage.output_tokens) where session.id = X` |
| 跨 session 平均成本 | `avg(visp.llm.cost_usd) group by gen_ai.request.model` |
| Cache 命中率 | `sum(gen_ai.usage.cache_read_input_tokens) / sum(gen_ai.usage.input_tokens)` |

## A.2 延迟类

| 指标 | 查询逻辑 |
|---|---|
| LLM P50/P95/P99 延迟 | `histogram_quantile(0.5/0.95/0.99, gen_ai.client.operation.duration)` |
| 首 token 延迟 | `gen_ai.client.first_token.timestamp - gen_ai.client.operation.start_time` |
| Tool P95 延迟 | `histogram_quantile(0.95, visp.tool.execute.duration) group by gen_ai.tool.name` |
| 整个 session 耗时 | `max(visp.agent.run.end_time) - min(visp.agent.run.start_time) where session.id = X` |

## A.3 行为类

| 指标 | 查询逻辑 |
|---|---|
| 最常用工具 Top N | `count(visp.tool.execute) group by gen_ai.tool.name order by count desc` |
| Tool 错误率 | `count(visp.tool.execute where visp.tool.is_error=true) / count(visp.tool.execute)` |
| Sub-agent 调用频次 | `count(visp.subagent.spawn) group by visp.subagent.name` |
| 平均 iteration 数 | `avg(count(visp.agent.iteration)) group by session.id` |
| LLM 重试率 | `count(gen_ai.client.retry) / count(gen_ai.client.operation)` |
| 取消率 | `count(visp.agent.cancelled) / count(visp.agent.run)` |

## A.4 推荐 backend

- **本地开发**：Jaeger（docker 单容器即用）
- **轻量自托管**：Grafana Tempo + Grafana
- **SaaS**：Honeycomb / Datadog / Lightstep / Grafana Cloud

---

# 附录 B：术语表

| 术语 | 含义 |
|---|---|
| Span | 一段有起止时间的执行单元（如一次 LLM 调用） |
| Event | span 内一次性事件点（如首 token 到达） |
| Field | span / event 携带的键值对（如 `gen_ai.usage.input_tokens=120`） |
| Subscriber | tracing 数据的消费方（fmt / OTel exporter / 自定义 Layer） |
| Layer | tracing-subscriber 的可组合处理单元 |
| TraceContext | visp 自定义的纯数据类型，用于跨 mpsc 传播 W3C trace context |
| MetricsLayer | visp 自定义 Layer，session 结束时输出汇总日志 |
| ParentLinkLayer | visp 自定义 Layer，根据 TraceContext 重建跨 mpsc 的 parent 链 |
| OTLP | OpenTelemetry Protocol，OTel 标准 wire format |
| OTel GenAI Semantic Conventions | OTel 针对 LLM 场景的 span / field 命名规范 |

---


