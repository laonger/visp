# visp Langfuse OpenTelemetry 兼容设计

## 背景

visp 已具备 OpenTelemetry trace 输出能力，并在 agent、LLM、tool、subagent 等关键路径上创建 span。Langfuse Native OpenTelemetry 接收 OTLP/HTTP，而当前 visp 使用 OTLP/gRPC exporter。

本设计采用 OpenTelemetry Collector 作为协议转换、认证和转发边界：visp 继续作为通用 OTel 生产者，Collector 负责转发到 Langfuse。

## 目标

1. 保持 visp 侧 OTLP/gRPC 导出路径不变。
2. 通过 Collector 转发到 Langfuse OTLP/HTTP endpoint。
3. 在 visp spans 上补充 Langfuse 可识别的 trace 与 observation attributes。
4. 明确 input/output、tool args/results 的采集、截断和脱敏策略。
5. 让 Langfuse 稳定呈现 trace/session/user/tags、LLM generation、token usage、cache 命中、cost、tool observation 和错误状态。
6. 保持默认安全：不默认上传 prompt、completion、tool arguments 或 tool results。

## 非目标

1. visp 不直接使用 Langfuse SDK。
2. visp 不直接连接 Langfuse OTLP/HTTP endpoint。
3. visp 不保存 Langfuse public key 或 secret key。
4. 第一版不实现完整 DLP 或 PII 脱敏。
5. 第一版不自动管理或启动 Collector 进程。
6. 不改变现有通用 OpenTelemetry 后端支持模型。
7. 不把 visp runtime 版本默认写入 Langfuse 业务 version/release 字段。

## 总体架构

目标链路为：visp-daemon 通过 OTLP/gRPC 发送 trace 到 OpenTelemetry Collector；Collector 通过 OTLP/HTTP 发送到 Langfuse；Langfuse 根据 `langfuse.*` 和 `gen_ai.*` attributes 识别 trace、generation、tool observation、metadata、usage 和 cost。

职责划分：

| 组件 | 职责 |
|---|---|
| visp-daemon | 创建 trace/span，写入通用 OTel 字段和 Langfuse 语义字段，通过 OTLP/gRPC 发给 Collector |
| OpenTelemetry Collector | 接收 visp gRPC trace，转换为 OTLP/HTTP，附加 Langfuse auth/header，转发到 Langfuse |
| Langfuse | 展示和分析 trace、generation、tool observation、metadata、usage 和 cost |

## 协议与配置边界

visp 保持 OTLP/gRPC exporter。使用 Langfuse 时，visp 的 OTLP endpoint 应配置为 Collector 的 gRPC receiver，而不是 Langfuse 的 HTTP endpoint。

Langfuse endpoint、Authorization Basic header、`x-langfuse-ingestion-version` header 都属于 Collector 配置，不进入 visp daemon 配置。

visp 侧不配置 Langfuse endpoint、public key、secret key 或 Langfuse OTLP/HTTP protocol。

## Collector 配置要求

Collector 至少需要具备：

1. OTLP gRPC receiver：接收 visp 发出的 OTLP/gRPC trace。
2. OTLP HTTP exporter：发送到 Langfuse `/api/public/otel` 或 trace 专用 endpoint。
3. Basic Auth header：使用 Langfuse public key 和 secret key 组合后的 Basic auth。
4. `x-langfuse-ingestion-version = 4` header：用于 Langfuse Fast Preview。

P0 实施阶段应新增完整示例文件 `docs/otel-collector-langfuse.example.yaml`。该示例文件只使用占位符或环境变量，不包含真实 key；它用于降低用户配置 Collector 的成本，但 visp 本身仍不管理 Collector。

## `[observability.langfuse]` 配置设计

`[observability.langfuse]` 是 Langfuse 语义字段配置，不是 Langfuse 连接配置。

| 字段 | 默认值 | 说明 |
|---|---:|---|
| `enabled` | `false` | 是否写入任何 `langfuse.*` attributes |
| `user_id` | 空 | 映射到 `langfuse.user.id` |
| `tags` | 空数组 | 映射到 `langfuse.trace.tags` |
| `environment` | `default` | 映射到 `langfuse.environment` |
| `release` | 空 | 显式配置后映射到 `langfuse.release` |
| `version` | 空 | 显式配置后映射到 `langfuse.version` |
| `public` | 未设置 | 显式配置后映射到 `langfuse.trace.public` |
| `metadata` | 空 | 映射到 `langfuse.trace.metadata.*` |
| `capture_input` | `false` | 是否记录 LLM 输入 |
| `capture_output` | `false` | 是否记录 LLM 输出 |
| `capture_tool_args` | `false` | 是否记录 tool call arguments |
| `capture_tool_results` | `false` | 是否记录 tool execution result |
| `max_capture_chars` | `20000` | capture 内容最大字符数 |
| `redact_secrets` | `true` | 是否执行简单 secret 脱敏 |

### 默认行为

`enabled` 默认值为 `false`。只有显式开启时，visp 才写入任何 `langfuse.*` attribute。即使存在 `user_id`、`tags`、`metadata` 或 capture 相关字段，只要未启用，visp 也不写 Langfuse 专用字段。

启用 Langfuse 后，`environment` 默认写为 `default`，用户可覆盖。

`public` 为可选布尔值，具备三态语义：未设置时不写 `langfuse.trace.public`；设置为 `true` 时写入 true；设置为 `false` 时写入 false。非布尔类型属于配置错误，daemon 应在配置解析或启动阶段报错，而不是静默降级。

metadata 支持复杂 TOML 值。所有 metadata 以 `langfuse.trace.metadata.<key>` 形式写入独立 attribute：字符串、布尔、整数、浮点值直接写入对应值；数组和表结构序列化为紧凑 JSON 字符串后写入对应 `<key>`。第一版不把整个 metadata 写成单个 JSON blob，也不递归展开嵌套表为多级 attribute。

## Trace 级字段设计

启用 Langfuse 后默认写入：

| Langfuse attribute | 来源 |
|---|---|
| `langfuse.session.id` | visp session id |
| `langfuse.user.id` | `observability.langfuse.user_id` |
| `langfuse.trace.tags` | `observability.langfuse.tags` |
| `langfuse.trace.name` | `visp.agent.run.<short-id>` |
| `langfuse.environment` | `observability.langfuse.environment`，默认 `default` |

可选写入：

| Langfuse attribute | 来源 |
|---|---|
| `langfuse.release` | `observability.langfuse.release` |
| `langfuse.version` | `observability.langfuse.version` |
| `langfuse.trace.public` | `observability.langfuse.public` |
| `langfuse.trace.metadata.*` | `observability.langfuse.metadata` |

默认不写入 `langfuse.trace.input` 和 `langfuse.trace.output`，避免默认上传 prompt、上下文、文件内容、模型回复或敏感业务内容。

trace name 使用 `visp.agent.run.<short-id>`，其中 `<short-id>` 为当前 visp session id 的短 ID。该命名与现有 root span 保持一致，可区分 session，且不包含用户输入。

visp 自身版本只保留在 OTel Resource 的 `service.version`。`langfuse.release` 和 `langfuse.version` 只从用户配置读取，未配置时不写，避免把 visp runtime 版本误当作用户应用、prompt 或部署版本。

## 字段传播策略

Langfuse trace 级字段采用手动传播，不使用 OpenTelemetry baggage。

需要传播到以下关键 span：

1. `visp.agent.run`
2. `visp.agent.iteration`
3. `gen_ai.client.operation`
4. `visp.tool.execute`
5. `visp.subagent.spawn`

传播字段包括 session、user、tags、trace name、environment，以及已显式配置的 release、version、public 和 metadata。

不使用 baggage 的原因：避免 user/session/tags 跨服务传播到第三方 API；Rust OTel baggage processor 生态和行为还需要额外验证；当前关键 span 数量有限，手动传播更可控。

## LLM generation observation 设计

`gen_ai.client.operation` 映射为 Langfuse generation observation。

默认补充：

| Attribute | 说明 |
|---|---|
| `langfuse.observation.type` | 固定为 `generation` |
| `gen_ai.operation.name` | 固定为 `chat` |
| `gen_ai.provider.name` | provider 层提供的稳定小写 provider id |
| `gen_ai.system` | provider 层提供的稳定小写 provider id |
| `gen_ai.request.model` | 请求模型 |
| `gen_ai.response.model` | 响应模型 |
| `gen_ai.request.max_tokens` | 请求参数 |
| `gen_ai.request.temperature` | 请求参数 |
| `gen_ai.response.finish_reasons` | 结束原因，JSON 数组字符串 |
| `gen_ai.usage.input_tokens` | 输入 tokens，必须包含 cached tokens |
| `gen_ai.usage.output_tokens` | 输出 tokens |
| `gen_ai.usage.prompt_tokens` | 与 input tokens 同义的兼容字段 |
| `gen_ai.usage.completion_tokens` | 与 output tokens 同义的兼容字段 |
| `gen_ai.usage.cache_read.input_tokens` | 命中 prompt/cache 的输入 tokens |
| `gen_ai.usage.cache_creation.input_tokens` | 写入 prompt/cache 的输入 tokens |
| `visp.llm.cache_hit_tokens` | visp 内部 cache 命中 token 数，等同 cache read tokens |
| `visp.llm.cache_write_tokens` | visp 内部 cache 写入 token 数，等同 cache creation tokens |
| `visp.llm.cache_hit_ratio` | cache 命中占比，按 cache read tokens / input tokens 计算 |
| `visp.llm.token_limit_hit` | finish reason 归一化为 `length` 时写入 true |
| `gen_ai.usage.cost` | Langfuse 可识别的成本字段 |
| `visp.llm.cost_usd` | visp 内部成本字段，保留用于调试和兼容旧字段 |

暂不额外写入 `langfuse.observation.model.name`。model parameters 继续通过 `gen_ai.request.*` 表达，不额外构造 `langfuse.observation.model.parameters` JSON 字段。

`gen_ai.provider.name` 和 `gen_ai.system` 都使用 provider 层提供的稳定小写 provider id，例如 `anthropic` 或 `openai`。它们不从用户配置中的展示名、模型别名或自定义 provider name 推导。

`gen_ai.system` 不再承载 system prompt 文本。system prompt 默认不上传；只有显式开启 `capture_input` 后，才允许作为 input 的一部分记录，并可写入 `gen_ai.system_instructions`。该内容仍必须经过统一脱敏和截断。

finish reason 使用 `gen_ai.response.finish_reasons`，格式为 JSON 数组字符串。provider 原始结束原因能明确映射时应归一化为稳定值，例如 `stop`、`length`、`tool_calls`、`content_filter`、`error`；不能明确映射时保留原始值，避免丢失排障信息。

cache 命中统计属于 LLM usage 的一部分。Anthropic 等 provider 返回 cache read / cache creation token 时，应同时记录 GenAI usage 字段和 visp 内部字段。`cache_hit_ratio` 用于快速判断本次 LLM 调用是否有效复用了 prompt/cache；当 input tokens 为 0 或 provider 不返回 cache usage 时，该比例不写入。

## LLM input/output capture 策略

默认不采集 LLM input/output。

| 配置 | 默认值 | 开启后写入字段 | 内容范围 |
|---|---:|---|---|
| `capture_input` | `false` | `langfuse.observation.input`，可包含 `gen_ai.system_instructions` | system prompt 与 conversation messages |
| `capture_output` | `false` | `langfuse.observation.output` | assistant text 与 tool call 名称摘要 |

LLM generation output 不内嵌完整 tool arguments 或 tool results。工具输入输出由 tool span 自己承载。

## Tool observation 设计

每个工具调用 `visp.tool.execute` 映射为 Langfuse 普通 span observation。

默认写入：

| Attribute | 说明 |
|---|---|
| `langfuse.observation.type` | 固定为 `span` |
| `gen_ai.operation.name` | 固定为 `execute_tool` |
| `gen_ai.tool.name` | 工具名 |
| `gen_ai.tool.call.id` | LLM 返回的 tool call id |
| `gen_ai.tool.type` | 工具类型，当前为 function |
| `visp.tool.duration_ms` | 工具执行耗时 |
| `visp.tool.is_error` | 是否失败 |
| `langfuse.observation.level` | 成功为 DEFAULT，失败为 ERROR |
| `langfuse.observation.status_message` | 失败时记录错误摘要 |

可选 capture：

| 配置 | 写入字段 | 内容 |
|---|---|---|
| `capture_tool_args` | `langfuse.observation.input` | tool call arguments |
| `capture_tool_results` | `langfuse.observation.output` | tool execution result |

## 统一 capture sanitizer

所有 capture 内容受统一 sanitizer 处理。适用范围包括 `langfuse.observation.input`、`langfuse.observation.output`、tool arguments、tool results、status message，以及 capture 开启后写入的 system instructions。

统一处理顺序为：原始内容先执行 best-effort secret redaction，再按 `max_capture_chars` 字符数截断，最后写入目标 attribute。

超过限制时，截断文本末尾追加明确标记，包含原始长度和限制长度，格式为：`...[truncated: original_chars=<n>, max_chars=<m>]`。

同时写入辅助字段，表达内容已截断、原始字符数和最大字符数。若同一 span 内多个字段都可能被截断，辅助字段使用字段作用域命名，避免无法判断是 input、output 还是 status message 被截断：

| 内容字段 | 截断标记字段 | 原始长度字段 | 最大长度字段 |
|---|---|---|---|
| `langfuse.observation.input` | `visp.capture.input.truncated` | `visp.capture.input.original_chars` | `visp.capture.input.max_chars` |
| `langfuse.observation.output` | `visp.capture.output.truncated` | `visp.capture.output.original_chars` | `visp.capture.output.max_chars` |
| `langfuse.observation.status_message` | `visp.capture.status_message.truncated` | `visp.capture.status_message.original_chars` | `visp.capture.status_message.max_chars` |
| `gen_ai.system_instructions` | `visp.capture.system_instructions.truncated` | `visp.capture.system_instructions.original_chars` | `visp.capture.system_instructions.max_chars` |

## 简单脱敏策略

`redact_secrets` 默认值为 `true`。脱敏是 best-effort，不保证覆盖所有敏感信息。开启任何 capture 配置后，用户仍需承担上传 prompt、completion、tool arguments、tool results 到 Langfuse 的风险。

结构化数据中，authorization、proxy-authorization、cookie、set-cookie、x-api-key、api_key、apikey、access_token、refresh_token、id_token、token、secret、client_secret、password、passwd、private_key 等敏感 key 的 value 会被替换为 `[REDACTED]`。key 匹配大小写不敏感。

非结构化文本执行常见 secret 模式脱敏，包括 Bearer token、Basic token、常见 API key 前缀、Langfuse key、OpenAI/Anthropic API key 环境变量形式、private key block 等。

第一版不做 PII 自动识别、文件路径脱敏、业务字段脱敏、模型输出语义脱敏或自定义脱敏规则 DSL。

## 错误、取消和 panic 映射

| 场景 | Span | Level | Status message |
|---|---|---|---|
| rate limit / 429 | `gen_ai.client.operation` | ERROR | rate limit summary |
| provider API error | `gen_ai.client.operation` | ERROR | provider error summary |
| network error | `gen_ai.client.operation` | ERROR | network error summary |
| tool timeout | `visp.tool.execute` | ERROR | timeout summary |
| tool 执行失败 | `visp.tool.execute` | ERROR | tool error summary |
| LLM token limit / max tokens | `gen_ai.client.operation` | DEFAULT | 不标 ERROR，写 `gen_ai.response.finish_reasons = ["length"]` 和 `visp.llm.token_limit_hit = true` |
| LLM 请求失败 | `gen_ai.client.operation` | ERROR | LLM error summary |
| agent panic | `visp.agent.run` | ERROR | panic summary |
| agent cancelled | `visp.agent.run` | WARNING | agent cancelled |
| iteration limit reached | `visp.agent.run` | WARNING | iteration limit reached |
| invalid tool arguments | tool span 或 iteration span | ERROR | invalid args summary |

如果 `redact_secrets = true`，status message 和 observation output 也需要经过脱敏。同时保留标准 OTel span status，用于非 Langfuse 后端识别错误状态。

Token limit / max tokens 命中属于“成功返回但内容被截断”，不自动标记为错误，避免污染错误率。rate limit、provider/network/API error、tool timeout 和 panic 视为失败。

## 安全与隐私边界

| 配置 | 默认值 | 风险等级 |
|---|---:|---|
| `capture_input` | false | 中 |
| `capture_output` | false | 中 |
| `capture_tool_args` | false | 高 |
| `capture_tool_results` | false | 最高 |
| `redact_secrets` | true | 降低 secret 泄露风险，但不保证完整脱敏 |

原则：不默认上传 prompt、completion、tool arguments 或 tool results；用户显式开启 capture 后，visp 才写入相关 Langfuse observation input/output；简单脱敏不能替代用户的数据治理策略。

## 实施范围建议

P0：完善 Langfuse 配置结构；修正 tags 字段为 `langfuse.trace.tags`；增加 trace name、environment、可选 release/version/public、metadata 映射；将 Langfuse trace 字段手动传播到关键 span；LLM span 标记为 generation，并补充 provider、operation、finish reason、token、cache 命中、token limit、cost 兼容字段；tool span 标记为普通 observation 并补充 operation、level/status message；新增 Collector 示例配置文件。

P0 会把现有 `langfuse.tags` attribute 迁移为 `langfuse.trace.tags`。若外部后端或 dashboard 已依赖旧字段名，需要同步调整查询；本项目当前按 Langfuse 兼容性优先处理，不保留旧字段双写。

P0 的 status message 只应包含错误类型和摘要，不应拼接 tool arguments、tool results、prompt、completion 或大段 provider 原始响应。capture 内容的统一脱敏和截断属于 P1。

P1：实现 input/output、tool args/results capture；实现统一截断；实现简单 secret 脱敏。

## 验证标准

1. Langfuse 关闭时，不写任何 `langfuse.*` 字段。
2. Langfuse 开启时，root、iteration、LLM、tool、subagent 关键 span 都包含 trace 级字段。
3. tags 使用 `langfuse.trace.tags`，不再使用旧的 `langfuse.tags`。
4. `langfuse.trace.public` 未配置时不写，配置后按布尔值写入。
5. `langfuse.release` 和 `langfuse.version` 只从配置读取；visp 自身版本只体现在 `service.version`。
6. LLM span 能被识别为 generation，并包含 provider、operation、finish reason、模型、token、cache 命中、token limit、cost 字段。
7. cache 字段使用 `gen_ai.usage.cache_read.input_tokens` 和 `gen_ai.usage.cache_creation.input_tokens`。
8. `gen_ai.system` 表示 provider id，不再承载 system prompt；system prompt 只在 capture input 开启后允许记录。
9. tool span 能被识别为普通 observation，并包含 `gen_ai.operation.name = "execute_tool"`。
10. token limit 不标 ERROR；rate limit、provider/network/API error、tool timeout 和 panic 标 ERROR。
11. capture 默认关闭；开启后内容经过统一脱敏和截断。
12. Collector 示例能说明 visp gRPC 到 Langfuse HTTP 的转发链路。
