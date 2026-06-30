# visp 工作计划：Langfuse OpenTelemetry 兼容性

## 概述

基于 `docs/design/visp-design-langfuse-otel-compatibility.md` 实施 P0：补齐 Langfuse 语义配置、trace 级字段、LLM generation 字段、tool observation 字段、错误语义和 Collector 示例。P1 的 input/output capture、tool args/results capture、统一截断和脱敏不在本轮实现。

执行原则：每个子步骤先写测试并确认失败，再写最小实现；只改相关文件；最终通过 Rust 质量门禁。实际提交需用户另行明确授权。

## 步骤 1：配置模型

### 1a. 扩展 daemon Langfuse 配置

红：新增/更新配置解析测试。

| # | 测试用例 | 说明 |
|---|---|---|
| 1 | 默认配置 | `enabled=false`，optional 字段未设置 |
| 2 | 完整配置 | user、tags、environment、release、version、public、metadata、capture 开关可解析 |
| 3 | public 三态 | 未设置不写；true/false 保留布尔语义 |
| 4 | metadata 复杂值 | 标量保留；数组/table 后续可转紧凑 JSON 字符串 |
| 5 | public 非布尔报错 | 不把空字符串或数字静默降级 |

绿：补齐 `LangfuseConfig` 与默认值；不加入 Langfuse endpoint/auth/header。

验证：`cargo test -p visp-daemon config`；`cargo clippy -p visp-daemon -- -D warnings`。

提交点：`feat(daemon): 扩展 langfuse observability 配置`。

### 1b. 传递配置到核心 AgentConfig

红：测试 enabled 总开关、user/tags/environment/release/version/public/metadata 传递、空 tags 策略、metadata 跨 crate 表达。

绿：daemon 构造核心配置时传递 Langfuse 语义配置；enabled 控制是否写 `langfuse.*`。

验证：`cargo test -p visp-daemon langfuse`；`cargo test -p visp-core langfuse`；相关 clippy。

提交点：`feat(core): 传递 langfuse trace 配置`。

## 步骤 2：Trace 级 Langfuse 字段

### 2a. root span 字段映射

红：测试 disabled 不写任何 `langfuse.*`；enabled 时 root span 写 session/user/tags/name/environment；release/version 仅显式配置写；public 三态；metadata 写为 `langfuse.trace.metadata.<key>`；不再写旧 `langfuse.tags`。

绿：在 `visp.agent.run` 写 trace 级字段；trace name 为 `visp.agent.run.<short-id>`；visp 版本只保留在 `service.version`。

验证：`cargo test -p visp-core langfuse`；`cargo clippy -p visp-core -- -D warnings`。

提交点：`feat(core): 映射 langfuse trace 字段`。

### 2b. 子 span 手动传播 trace 字段

红：测试 iteration、tool、LLM、subagent spawn span 都包含 trace 级字段；disabled 时全部不写 `langfuse.*`。

绿：手动传播 session/user/tags/name/environment/release/version/public/metadata；不使用 baggage。

验证：`cargo test -p visp-core langfuse`；`cargo test -p visp-llm langfuse`；`cargo test -p visp-agent langfuse`；相关 clippy。

提交点：`feat(core): 传播 langfuse trace 字段`。

## 步骤 3：LLM generation 字段

### 3a. provider 与 operation

红：Anthropic/OpenAI span 均写 `gen_ai.operation.name=chat`；`gen_ai.provider.name` 和 `gen_ai.system` 分别为稳定 provider id；system prompt 默认不进入 `gen_ai.system`。

绿：由 provider 层提供稳定小写 provider id，不从用户展示名或模型别名推导；修正 `gen_ai.system` 语义。

验证：`cargo test -p visp-llm gen_ai`；`cargo clippy -p visp-llm -- -D warnings`。

提交点：`feat(llm): 补充 genai provider operation 字段`。

### 3b. finish reason 与 token limit

红：测试 `gen_ai.response.finish_reasons` 为 JSON 数组字符串；Anthropic/OpenAI reason 归一化；未知 reason 保留原始值；`length` 写 `visp.llm.token_limit_hit=true` 且不标 ERROR；rate limit/API/network error 标 ERROR。

绿：统一 finish reason 格式，区分失败和非失败。

验证：`cargo test -p visp-llm finish_reason`；`cargo test -p visp-llm token_limit`；相关 clippy。

提交点：`feat(llm): 规范 generation finish reason 字段`。

### 3c. cache usage 字段

红：Anthropic cache read/write 使用 `gen_ai.usage.cache_read.input_tokens` 与 `gen_ai.usage.cache_creation.input_tokens`；旧下划线字段不再写；input tokens 包含 cached tokens；input=0 不写 ratio；OpenAI 不写 cache 字段。

绿：修正 cache 字段名与 ratio 计算，保留 `visp.llm.*` 调试字段。

验证：`cargo test -p visp-llm cache`；`cargo clippy -p visp-llm -- -D warnings`。

提交点：`feat(llm): 修正 genai cache usage 字段`。

## 步骤 4：Tool observation 字段

### 4a. tool span 语义

红：测试 tool span 写 `langfuse.observation.type=span`、`gen_ai.operation.name=execute_tool`、tool name/call id/type；成功 level 为 DEFAULT；失败 level 为 ERROR 且 status_message 只含摘要；timeout 标 ERROR。

绿：补充 tool observation 字段和错误摘要；不实现 args/results capture。

验证：`cargo test -p visp-core tool_execute`；`cargo test -p visp-core langfuse`；`cargo clippy -p visp-core -- -D warnings`。

提交点：`feat(core): 补充 tool observation 字段`。

## 步骤 5：Subagent span 字段

### 5a. subagent spawn 传播

红：测试 spawn span 包含 Langfuse trace 级字段；disabled 时不写；多层 subagent trace_id 关系不被破坏；现有 subagent name/session/call/task/depth 字段保留。

绿：在 orchestrator spawn span 补充 Langfuse trace 字段，不改变 parent link 和 trace context 协议。

验证：`cargo test -p visp-agent subagent_spawn`；`cargo test -p visp-agent trace_context`；`cargo clippy -p visp-agent -- -D warnings`。

提交点：`feat(agent): 传播 subagent langfuse 字段`。

## 步骤 6：Collector 示例与示例配置

### 6a. 新增 Collector 示例

红：文档/快照类测试或轻量检查覆盖：示例文件存在；包含 OTLP gRPC receiver、OTLP HTTP exporter、`x-langfuse-ingestion-version=4`；不包含真实 secret。

绿：新增 `docs/otel-collector-langfuse.example.yaml`，同步 `docs/daemon.example.toml` 的 Langfuse 配置示例，并声明 Collector 示例是参考配置，不代表 visp 管理 Collector。

验证：相关测试；`cargo clippy -p visp-daemon -- -D warnings`。

提交点：`docs(daemon): 添加 langfuse collector 示例`。

## Wave 并行策略

### Wave 1：配置基础（串行）

1a → 1b。后续所有 span 字段依赖统一配置模型。

### Wave 2：独立模块并行

- 任务 A：2a → 2b，core root/iteration/tool trace 级字段。
- 任务 B：3a → 3b → 3c，visp-llm Anthropic/OpenAI generation 字段。
- 任务 C：5a，visp-agent subagent spawn 字段。

### Wave 3：错误语义收口

4a 与 3b 对齐，确保 tool timeout、LLM error、token limit 的 level/status 语义一致。

### Wave 4：示例文档与全量验证

6a → 全量质量门禁。

## 依赖关系

```text
1a 配置结构
  -> 1b 配置传递
      -> 2a root trace 字段
      -> 3a LLM provider/operation
      -> 5a subagent spawn
          -> 2b 子 span 传播
          -> 3b finish reason/token limit
          -> 3c cache usage
              -> 4a tool observation/error
                  -> 6a 示例文档
                      -> 全量验证
```

## 全量验证标准

最终执行：

```bash
cargo test
cargo clippy -- -D warnings
cargo fmt -- --check
```

通过条件：

1. Langfuse disabled 时无任何 `langfuse.*` 字段。
2. Langfuse enabled 时 root、iteration、LLM、tool、subagent 关键 span 都有 trace 级字段。
3. tags 使用 `langfuse.trace.tags`，不再写 `langfuse.tags`。
4. LLM span 可识别为 generation，并包含 provider、operation、finish reason、usage/cache/cost 字段。
5. tool span 可识别为普通 observation，并包含 tool operation 与错误 level/status。
6. token limit 不标 ERROR；rate limit、API/network error、tool timeout、panic 标 ERROR。
7. Collector 示例表达 visp OTLP/gRPC 到 Langfuse OTLP/HTTP 的链路，且不包含真实 secret。

## 注意事项

- 不改无关文件，尤其避免混入已有工作区无关改动。
- 不在 P0 默认上传 prompt、completion、tool args 或 tool results。
- 不把 Langfuse endpoint/public key/secret key 放入 visp daemon 配置。
- 不改变 `visp-core` 的纯逻辑边界，不向 core 引入 IO。
- 实际 commit 需要用户明确授权后再执行。
