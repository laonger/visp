# visp 工作计划：LLM 调用 400 错误修复

## 概述

修复两类 400：① openai 协议 max_tokens 截断导致畸形 tool_call 污染会话历史；② anthropic 协议 thinking 配置违规。采用分层防御，改动集中在 visp-llm（openai.rs、anthropic.rs），agent_loop 仅验证兼容性。

设计文档：`docs/design/2026-08-03-llm-400-truncated-toolcall-fix-design.md`

## 步骤 1：openai 截断修复（visp-llm/openai.rs）

### 1a：源头拦截 —— SSE Finish 事件丢弃截断 tool_call

**改动位置**：`openai.rs` Finish 事件处理（约 559-583 行）+ 发射逻辑（约 621-638 行）

**策略**：`finish_reason == "length"` 时，对 `tool_acc` 中每个 tool_call 的累积 arguments 做 JSON 合法性校验（`serde_json::from_str`）：合法的保留并正常发射 `ChatEvent::ToolCall`；非法（截断）的丢弃，不转入 `pending_tool_calls`、不发射。若丢弃后无任何 tool_call 且本轮无文本输出，发射一条提示文本（TextDelta），内容表明因长度限制工具调用被截断、需重新发起，保证 assistant 消息非空。

#### 🔴 红 - 测试

| # | 测试用例 | 简明描述 |
|---|---|---|
| 1 | length + arguments 畸形 | finish_reason='length'，tool_acc 含截断 arguments（非法 JSON）-> 不发射 ToolCall 事件 |
| 2 | length + 畸形 + 有文本 | 同上但已有 TextDelta -> 文本正常保留，tool_call 丢弃 |
| 3 | length + 畸形 + 无文本 | 截断且无文本 -> 发射提示文本，不发射 ToolCall |
| 4 | length + arguments 合法 | finish_reason='length' 但 tool_call arguments 是合法 JSON -> 正常发射 ToolCall（不误丢） |
| 5 | stop + tool_call | finish_reason='stop' + 合法 tool_call -> 正常发射（回归） |
| 6 | length + 多 tool_call 部分截断 | 多个 tool_call，部分合法部分畸形 -> 保留合法，丢弃畸形 |
| 7 | length + 全畸形 + 无文本 | 全部截断且无文本 -> 提示文本，无 ToolCall |

#### 🟢 绿 - 实现
修改 Finish 事件处理：length 时对 tool_acc 逐个 JSON 校验，畸形的从 tool_acc 移除（不进 pending_tool_calls）。在发射收尾逻辑中，若 length 截断导致无 tool_call 且无文本，发射提示 TextDelta。

#### 🧪 测试 -> 🔍 类型检查
`cargo test -p visp-llm` && `cargo clippy -p visp-llm -- -D warnings`

#### ♻️ 重构
提取"arguments 合法性校验"为小函数，源头层与防御层复用。

#### 📦 提交
`fix(llm): drop truncated tool calls on finish_reason=length in openai stream`

---

### 1b：防御兜底 —— build_openai_messages 校验 arguments

**改动位置**：`openai.rs` `build_openai_messages`（约 174-188 行）

**策略**：序列化 tool_calls 前，逐个校验 `arguments` 是否合法 JSON。畸形的替换为 `{}`（不丢弃整个 tool_call，避免后继 tool_result 消息变孤儿）。复用 1a 提取的校验函数。

#### 🔴 红 - 测试

| # | 测试用例 | 简明描述 |
|---|---|---|
| 1 | 合法 arguments | 合法 JSON 字符串 -> 原样序列化 |
| 2 | 畸形 arguments | 截断/非法 JSON -> arguments 替换为 `{}` |
| 3 | 混合合法/畸形 | 多个 tool_call，部分合法部分畸形 -> 只替换畸形的 |
| 4 | 空字符串 arguments | `""` -> 原样（空参数合法，不替换） |
| 5 | 无 tool_calls | assistant 无 tool_calls -> 不变（回归） |

#### 🟢 绿 - 实现
在 tool_calls 序列化的 map 闭包中，对 `tc.arguments` 先 `serde_json::from_str`，失败则替换为 `"{}"`。

#### 🧪 测试 -> 🔍 类型检查
`cargo test -p visp-llm` && `cargo clippy -p visp-llm -- -D warnings`

#### 📦 提交
`fix(llm): sanitize malformed tool_call arguments in build_openai_messages`

## 步骤 2：anthropic thinking 校验（visp-llm/anthropic.rs）

### 2a：构建时强制 thinking 合规

**改动位置**：`anthropic.rs` `build_anthropic_request`（约 43-86 行）

**策略**：① thinking 启用（`extra["thinking_budget_tokens"]` 存在且可解析）时，请求体 `temperature` 强制为 1.0，覆盖 config 值；thinking 未启用时保持 config.temperature。② `budget >= max_tokens` 时钳制为 `max_tokens - 1`；`max_tokens == 0` 时跳过 thinking 启用并 warn。

#### 🔴 红 - 测试

| # | 测试用例 | 简明描述 |
|---|---|---|
| 1 | thinking 启用 + temp=0.7 | -> 请求体 temperature=1.0 |
| 2 | thinking 启用 + budget > max_tokens | budget=12800, max_tokens=8000 -> budget=7999 |
| 3 | thinking 启用 + budget = max_tokens | -> budget=max_tokens-1 |
| 4 | thinking 启用 + budget < max_tokens | budget=4096, max_tokens=8000 -> budget 不变 |
| 5 | thinking 未启用 | 无 thinking_budget_tokens -> temperature 保持原值，无 thinking 字段 |
| 6 | budget 未配置 | extra 无 thinking_budget_tokens -> 不启用 thinking |
| 7 | max_tokens=0 | -> 跳过 thinking 启用，warn |
| 8 | thinking 启用 + temp 已=1.0 | -> temperature=1.0（幂等，回归） |

#### 🟢 绿 - 实现
在写入 temperature 前判断 thinking 是否将启用；thinking 启用分支中写 temperature=1.0。budget 解析后与 max_tokens 比较，超限钳制或跳过。

#### 🧪 测试 -> 🔍 类型检查
`cargo test -p visp-llm` && `cargo clippy -p visp-llm -- -D warnings`

#### 📦 提交
`fix(llm): enforce anthropic thinking constraints (temperature=1.0, budget<max_tokens)`

## Wave 并行策略

### Wave 1：实现修复（2 个并行任务，并行）

两个任务改动不同文件，互不依赖，可由两个 fixer 并行执行。

- **任务 A（openai.rs）**：1a 源头拦截 -> 1b 防御兜底（串行，同文件两个 commit）
- **任务 B（anthropic.rs）**：2a thinking 校验（独立 commit）

### Wave 2：集成验证（串行，orchestrator 执行）

依赖 Wave 1 全部完成。

- 全 workspace 构建：`cargo build`
- 全量测试：`cargo test`（确认无回归）
- Lint：`cargo clippy -- -D warnings`
- 格式：`cargo fmt --check`
- 验证 agent_loop 文本分支兼容截断场景（确认源头拦截后 tool_calls 为空走文本路径正常，必要时补测试）

## 依赖关系总览

```
Wave 1 (并行)
├─ 任务A: 1a (openai SSE) ──> 1b (openai build_messages)   [同文件串行]
└─ 任务B: 2a (anthropic build_request)                      [独立]
                    │
                    ▼
              Wave 2: 集成验证 (cargo test/clippy/fmt + agent_loop 兼容性)
```

1a 与 1b 共享"arguments 合法性校验"小函数，故 1a 先行（定义函数），1b 复用。2a 完全独立。

## 测试覆盖汇总

| Wave | 并行数 | 模块/文件 | 步骤 | 测试用例数 |
|---|---|---|---|---|
| 1 | 2 | visp-llm/openai.rs | 1a 源头拦截 | 7 |
| 1 | 2 | visp-llm/openai.rs | 1b 防御兜底 | 5 |
| 1 | 2 | visp-llm/anthropic.rs | 2a thinking 校验 | 8 |
| 2 | 1 | 全 workspace | 集成验证 | 既有测试回归 |

## 备注

- **agent_loop 兼容性**：源头拦截后截断轮 tool_calls 为空，走 `handle_stream_result` 已有文本分支（约 531 行 `if tool_calls.is_empty()`）。预期零改动，Wave 2 验证确认；若发现空 content 边界问题，补测试 + 微调。
- **测试位置**：visp-llm 内联测试模块（`#[cfg(test)]`），遵循项目既有惯例（agent_loop.rs 等已有内联测试）。fixer 实现时确认 openai.rs/anthropic.rs 既有测试模块结构。
- **不改配置**：daemon.toml 中 `thinking_budget_tokens=12800 > max_tokens=8000` 由代码钳制兜底，本次不动配置。后续可建议用户修正为合理值。
- **提示文本语义**：源头拦截注入的提示文本作为 assistant content 进入历史，模型下轮可见，语义为"工具调用因长度被截断"。实现时文案保持简短中性。
- **不引入新依赖**：复用既有 serde_json。
