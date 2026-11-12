# Agent 工具化设计

## 1. 目标

将子 Agent 从"通过 `task` 元工具间接调用"改为"作为一等工具直接暴露给 LLM"，降低 LLM 触发子 Agent 的认知门槛，提升子 Agent 使用率。

**一句话总结**：去掉 `task` 工具，每个子 Agent 类型（fixer、explorer、designer 等）作为独立工具注册到 ToolRegistry，`execute()` 内部 spawn 子 Agent 并等待完成，agent_loop 无需任何特殊处理。

## 2. 背景

### 2.1 当前状态

- 子 Agent 通过 `task` 工具调用：`task(subagent_type="fixer", prompt="...", description="...")`
- LLM 需要先学会"调 task 工具 → 选 subagent_type"这个元模式，多一层认知负担
- agent_loop 中通过 `tc.name == "task"` 硬编码拦截，创建 SpawnRequest 发送给 Orchestrator，然后跳过该工具结果、加入 pending_spawns 等待子 Agent 完成
- 子 Agent 完成后，Orchestrator 通过 inbox 发送 `SubAgentComplete` 给父 Agent

### 2.2 问题

- **元工具模式降低触发率**：LLM 在工具列表中看到 "task" 而非 "fixer"/"explorer"，无法直观判断何时该用
- **两层参数传递**：用户先选 subagent_type，再写 prompt，有些工具还有额外参数——信息在两层间传递
- **agent_loop 职责过重**：agent_loop 需要理解 SpawnRequest 结构、管理 pending_spawns、处理 SubAgentComplete 消息——这些与"工具调用"本身无关

### 2.3 设计决策：execute 内部 spawn + 等待

agent 工具与普通工具的本质区别在于"执行方式"而非"调用方式"。对于 LLM 来说，调用 `fixer(prompt="...")` 和调用 `bash(cmd="...")` 没有区别——都是发起一个调用，等待结果返回。

因此，agent 工具的 `execute()` 内部完成 spawn + 等待 + 返回结果，agent_loop 无需区分 agent 工具和普通工具。这和等待一个长时间运行的 bash 命令完成是一样的。

## 3. 架构总览

```
当前（task 元工具，agent_loop 拦截）：
  LLM → task(...) → agent_loop 拦截 → SpawnRequest → Orchestrator → 子Agent → inbox → agent_loop

目标（agent 工具，execute 自包含）：
  LLM → fixer(prompt="...") → tool.execute() → SpawnRequest → Orchestrator → 子Agent
                                                      │
                                                      ▼
                                            oneshot 等待 ← 子Agent 完成
                                                      │
                                                      ▼
                                            ToolResult 返回给 LLM
```

agent_loop 不感知任何 agent 工具特殊逻辑——它只是调用 `tool.execute()` 并等待 `ToolResult`，和对待 bash、grep 完全一样。

## 4. 模块划分

| Crate | 变更 | 类型 |
|-------|------|------|
| **visp-core** | `Tool` trait 新增 `ToolType` 枚举 + `tool_type()` 方法；`ToolContext` 扩展 spawn 所需字段；`SpawnRequest` 新增 `response_tx` 字段；新增各 agent 工具结构体定义 + 公共执行函数（含 Semaphore 并发控制 + tracing span）；`agent_loop` 移除 task 拦截、pending_spawns、Phase 2 select 循环，简化回 `join_all`；`AgentDefinition` 新增 `allowed_sub_agents` 字段；更新 `render_tool_guide()` 指导文本 | 扩展+删除 |
| **visp-agent** | `Orchestrator` 的 `handle_done` 和 `handle_agent_error` 在子 Agent 完成/错误时检查 `response_tx`，通过 oneshot 返回结果；`build_subagent_prompt()` 更新子 Agent 指导文本 | 修改 |
| **visp-daemon** | 注册 agent 工具替代 task 工具；从 AgentRegistry 驱动 agent 工具注册 | 修改 |
| **visp-tools** | 删除 `TaskTool` | 删除 |

## 5. 各模块详细设计

### 5.1 ToolType 枚举（visp-core/src/tool.rs）

新增枚举，区分工具类型：

- `Builtin` — 内置工具（bash、grep、read、write 等）
- `Mcp` — MCP 协议工具
- `Agent` — 子 Agent 工具（fixer、explorer、designer 等）
- `Skill` — 技能工具

`Tool` trait 新增默认方法 `fn tool_type(&self) -> ToolType { ToolType::Builtin }`。

MCP 工具覆盖返回 `Mcp`，agent 工具覆盖返回 `Agent`，skill 工具覆盖返回 `Skill`。

### 5.2 ToolContext 扩展（visp-core/src/tool.rs）

为支持 agent 工具在 `execute()` 中创建 SpawnRequest，ToolContext 需要新增以下字段：

| 字段 | 类型 | 说明 |
|------|------|------|
| `global_tx` | `Option<mpsc::Sender<Envelope>>` | 全局事件总线，发送 SpawnRequest |
| `visp_trace_id` | `Option<String>` | 当前迭代的 trace_id，用于生成 TraceContext |
| `iter_span_w3c_id` | `Option<String>` | 当前迭代的 span_id，用于 TraceContext 的 parent_span_id |

**设计决策**：这些字段用 `Option` 包装，因为单 Agent 模式下不需要。非 multi_agent 模式时这些字段为 `None`。

### 5.3 SpawnRequest 扩展（visp-core/src/agent.rs）

新增 `response_tx` 字段，用于子 Agent 完成时直接返回结果给等待中的 `execute()`：

新增字段：`response_tx: Option<oneshot::Sender<String>>`

- 当 agent 工具调用时设置，Orchestrator 在子 Agent 完成后通过此通道发送结果
- agent 工具始终设置 `Some`；保留 `Option` 仅为类型安全（避免在未初始化时使用）
- 由于 inbox 管道已在 §5.6 中删除，`None` 路径无实际使用者

**设计决策**：采用 `UserQuery` 已有的 `oneshot::Sender` 模式，AgentMessage 本身不要求 Clone。

### 5.4 Agent 工具定义（visp-core/src/agent.rs）

每个子 Agent 类型对应一个工具结构体，实现 `Tool` trait。

**职责**：工具的 `execute()` 方法内完成：
1. 从 `arguments` 中提取 `prompt` 字段
2. 从 `ToolContext` 中获取 `global_tx`、`trace_id` 等
3. 创建 `oneshot::channel()`
4. 构造 `SpawnRequest`（含 `response_tx`）
5. 通过 `global_tx` 发送
6. 等待 `oneshot::receiver`
7. 将结果包装为 `ToolResult` 返回

**参数约定**：

| 参数 | 必填 | 说明 |
|------|------|------|
| `prompt` | 是 | 传给子 Agent 的详细任务描述（目标+上下文+约束+期望输出） |
| `task_id` | 否 | 可选的追踪 ID |
| 各工具特有参数 | 否 | 如 `fixer` 有 `files`、`explorer` 有 `query` 等 |

**示例参数设计**：

- `fixer`：`prompt`（必填）、`files`（可选，建议关注的文件列表）
- `explorer`：`prompt`（必填）、`query`（可选，搜索关键词）
- `designer`：`prompt`（必填）、`component`（可选，目标组件名）
- `oracle`：`prompt`（必填）
- `librarian`：`prompt`（必填）、`library`（可选，目标库名）

**参数映射**：

| SpawnRequest 字段 | 来源 |
|------|------|
| `subagent_type` | 工具名（`self.name()`） |
| `description` | 工具名（自动填充，作为短标签） |
| `prompt` | 工具参数 `prompt` 字段 |
| `task_id` | 工具参数 `task_id` 字段（可选） |
| `response_tx` | execute() 内部创建的 oneshot sender |

**额外参数处理**：各 agent 工具可能有额外参数（如 `fixer` 有 `files`、`explorer` 有 `query`），这些参数直接拼入 `prompt` 文本传给子 Agent，格式如下：

```
<原始 prompt 内容>

<参数名>:
- <值1>
- <值2>
```

例如 `fixer(prompt="修复 login 函数的 bug", files=["src/auth/login.rs", "tests/auth_test.rs"])` 最终 prompt 为：

```
修复 login 函数的 bug

相关文件:
- src/auth/login.rs
- tests/auth_test.rs
```

子 Agent 看到的就是一段完整任务描述，无需在 SpawnRequest 协议层新增字段。

### 5.5 公共执行函数（visp-core/src/agent.rs）

各 agent 工具（fixer、explorer、designer、oracle、librarian 等）的 `execute()` 核心逻辑完全相同：提取参数 → 拼 prompt → 构造 SpawnRequest → 发送 → 等待 → 返回 ToolResult。

为避免 5 份重复代码，在 `visp-core` 中抽取一个公共函数：

```
async fn execute_agent_spawn(
    name: &str,
    arguments: Value,
    context: &ToolContext,
) -> ToolResult
```

各 agent 工具的 `execute()` 只需一行调用此函数。参数 schema（`name()`、`description()`、`parameters()`）由各工具独立定义，`execute()` 统一走公共函数。

**并发控制**：`execute_agent_spawn` 内部通过 `tokio::sync::Semaphore` 限制同时运行的 agent 工具数量。上限通过配置文件可配置，默认值建议为 2。防止 LLM 在一次工具调用中并发启动过多子 Agent 导致资源爆炸。

**错误前缀**：子 Agent 失败时返回的错误内容前加 `[SubAgent Error]` 前缀，让 LLM 明确识别这是子 Agent 级错误而非工具参数错误，避免无效重试。

**可观测性**：`execute_agent_spawn` 入口/出口打 tracing span（如 `agent_spawn.fixer`），span 内包含子 Agent 的 session_id 和耗时，确保父 Agent 的 tracing 中能看到子 Agent 调用。

### 5.6 agent_loop 简化（visp-core/src/agent_loop.rs）

**移除的逻辑**：
- task 工具拦截分支（`if is_multi_agent && tc.name == "task"`）
- SpawnRequest 构造逻辑
- `pending_spawns` 管理
- `SubAgentComplete` 消息处理
- `execute_tool_calls` 中约 200 行的 Phase 2 select 循环（多 Agent 收集逻辑），简化回普通的 `join_all` 模式

**保留**：
- `OrchestratorMessage` 枚举定义和 inbox 管道基础设施（`mpsc` channel），标记 `#[allow(dead_code)]`，注释说明预留给未来流式子 Agent 通信。仅删除 agent_loop 中的消费端，不删除整个 inbox 概念
- agent_loop 正常调用 `tool.execute(args, context)`，无论工具类型

**注意**：agent 工具的 `execute()` 是阻塞等待（await oneshot），这意味着该工具调用会阻塞当前 tokio task。agent_loop 的并行工具执行机制（`tokio::spawn` 每个工具调用）确保其他工具调用不受影响。

**取消路径**：父 Agent 被取消时，`tokio::spawn` 中的 agent 工具 execute() 被 abort，oneshot receiver 被 drop。Orchestrator 后续 `response_tx.send()` 会返回 `Err`（receiver 已消失），需要静默处理。这与当前普通工具调用被 abort 的行为一致，无需特殊处理。

### 5.7 Orchestrator 变更（visp-agent/src/orchestrator.rs）

**`spawn_sub_agent` 签名**：新增 `response_tx` 参数。

**`handle_done` 变更**：子 Agent 正常完成时，检查 `response_tx`：
- 如果 `response_tx` 为 `Some`，通过 oneshot 发送结果（agent 工具路径）
- 无需 `None` fallback——inbox 管道已删除，`response_tx` 始终为 `Some`

**`handle_agent_error` 变更**：子 Agent 错误退出时，同样检查 `response_tx`：
- 如果 `response_tx` 为 `Some`，通过 oneshot 发送错误信息（确保 agent 工具 execute() 中的 rx.await 不会被永久挂起）
- 无需 `None` fallback——同上

**注意**：`response_tx` 不存入 `ActiveAgent`（`oneshot::Sender` 不实现 Clone，会破坏 `ActiveAgent` 的 `#[derive(Clone)]`）。改为在 Orchestrator 中维护独立的 `HashMap<String, oneshot::Sender<String>>`（key = session_id），`spawn_sub_agent` 时插入，`handle_done` / `handle_agent_error` 时取出并 send。Orchestrator 主循环 `&mut self` 保证单线程访问，无需额外同步。

### 5.8 Agent 工具注册（visp-daemon/src/main.rs）

**变更前**：手动注册 `TaskTool`。

**变更后**：从 `AgentRegistry` 中遍历 `list_subagents()`，为每个 subagent 创建对应的 agent 工具并注册到 `ToolRegistry`。

注册逻辑：
1. 遍历 `agent_registry.list_subagents()`
2. 为每个 AgentDefinition 创建对应的 agent 工具结构体
3. 工具名称与 AgentDefinition.name 一致
4. 工具的 `description` 和 `parameters` 由 AgentDefinition 决定
5. 注册到 `ToolRegistry`，在 `seal_core_tools()` 之前完成

**单 Agent 模式过滤**：注册 agent 工具时检查 `global_tx.is_some()`（agent 工具需要 `global_tx` 来发送 SpawnRequest）。单 Agent 模式（测试/standalone）下 `global_tx` 为 `None`，跳过 agent 工具注册，LLM 工具列表中不会出现 agent 工具，自然无法调用。

### 5.9 子 Agent 工具筛选

子 Agent 继承同一 `tool_registry`，能看到所有 agent 工具。为防止不必要的递归委托（如 explorer 调用 fixer），在 `spawn_sub_agent` 时为子 Agent 按类型筛选可用的 agent 工具：

**筛选规则**：默认不授予任何 agent 工具。仅当 AgentDefinition 中显式声明了 `allowed_sub_agents` 列表时，才授予列表中指定的 agent 工具。

**AgentDefinition 扩展**：新增 `allowed_sub_agents: Vec<String>` 字段（默认空列表），列出该 Agent 可调用的子 Agent 类型名称。

**示例**：
- `oracle` 的 `allowed_sub_agents: []` — 不能调用任何 agent 工具
- `explorer` 的 `allowed_sub_agents: []` — 不能调用任何 agent 工具
- `fixer` 的 `allowed_sub_agents: []` — 不能调用任何 agent 工具（fixer 的 system prompt 明确说"不委托"）

子 Agent 的工具注册时，只注册 `allowed_sub_agents` 中列出的 agent 工具，其余 agent 工具不可见。`compute_depth` 检查（max_depth=5）仍作为硬性限制保留。

### 5.10 删除 TaskTool（visp-tools/src/task.rs）

直接删除整个文件，移除 daemon 中的注册代码。

### 5.11 System Prompt 更新

更新关于子 Agent 调用的指导：

- 不再提 "task" 工具
- 直接说明各 agent 工具的名称和用途
- 保留 prompt 编写要求（自包含、包含上下文、不转发原始请求）

**具体修改位置**：

| 文件 | 位置 | 变更 |
|------|------|------|
| `visp-core/src/agent.rs` | `render_tool_guide()` | 当前文本 "delegate to the appropriate sub-agent via the `task` tool" → 改为直接列出各 agent 工具 |
| `visp-agent/src/orchestrator.rs` | `build_subagent_prompt()` | 当前生成 "Available sub-agents (use via the `task` tool)" 文本 → 改为不再提 `task`；同时确保子 Agent 的 system prompt 正确理解可直接调用其他 agent 工具（受深度限制约束） |

## 6. 依赖关系

```
AgentRegistry（AgentDefinition 列表）
    │
    ▼
visp-daemon（注册时遍历 subagents，创建 agent 工具）
    │
    ▼
ToolRegistry（agent 工具与其他工具共存）
    │
    ▼
agent_loop（正常调用 tool.execute()，无特殊处理）
    │
    ├── tool.execute() 内部：
    │     ├── 创建 SpawnRequest（含 response_tx）
    │     ├── global_tx.send()
    │     └── await oneshot → ToolResult
    │
    ▼
Orchestrator（spawn_sub_agent + handle_done 通过 response_tx 返回结果）
```

## 7. 核心数据流

```
LLM 决定调用子 Agent
    │
    ▼
调用 fixer(prompt="修改 foo.rs 的 bug")
    │
    ▼
agent_loop::execute_tool_calls()
    │
    ├── 正常调用 tool.execute(arguments, context)
    │     （不区分 agent 工具和普通工具）
    │
    ▼
fixer.execute(arguments, context):
    │
    ├── 1. 提取 prompt = "修改 foo.rs 的 bug"
    ├── 2. 创建 oneshot::channel()
    ├── 3. 构造 SpawnRequest {
    │       subagent_type: "fixer",
    │       description: "fixer",
    │       prompt: "修改 foo.rs 的 bug",
    │       response_tx: Some(tx),
    │       ...
    │   }
    ├── 4. context.global_tx.send(Envelope { SpawnRequest })
    ├── 5. await rx → 等待子 Agent 完成
    └── 6. 返回 ToolResult::success(content)
    │
    ▼
Orchestrator::handle_agent_message() → spawn_sub_agent()
    │
    ├── 用 prompt 作为子 Agent 的首条消息
    ├── 子 Agent 独立运行 run_agent_loop
    │
    └── handle_done():
          ├── 提取子 Agent 结果
          ├── response_tx.send(content) → 唤醒 execute() 中的 await
          └── 清理 session
```

**与当前流程的差异**：

| 步骤 | 当前 | 目标 |
|------|------|------|
| 工具名 | `task` | `fixer` / `explorer` / ... |
| subagent_type 来源 | 工具参数 | 工具名（`self.name()`） |
| description 来源 | 工具参数 | 工具名（自动填充） |
| spawn 逻辑位置 | agent_loop 拦截 | 工具 execute() 内部 |
| 结果返回 | SubAgentComplete → inbox | oneshot 直接返回 |
| agent_loop 是否感知 | 是（拦截+pending_spawns） | 否（正常工具调用） |
| inbox 管道 | 存在（约 200 行 select 循环） | 删除（简化为 join_all） |
| 拦截条件 | `tc.name == "task"` | 无需拦截 |

## 8. 不做什么

- 不改变 AgentDefinition 的字段定义（除新增 `allowed_sub_agents` 字段外）
- 不改变子 Agent 的权限继承、深度限制、模型覆盖等逻辑
- 不改变 TraceContext 跨 mpsc 传播机制
- 不改变 AgentRegistry 的注册和查询方式
- 不改变子 Agent 的 run_agent_loop 内部逻辑
- 不改变其他普通工具（bash、grep 等）的任何行为

## 9. 验收标准

1. `task` 工具完全移除，所有相关代码删除
2. 每个子 Agent 类型有对应的工具注册到 ToolRegistry，LLM 可直接看到 `fixer`、`explorer` 等
3. 调用 agent 工具时，`execute()` 内部正确创建 SpawnRequest 并发送
4. 子 Agent 完成时，结果通过 oneshot 返回给 `execute()`，`execute()` 返回正确 ToolResult
5. agent_loop 不再包含任何 task 拦截或 pending_spawns 逻辑
6. 子 Agent 收到正确的首条消息（prompt 内容）
7. 所有现有测试通过
8. agent 工具新增测试：覆盖 execute() 的 spawn + 等待 + 返回流程
9. ToolType 枚举测试覆盖所有变体
10. 并发 Semaphore 测试：验证超过上限时阻塞等待
11. 子 Agent 工具筛选测试：验证无 `allowed_sub_agents` 的子 Agent 看不到 agent 工具