# LLM 调用 400 错误修复设计

> 日期：2026-08-03
> 状态：待审核
> 范围：visp-llm（主）、visp-core/agent_loop（配合）

## 1. 问题概述

visp 调用火山引擎 GLM-5.2 时存在两类 400 风险：

### 问题 A：openai 协议截断 tool_call 污染会话历史（已触发，间歇性）

- **现象**：长会话中 glm-5.2 输出达 `max_tokens=8000` 被截断（`finish_reason='length'`），截断发生在 tool_call arguments 的 JSON 字符串中间。畸形 tool_call 被写入会话历史，下次请求把含畸形 arguments 的 assistant 消息发回火山引擎 Ark，服务端解析 JSON 失败，返回 `400 InvalidParameter: Invalid request body`。
- **频率**：日志实证 7/2632 ≈ 0.27%。但在**被污染的会话内**，历史持续携带畸形 tool_call，该会话后续每次请求都 400、无法自愈--体感为"无论怎么操作都持续报错"。开新会话恢复正常。
- **触发条件**：glm-5.2 + openai 协议（`zijie Agent Plan`，`/api/plan/v3`）+ 长输出 tool call。

### 问题 B：anthropic 协议 thinking 配置违规（定时炸弹，未触发）

- **现象**：`daemon.toml` 多条模型 `thinking_budget_tokens=12800 > max_tokens=8000`，且 thinking 模式下 `temperature=0.7`。
- **规范冲突**：Anthropic 规范要求 ① `thinking.budget_tokens` 必须小于 `max_tokens`；② thinking 启用时 `temperature` 必须为 1.0。
- **状态**：当前 anthropic 路径（`zijie Coding Plan`，`/api/coding`）有 152 次请求零 400--说明 thinking 未实际启用或端点宽松。但配置一旦生效即必然 400，是隐患。

## 2. 根因

### A 根因（污染链路，7 步）

1. `visp-llm/openai.rs` SSE Finish 事件处理（约 559-583 行）：`finish_reason='length'` 且 `tool_acc` 非空时，仅打 warn 日志，**无条件**把截断的 tool_call drain 到 `pending_tool_calls`，无丢弃/校验/降级。
2. 同文件约 621-638 行：发射 `ChatEvent::ToolCall`，畸形 arguments 原样发出，无 JSON 校验。
3. `visp-core/agent_loop.rs` 约 448 行：`collect_stream_events` 收集为 `ToolCallRequest`，无校验。
4. **污染点** `agent_loop.rs` 约 793-794 行：assistant 消息组装后 `ctx.history.push` + `sm.append_message` 写入历史，无校验。
5. `agent_loop.rs` 约 1039-1056 行：工具执行阶段才 parse arguments，失败仅返回 `[TRUNCATED]` error，**不清理已入历史的畸形消息**。
6. 下一轮 `setup_iteration` 用含畸形 tool_call 的历史再次构建请求。
7. `openai.rs` 约 174-188 行 `build_openai_messages`：畸形 arguments 原样序列化为 JSON 字符串发往服务端 -> 400。

### B 根因

- `visp-llm/anthropic.rs` `build_anthropic_request` 约 47 行：`temperature` 无条件写入请求体，thinking 启用时未强制为 1.0。
- 同函数约 73-86 行：读取 `extra["thinking_budget_tokens"]` 后直接透传为 `budget_tokens`，无 `budget < max_tokens` 校验/钳制。
- 全项目无任何 temperature/budget 校验逻辑（已确认）。
- `build_anthropic_request` 拥有最终生效的 `config.max_tokens` 与 `config.temperature`，是做最终校验的最可靠位置。

## 3. 修复架构

采用**分层防御**，每层职责单一、改动最小化（遵循简单优先）。

### 3.1 问题 A：双层修复

#### 第一层：源头拦截（visp-llm/openai.rs，SSE 流解析）

- **职责**：在 Finish 事件处理处，识别 `finish_reason='length'` 且存在未完成 tool_call 的情况，**丢弃被截断的 tool_call，不发射 `ChatEvent::ToolCall`**。
- **策略**：
  - 保留本轮已生成的文本（TextDelta 已正常累积），作为 assistant content。
  - 截断的 tool_call 不进入 `pending_tool_calls`，即不向上层发射。
  - 若截断后文本为空，插入一条简短提示文本作为 assistant content，提示模型上轮因长度限制被截断、需重新发起工具调用，保证 assistant 消息非空且语义可追踪。
- **效果**：从源头不产生畸形 tool_call，历史不被污染。模型下一轮可重新生成完整 tool_call。
- **权衡**：丢失本次模型意图，但优于 400 崩溃；下一轮模型基于上下文重新决策。

#### 第二层：防御兜底（visp-llm/openai.rs，build_openai_messages）

- **职责**：序列化 tool_calls 前，逐个校验 `arguments` 是否为合法 JSON。
- **策略**：畸形的 arguments 替换为 `{}`（空对象），**不丢弃整个 tool_call**。
- **理由**：丢弃整个 tool_call 会导致其后继的 `tool` 消息（tool_result）成为孤儿，破坏 messages 序列结构，仍可能 400。替换为 `{}` 保持结构完整，服务端可解析。
- **效果**：即使历史中已存在畸形数据（存量污染），也不会发出无法解析的请求。此层为兜底，正常运行时不触发。

### 3.2 问题 B：构建时校验（visp-llm/anthropic.rs，build_anthropic_request）

- **职责**：thinking 启用时，强制合规。
- **策略**：
  - thinking 启用时，`temperature` 强制为 1.0（覆盖 config 值）；thinking 未启用时保持原值。
  - `thinking_budget_tokens` 钳制：若 `budget >= max_tokens`，降为 `max_tokens - 1`；若 `max_tokens` 为 0（极端），跳过 thinking 启用并 warn。
- **位置**：`build_anthropic_request` 内，因为它持有最终生效的 config。
- **效果**：无论配置如何，发往 Anthropic 端点的请求体始终合规。

### 3.3 agent_loop 配合（visp-core/agent_loop.rs）

- 源头拦截后，截断轮的 tool_calls 为空，`handle_stream_result` 走已有的文本分支（约 531 行 `if tool_calls.is_empty()`）。
- 需验证：文本分支能正确处理"截断后保留的文本 + 提示文本"，正常写入历史并继续循环。
- 预期改动极小或零改动（复用现有文本路径），具体在工作计划阶段确认。

## 4. 模块影响范围

| 模块 | 改动 | 说明 |
|---|---|---|
| visp-llm/openai.rs | 改 | SSE Finish 事件截断处理 + build_openai_messages 校验 |
| visp-llm/anthropic.rs | 改 | build_anthropic_request thinking/temperature 校验 |
| visp-core/agent_loop.rs | 可能改 | 验证文本分支兼容截断场景，必要时微调 |
| visp-core/provider.rs | 不改 | ChatEvent / LlmProvider trait 不变 |
| visp-daemon config.rs/service.rs | 不改 | 配置数据流不变 |
| daemon.toml | 不改 | 代码层钳制即可，不改用户配置 |

## 5. 边界情况

- **截断发生在多个 tool_call 中的某一个**：仅丢弃被截断的，保留完整的（按 index 区分）。
- **截断发生在纯文本（非 tool_call）**：当前已正常处理（文本截断不导致 400），不受影响。
- **arguments 为空字符串**：合法（空参数），不替换。
- **arguments 非法但非截断**（如模型输出错误 JSON）：防御层同样替换为 `{}`。
- **thinking_budget_tokens 未配置**：不启用 thinking，temperature 保持原值。
- **thinking_budget_tokens = 0**：跳过 thinking 启用。
- **thinking_budget_tokens < max_tokens**：正常启用，不钳制。
- **finish_reason='length' 但 tool_call 完整**（边界）：保留 tool_call，正常发射。

## 6. 测试策略（TDD）

所有改动以单元测试先行，覆盖正常与边界情况。

### openai 截断修复测试

- finish_reason='length' + 截断 tool_call -> 不发射 ToolCall，保留文本
- finish_reason='length' + 截断 tool_call + 无文本 -> 发射提示文本，不发射 ToolCall
- finish_reason='length' + 完整 tool_call（边界）-> 正常发射 ToolCall
- finish_reason='stop' + tool_call -> 正常发射
- build_openai_messages：合法 arguments -> 原样序列化
- build_openai_messages：畸形 arguments -> 替换为 `{}`
- build_openai_messages：混合合法/畸形 -> 只替换畸形的

### anthropic thinking 修复测试

- thinking 启用 + temperature=0.7 -> 请求体 temperature=1.0
- thinking 启用 + budget > max_tokens -> budget 钳制为 max_tokens-1
- thinking 启用 + budget = max_tokens -> 钳制为 max_tokens-1
- thinking 启用 + budget < max_tokens -> budget 不变
- thinking 未启用 -> temperature 保持原值，无 thinking 字段
- budget 未配置 -> 不启用 thinking
- max_tokens=0 -> 跳过 thinking 并 warn

## 7. 风险与权衡

- **源头丢弃截断 tool_call**：丢失模型本次意图，但下一轮可基于上下文重新生成。优于 400 崩溃。
- **防御层替换为 `{}`**：语义不准确，但仅兜底；配合源头拦截，正常运行不触发。
- **anthropic temperature 强制 1.0**：改变生成随机性，但符合 thinking 模式规范要求，非 thinking 模式不受影响。
- **不改配置**：代码层钳制使任何配置都安全，但用户可能仍想修正 `daemon.toml` 中不合理的 `thinking_budget_tokens=12800 > max_tokens=8000`--可作为后续建议，本次不动配置。

## 8. 验证标准

- 新增单元测试全部通过。
- 既有测试全部通过（无回归）。
- `cargo clippy -D warnings` 零警告。
- `cargo fmt --check` 通过。
- 手动复现验证（可选）：构造截断场景，确认不再 400。
