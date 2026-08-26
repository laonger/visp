# visp OTel / Langfuse Trace 字段说明

本文档记录 visp 当前通过 OpenTelemetry 导出的 trace/span 结构，以及 Langfuse 兼容字段。

## 导出链路

visp-daemon 使用 `tracing` 创建 span，经 `tracing-opentelemetry` 转换为 OpenTelemetry span，再通过 OTLP/gRPC 导出到 OTel Collector 或 Langfuse。

```text
visp-daemon tracing span
  -> tracing-opentelemetry
  -> OpenTelemetry span
  -> OTLP/gRPC exporter
  -> OTel Collector / Langfuse
```

## Resource 信息

| 字段 | 说明 |
|---|---|
| `service.name` | 固定为 `visp-daemon` |
| `service.version` | 当前 `visp-daemon` crate 版本 |
| `host.name` | `HOSTNAME` 环境变量，缺省为 `unknown` |
| `process.pid` | daemon 进程 PID |

## Langfuse 配置

```toml
[observability.langfuse]
enabled = true
user_id = "user_456"
tags = ["agent", "vibe"]

[observability.langfuse.capture]
input = true
output = true
max_chars = 20000
redact_secrets = true
```

说明：

- `enabled = true` 后才记录 Langfuse trace/session/user/tags 等字段。
- `capture.input = true` 后记录 root observation input 和 generation input。
- `capture.output = true` 后记录 root observation output 和 generation output。
- capture 默认关闭，避免默认导出 prompt、system prompt、completion 等内容。
- capture 内容按 `max_chars` 截断；provider 层 generation input/output 会做基础敏感字段脱敏。

## Span 结构

```text
visp.agent.run
  -> visp.agent.iteration
       -> gen_ai.client.operation
       -> visp.tool.execute

visp.subagent.spawn
  -> visp.agent.run
       -> ...
```

## 根 Span：`visp.agent.run`

`visp.agent.run` 是每次 agent run 的根 span，也是 Langfuse trace preview 的主要承载位置。

| 字段 | 说明 |
|---|---|
| `session.id` | visp 当前 session UUID |
| `session.short_id` | session id 前 8 位 |
| `langfuse.session.id` | Langfuse session id，等于 visp session id |
| `langfuse.trace.name` | 固定为 `visp.agent.run` |
| `langfuse.user.id` | 来自 `[observability.langfuse].user_id` |
| `langfuse.trace.tags` | 来自 `[observability.langfuse].tags`，以 JSON 数组字符串记录 |
| `langfuse.environment` | Langfuse environment，有配置时记录 |
| `langfuse.release` | Langfuse release，有配置时记录 |
| `langfuse.version` | Langfuse version，有配置时记录 |
| `langfuse.trace.public` | Langfuse trace public 开关，有配置时记录 |
| `langfuse.trace.metadata` | Langfuse trace metadata，紧凑 JSON 字符串 |
| `langfuse.observation.type` | root observation 类型，记录为 `span` |
| `langfuse.observation.input` | 当前用户输入的 JSON 字符串，用于 Langfuse trace preview |
| `langfuse.observation.output` | agent 最终回复的 JSON 字符串，用于 Langfuse trace preview |
| `visp.agent.kind` | agent 类型：`primary` / `sub` |
| `visp.agent.depth` | agent 嵌套深度 |
| `visp.span.w3c_id` | visp 生成的 W3C span id |

root observation I/O 只用于 trace preview：root input 只记录本轮用户消息，不记录完整 system prompt；root output 只记录最终回复，不记录工具中间结果。

## 迭代 Span：`visp.agent.iteration`

| 字段 | 说明 |
|---|---|
| `visp.span.w3c_id` | visp 生成的 span id，用于内部父子链路桥接 |
| `iteration.count` | 当前第几轮 agent loop |
| `langfuse.*` trace 字段 | 从 root trace 配置传播 |

## LLM Span：`gen_ai.client.operation`

Anthropic 和 OpenAI provider 都会创建 `gen_ai.client.operation` span。该 span 在 Langfuse 中映射为 `GENERATION` observation。

| 字段 | 说明 |
|---|---|
| `gen_ai.system` | provider 标识，当前为 `openai` 或 `anthropic`，不是 system prompt 文本 |
| `gen_ai.request.model` | 请求模型 |
| `gen_ai.operation.name` | 当前固定为 `chat` |
| `gen_ai.provider.name` | provider 名称 |
| `gen_ai.request.max_tokens` | 请求 max tokens |
| `gen_ai.request.temperature` | 请求 temperature |
| `gen_ai.usage.input_tokens` | 输入 token |
| `gen_ai.usage.output_tokens` | 输出 token |
| `gen_ai.usage.cache_read.input_tokens` | cache read token |
| `gen_ai.usage.cache_creation.input_tokens` | cache creation token |
| `gen_ai.response.finish_reasons` | finish reason，JSON/数组字符串形式 |
| `gen_ai.response.model` | 实际响应模型 |
| `visp.llm.token_limit_hit` | 命中 token limit 时记录 |
| `langfuse.observation.type` | 固定为 `generation` |
| `langfuse.observation.input` | provider 请求体，JSON 字符串 |
| `langfuse.observation.output` | provider 响应内容 |
| `langfuse.*` trace 字段 | 从 agent trace 配置传播 |

说明：

- system prompt 不记录在 `gen_ai.system`，而是在开启 capture 后通过 generation input 的请求体可见。
- OpenAI input 使用 Chat request body，system role message 在 `messages` 中。
- Anthropic input 使用 Messages request body，system prompt 在独立 `system` 字段中。
- generation I/O 用于 Langfuse trace detail；trace list / preview 使用 root span 的 observation I/O。

## 工具 Span：`visp.tool.execute`

| 字段 | 说明 |
|---|---|
| `gen_ai.tool.name` | 工具名 |
| `gen_ai.tool.call.id` | LLM 返回的 tool call id |
| `gen_ai.tool.type` | 当前固定为 `function` |
| `gen_ai.operation.name` | 当前为 `execute_tool` |
| `langfuse.observation.type` | 记录为 `span` |
| `level` / `status_message` | 工具错误状态和摘要 |
| `langfuse.*` trace 字段 | 从 agent trace 配置传播 |
| `visp.tool.is_error` | 工具执行是否失败 |
| `visp.tool.duration_ms` | 工具执行耗时，毫秒 |

当前不记录工具参数和工具结果内容到 `langfuse.observation.input/output`，避免默认导出过多用户数据或文件内容。

## 子 Agent Span：`visp.subagent.spawn`

| 字段 | 说明 |
|---|---|
| `visp.subagent.name` | 子 agent 类型 |
| `visp.subagent.session_id` | 子 agent session id |
| `visp.subagent.call_id` | 对应 tool call id |
| `visp.subagent.task_id` | task id，有值时记录 |
| `visp.subagent.depth` | 子 agent 深度 |
| `trace_id` / `parent_span_id` / `trace_state` | 跨 agent 传播字段 |
| `langfuse.*` trace 字段 | 从父 agent trace 配置传播，`langfuse.trace.name` 固定为 `visp.agent.run` |

## Langfuse UI 显示关系

| 位置 | 主要来源 |
|---|---|
| Trace list / preview | root span `visp.agent.run` 的 `langfuse.observation.input/output` |
| Trace detail 中的 generation | `gen_ai.client.operation` 的 `langfuse.observation.input/output` |

因此 visp 当前同时记录 root observation I/O 和 generation observation I/O：root I/O 是用户可读摘要，generation I/O 是 provider 请求/响应详情。

## 日志说明

常见事件包括 `visp.agent.completed`、`visp.agent.cancelled`、`gen_ai.client.completed`、工具参数异常和 agent panic。是否在后端中显示为 OTel event，取决于当前 OTel layer 和后端展示方式。

capture 排查阶段的临时 debug 日志已删除；当前不通过日志打印 prompt、completion、system prompt 或 captured input/output 内容。

## 接入 Langfuse 的关键检查项

1. 后端按 `service.name = visp-daemon` 查询。
2. root span `visp.agent.run` 上应存在 `langfuse.session.id`、`langfuse.trace.name = visp.agent.run`、`langfuse.observation.type = span`；开启 capture 后应存在 root input/output。
3. LLM span `gen_ai.client.operation` 上应存在 `langfuse.observation.type = generation`、trace 级 Langfuse 字段、模型字段、token 字段和成本字段；开启 capture 后应存在 generation input/output。
4. 工具 span `visp.tool.execute` 上应存在工具名、tool call id、耗时和错误状态字段。
