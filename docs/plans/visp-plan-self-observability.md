# 工作计划：visp 自观测平台（tracing + OTel 方案）

对应设计：`docs/design/visp-design-self-observability.md`

## 0. 总原则

- 严格 TDD：每个步骤遵循 **红 → 绿 → 测试 → clippy → fmt → 提交** 循环
- 设计文档对应章节用 `§x.y` 引用；本计划只描述步骤、测试用例、验证标准，**不含代码片段**
- 步骤间依赖以「依赖前置」列为准，未列依赖即可并行
- Wave 依赖关系（修订）：
  - **Wave 0** 与 **Wave 1 Step 1 / Step 2** 完全独立，可同时启动
  - **Wave 1 Step 3 / Step 4 / Step 5-cfg** 仅依赖 Step 1，可三路并行
  - **Wave 1 Step 5-sub / Step 5-e2e** 需 Step 3 / Step 4 全部完成
  - **Wave 2** 依赖 Wave 1 全部完成且稳定运行
- 每步骤完成单独 commit；提交信息使用 `feat(scope): xxx` / `fix(scope): xxx` / `test(scope): xxx` 格式
- scope 取值：`core` / `llm` / `agent` / `tools` / `daemon` / `db` / `proto`
- visp-core 仍保持 IO-free：`tracing-subscriber` **仅** 作为 dev-dependency

## 1. 测试基线（Wave 0 启动前记录）

```bash
cargo test 2>&1 | tail -3        # 记录通过数 N0
cargo clippy --all-targets -- -D warnings 2>&1 | tail -3   # 0 警告
cargo fmt -- --check
```

每个 Wave 结束后必须满足：测试 ≥ N0 + 本 Wave 新增数；clippy 零新增警告；fmt 通过。

## 2. 路线总览

| Wave | 范围 | 状态 | 必选 |
|---|---|---|---|
| Wave 0 | 数据正确性修复（§4.1 / §4.2） | 待启动 | 必选 |
| Wave 1 | tracing 基础设施 + 本地观测能力（§9.2） | 待启动 | 必选 |
| Wave 2 | OTLP exporter（§9.3） | 待启动 | 可选 |
| Wave 3 | 采样 / TUI trace 查看 / OTel Metrics（§9.4） | 暂不规划 | 可选 |

预估新增测试用例：Wave 0 约 6 个 · Wave 1 约 47 个 · Wave 2 约 8 个。

## 3. 执行委托与并行总表

本节集中说明每一步骤的执行者、依赖与并行兄弟。具体技术内容仍在各 Step 章节内描述。

### 3.1 角色分工

| 角色 | 职责 | 触发条件 |
|---|---|---|
| **orchestrator** | 路径决策、Wave 衔接、用户审核点、跨 crate 整体验证、失败诊断与回滚决策、合流后整合验证 | 默认协调者 |
| **@fixer** | 单 crate 内 TDD 红 → 绿 → clippy/fmt/commit 闭环；多 site 字段补齐；测试编写 | 任务可在单 crate（或 ≤2 crate）内闭环 |
| **@explorer** | 跨文件搜索 / 调用点定位 / 调用图分析 / 审计清单生成 | 需要在工作区扫描多文件归纳 |
| **@librarian** | 外部库 API 文档与最佳实践集中查询 | 使用陌生 crate 或不熟悉的 API |
| **@oracle** | 高风险设计 review、跨 mpsc 类深层调试、架构 review | 触及并发 / 跨进程 / 全局 subscriber 等高风险路径 |

### 3.2 全局并行依赖图

```
┌───────────────────────── 阶段 A（并行 4 路）─────────────────────────┐
│ Wave 0 Task 0A  (fixer-A, visp-core)                                  │
│ Wave 0 Task 0B  (fixer-B, visp-core + visp-llm)                       │
│ Wave 1 Step 1   (fixer-1, visp-core TraceContext + 字段扩展)          │
│ Wave 1 Step 2   (explorer, tokio::spawn 审计清单) → 用户审核          │
└───────────────────────────────────────────────────────────────────────┘
        ↓ Wave 0 + Step 1 完成（Step 2 审核通过）
┌───────────────────── 阶段 B（并行 5+1 路）───────────────────────────┐
│ Librarian 查询 ①：tracing API（instrument / Span::record / TestLayer）│
│ Step 3a (fixer-3a, visp-core 注入)                                    │
│ Step 3b (fixer-3b, visp-llm 注入)                                     │
│ Step 3c (fixer-3c, visp-agent 注入)                                   │
│ Step 4a (fixer-4a, ParentLinkLayer)                                   │
│ Step 4b (fixer-4b, MetricsLayer)                                      │
│ Step 5-cfg (fixer-5cfg, ObservabilityConfig)                          │
└───────────────────────────────────────────────────────────────────────┘
        ↓ 全部完成 + Step 4 完成后 @oracle review
┌──────────────────────── 阶段 C（串行）──────────────────────────────┐
│ Step 5-sub (fixer-5sub, subscriber 装配，依赖 4a/4b/5cfg)             │
│ Step 5-e2e 单 agent (fixer-5e2e, 依赖 3a/3b/5sub)                     │
│ Step 5-e2e 多 agent  (同 fixer-5e2e 会话, 依赖 3a/3b/3c/4a/5sub)      │
│ 跨 mpsc unmatched_count > 0 调试 → @oracle review                     │
│ 手工 cargo run + tail 日志验证 → 用户审核                             │
└───────────────────────────────────────────────────────────────────────┘
        ↓ Wave 1 验收通过
┌──────────────────────── Wave 2（按需）──────────────────────────────┐
│ Librarian 查询 ②：opentelemetry-otlp / SpanContext / set_parent       │
│ W2-S1 (fixer-otlp-1, feature gate)                                    │
│ W2-S2 (fixer-otlp-2, Tracer + Exporter 装配)                          │
│ W2-S3 (fixer-otlp-3, 跨 mpsc 重建 OTel parent) → @oracle review       │
│ W2-S4 (fixer-otlp-4, OTel Metrics)  ⫽ 与 S3 并行                      │
└───────────────────────────────────────────────────────────────────────┘
```

### 3.3 步骤委托速查表

| 步骤 | 执行者 | 依赖前置 | 可并行兄弟 |
|---|---|---|---|
| W0 Task 0A | @fixer-A | — | W0 Task 0B / W1 S1 / W1 S2 |
| W0 Task 0B (前半 W0B-1~5) | @fixer-B | — | W0 Task 0A / W1 S1 / W1 S2 |
| W0 Task 0B (W0B-6/7 注入 agent_loop) | @fixer-B（接 0A 合流后） | W0A 完成（同改 agent_loop.rs） | — |
| W1 Step 1 | @fixer-1 | — | W0 / W1 S2 |
| W1 Step 2 (审计) | @explorer | — | W0 / W1 S1 |
| W1 Step 2 (审核) | orchestrator + 用户 | Step 2 审计完成 | — |
| W1 Step 3a / 3b / 3c | @fixer-3a / 3b / 3c（三路并行） | Step 1 | Step 4a / 4b / 5-cfg |
| W1 Step 4a / 4b | @fixer-4a / 4b（两路并行） | Step 1 | Step 3 三路 / 5-cfg |
| W1 Step 4 完成后 review | @oracle | 4a/4b 完成 | — |
| W1 Step 5-cfg | @fixer-5cfg | — | Step 3 / Step 4 |
| W1 Step 5-sub | @fixer-5sub | Step 4a / 4b / 5-cfg | — |
| W1 Step 5-e2e | @fixer-5e2e | Step 3 / Step 5-sub | — |
| W1 Step 5 跨 mpsc 调试（如需） | @oracle | unmatched_count > 0 | — |
| W2 Step 1 | @fixer-otlp-1 | Wave 1 | — |
| W2 Step 2 / Step 4 | @fixer-otlp-2 / otlp-4（并行） | W2 S1 | 互相并行 |
| W2 Step 3 | @fixer-otlp-3 + @oracle review | W2 S2 | — |
| 所有 librarian 查询 | @librarian | 各 Step 启动前一次性查询 | 与 fixer 工作并行（先查询后开工） |

### 3.4 预估并行节省

| Wave | 串行预估（人时） | 并行预估（人时） | 节省 | 节省比 |
|---|---|---|---|---|
| Wave 0 | 6h（A 串 B） | 3.5h（A ⫽ B 前半, B 后半 1h） | 2.5h | 42% |
| Wave 1 | 24h（5 步串行） | 13h（阶段 A 3h ⫽ + 阶段 B 6h ⫽ + 阶段 C 4h 串） | 11h | 46% |
| Wave 2 | 8h | 5h（S2 ⫽ S4） | 3h | 38% |

数字为粗略量级估计，不作为承诺；目的是辅助判断「该不该并行」。

---

# Wave 0：数据正确性修复（独立 PR）

**目标**：修复 `Message::tool().tool_result_duration_ms` 始终为 None、`Message::assistant().provider_metadata` 始终为 None 两处现存 bug，并补齐 LLM 端到端 latency 记录。设计文档 §4。

**与 Wave 1 关系**：Wave 0 是数据源修复，Wave 1 才把这些数据注入 tracing span。两者可独立合并；**Wave 0 与 Wave 1 Step 1 / Step 2 可同时启动**，不再强制 Wave 0 先完成（详见 §3.2 并行依赖图）。Wave 1 Step 3 注入 span 时才硬依赖 Wave 0 完成。

**并行性**：Task 0A 与 Task 0B 前半（W0B-1~5）完全独立，可派两个 fixer 并行执行；Task 0B 后半（W0B-6/7 注入 agent_loop）需在 W0A 合流后串行进行（同改 `agent_loop.rs`）。

---

## Task 0A：修复 `tool_result_duration_ms`

**职责**：`crates/visp-core/src/agent_loop.rs` 工具执行点测量耗时，注入 `Message::tool()`。

### W0A-1 红：补构造接口测试
- **文件**：`crates/visp-core/src/message.rs`（与 `Message::tool` 同文件 `#[cfg(test)]` 模块）
- **测试**：
  - `test_message_tool_accepts_duration_ms`：调用 `Message::tool(...)` 构造接口传入 duration，断言返回 Message 的 `tool_result_duration_ms == Some(预期值)`
  - `test_message_tool_duration_none_when_not_supplied`：旧路径不传 duration 时字段保持 None（向后兼容）
- **验收**：测试运行失败，原因是构造接口暂不接受 duration 参数

### W0A-2 绿：扩展 `Message::tool` 构造接口
- 扩展构造接口签名以接受 `duration_ms: Option<u64>`（参考设计 §12.1）
- **不修改**已有调用方，仅新增重载或默认参数路径（保持向后兼容）
- 跑 W0A-1 测试通过

### W0A-3 红：agent_loop 测量耗时单元测试
- **文件**：`crates/visp-core/src/agent_loop.rs` 测试模块
- **测试 `test_agent_loop_records_tool_duration`**：
  - 用 MockTool 在 `execute` 中 `tokio::time::sleep(50ms)`
  - 跑 agent_loop 一次工具调用流程
  - 断言：消息历史里对应的 ToolMessage `tool_result_duration_ms >= 50` 且 `< 5000`（容差）
- **测试 `test_agent_loop_tool_duration_on_error`**：MockTool 返回错误，duration 仍正确记录（不丢失）
- **测试 `test_agent_loop_tool_duration_isolated_per_tool`**：连续两次工具调用，第二次 duration 不包含第一次

### W0A-4 绿：agent_loop 注入测量逻辑
- 在 `execute_tool_calls` 内每个 tool 调用前后调用 `Instant::now()` 测量
- 把 duration 传入 `Message::tool(...)` 构造接口
- 不引入 IO（visp-core 约束）；`Instant` 是纯 std

### W0A-5 验证 + 提交
- `cargo test -p visp-core`
- `cargo clippy -p visp-core --all-targets -- -D warnings`
- `cargo fmt -- --check`
- Commit：`fix(core): 修复 tool_result_duration_ms 始终为 None`

---

## Task 0B：修复 `provider_metadata` + LLM 端到端 latency

**职责**：在 `crates/visp-llm/` 各 provider 返回时构造 `ProviderMetadata`，并在 `agent_loop` 收到响应时注入 `Message::assistant().provider_metadata`。同时记录 LLM 单次调用的端到端耗时（供 §5.3.2 `gen_ai.client.operation` span 使用）。

### W0B-1 红：明确 `ProviderMetadata` 公共契约
- **文件**：`crates/visp-core/src/message.rs` 或新文件
- **测试**：
  - `test_provider_metadata_has_required_fields`：构造一个 `ProviderMetadata`，断言含 model、finish_reason、input_tokens、output_tokens、cache_read_input_tokens、cache_creation_input_tokens、latency_ms 字段（参考设计 §5.3.2）
  - `test_provider_metadata_serializes_to_json`：可序列化为 JSON（供 `provider_metadata` 列持久化）
- **验收**：测试失败（类型不存在或字段不全）

### W0B-2 绿：定义 `ProviderMetadata` 数据结构
- 在 visp-core 中新增 `ProviderMetadata` struct（pub），字段按设计 §5.3.2 公共契约
- `Serialize + Deserialize` 派生
- `Message::assistant(...)` 构造接口扩展支持 `provider_metadata: Option<ProviderMetadata>`
- 跑 W0B-1 通过

### W0B-3 红：Anthropic provider 构造 `ProviderMetadata`
- **文件**：`crates/visp-llm/src/anthropic.rs`（或相应模块）测试段
- **测试**：
  - `test_anthropic_response_carries_metadata`：用 mock HTTP server 返回标准 Anthropic JSON 响应（含 usage / model / stop_reason），断言 provider 解析后构造的 `ProviderMetadata` 字段完整
  - `test_anthropic_cache_tokens_extracted`：响应含 `cache_read_input_tokens` / `cache_creation_input_tokens` 时正确提取
  - `test_anthropic_latency_ms_recorded`：provider 返回的 `ProviderMetadata.latency_ms` ≥ mock 设定的人为延迟

### W0B-4 绿：Anthropic provider 实现
- 在 Anthropic provider 完成 LLM 请求后：测量端到端 latency，解析 usage / model / stop_reason / cache fields，组装 `ProviderMetadata`
- Provider 公共返回类型扩展为同时提供 `ProviderMetadata`（不破坏 streaming chunk 通道）
- 跑 W0B-3 通过

### W0B-5 红+绿：OpenAI provider 同步实现
- 对 `crates/visp-llm/src/openai.rs`（如有）镜像 W0B-3/W0B-4 的测试与实现
- finish_reason 命名差异：OpenAI 用 `finish_reason`，Anthropic 用 `stop_reason`，统一映射到 `ProviderMetadata.finish_reasons: Vec<String>`（设计 §5.3.2 finish_reasons 类型修正）

### W0B-6 红：agent_loop 注入 `provider_metadata`
- **文件**：`crates/visp-core/src/agent_loop.rs` 测试段
- **测试**：
  - `test_agent_loop_assistant_message_has_provider_metadata`：mock provider 返回固定 metadata，断言历史里 assistant message 的 `provider_metadata == Some(预期值)`
  - `test_agent_loop_provider_metadata_persists_through_multi_turn`：多轮对话每个 assistant message 都有自己的 metadata（不串扰）

### W0B-7 绿：agent_loop 注入逻辑
- 调用 provider 收到响应后，把 `ProviderMetadata` 通过扩展的 `Message::assistant(...)` 构造接口注入
- 跑 W0B-6 通过

### W0B-8 验证 + 提交
- `cargo test -p visp-core -p visp-llm`
- `cargo clippy -p visp-core -p visp-llm --all-targets -- -D warnings`
- `cargo fmt -- --check`
- Commit：`fix(core,llm): 填充 provider_metadata 与 LLM latency`

---

## Wave 0 完工验收

- [ ] 6 个新增测试全部通过
- [ ] `cargo test` 总数 ≥ N0 + 6
- [ ] clippy 零新增警告 + fmt 通过
- [ ] 现存 message 表 `provider_metadata` 列在 session 恢复时能正确反序列化（手工验证：跑一次 `cargo run --bin visp -- -p /tmp/test`，发一条消息，`/list` 重启恢复，查 SQLite 该列非空）
- [ ] Wave 0 两个 commit 已 push


---

# Wave 1：tracing 基础设施 + 本地观测能力

**目标**：实现设计 §5–§8 的 tracing 注入与 subscriber 栈，daemon 启动后即可在 `~/.visp/logs/daemon-*.log` 看到完整 span 树（含跨 mpsc parent_span_id 字段），primary agent 完成时输出一条 session 汇总日志。

**前置依赖（修订）**：
- **Step 1 / Step 2** 可与 Wave 0 同时启动（无依赖）
- **Step 3** 依赖 Step 1（TraceContext 类型）+ Wave 0（注入 span 需要数据源）
- **Step 4** 依赖 Step 1（仅需 TraceContext 类型，不依赖 Step 3 落地）
- **Step 5-cfg** 无依赖，可与 Step 3 / 4 并行
- **Step 5-sub / 5-e2e** 需 Step 3 / 4 / 5-cfg 全部完成

**步骤总览**：

| 步骤 | 标题 | 执行者 | 依赖 | 可并行兄弟 |
|---|---|---|---|---|
| Step 1 | `TraceContext` 数据类型 + `AgentMessage`/`Envelope` 字段扩展 | @fixer-1 | — | Wave 0 / Step 2 |
| Step 2 | 关键路径 `tokio::spawn` 审计清单（前置） | @explorer + 用户审核 | — | Wave 0 / Step 1 |
| Step 3 | Instrumentation 注入（3a/3b/3c） | @fixer-3a/3b/3c（3 路并行） | Step 1 + Wave 0 | Step 4 / Step 5-cfg |
| Step 4 | `ParentLinkLayer` + `MetricsLayer` 实现 | @fixer-4a/4b（2 路并行） | Step 1 | Step 3 / Step 5-cfg |
| Step 5-cfg | `ObservabilityConfig` 配置项 | @fixer-5cfg | — | Step 3 / Step 4 |
| Step 5-sub | daemon subscriber 装配 | @fixer-5sub | Step 4 + Step 5-cfg | — |
| Step 5-e2e | 集成测试（单 agent / 多 agent） | @fixer-5e2e | Step 3 + Step 5-sub | — |

预估测试新增：~47 用例。

**Librarian 触点 ①**：阶段 B 启动前，由 orchestrator 派 @librarian 一次性查询 `tracing-subscriber` Layer<S> trait / `Span::current().record` / TestLayer / Extensions API 用法，输出供 3a/3b/3c/4a/4b 五路 fixer 共享。

---

## Step 1：`TraceContext` 数据类型 + 消息字段扩展

**对应设计**：§7.2 / §7.3 / §12.1。
**目标**：visp-core 引入 W3C 格式的 `TraceContext` 纯数据，并在 `AgentMessage::SpawnRequest` 与 `Envelope` 中新增 `Option<TraceContext>` 字段。

### W1-S1-1 红：`TraceContext` 数据契约
- **文件**：`crates/visp-core/src/trace_context.rs`（新建）+ 同文件 `#[cfg(test)]`
- **测试**：
  - `test_trace_context_w3c_format`：构造一个合法 32 hex trace_id + 16 hex span_id 的 TraceContext，断言长度合法
  - `test_trace_context_invalid_length_rejected`：构造接口对不合法 hex 长度返回错误
  - `test_trace_context_clone_eq`：派生 `Clone + PartialEq + Debug`
  - `test_trace_context_serde_roundtrip`：JSON 序列化往返一致（含 `trace_state: None` / `Some` 两种）
  - `test_trace_context_from_w3c_traceparent_header`：解析 W3C `traceparent` 头字符串构造（供 Wave 2 跨进程使用）
- **验收**：测试失败（类型不存在）

### W1-S1-2 绿：实现 `TraceContext`
- 纯数据 struct，字段按设计 §7.2 表格
- 提供构造接口：`new(...)`、`from_traceparent(s: &str) -> Result<Self, _>`
- 派生 `Clone + Debug + PartialEq + Serialize + Deserialize`
- **不依赖** `tracing` / `opentelemetry` crate（visp-core IO-free + 跨版本解耦）
- 跑 W1-S1-1 通过

### W1-S1-3 红：`AgentMessage::SpawnRequest` 携带 `TraceContext`
- **文件**：`crates/visp-core/src/agent.rs` 测试段
- **测试**：
  - `test_spawn_request_carries_trace_context`：构造 SpawnRequest 含 `trace_context: Some(...)`，断言字段保留
  - `test_spawn_request_backward_compat`：旧调用方未填 trace_context 仍成功（字段 `Option`，默认 None）
  - `test_envelope_carries_trace_context`：Envelope 顶层亦含 `trace_context: Option<TraceContext>`

### W1-S1-4 绿：扩展枚举/结构体字段
- `AgentMessage::SpawnRequest` 加 `trace_context: Option<TraceContext>`
- `Envelope` 顶层加 `trace_context: Option<TraceContext>`
- 现有所有构造点（搜索 `SpawnRequest {` / `Envelope {`）补 `trace_context: None`
- 跑 W1-S1-3 通过

### W1-S1-5 验证 + 提交
- `cargo test -p visp-core`
- `cargo clippy -p visp-core --all-targets -- -D warnings`
- `cargo fmt -- --check`
- Commit：`feat(core): TraceContext 数据类型 + AgentMessage 字段扩展`

---

## Step 2：关键路径 `tokio::spawn` 审计清单（前置）

**对应设计**：§11.2。
**执行者**：@explorer（W1-S2-1 审计）+ orchestrator/用户（W1-S2-2 审核）。
**目标**：枚举全工程所有 `tokio::spawn` 调用点，分类为「关键业务路径 / 后台任务 / 测试」，作为 Step 3 注入工作的依据。**本步骤产出文档不修改代码**。

### W1-S2-1：执行审计（@explorer）
- **委托原因**：典型跨文件搜索归纳任务，符合 explorer 委托规则
- explorer 用 codegraph + grep 在 `crates/` 下找全部 `tokio::spawn` 调用（约 35 处含测试）
- 每一处填表：路径:行号 / 所属 task 用途 / 分类（关键 / 后台 / 测试） / Step 3 处理动作（`.instrument(...)` 或不动）
- 关键路径预期 8 处（设计 §11.2 已识别）：
  1. `crates/visp-agent/src/orchestrator.rs::spawn_sub_agent` L584
  2. orchestrator 主循环接收 mpsc message 后的 spawn
  3. daemon gRPC `Chat` 流入口的 per-stream spawn
  4. agent_loop 内部异步任务（如有）
  5-8. 待审计补全
- **产出文件**：`docs/plans/visp-plan-self-observability-spawn-audit.md`

### W1-S2-2：审核 + 提交
- 用户审核审计表完整性
- Commit：`docs(plans): Wave 1 spawn 审计清单`

**验收**：审计文件存在且关键路径处理动作明确（每一项都有 instrument 或 root-span 决策）。


---

## Step 3：Instrumentation 注入

**对应设计**：§5.1 / §5.2 / §5.3 / §5.4 / §7.4 / §12.1–§12.4。
**目标**：在 visp-core / visp-llm / visp-agent 三处分别注入 span 与 event。三个 sub-step 完全独立，**可并行派 3 个 fixer 执行**。

### Step 3a：visp-core 注入（agent_loop / tool / iteration）

#### W1-S3a-1 红：`visp.agent.run` span
- **文件**：`crates/visp-core/src/agent_loop.rs` 测试段（用自定义 TestLayer 捕获，见设计 §10.1）
- **测试**：
  - `test_agent_run_span_created`：跑一次 mock agent loop，TestLayer 捕获到名为 `visp.agent.run` 的 span
  - `test_agent_run_carries_session_id_field`：span 含 `session.id` / `session.short_id` / `visp.agent.kind` / `visp.agent.depth` field（§5.3.1）
  - `test_agent_run_emits_completed_event`：正常结束发 `visp.agent.completed` event
  - `test_agent_run_emits_cancelled_event_on_cancel`：取消时发 `visp.agent.cancelled`
  - `test_agent_run_emits_iteration_limit_event`：达到上限发 `visp.agent.iteration_limit`

#### W1-S3a-2 绿：注入 `visp.agent.run`
- `run_agent_loop` 用 `#[instrument]` 包裹，name = `visp.agent.run`
- 公共字段按 §5.3.1
- 在合适分支发 `visp.agent.completed / cancelled / iteration_limit` event
- 跑 W1-S3a-1 通过

#### W1-S3a-3 红：`visp.agent.iteration` span
- **测试**：
  - `test_agent_iteration_span_nested_under_run`：每次 loop body 创建 `visp.agent.iteration` span，TestLayer 验证 parent 是 `visp.agent.run`
  - `test_agent_iteration_field_count`：iteration 编号通过 field 暴露

#### W1-S3a-4 绿：注入 `visp.agent.iteration`
- loop 体内每次迭代手动创建 span 并进入 scope
- 跑 W1-S3a-3 通过

#### W1-S3a-5 红：`visp.tool.execute` span
- **测试**：
  - `test_tool_execute_span_per_call`：3 次工具调用 → 3 个 `visp.tool.execute` span
  - `test_tool_execute_fields`：含 `gen_ai.tool.name` / `gen_ai.tool.call.id` / `gen_ai.tool.type` / `visp.tool.is_error` / `visp.tool.duration_ms`（§5.3.3）
  - `test_tool_execute_duration_ms_uses_authoritative_value`：与 Wave 0 注入到 Message 的值完全一致（§5.3.3 权威值约束）
  - `test_tool_execute_is_error_true_on_failure`：失败工具调用 is_error=true

#### W1-S3a-6 绿：注入 `visp.tool.execute`
- `execute_tool_calls` 内每个 tool 创建手动 span
- 在 tool 完成时通过 `Span::current().record(...)` 写入 fields（duration 复用 Wave 0 测量值）
- 跑 W1-S3a-5 通过

#### W1-S3a-7 红：TraceContext 注入到 SpawnRequest
- **测试**：
  - `test_task_tool_intercepts_and_attaches_trace_context`：agent_loop 拦截 task tool 时，从当前 `visp.agent.iteration` span 提取 trace_id/span_id 写入 SpawnRequest + Envelope
  - `test_trace_context_extracted_from_iteration_span`：提取的 trace_id 是 `visp.agent.run` 根的 trace_id，parent_span_id 是 `visp.agent.iteration` 的 span_id

#### W1-S3a-8 绿：实现 TraceContext 提取
- 在 `agent_loop.rs:809` task tool 拦截处提取当前 span 的 W3C ID
- 写入 `Envelope.trace_context` + `SpawnRequest.trace_context`，通过 `global_tx.send(...)` 投递
- 跑 W1-S3a-7 通过

#### W1-S3a-9 验证 + 提交
- `cargo test -p visp-core`
- clippy + fmt
- Commit：`feat(core): 注入 agent.run / iteration / tool.execute span + TraceContext 传播`


### Step 3b：visp-llm 注入（`gen_ai.client.operation` span + 重试/首 token event）

#### W1-S3b-1 红：`gen_ai.client.operation` span 命名 + 创建时字段
- **文件**：`crates/visp-llm/src/anthropic.rs`（与 openai.rs 对称）测试段
- **测试**：
  - `test_gen_ai_client_operation_span_created`：跑一次 mock provider 请求，TestLayer 捕获到名为 `gen_ai.client.operation` 的 span（§5.2 OTel 标准命名）
  - `test_gen_ai_request_fields_at_span_start`：span 创建时已 record `gen_ai.system` / `gen_ai.request.model` / `gen_ai.operation.name` / `gen_ai.request.max_tokens` / `gen_ai.request.temperature` / `visp.llm.attempt`（§5.3.2）
  - `test_max_tokens_field_aligned_with_anthropic_api`：字段名是 `gen_ai.request.max_tokens`（不是 `max_output_tokens`），符合设计 §5.3.2 注释

#### W1-S3b-2 绿：创建 `gen_ai.client.operation` span
- 在 provider 实际请求点用 `info_span!("gen_ai.client.operation", ...)` 创建
- 通过 `attempt` field 暴露当前重试次数（首次=0）
- 跑 W1-S3b-1 通过

#### W1-S3b-3 红：completion 时 record usage fields
- **测试**：
  - `test_gen_ai_usage_fields_recorded_on_completion`：完成后 span 含 `gen_ai.usage.input_tokens` / `output_tokens` / `cache_read_input_tokens` / `cache_creation_input_tokens` / `gen_ai.response.finish_reasons` / `gen_ai.response.model` / `visp.llm.cost_usd`（§5.3.2）
  - `test_finish_reasons_serialized_as_comma_separated_string`：tracing field 不支持 Vec，存为逗号分隔字符串（设计 §5.3.2 finish_reasons 类型修正）
  - `test_cost_usd_computed_from_usage_and_pricing`：cost_usd 通过 provider 已知定价表与 usage 计算得出

#### W1-S3b-4 绿：record 完成时字段
- LLM 响应处理结束时调用 `Span::current().record(...)` 填入 usage / model / finish_reasons / cost
- 复用 Wave 0 已构造的 `ProviderMetadata` 作为唯一数据源（"一次提取，两处写入"，设计 §4.3）
- 跑 W1-S3b-3 通过

#### W1-S3b-5 红：retry / first_token / completed event
- **测试**：
  - `test_gen_ai_client_retry_event_emitted`：重试发生时发 `gen_ai.client.retry` event 含原因 field（§5.4）
  - `test_gen_ai_client_first_token_event`：流式响应首 token 到达时发 `gen_ai.client.first_token`
  - `test_gen_ai_client_completed_event`：完成时发 `gen_ai.client.completed` event 含 usage 摘要

#### W1-S3b-6 绿：注入 event
- 在重试逻辑、流首 token、完成点分别 `tracing::event!`
- 跑 W1-S3b-5 通过

#### W1-S3b-7 红+绿：OpenAI provider 镜像
- 对 `openai.rs` 做与 W1-S3b-1~6 等价的注入与测试
- finish_reason 映射差异已在 Wave 0 处理（统一为 `Vec<String>`），span 层直接用

#### W1-S3b-8 验证 + 提交
- `cargo test -p visp-llm`
- clippy + fmt
- Commit：`feat(llm): 注入 gen_ai.client.operation span 与重试/首 token/completed event`

### Step 3c：visp-agent 注入（`visp.subagent.spawn` + orchestrator parent）

#### W1-S3c-1 红：`visp.subagent.spawn` span 创建点
- **文件**：`crates/visp-agent/src/orchestrator.rs` 测试段
- **测试**：
  - `test_subagent_spawn_span_created_in_orchestrator`：orchestrator 收到 SpawnRequest 后，在 `spawn_sub_agent` 调用处创建 `visp.subagent.spawn` span
  - `test_subagent_spawn_fields`：span 含 `visp.subagent.name` / `visp.subagent.session_id` / `visp.subagent.call_id` / `visp.subagent.task_id?` / `visp.subagent.depth`（§5.3.4）
  - `test_subagent_run_loop_attached_via_instrument`：内部 `tokio::spawn(run_agent_loop)` 通过 `.instrument(spawn_span)` 挂载，TestLayer 验证子 `visp.agent.run` 的 parent 在原生 tree 中是 `visp.subagent.spawn`

#### W1-S3c-2 绿：在 `spawn_sub_agent` L584 处注入
- 用 `info_span!("visp.subagent.spawn", ...)` 创建（**不是** `#[instrument]` on task tool — 设计 §7.4 已修正注入点）
- 子 `tokio::spawn(run_agent_loop(...).instrument(spawn_span.clone()))`
- 跑 W1-S3c-1 通过

#### W1-S3c-3 红：TraceContext 从 Envelope 转给 ParentLinkLayer
- **测试**：
  - `test_orchestrator_reads_trace_context_from_envelope`：收到带 `trace_context: Some(tc)` 的 envelope 时，spawn span 通过 extension 携带这份 tc，供 ParentLinkLayer 读取
  - `test_orchestrator_missing_trace_context_falls_back_to_orphan`：envelope 无 trace_context 时（异常路径），spawn span 不带 tc，仍能正常运行（不 panic）

#### W1-S3c-4 绿：把 TraceContext 写入 span extension
- orchestrator 在创建 `visp.subagent.spawn` span 后立刻 `span.with_subscriber(|s| ...)` 把 TraceContext 注册到 span extension（供 ParentLinkLayer 在 `on_new_span` 中 retrieve）
- 跑 W1-S3c-3 通过

#### W1-S3c-5 验证 + 提交
- `cargo test -p visp-agent`
- clippy + fmt
- Commit：`feat(agent): 注入 visp.subagent.spawn span + TraceContext 转发`

### Step 3 并行执行说明
- 3a / 3b / 3c 之间无代码依赖（不同 crate，TraceContext 类型在 Step 1 已就绪）
- **执行者**：派 3 个 fixer 同时启动 — fixer-3a 处理 visp-core、fixer-3b 处理 visp-llm、fixer-3c 处理 visp-agent
- **Librarian 触点 ①**：在派出 fixer 之前由 orchestrator 派 @librarian 集中查询：
  - `#[instrument]` 宏的字段记录 / parent / skip 用法
  - `Span::current().record(...)` 与 `info_span!` field 类型约束（Vec 不支持，需序列化）
  - `tracing_subscriber::Layer` 的 `on_new_span` / `on_event` / Extensions 写法（TestLayer 模板）
  - 查询结果传给 3a/3b/3c/4a/4b 五路 fixer 复用，避免重复查文档
- 三者完成后整体跑一遍 `cargo test --workspace` 防止意外破坏


---

## Step 4：`ParentLinkLayer` + `MetricsLayer` 实现

**对应设计**：§6.2 / §7.3 Wave 1 段 / §10.1。
**执行者**：@fixer-4a / @fixer-4b 两路并行。
**目标**：在 daemon 侧实现两个自定义 tracing Layer。两者无依赖，**可并行派 2 个 fixer**。

**Oracle review 触点 ①**：4a 完成后由 orchestrator 派 @oracle 对 `ParentLinkLayer` 跨 mpsc 设计与 Wave 2 升级兼容性做架构 review（高风险模块）。

### Step 4a：`ParentLinkLayer`

**职责**：跨 mpsc 边界为 `visp.subagent.spawn` 与 `visp.agent.run` span 补 JSON 字段 `trace_id` / `parent_span_id`，并维护 W3C ID ↔ tracing span Id 双向映射。

**Wave 1 限制**（设计 §7.3）：仅做字段补全，**不修改 tracing tree parent**。

#### W1-S4a-1 红：Layer 基本骨架
- **文件**：`crates/visp-daemon/src/observability/parent_link.rs`（新建）
- **测试**：
  - `test_parent_link_layer_compiles_and_registers`：用 `tracing-subscriber::registry()` + Layer 组装通过编译
  - `test_parent_link_layer_no_op_when_no_trace_context`：未携带 TraceContext 的 span 不报错，正常通过

#### W1-S4a-2 绿：实现 Layer trait
- 实现 `tracing_subscriber::Layer<S>`，重写 `on_new_span`
- 从 span attributes 或 extensions 取 TraceContext（设计 §7.3：orchestrator 已写入 extension）
- 维护 `DashMap<W3CSpanId, tracing::span::Id>` 双向映射
- 跑 W1-S4a-1 通过

#### W1-S4a-3 红：JSON 字段补全
- **测试**：
  - `test_parent_link_layer_writes_trace_id_field`：携带 TraceContext 的 span 在 JSON 输出中含 `trace_id` 字段（值等于 TraceContext.trace_id）
  - `test_parent_link_layer_writes_parent_span_id_field`：含 `parent_span_id` 字段
  - `test_parent_link_layer_fields_present_only_on_subagent_spans`：普通 span（非跨 mpsc）不附加字段（避免冗余）

#### W1-S4a-4 绿：字段写入实现
- 用 `tracing-subscriber` 的 fmt visitor / FormatFields 钩子注入额外字段
- 仅在 span extension 中存在 TraceContext 时写入
- 跑 W1-S4a-3 通过

#### W1-S4a-5 红：失败容忍 + metric
- **测试**：
  - `test_parent_link_layer_unmatched_parent_recorded`：TraceContext 的 parent_span_id 在映射表中找不到时，记录到内部计数器（不 panic、不 abort）
  - `test_parent_link_layer_exposes_unmatched_count`：内部计数器可通过 public 方法读取（供调试 / Wave 2 验收）

#### W1-S4a-6 绿：实现计数与诚实失败
- DashMap 找不到时累加 `unmatched_count`
- 提供 `fn unmatched_count(&self) -> u64`
- 跑 W1-S4a-5 通过

#### W1-S4a-7 验证 + 提交
- `cargo test -p visp-daemon`
- clippy + fmt
- Commit：`feat(daemon): ParentLinkLayer 跨 mpsc parent_span_id 字段补全`

### Step 4b：`MetricsLayer`（session 汇总日志）

**职责**：按 `session.id` 隔离累加 token / cost / 调用次数，仅在 primary agent（depth=0）的 `visp.agent.completed` event 触发输出汇总日志。

**强约束**（设计 §6.2.1）：必须按 session 隔离，绝不全局共享。

#### W1-S4b-1 红：Bucket 数据结构
- **文件**：`crates/visp-daemon/src/observability/metrics_layer.rs`（新建）
- **测试**：
  - `test_session_metrics_bucket_default_zero`：默认值全 0
  - `test_session_metrics_bucket_accumulates_tokens`：连续 3 次 add_llm_completion，input/output/cache tokens 累加
  - `test_session_metrics_bucket_accumulates_tool_calls`：tool_calls 计数 + duration 累加
  - `test_session_metrics_bucket_format_summary`：含 token / cost / 调用次数 / duration_ms / iterations / subagents 等字段，符合设计 §9.5 样例

#### W1-S4b-2 绿：实现 `SessionMetricsBucket`
- struct 含累加字段，方法 `add_llm_completion` / `add_tool_completion` / `format_summary(&self) -> tracing::Event`
- 跑 W1-S4b-1 通过

#### W1-S4b-3 红：Layer + session 隔离
- **测试**：
  - `test_metrics_layer_isolates_per_session`：并发 2 个 session 各自累加，互不干扰（断言两个 bucket 数据独立）
  - `test_metrics_layer_uses_event_session_id_field`：MetricsLayer 从 event 的 `session.id` field 取值，不从 span context 推断（设计 §6.2.1）
  - `test_metrics_layer_lazy_bucket_creation`：首次见到 session.id 时惰性创建 bucket

#### W1-S4b-4 绿：实现 Layer with `DashMap<SessionId, Bucket>`
- 监听 `gen_ai.client.completed` event → bucket.add_llm_completion
- 监听 `visp.tool.execute` span close → bucket.add_tool_completion
- 跑 W1-S4b-3 通过

#### W1-S4b-5 红：汇总输出 + bucket 销毁
- **测试**：
  - `test_metrics_layer_emits_summary_on_primary_agent_completed`：depth=0 的 `visp.agent.completed` event 触发输出单条 INFO 日志（用 TestLayer 捕获）
  - `test_metrics_layer_no_summary_for_subagent_completed`：depth>0 的 completed event 不输出汇总（消耗已被 primary bucket 累加）
  - `test_metrics_layer_bucket_removed_after_summary`：输出汇总后立即从 DashMap 移除该 session 的 bucket
  - `test_metrics_layer_summary_format_matches_spec`：输出格式与设计 §9.5 样例字段一致

#### W1-S4b-6 绿：实现汇总触发 + 销毁
- 在 `on_event` 中识别 `visp.agent.completed` + depth=0 → 取 bucket → 转 `tracing::info!` 一条结构化日志 → 立即移除
- 跑 W1-S4b-5 通过

#### W1-S4b-7 红：软上限 + LRU 防泄漏
- **测试**：
  - `test_metrics_layer_soft_limit_64_sessions`：连续创建 65 个 session bucket，最旧的被 LRU 淘汰（最新 64 个保留）
  - `test_metrics_layer_lru_promotes_on_access`：访问旧 bucket 时晋升为最新（标准 LRU 语义）

#### W1-S4b-8 绿：LRU 实现
- 用 `parking_lot::Mutex<LruCache>` 或 `lru` crate 实现软上限
- 阈值常量 `MAX_CONCURRENT_SESSIONS = 64`
- 跑 W1-S4b-7 通过

#### W1-S4b-9 验证 + 提交
- `cargo test -p visp-daemon`
- clippy + fmt
- Commit：`feat(daemon): MetricsLayer 按 session 累加 + 汇总日志 + LRU 软上限`


---

## Step 5：daemon subscriber 初始化 + 配置项 + 集成测试

**对应设计**：§6.1 / §8 / §9.4 / §11.1。
**目标**：把所有 Layer 在 `crates/visp-daemon/src/main.rs` 装配，添加配置项控制开关与采样，并跑 e2e 集成测试覆盖跨 mpsc 链路。

**执行拆分（修订）**：
- **Step 5-cfg**（W1-S5-1 / W1-S5-2）：@fixer-5cfg，无依赖，**与 Step 3 / Step 4 并行**
- **Step 5-sub**（W1-S5-3 / W1-S5-4）：@fixer-5sub，依赖 Step 4a / 4b / 5-cfg
- **Step 5-e2e**（W1-S5-5 ~ W1-S5-8）：@fixer-5e2e，依赖 Step 3 全部 + Step 5-sub
- **W1-S5-9 整体验收**：orchestrator + 用户手工验证

**Oracle review 触点 ②**：若 W1-S5-7 e2e 多 agent 测试中 `ParentLinkLayer.unmatched_count > 0`，由 orchestrator 派 @oracle 进行跨 crate 调试与根因分析。

### W1-S5-1 红：配置项 `ObservabilityConfig`
- **文件**：`crates/visp-daemon/src/config.rs` 测试段
- **测试**：
  - `test_observability_config_default`：默认 `enabled=true` / `level="info"` / `format="json"` / `parent_link=true` / `metrics_summary=true`（设计 §8）
  - `test_observability_config_disabled_via_toml`：TOML `[observability] enabled = false` 反序列化生效
  - `test_observability_config_log_file_path`：可配置 `log_file = "/path/to/file"`，默认沿用 `~/.visp/logs/daemon-<timestamp>.log`

### W1-S5-2 绿：实现配置 struct
- 在 `DaemonConfig` 加 `pub observability: ObservabilityConfig`
- Default impl 按设计 §8
- 跑 W1-S5-1 通过

### W1-S5-3 红：subscriber 装配测试
- **文件**：`crates/visp-daemon/src/observability/mod.rs`（新建）测试段
- **测试**：
  - `test_subscriber_init_with_all_layers`：构造默认 ObservabilityConfig，`init_subscriber()` 返回 `WorkerGuard`（非阻塞写入）+ Layer 栈含 fmt + ParentLink + Metrics
  - `test_subscriber_init_respects_disabled_flag`：`enabled=false` 时返回 no-op subscriber（不写文件）
  - `test_subscriber_init_idempotent`：调用两次返回错误或 no-op（防止重复全局 set）

### W1-S5-4 绿：装配实现
- 在 `observability/mod.rs` 实现 `init_subscriber(cfg: &ObservabilityConfig) -> Result<WorkerGuard, _>`
- 用 `tracing-subscriber::registry().with(...)` 串接：EnvFilter → fmt (JSON, non-blocking) → ParentLinkLayer → MetricsLayer
- 用 `tracing-appender` 提供 rolling daily file + non-blocking writer
- 在 `main.rs` 启动早期调用，保存 guard 到进程生命周期
- 跑 W1-S5-3 通过

### W1-S5-5 红：e2e 集成测试 — 单 agent
- **文件**：`crates/visp-daemon/tests/observability_e2e.rs`（新建）
- **测试**：
  - `test_e2e_single_agent_emits_full_span_tree`：启动 daemon + 模拟一次 Chat 请求（一轮 LLM + 一次 tool），读取日志文件断言含 `visp.agent.run` / `visp.agent.iteration` / `gen_ai.client.operation` / `visp.tool.execute` 四种 span
  - `test_e2e_single_agent_emits_summary_log`：primary agent 完成后日志末尾含一条 `metrics.session.summary` INFO 日志，字段齐全

### W1-S5-6 绿：让 e2e 通过
- 必要时补桩（mock LLM provider、mock tool）
- 跑 W1-S5-5 通过

### W1-S5-7 红：e2e 集成测试 — 多 agent 跨 mpsc
- **测试**：
  - `test_e2e_subagent_span_has_parent_span_id_field`：触发 task tool → 子 agent 启动，日志中子 `visp.agent.run` 的 JSON 含正确 `trace_id` + `parent_span_id`（指向父 iteration）
  - `test_e2e_two_concurrent_sessions_isolated`：并发两次 Chat 流，各自汇总日志的 session.id 独立、token 累加互不污染
  - `test_e2e_parent_link_unmatched_count_zero`：正常流程结束后 ParentLinkLayer.unmatched_count == 0

### W1-S5-8 绿：修复跨 mpsc 链路问题（如有）
- 若 unmatched_count > 0，根据计数定位丢失 TraceContext 的路径，回溯 Step 3a/3c 修复
- 跑 W1-S5-7 通过

### W1-S5-9 验证 + 提交
- `cargo test --workspace`（全量）
- `cargo clippy --workspace --all-targets -- -D warnings`
- `cargo fmt -- --check`
- 手工验证：`cargo run --bin visp -- -p /tmp/test`，跑 3 轮对话含 task tool，`tail -200 ~/.visp/logs/daemon-*.log` 目视确认 span 树
- Commit：`feat(daemon): 装配 observability subscriber + 配置项 + e2e 测试`

---

## Wave 1 总验收清单

- [ ] Step 1–5 全部 commit 已推送
- [ ] `cargo test --workspace` 通过，新增用例 ≥ 47
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` 0 警告
- [ ] `cargo fmt -- --check` 通过
- [ ] 默认配置启动 daemon 后，日志文件可见 4 种 Wave 1 span + 1 条 summary
- [ ] 跨 mpsc 链路 ParentLinkLayer.unmatched_count == 0（e2e 验证）
- [ ] visp-core 仍不依赖 `tracing-subscriber`（运行时），仅 dev-dep
- [ ] daemon 进程无 SQLite event 表 / 无 `/stats` `/replay` CLI 命令（确认未误引入）
- [ ] 设计文档 §11.2 spawn 审计清单 8 个关键路径已全部处理
- [ ] 关闭配置 `observability.enabled=false` 后日志文件不再写入新 span

---

# Wave 2：OTLP 导出（生产级观测后端接入）

**目标**：通过 `tracing-opentelemetry` + `opentelemetry-otlp` 将 Wave 1 已有的 span 导出到**用户自管**的 OTel collector（Jaeger / Tempo / Honeycomb / Grafana / SigNoz 任选），在 OTel 层真正重建跨 mpsc 的 parent-child 关系。

**范围**：仅 daemon 侧 OTel **接入层**（Tracer 装配 / Exporter / ParentLinkLayer 升级 / orchestrator OTel context 注入）。不在 visp 仓内提供 collector / 后端 / UI（场景 1：本地自用）。

**前置**：Wave 1 全部完成（commit `06b3b8d`）；测试基线 visp-core 211 / visp-llm 88 / visp-agent 36 / visp-daemon 144（1 ignored）。

**预估测试新增**：~14 用例。

## Wave 2 三大设计决策（用户已确认）

| 编号 | 决策 | 说明 |
|---|---|---|
| **D1** | ParentLinkLayer 双模式（Y 方案） | OTLP 未启用 → 走 W1 uuid 路径（W1 行为完全不变）；OTLP 启用 → 切换到 OTel 权威源，从 OTel span extension 读真实 trace_id/span_id 写入 `visp.span.w3c_id`，避免跨 mpsc 双源不一致 |
| **D2** | TraceContext schema 保留 | visp-core 的 `SpawnRequest.trace_context` 字段不变。OTLP 启用时 trace_id/span_id 来自 OTel SDK；子 agent 端用 TraceContext 重建 `opentelemetry::trace::SpanContext`，调 `OpenTelemetrySpanExt::set_parent()` 挂上真实 OTel parent |
| **D3** | 默认 disabled | `[observability.otlp] enabled = false` 默认值。无 collector 时不报错、不影响 W1 行为。不使用 cargo feature gate，纯运行时分支控制（依赖始终编译进 daemon） |

## 跨步骤架构要点

- **装配链顺序**：`registry → EnvFilter → OpenTelemetryLayer(条件, with_context_activation=true) → ParentLinkLayer → MetricsLayer → fmt`。OTel 层必须在 ParentLinkLayer **之前**（更外层），这样 ParentLinkLayer 在 `on_enter` 时能拿到 OTel 已固化的 SpanContext
- **`with_context_activation(true)` 强制声明**：tracing-opentelemetry 0.33 默认值为 `true`，仍显式声明以避免上游默认值变化（POC 已实测有此 API）；这是 Step 5 子 agent attach Context 方案能工作的硬前提
- **采样器**：`Sampler::ParentBased(Box::new(Sampler::TraceIdRatioBased(cfg.sample_rate)))`，默认 `sample_rate = 1.0`（本地自用全采样）；ParentBased 确保子 span 跟随父决策，避免半采样导致 trace 断裂
- **shutdown**：`SdkTracerProvider::shutdown()` 同步阻塞冲刷 BatchSpanProcessor，无 `shutdown_with_timeout` API（已 librarian 确认）；export timeout 改在 `SpanExporter::builder().with_tonic().with_timeout(Duration::from_secs(5))` 设置，间接限制 shutdown 阻塞时长
- **runtime**：`init_observability` 在 `#[tokio::main]` 内调用（main.rs:90），BatchSpanProcessor 可直接 spawn tonic 后台任务
- **Layer 内读 OTel SpanContext 的公开 API**：用 `tracing_opentelemetry::get_otel_context(&span_id, &dispatch) -> Option<opentelemetry::Context>`（POC 验证 + librarian 源码确认）。**不**直接读 `OtelData`（非 pub，存储类型为私有 `OtelDataLock`）；不在 Layer 回调内调 `OpenTelemetrySpanExt::context()`（需 `tracing::Span` 句柄，Layer 拿不到）
- **测试通道**：用 `opentelemetry_sdk::testing::trace::InMemorySpanExporter` 注入；新增 `init_observability_with_exporter` 测试钩子（`pub(crate)` + `cfg(test)`）
- **依赖版本绑定**：`opentelemetry 0.32` / `opentelemetry_sdk 0.32` / `opentelemetry-otlp 0.32` / `tracing-opentelemetry 0.33`（四者 minor 版本必须配套）；workspace 依赖已在 commit `10b6c36`（POC 阶段）引入

**POC 触点（已完成）**：Wave 2 启动前 3 项关键技术验证已通过 POC `crates/visp-daemon/src/bin/otel_poc.rs` 实测：
- ✅ B1：Layer 内用 `get_otel_context` 可读真实 SpanContext（替代失败的 `OtelData` 路径）
- ✅ B2：attach Context 方案 trace_id 继承（aa→aa）；对照实验中 set_parent 方案同样成功，但 attach 是 OTel 标准、无 `Result` 处理、语义更清晰，**Step 5 采用 attach 方案**
- ✅ `with_context_activation(true)` 在 0.33 存在，签名 `pub fn with_context_activation(self, bool) -> Self`，默认 true

**Librarian 触点 ②**：已完成（要点合并入上方"跨步骤架构要点"，不另出文档）。

**Oracle review 触点 ③**：W2-S3 完成后由 orchestrator 派 @oracle 做架构 review，重点检查切换逻辑是否破坏 W1 测试、跨 mpsc trace_id 一致性、`on_enter` 时机下 fmt 是否能捕获 `span.record` 写入的 field。

## Step 1：`ObservabilityConfig.otlp` 扩展（依赖已在 POC 阶段引入）

**委托**：@fixer

**前置说明**：4 项 OTel workspace 依赖已在 POC 阶段（commit `10b6c36`）引入 — `opentelemetry 0.32` / `opentelemetry_sdk 0.32`（dev: features=["testing"]） / `tracing-opentelemetry 0.33`。本 Step **仅补 `opentelemetry-otlp 0.32` workspace 依赖**与配置项。

### W2-S1-1 红：`OtlpConfig` 默认值与反序列化测试
- **文件**：`crates/visp-daemon/src/config.rs`（`#[cfg(test)] mod tests`）
- **测试**（5 个）：
  - `test_otlp_config_defaults_disabled`：默认 `enabled=false` / `endpoint="http://localhost:4317"` / `protocol="grpc"` / `timeout_secs=10` / `headers` 空 / `sample_rate=1.0`
  - `test_otlp_config_deserializes_from_toml`：完整 `[observability.otlp]` toml 段反序列化字段全部正确（含 `sample_rate=0.1`）
  - `test_otlp_config_omitted_section_is_default`：`ObservabilityConfig` 无 `otlp` section 时走 `OtlpConfig::default()`
  - `test_otlp_config_headers_kv_pairs`：`headers` 为 `BTreeMap<String, String>`，反序列化保留 key 排序
  - `test_otlp_config_sample_rate_clamped`：`sample_rate` 越界值（< 0.0 或 > 1.0）需 clamp 到 `[0.0, 1.0]`（避免 OTel SDK panic）
- 运行 `cargo test -p visp-daemon config::tests::test_otlp` 预期 5 个红

### W2-S1-2 绿：实现 `OtlpConfig` 结构体
- 新增 `OtlpConfig`：字段 `enabled` / `endpoint` / `protocol` / `timeout_secs` / `headers` / `sample_rate`，全部 `#[serde(default)]`
- `OtlpConfig::default()` 中 `sample_rate = 1.0`
- 反序列化后调用 `clamp` helper 保证 `sample_rate ∈ [0.0, 1.0]`
- `ObservabilityConfig` 增加 `pub otlp: OtlpConfig` 字段（`#[serde(default)]`）
- 运行 W2-S1-1 全部转绿

### W2-S1-3 绿：补充 `opentelemetry-otlp` 依赖
- workspace 根 `Cargo.toml [workspace.dependencies]` 加 1 项：`opentelemetry-otlp = { version = "0.32", features = ["grpc-tonic", "tls"] }`
- `crates/visp-daemon/Cargo.toml [dependencies]` 加 `opentelemetry-otlp = { workspace = true }`
- 跑 `cargo build -p visp-daemon` 验证编译通过
- （注：`opentelemetry` / `opentelemetry_sdk` / `tracing-opentelemetry` 已由 commit `10b6c36` 引入）

### W2-S1-4 验证 + 提交
- `cargo test -p visp-daemon config::`
- `cargo clippy -p visp-daemon -- -D warnings`（零新增警告口径）
- `cargo fmt -- --check`
- Commit：`feat(daemon): ObservabilityConfig.otlp 子段（含 sample_rate）+ otlp exporter 依赖`

## Step 2：OTel TracerProvider 装配 + `OpenTelemetryLayer` 接入

**委托**：@fixer

### W2-S2-1 红：装配链单元测试
- **文件**：`crates/visp-daemon/src/observability/init.rs`（`#[cfg(test)] mod tests`）
- **测试**（4 个，全部用 `InMemorySpanExporter` 注入，避免依赖真实 endpoint）：
  - `test_otlp_disabled_no_otel_layer`：`OtlpConfig.enabled=false` → `ObservabilityGuard.tracer_provider` 为 `None`
  - `test_otlp_enabled_attaches_otel_layer`：`enabled=true` + 注入 `InMemorySpanExporter` → `tracer_provider` 为 `Some(_)`，emit 一个 span 后 exporter 收到 1 条 SpanData
  - `test_otlp_resource_has_service_name`：导出的 SpanData Resource 含 `service.name="visp-daemon"` + `service.version=env!("CARGO_PKG_VERSION")`
  - `test_otlp_w1_layers_still_attached_when_enabled`：OTLP 启用时 ParentLinkLayer + MetricsLayer + fmt 仍在装配链（emit span 后 `visp.span.w3c_id` 字段仍写入；metrics summary 仍生成）

### W2-S2-2 红：测试钩子
- 新增 `pub(crate) fn init_observability_with_exporter(cfg: &ObservabilityConfig, exporter: InMemorySpanExporter) -> ObservabilityGuard`（`#[cfg(test)]` 门控）
- 测试 `test_init_with_exporter_function_exists`：编译通过即过（保护后续测试入口稳定）

### W2-S2-3 绿：实现 OTel 装配
- 新建 `crates/visp-daemon/src/observability/otlp.rs` 模块
  - `pub(crate) fn build_tracer_provider(cfg: &OtlpConfig) -> SdkTracerProvider`：生产路径，`SpanExporter::builder().with_tonic().with_endpoint(cfg.endpoint).with_timeout(Duration::from_secs(5)).with_metadata(headers).build()` → `BatchSpanProcessor::builder(exporter).build()` → `SdkTracerProvider::builder().with_span_processor(...).with_sampler(ParentBased(TraceIdRatioBased(cfg.sample_rate))).with_resource(...).build()`
  - `pub(crate) fn build_tracer_provider_with_exporter<E: SpanExporter + 'static>(exporter: E, cfg: &OtlpConfig) -> SdkTracerProvider`：测试路径，复用 BatchProcessor + Sampler + Resource 构造逻辑
  - 共用 helper `build_resource()`：`Resource::builder().with_service_name("visp-daemon").with_attribute(KeyValue::new("service.version", env!("CARGO_PKG_VERSION"))).with_attribute(KeyValue::new("host.name", hostname)).with_attribute(KeyValue::new("process.pid", std::process::id() as i64)).build()`
- 修改 `init.rs::init_observability_with_writer`：根据 `cfg.otlp.enabled` 条件性构造 tracer_provider；启用时追加 `tracing_opentelemetry::layer().with_tracer(provider.tracer("visp-daemon")).with_context_activation(true)`
- 装配链顺序：`registry → EnvFilter → OpenTelemetryLayer?(with_context_activation=true) → ParentLinkLayer → MetricsLayer → fmt`
- `ObservabilityGuard` 新增字段 `tracer_provider: Option<SdkTracerProvider>`（**字段声明顺序**：放在 `_file_guard` 之前，确保 Drop 时 tracer_provider 先 shutdown，再释放文件 writer）
- `Drop for ObservabilityGuard` 实现：先 `self.tracer_provider.take().map(|p| p.shutdown())`，再 drop 其他字段；shutdown 阻塞时长由 exporter `with_timeout(5s)` 间接限制

### W2-S2-4 验证 + 提交
- `cargo test -p visp-daemon observability::init::tests`
- `cargo test -p visp-daemon`（全量回归确认 W1 测试不掉）
- `cargo clippy -p visp-daemon -- -D warnings` / `cargo fmt -- --check`
- Commit：`feat(daemon): OTel TracerProvider 装配 + OpenTelemetryLayer 接入`

## Step 3：ParentLinkLayer 双模式升级（D1 核心，Y 方案）

**委托**：@fixer 实现 → **Oracle review 触点 ③**（实现完成后必经）

**关键变化**：Wave 2 的 ParentLinkLayer 不再单一从 uuid 生成器写 `visp.span.w3c_id`。OTLP 启用时切换到 OTel 权威源 — 在 **`on_enter` 回调**（不是 `on_new_span`，后者时 OTel SpanContext 尚未固化）用公开 API `tracing_opentelemetry::get_otel_context(&id, &dispatch)` 读真实 trace_id/span_id 转 hex 后写入，确保 fmt JSON 输出与 OTel 后端记录的 ID 一致（设计 §7.3 Wave 2 段）。

**API 选择说明**（POC + librarian 双重确认）：
- ❌ **不用** `OtelData`：非 pub 类型，存储类型实际是私有 `OtelDataLock`，0.33 起明确标注 "not part of public API"
- ❌ **不用** `OpenTelemetrySpanExt::context()`：是 `tracing::Span` 句柄上的 extension trait，Layer 回调只有 `&Id` + `&Context<S>`，拿不到 `Span` 句柄
- ✅ **用** `tracing_opentelemetry::get_otel_context(&span_id, &dispatch) -> Option<opentelemetry::Context>`：公开 pub 函数，签名匹配 Layer 回调，可在 `on_enter` 内直接调

**field 预声明做法 A**：root span 用 `info_span!("agent.run", visp.span.w3c_id = tracing::field::Empty, ...)` 预声明该 field，在 `on_enter` 用 `span.record("visp.span.w3c_id", value)` 写入；fmt layer 在 span 关闭时序列化 field，能捕获 record 写入值。

### W2-S3-1 红：双模式单元测试
- **文件**：`crates/visp-daemon/src/observability/parent_link.rs`（扩展现有 `#[cfg(test)] mod tests`）
- **测试**（6 个）：
  - `test_parent_link_uuid_mode_when_otel_disabled`：`ParentLinkLayer::new()` 走 W1 uuid 路径；复用现有 W1 断言（`visp.span.w3c_id` 为 16-hex 且每个 span 唯一）
  - `test_parent_link_otel_mode_when_otel_enabled`：`ParentLinkLayer::with_otel_mode(true)` 构造后，挂接到含 `OpenTelemetryLayer` 的 registry 上，emit span 后 `visp.span.w3c_id` 等于 OTel `SpanContext.span_id()` 的 hex
  - `test_otel_mode_reads_real_trace_id`：同上变体，断言写入值与 OTel SDK 生成的 span_id（通过 InMemoryExporter 导出 SpanData 读取）逐字节相等
  - `test_otel_mode_same_trace_id_within_run`：单个 agent run 内多个 nested span 共享 OTel trace_id（验证 OTel 父子链已经接通）
  - `test_parent_link_otel_mode_skips_uuid_generation`：OTel 模式下不调 W1 的 uuid 生成函数（用 `AtomicUsize` 计数器或 mock counter 包装 `generate_w3c_span_id` 验证）
  - `test_otel_mode_field_appears_in_fmt_output`：OTel 模式下，fmt JSON 输出中 `visp.span.w3c_id` field 非空且等于 OTel span_id hex（验证 `on_enter` + `record` 顺序与 fmt 兼容）

### W2-S3-2 绿：升级 `ParentLinkLayer`
- 新增字段 `otel_mode: bool`
- 新增构造器 `pub fn with_otel_mode(enable: bool) -> Self`，保留 `new()` 默认 `otel_mode=false`
- **回调时机调整**：将 OTel 模式下的 ID 读取从 `on_new_span` 移到 `on_enter(&self, id: &Id, ctx: Context<S>)`
- `on_enter` 分支：
  - **OTel 模式**：用 `dispatcher::get_default(|dispatch| tracing_opentelemetry::get_otel_context(id, dispatch))` 取 `OtelContext`，从其 `span()` 取 `SpanContext`，若 `is_valid()` → `span_id` 转 16-hex lowercase → `ctx.span(id).map(|s| s.record_field("visp.span.w3c_id", &hex_str))`（用 `Span` ref + record API）；若 invalid（罕见，OTel layer 未挂或采样未命中）→ fallback 到 W1 uuid 路径
  - **非 OTel 模式**：保持 W1 uuid 生成原逻辑不变（W1 路径仍在 `on_new_span` 内，回调时机不动）
- 跨模式共用 unmatched parent 统计逻辑
- root span 处（visp-core agent_loop.rs 的 3 个 `info_span!`）补加 `visp.span.w3c_id = tracing::field::Empty` 预声明 field（若 W1 已声明则跳过）

### W2-S3-3 绿：装配端选择构造器
- `init.rs::init_observability_with_writer` 根据 `cfg.otlp.enabled` 选择 `ParentLinkLayer::new()` 或 `ParentLinkLayer::with_otel_mode(true)`

### W2-S3-4 Oracle review 触点 ③
- @fixer 实现完成、本步 6 个测试转绿后，orchestrator 派 @oracle 做架构 review
- review 重点：
  - 切换逻辑是否破坏任何 W1 测试（uuid 路径完全不动？回调时机 W1 仍在 `on_new_span`？）
  - 跨 mpsc 场景下 trace_id 一致性（orchestrator → sub-agent → 回 daemon 的链路）
  - `get_otel_context` 在 `on_enter` 内调用是否有 deadlock 风险（librarian 标注：仅在不持有 `ExtensionsMut` 时安全，`on_enter` 的 `Context` 不暴露 mutable extensions，应该 OK，但需 review 确认）
  - `span.record` 在 `on_enter` 内调用的时机是否会被 fmt layer 在同 span 周期内捕获（做法 A field 预声明顺序问题）
- review 建议若涉及代码修改，由 @fixer 续做并补测试，**不直接提交**直至 review 通过

### W2-S3-5 验证 + 提交
- `cargo test -p visp-daemon`（含 W1 全部 + 新增 6 个）
- `cargo clippy -p visp-daemon -- -D warnings` / `cargo fmt -- --check`
- Commit：`feat(daemon): ParentLinkLayer 双模式（OTLP 启用时用 OTel ID 源，on_enter 时机）`


## Step 4：orchestrator OTel context 注入（D2 — 父端）

**委托**：@fixer

**目标**：orchestrator 在 spawn 子 agent 时，从当前 OTel context 提取 `SpanContext`，填入 `SpawnRequest.trace_context`。OTel 未启用时保持 W1 行为（trace_id/span_id 由 uuid 生成）。**TraceContext schema 不变**。

### W2-S4-1 红：父端注入测试
- **文件**：`crates/visp-agent/src/orchestrator.rs`（或对应 spawn 触发点测试模块）
- **测试**（3 个）：
  - `test_spawn_trace_context_carries_otel_ids_when_otel_active`：测试内套 `OpenTelemetryLayer`（注入 InMemoryExporter）+ 真实 tracing span；触发 spawn，验证 `SpawnRequest.trace_context.trace_id` / `span_id` 等于 OTel SDK 当前 span 的 ID（hex）
  - `test_spawn_trace_context_carries_uuid_when_otel_inactive`：未挂 `OpenTelemetryLayer`，沿用 W1 uuid 行为（断言为 16/32-hex 且非全 0）
  - `test_spawn_trace_context_uses_current_otel_span_not_root`：在嵌套 span 内触发 spawn，断言 trace_context.span_id 来自最内层 span（验证 `OpenTelemetrySpanExt::context()` 取的是 current）

### W2-S4-2 绿：实现父端注入
- spawn 触发点新增 helper：从 `tracing::Span::current()` 调用 `OpenTelemetrySpanExt::context()` 提取 `OtelContext`，从中读 `SpanContext`
- 若 `SpanContext.is_valid() == true` → 用 OTel ID 填 `TraceContext`；否则走 W1 uuid 路径（fallback）
- 不引入新依赖；该 helper 位于 visp-daemon 或 visp-agent（取实际 spawn 触发 crate）

### W2-S4-3 验证 + 提交
- `cargo test -p visp-agent`（含 W1 全部 + 新增 3 个）
- `cargo clippy -p visp-agent -- -D warnings` / `cargo fmt -- --check`
- Commit：`feat(agent): orchestrator spawn 时注入 OTel SpanContext 到 TraceContext`

## Step 5：子 agent OTel parent 重建（D2 — 子端，attach Context 方案）

**委托**：@fixer

**目标**：子 agent 接收 `SpawnRequest` 后，用 `TraceContext` 重建 `opentelemetry::trace::SpanContext`，**先 attach 到当前 Context，再创建子 agent root span**，让 OpenTelemetryLayer 在 `on_new_span` 时自动继承 trace_id（设计上不再使用 `set_parent`，避免"span 已分配 trace_id 再尝试覆盖"的内部状态歧义）。

**方案选择**（POC 实测对比）：
- ✅ **attach Context 方案**：`Context::current().with_remote_span_context(remote_sc).attach()` → 创建 span。POC 验证 trace_id 继承成功（aa→aa）；OTel 标准做法，无 Result 处理，语义清晰
- ⚠️ **set_parent 方案**：POC 实测也能成功覆盖 trace_id，但 oracle 仍对"先建后改"的语义有顾虑；保留为 fallback 注释，主路径不使用

### W2-S5-1 红：子端 attach 测试
- **文件**：子 agent 入口测试（与 W1 子 agent 测试同位）
- **测试**（2 个，用 InMemoryExporter 验证）：
  - `test_subagent_root_span_inherits_trace_id_via_attach`：父 + 子两次 agent run；导出的子 agent root span `trace_id` 等于父端注入的 `TraceContext.trace_id`，`parent_span_id` 等于父端 `span_id`
  - `test_subagent_falls_back_to_new_root_when_trace_context_invalid`：`TraceContext` 字段为空字符串或非 hex 时，子 agent root span 为独立新 trace（不报错，回退到 W1 行为，不调 attach）

### W2-S5-2 绿：实现子端 attach + 重建
- 子 agent 入口（接收 SpawnRequest 后，**创建 root span 之前**）：
  1. 解析 `TraceContext.trace_id` / `span_id` → `opentelemetry::trace::TraceId::from_hex` / `SpanId::from_hex`
  2. 解析失败 → log warn 并跳过 attach，直接创建 span（W1 行为）
  3. 解析成功 → 构造 `SpanContext::new(trace_id, span_id, TraceFlags::SAMPLED, /* is_remote */ true, TraceState::default())`
  4. `let parent_ctx = opentelemetry::Context::current().with_remote_span_context(span_ctx);`
  5. `let _attach_guard = parent_ctx.attach();`（**guard 必须覆盖整个 root span 生命周期**：放在 agent run 顶层 fn 内 `let span = info_span!(...)` 之前，guard 与 span 同 scope 持有）
  6. 后续 `let span = info_span!("agent.run", ...); let _enter = span.enter();` 创建的 span 由 OpenTelemetryLayer 在 `on_new_span` 时读 `OtelContext::current()` 自动继承 trace_id（依赖装配链 `.with_context_activation(true)`，见 Step 2）
- **不调** `OpenTelemetrySpanExt::set_parent`（避免 oracle 担忧的"span 已建再改"语义）

### W2-S5-3 验证 + 提交
- `cargo test -p visp-agent`
- `cargo clippy -p visp-agent -- -D warnings` / `cargo fmt -- --check`
- Commit：`feat(agent): 子 agent 入口 attach OTel Context 重建跨 mpsc trace`


## Step 6：端到端 OTLP 链路验证（in-memory）

**委托**：@fixer

### W2-S6-1 红：e2e 集成测试
- **文件**：`crates/visp-daemon/tests/observability_otlp_e2e.rs`（新建）
- **测试**（3 个，标 `#[serial_test::serial]`）：
  - `test_otlp_e2e_single_agent_emits_spans`：启动 daemon（OTLP enabled + 注入 InMemoryExporter），跑一次单 agent run，断言导出 span 数 ≥ W1 默认 4 种 span 总数，trace_id 单一
  - `test_otlp_e2e_orchestrator_to_subagent_single_trace`：跨 mpsc 触发 sub-agent；断言父+子所有 span 在同一 trace_id，子 agent root span 的 parent_span_id 等于父端最内层 span_id
  - `test_otlp_e2e_export_failure_graceful`：启用 OTLP 但 endpoint 配置为不可达地址（`http://127.0.0.1:1`），主流程仍能完成一次 agent run，不 panic，日志含 export 失败 warning（验证 D3 — collector 不可达时不影响主路径）

### W2-S6-2 绿：补齐测试钩子
- 若 W2-S2-2 的 `init_observability_with_exporter` 不够用（缺 metrics writer 注入），按需扩展
- 确保 e2e 测试不依赖真实 endpoint（全部走 InMemoryExporter）

### W2-S6-3 验证 + 提交
- `cargo test -p visp-daemon --test observability_otlp_e2e`
- `cargo test --workspace`（全量回归，含 W1）
- `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt -- --check`
- Commit：`test(daemon): OTLP 端到端 in-memory 验证（含跨 mpsc 单 trace_id）`

## Wave 2 总验收清单

- [ ] `cargo build -p visp-daemon` 默认编译通过（OTel 依赖始终编入，无 cargo feature gate）
- [ ] `cargo test --workspace` 全绿（W1 全部 + Wave 2 新增 ~16 用例：Step1×5 + Step2×3 + Step3×6 + Step4×3 + Step5×2 + Step6×3 ≈ 22，含部分既有改造）
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` 零新增警告
- [ ] `cargo fmt -- --check` 通过
- [ ] 默认配置（`[observability.otlp] enabled = false`）下：OTel layer 不挂载，W1 行为完全不变；`ObservabilityGuard.tracer_provider` 为 `None`
- [ ] 启用 OTLP（`enabled = true` + 本地 Jaeger / Tempo）：UI 可见单 trace 跨 orchestrator + sub-agent，子 agent root span `parent_span_id` 非 0 且等于父端最内层 span
- [ ] OTLP 启用时 fmt JSON 输出的 `visp.span.w3c_id` 与 OTel 后端记录的 span_id 严格一致（D1 验证：on_enter + get_otel_context）
- [ ] `SpawnRequest.trace_context` schema 不变（D2 验证）；子端通过 **attach Context** 重建（非 set_parent）
- [ ] 装配链含 `OpenTelemetryLayer::new(tracer).with_context_activation(true)`（默认即为 true，显式调用以防上游变更）
- [ ] 采样器为 `ParentBased(TraceIdRatioBased(sample_rate))`，配置项 `sample_rate` 默认 1.0 且 clamp 到 `[0.0, 1.0]`
- [ ] daemon 关闭时 `SdkTracerProvider::shutdown` 被调用；exporter `with_timeout(5s)` 间接限制 flush 时长，无 BatchSpanProcessor 泄漏 warning
- [ ] 设置 `enabled = true` 但 collector 不可达：daemon 启动成功，仅后台 export 失败 warning，主流程不受影响（D3 验证）
- [ ] `ObservabilityGuard` 字段顺序为 `tracer_provider` 在 `_file_guard` **之前**，确保 drop 顺序：先 flush spans → 再关日志文件

---

# 全局收尾

## 风险检查表

- [ ] visp-core 未引入 IO 依赖（`cargo tree -p visp-core` 验证无 tokio runtime feature / 无 reqwest / 无文件系统 crate）
- [ ] mpsc `AgentEvent` 通道行为零变更（CLI UI 测试通过）
- [ ] message 表 schema 零变更（仅复用 V2 已有 `tool_calls_json` + `provider_metadata` 列）
- [ ] 性能基线：Wave 1 默认配置下，daemon CPU 占用增量 < 5%（手工 perf 验证）
- [ ] 日志文件不含敏感信息（API key / 用户消息原文）— 设计 §9.3 明示策略

## 依赖升级注意

- `tracing` 系列保持同一 minor 版本（subscriber / appender / opentelemetry 互相版本绑定严格）
- `opentelemetry` ecosystem 至少 0.27+（设计 §A.1 已确认版本）
- 升级时先在 feature gate 内验证再放开默认

## 跨 Wave 通用回滚策略

- Wave 1 失败：关闭 `observability.enabled=false` 即可，无 schema 变更
- Wave 2 失败：将 `[observability.otlp] enabled` 改回 `false` 即可，daemon 自动回到 Wave 1 行为；如需移除依赖再走单独的 revert PR
- 任何 wave 内步骤回滚：commit 粒度按 step 切分，单 step 可独立 revert
