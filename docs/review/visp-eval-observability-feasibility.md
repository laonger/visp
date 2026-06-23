# eval.md 方案对 visp 可行性评估

Version: v1.0

Status: 评审稿

被评估对象: `docs/eval.md` —《Multi-Agent Vibe Agent Observability Platform》

评估范围: visp 代码库当前架构与该方案的匹配度、落地可行性、风险

---

# 1. 评估结论（TL;DR）

**方向可行，visp 基础良好，但方案存在定位模糊与过度设计风险，不应照搬，建议分阶段落地。**

- ✅ **可行性**：visp 已有的持久化与多 agent 架构，为 Tracing / Replay 提供了 70% 的数据基础。
- ⚠️ **照搬风险**：方案的 Event Bus / OpenTelemetry / "独立通用平台" 定位对 visp 偏重，直接全量落地属于 over-engineering。
- 💎 **最高 ROI**：补齐已存在但未填充的字段（latency / metadata）+ 轻量 Metrics 聚合，投入小、收益直接。
- ❓ **最大不确定性**：Evaluation 模块（尤其 Feature Completion 自动化），建议剥离单独评估。

---

# 2. 评估方法

本评估基于对 visp 代码库的只读结构化探索，覆盖：

- Agent 循环实现（`visp-core/src/agent_loop.rs`）
- 多 Agent 编排（`visp-agent/src/orchestrator.rs`、`AgentDefinition`、`Envelope`/`AgentMessage`）
- 持久化模型（`visp-db/src/schema.rs`、`store.rs`、`message` 表 V5）
- Tool / MCP 调用链（`Tool` trait、`McpToolAdapter`）
- Session 管理（`Session`、`SqliteSessionStore`）
- 现有观测能力（`tracing` 使用、daemon 日志、`AgentEvent` 枚举）

评估维度：数据完整性、架构匹配度、概念一致性、成本效益、风险。

---

# 3. visp 现状基线

## 3.1 已具备的有利基础

| 方案要求 | visp 现状 | 评估 |
|---|---|---|
| 完整消息链 | `message` 表记录 user/assistant/tool_call/tool_result 全链，按 id ASC 可还原顺序 | ✅ 已有 |
| Tool / MCP 调用关系 | 统一走 `Tool` trait；MCP 通过 `McpToolAdapter` 适配；`tool_calls_json` 存完整调用 | ✅ 已有 |
| Token / Cost | per-message 的 `actual_tokens_input/output`、`actual_cache_read/write`、`actual_cost` | ✅ 已有 |
| Session 可恢复 | `SqliteSessionStore::get` 从 DB 重建完整 `history: Vec<Message>` | ✅ 天然支持回放 |
| 多 Agent 架构 | `Orchestrator` + `Envelope`/`AgentMessage` + sub-agent spawn 机制（parent_id 关联） | ✅ 匹配"Multi-Agent"定位 |
| 事件枚举 | `AgentEvent`（CLI 通道）+ `AgentMessage`（Orchestrator 通道），8 种 MessageType | ⚠️ 有枚举，无统一总线 |

**小结**：方案五大模块里，Tracing 和 Replay 的数据基础已基本就绪，不是从零起步。

## 3.2 已发现的数据层缺陷（bug 级）

两个字段在 schema 中已定义，但运行时从未填充，本应是观测数据的低成本来源：

| 字段 | 位置 | 现状 |
|---|---|---|
| `tool_result_duration_ms` | `message` 表 V4 | `Message::tool()` 构造时不设置，恒为空 |
| `provider_metadata` | `message` 表 | agent_loop 中始终填 `None`，LLM 原始元数据丢失 |

此外，LLM 调用的**端到端 latency** 当前完全不记录（只记录 tool 执行耗时）。

---

# 4. 五大模块缺口分析

对照 eval.md 第 5 节的五个核心模块：

| 模块 | 现状 | 关键缺口 | 严重度 | 工作量 |
|---|---|---|---|---|
| **Tracing** | 有 `AgentEvent`/`AgentMessage` 枚举与 mpsc 通道 | 无统一 Event Bus（只有 point-to-point channel）；**sub-agent 执行过程不可见**（只回传最终结果，无执行轨迹） | 高 | 中 |
| **Replay** | message 表数据基本完整 | 缺 LLM 端到端 latency；缺 sub-agent 过程 | 中 | 小 |
| **Decision Trace** | 仅存 action；Thinking block 在 `extra_blocks` 但无因果链 | **完全缺失**——无"为什么做此 tool call"的结构化决策记录 | 高 | 中大 |
| **Evaluation** | 无任何评估能力 | **完全缺失**——无 feature completion / regression / build / test 结果记录 | 高 | 大（最难自动化） |
| **Metrics** | 有 per-message token/cost 原始数据 | **无任何聚合查询**——success_rate / total_cost / average_latency 全为 0 代码 | 中 | 中 |

---

# 5. 定位与设计风险（需先澄清）

在判断"方案是否该照做"前，以下问题必须先明确，否则后续设计会跑偏。

## 5.1 内建自观测 vs 通用观测平台

eval.md 通篇将平台定位为"独立基础设施，为 Agent Runtime 提供观测能力"，读起来像是要做一个**服务外部 agent 的通用产品**。但 visp 本身就是一个 coding agent，两种理解差异巨大：

- **内建自观测**：在 visp 内部增强可观测性。成本可控，价值直接，与现有 SQLite/mpsc 架构契合。
- **通用平台**：抽象出与 visp 解耦的 event 协议、独立部署、多 runtime 接入。这是另一个量级的项目，偏离 visp 当前定位。

**评估判断**：若为前者，可行性高；若为后者，成本效益不合理，不建议做。本报告后续假设定位为"内建自观测"。

## 5.2 OpenTelemetry 是否必要

方案架构图明确画了 OpenTelemetry 层。OTel 的价值在于**标准化导出 + 多后端对接**（Jaeger/Prometheus 等）。但：

- visp 已有 SQLite 持久化 + mpsc 事件流
- visp 是单机单用户工具，非分布式系统
- 无现有"导出到外部 dashboard"的需求证据

**评估判断**：引入 OTel 属 over-engineering。建议先用现有 `tracing` + 结构化 SQLite，待出现"对接外部观测后端"的真实需求再加 OTel 导出层。

## 5.3 概念错配

方案中若干追踪对象与 visp 实际模型对不上，照搬事件枚举会造出一堆 visp 里根本不会触发的事件：

| 方案事件 | visp 实际 |
|---|---|
| `SkillExecuted` | visp **无 Skill 概念**，只有 Tool |
| `AgentHandoff` | visp 多 agent 是 **Orchestrator→Subagent spawn** 模式，非 handoff |
| `Coordinator→Coding→Review→Test` 拓扑 | visp 当前是 `Primary agent + 可选 task 工具触发 sub-agent`，**无固定 Review/Test agent 角色** |

**评估判断**：事件枚举需按 visp 实际模型重新设计，不能照抄。

## 5.4 Decision Trace 的本质难题

方案 Principle 2 要求记录"为什么"。但 LLM 决策本质是黑盒，能记录的只是**上下文 + thinking + 最终选择**，并非真正因果推理。

- visp 已存 Thinking block，这已是最接近"decision"的原始材料。
- 要做成真正的 Decision Trace，需在 agent loop 中**主动注入结构化决策点**（本轮候选 tool、为何选此、预期效果），这需要改 prompt + loop，会侵入 agent 行为，工作量中等。

**评估判断**：可行但需谨慎，建议作为后期独立模块评估，而非随 Tracing 一起做。

## 5.5 Evaluation 是最虚的部分

"Feature Completion / Regression" 自动化评估非常难：

- 需对照需求语义判断，无客观基准
- 需要历史基线，visp 当前无任何评估数据积累
- 自动化评分本身可能引入误导

**评估判断**：低可行性 / 高不确定性。建议降级为"LLM 自评 + 测试/构建结果记录"，**不追求自动化的 feature completion 评分**。该模块应剥离单独评估。

---

# 6. 总体可行性结论

| 维度 | 结论 |
|---|---|
| 方向可行性 | ✅ 可行，visp 基础比方案假设更好 |
| 照搬风险 | ⚠️ Event Bus / OTel / 通用平台定位对 visp 偏重，全量落地属过度设计 |
| 最高价值点 | Wave 1 字段补齐 + Metrics 聚合，ROI 最高 |
| 最大不确定性 | Evaluation 模块，建议剥离 |

---

# 7. 建议落地路径（分阶段）

假设定位为"内建自观测"，建议分三波，**先吃掉已有字段的低成本收益，再谈架构层改造**。

## Wave 1：低成本、立即收益（不引入新架构）

目标：补齐已存在但未用的数据通道，提供基础指标可见性。

- 修复 `tool_result_duration_ms` 填充（`Message::tool()` 接入实际耗时）
- 修复 `provider_metadata` 填充（LLM 响应元数据落库）
- 新增 LLM 调用端到端 latency 记录（per assistant message）
- 轻量 Metrics 聚合查询：per-session 的 total_cost / total_tokens / avg_latency / tool_call_count
- CLI 展示 session 级指标（复用现有 `/list` 或新增 `/stats`）

**验证标准**：一次 agent 执行后，DB 中每条 tool_result 有 duration、每条 assistant 有 latency；CLI 能展示 session 汇总指标。

## Wave 2：中成本（引入轻量 Event 层，不引入 OTel）

目标：让多 agent 执行过程可观测、可回放。

- 在 `AgentEvent` 基础上抽象一个**轻量 Event 层**（visp 内部，非 OTel）
- sub-agent 执行过程可见化（spawn/start/iterate/complete 事件落库，parent_id 关联）
- Replay：基于已有 message 表 + Event 层，在 CLI 提供回放视图（时间线 + 事件详情）

**验证标准**：一个触发 sub-agent 的任务，能在 CLI 中看到完整子 agent 执行轨迹；Replay 能还原执行顺序与每步耗时。

## Wave 3：高成本、需重新评估（建议单独出设计文档）

目标：决策可解释、结果可评估。

- Decision Trace：在 agent loop 注入结构化决策点（候选 tool、选择理由、预期效果）——**需评估对 agent 行为的侵入性**
- Evaluation：记录测试/构建结果 + LLM 自评，**不做自动 feature completion 评分**

**验证标准**：每个 tool_call 关联一条决策记录；测试/构建结果落库可查。

---

# 8. 待用户决策的开放问题

1. **定位确认**：确认"内建自观测"而非"通用观测平台"？（决定是否引入 OTel / 解耦 event 协议）
2. **Wave 1 是否立即推进**？若推进，是否走标准流程（轻量设计文档 → 工作计划 → 执行）？
3. **Decision Trace / Evaluation 是否纳入近期规划**，还是先搁置？
4. **sub-agent 过程可见化**是否优先级高于 Metrics 聚合？（取决于当前用多 agent 的频率）
