# visp 工作计划：Agent 工具化

## 概述

将子 Agent 从 `task` 元工具改为一等工具直接暴露给 LLM。核心变更：agent 工具 `execute()` 内部 spawn 子 Agent 并等待完成，agent_loop 移除 task 拦截和 pending_spawns 逻辑。

设计文档：`docs/design/visp-design-agent-tools.md`

---

## Wave 1：基础类型定义（串行）

**依赖**：无。后续所有 Wave 依赖此 Wave。

### 1a：ToolType 枚举 + tool_type() 方法

**涉及文件**：`crates/visp-core/src/tool.rs`

#### 测试

| # | 测试用例 | 描述 |
|---|---------|------|
| 1 | `test_tool_type_default_is_builtin` | 普通工具 tool_type() 默认返回 Builtin |
| 2 | `test_tool_type_mcp_returns_mcp` | MCP 工具覆盖返回 Mcp |
| 3 | `test_tool_type_agent_returns_agent` | Agent 工具覆盖返回 Agent |
| 4 | `test_tool_type_skill_returns_skill` | Skill 工具覆盖返回 Skill |

#### 实现

- 新增 `ToolType` 枚举（Builtin/Mcp/Agent/Skill）
- `Tool` trait 新增 `fn tool_type(&self) -> ToolType { ToolType::Builtin }`
- 现有 MCP 工具覆盖返回 `Mcp`

#### 验证

`cargo test -p visp-core -- tool_type && cargo clippy -p visp-core -- -D warnings`

#### 提交

`feat(visp-core): add ToolType enum and tool_type() method to Tool trait`

---

### 1b：ToolContext 扩展

**涉及文件**：`crates/visp-core/src/tool.rs`

#### 测试

| # | 测试用例 | 描述 |
|---|---------|------|
| 1 | `test_tool_context_global_tx_none_by_default` | 默认构造时 global_tx 为 None |
| 2 | `test_tool_context_trace_fields_none_by_default` | visp_trace_id 和 iter_span_w3c_id 默认为 None |

#### 实现

- `ToolContext` 新增字段：`global_tx: Option<mpsc::Sender<Envelope>>`、`visp_trace_id: Option<String>`、`iter_span_w3c_id: Option<String>`
- 更新所有构造 `ToolContext` 的地方（agent_loop 传入 `Some` 值）

#### 验证

`cargo test -p visp-core && cargo clippy -- -D warnings`

#### 提交

`feat(visp-core): extend ToolContext with spawn-related fields`
---

### 1c：SpawnRequest 扩展

**涉及文件**：`crates/visp-core/src/agent.rs`

#### 测试

| # | 测试用例 | 描述 |
|---|---------|------|
| 1 | `test_spawn_request_response_tx_default_none` | SpawnRequest 构造时 response_tx 默认为 None |
| 2 | `test_spawn_request_with_response_tx` | 设置 response_tx 后正确持有 |

#### 实现

- `SpawnRequest` 新增字段：`response_tx: Option<oneshot::Sender<String>>`

#### 验证

`cargo test -p visp-core -- spawn_request && cargo clippy -p visp-core -- -D warnings`

#### 提交

`feat(visp-core): add response_tx field to SpawnRequest`

---

### 1d：AgentDefinition 扩展

**涉及文件**：`crates/visp-core/src/agent_definition.rs`

#### 测试

| # | 测试用例 | 描述 |
|---|---------|------|
| 1 | `test_agent_definition_allowed_sub_agents_default_empty` | 默认 allowed_sub_agents 为空列表 |
| 2 | `test_agent_definition_with_allowed_sub_agents` | 设置 allowed_sub_agents 后正确持有 |

#### 实现

- `AgentDefinition` 新增字段：`allowed_sub_agents: Vec<String>`（默认空列表）

#### 验证

`cargo test -p visp-core -- agent_definition && cargo clippy -p visp-core -- -D warnings`

#### 提交

`feat(visp-core): add allowed_sub_agents field to AgentDefinition`

---

## Wave 2：核心实现（并行）

**依赖**：Wave 1 全部完成。

### 2a：agent 工具结构体定义 + execute_agent_spawn

**涉及文件**：`crates/visp-core/src/agent.rs`

#### 测试

| # | 测试用例 | 描述 |
|---|---------|------|
| 1 | `test_fixer_tool_name` | FixerTool.name() 返回 "fixer" |
| 2 | `test_fixer_tool_parameters` | 参数 schema 包含必填 prompt 和可选 files |
| 3 | `test_explorer_tool_name` | ExplorerTool.name() 返回 "explorer" |
| 4 | `test_explorer_tool_parameters` | 参数 schema 包含必填 prompt 和可选 query |
| 5 | `test_all_agent_tools_tool_type_is_agent` | 所有 agent 工具的 tool_type() 返回 Agent |
| 6 | `test_execute_agent_spawn_no_global_tx` | global_tx 为 None 时返回错误 |
| 7 | `test_execute_agent_spawn_sends_request` | 正常调用时发送 SpawnRequest 到 global_tx |
| 8 | `test_execute_agent_spawn_waits_for_response` | 等待 oneshot 响应并返回 ToolResult |
| 9 | `test_execute_agent_spawn_error_prefix` | 子 Agent 错误时返回内容以 [SubAgent Error] 开头 |
| 10 | `test_execute_agent_spawn_semaphore_limit` | 并发超过 Semaphore 上限时阻塞等待 |
| 11 | `test_execute_agent_spawn_extra_params_appended` | 额外参数（如 files）拼入 prompt 文本 |

#### 实现

- 定义 `FixerTool`、`ExplorerTool`、`DesignerTool`、`OracleTool`、`LibrarianTool` 结构体
- 每个结构体实现 `Tool` trait（name、description、parameters、execute、category、tool_type）
- 各工具的 `execute()` 委托给公共函数 `execute_agent_spawn`
- `execute_agent_spawn`：提取参数 → 拼 prompt（含额外参数） → 构造 SpawnRequest → 获取 Semaphore permit → 发送 → await oneshot → 返回 ToolResult
- Semaphore 上限通过配置文件可配置，默认 2
- 入口/出口 tracing span（`agent_spawn.{name}`）
- 错误前缀 `[SubAgent Error]`

#### 验证

`cargo test -p visp-core -- agent && cargo clippy -- -D warnings`

#### 提交

`feat(visp-core): add agent tool structs and execute_agent_spawn with semaphore`

---

### 2b：Orchestrator 变更

**涉及文件**：`crates/visp-agent/src/orchestrator.rs`

#### 测试

| # | 测试用例 | 描述 |
|---|---------|------|
| 1 | `test_spawn_sub_agent_with_response_tx` | 传入 response_tx 时，spawn 后 pending_responses 包含对应条目 |
| 2 | `test_handle_done_response_tx_some` | 子 Agent 完成时，通过 response_tx 发送结果 |
| 3 | `test_handle_done_response_tx_none` | response_tx 为 None 时走原有 inbox 路径 |
| 4 | `test_handle_agent_error_response_tx_some` | 子 Agent 错误时，通过 response_tx 发送错误信息 |
| 5 | `test_handle_agent_error_response_tx_none` | response_tx 为 None 时走原有 inbox 路径 |
| 6 | `test_pending_responses_cleanup_on_done` | handle_done 后 pending_responses 中条目被移除 |
| 7 | `test_pending_responses_cleanup_on_error` | handle_agent_error 后 pending_responses 中条目被移除 |

#### 实现

- Orchestrator 新增 `pending_responses: HashMap<String, oneshot::Sender<String>>` 字段
- `spawn_sub_agent` 新增 `response_tx: Option<oneshot::Sender<String>>` 参数，插入 `pending_responses`
- `handle_done`：检查 `pending_responses.remove(session_id)`，有则 send，无则走 inbox
- `handle_agent_error`：同上

#### 验证

`cargo test -p visp-agent && cargo clippy -p visp-agent -- -D warnings`

#### 提交

`feat(visp-agent): add oneshot response path to Orchestrator handle_done/error`

---

### 2c：agent_loop 简化

**涉及文件**：`crates/visp-core/src/agent_loop.rs`

#### 测试

| # | 测试用例 | 描述 |
|---|---------|------|
| 1 | `test_agent_tool_execute_via_agent_loop` | agent 工具通过 agent_loop 正常调用，不再被拦截 |
| 2 | `test_no_task_interception` | agent_loop 不再检查 tool.name == "task" |
| 3 | `test_join_all_replaces_phase2` | 工具结果收集使用 join_all 而非 Phase 2 select 循环 |
| 4 | `test_cancel_aborts_agent_tool` | 取消时 agent 工具 execute() 被 abort，不挂起 |

#### 实现

- 移除 task 工具拦截分支（`if is_multi_agent && tc.name == "task"`）
- 移除 `pending_spawns` 管理和 `SpawningStatus` 相关变量
- 移除 Phase 2 select 循环（约 200 行），简化为 `join_all`
- 移除 `AgentLoopContext.inbox_rx` 字段
- **保留** `OrchestratorMessage` 枚举定义和 channel 基础设施，标记 `#[allow(dead_code)]`

#### 验证

`cargo test -p visp-core && cargo clippy -- -D warnings`

#### 提交

`refactor(visp-core): remove task interception and pending_spawns from agent_loop`

---

## Wave 3：集成（并行）

**依赖**：Wave 2 全部完成。

### 3a：子 Agent 工具筛选

**涉及文件**：`crates/visp-agent/src/orchestrator.rs`

#### 测试

| # | 测试用例 | 描述 |
|---|---------|------|
| 1 | `test_sub_agent_no_allowed_sub_agents` | allowed_sub_agents 为空时，子 Agent 看不到任何 agent 工具 |
| 2 | `test_sub_agent_with_allowed_sub_agents` | allowed_sub_agents 有值时，子 Agent 只看到列表中指定的 agent 工具 |
| 3 | `test_sub_agent_depth_limit_still_applies` | compute_depth 检查在工具筛选后仍生效 |

#### 实现

- `spawn_sub_agent` 中根据 `AgentDefinition.allowed_sub_agents` 筛选子 Agent 的工具列表
- 筛选逻辑：遍历工具列表，仅保留非 agent 工具 + allowed_sub_agents 中列出的 agent 工具

#### 验证

`cargo test -p visp-agent && cargo clippy -p visp-agent -- -D warnings`

#### 提交

`feat(visp-agent): filter agent tools for sub-agents based on allowed_sub_agents`

---

### 3b：daemon 工具注册

**涉及文件**：`crates/visp-daemon/src/main.rs`

#### 测试

| # | 测试用例 | 描述 |
|---|---------|------|
| 1 | `test_agent_tools_registered_from_registry` | 从 AgentRegistry 遍历 subagents，正确注册 agent 工具 |
| 2 | `test_agent_tools_not_registered_in_single_agent_mode` | 单 Agent 模式（global_tx 为 None）下不注册 agent 工具 |
| 3 | `test_agent_tool_names_match_agent_definition` | 注册的工具名称与 AgentDefinition.name 一致 |

#### 实现

- 移除 `TaskTool` 注册代码
- 遍历 `agent_registry.list_subagents()`，为每个 subagent 创建对应的 agent 工具
- 注册到 ToolRegistry，在 `seal_core_tools()` 之前
- 单 Agent 模式（`global_tx.is_none()`）时跳过 agent 工具注册

#### 验证

`cargo test -p visp-daemon && cargo clippy -p visp-daemon -- -D warnings`

#### 提交

`feat(visp-daemon): register agent tools from AgentRegistry, remove TaskTool`

---

### 3c：删除 TaskTool

**涉及文件**：`crates/visp-tools/src/task.rs`（删除）、`crates/visp-tools/src/lib.rs`（移除 mod 声明）

#### 实现

- 删除 `crates/visp-tools/src/task.rs`
- 移除 `lib.rs` 中的 `pub mod task;` 声明
- 移除 daemon 中的 `use visp_tools::task::TaskTool;`（3b 中已处理）

#### 验证

`cargo test && cargo clippy -- -D warnings`

#### 提交

`refactor(visp-tools): remove TaskTool (replaced by agent tools)`

---

## Wave 4：收尾（并行）

**依赖**：Wave 3 全部完成。

### 4a：System Prompt 更新

**涉及文件**：`crates/visp-core/src/agent.rs`（`render_tool_guide`）、`crates/visp-agent/src/orchestrator.rs`（`build_subagent_prompt`）

#### 测试

| # | 测试用例 | 描述 |
|---|---------|------|
| 1 | `test_render_tool_guide_no_task_reference` | render_tool_guide 输出不再包含 "task" 工具引用 |
| 2 | `test_render_tool_guide_lists_agent_tools` | render_tool_guide 直接列出各 agent 工具名称 |
| 3 | `test_build_subagent_prompt_no_task_reference` | build_subagent_prompt 不再提 "task" 工具 |
| 4 | `test_build_subagent_prompt_agent_tools_guidance` | 子 Agent prompt 包含 agent 工具的正确使用指导 |

#### 实现

- `render_tool_guide()`：当前 "delegate via task tool" → 改为直接列出各 agent 工具
- `build_subagent_prompt()`：当前 "use via the task tool" → 改为 agent 工具指导

#### 验证

`cargo test && cargo clippy -- -D warnings`

#### 提交

`docs(visp): update system prompts to replace task tool with agent tools`

---

### 4b：全量回归

**涉及文件**：所有修改过的文件

#### 测试

| # | 测试用例 | 描述 |
|---|---------|------|
| 1 | 现有测试全量回归 | 所有 cargo test 通过 |
| 2 | `test_end_to_end_agent_tool_spawn` | 端到端：agent 工具调用 → spawn 子 Agent → 完成 → 结果返回 |

#### 实现

- 运行全量测试，修复回归问题
- 新增端到端集成测试

#### 验证

`cargo test && cargo clippy -- -D warnings && cargo fmt -- --check`

#### 提交

`test(visp): add end-to-end test for agent tool spawn flow`

---

## Wave 并行策略

```
Wave 1（串行，4 个步骤）
  1a → 1b → 1c → 1d

Wave 2（并行，3 个任务）
  2a（agent 工具 + execute_agent_spawn）
  2b（Orchestrator 变更）
  2c（agent_loop 简化）

Wave 3（并行，3 个任务）
  3a（子 Agent 工具筛选）
  3b（daemon 工具注册）
  3c（删除 TaskTool）

Wave 4（并行，2 个任务）
  4a（System Prompt 更新）
  4b（全量回归）
```

## 依赖关系总览

```
1a → 1b → 1c → 1d
                │
     ┌──────────┼──────────┐
     ▼          ▼          ▼
    2a          2b         2c
     │          │          │
     └──────────┼──────────┘
                │
     ┌──────────┼──────────┐
     ▼          ▼          ▼
    3a          3b         3c
     │          │          │
     └──────────┼──────────┘
                │
        ┌───────┴───────┐
        ▼               ▼
       4a              4b
```

## 测试覆盖汇总

| Wave | 并行数 | 步骤 | 新增测试 | 删除测试 |
|------|--------|------|---------|---------|
| 1 | 1 | 4 | 8 | 0 |
| 2 | 3 | 3 | 22 | 0 |
| 3 | 3 | 3 | 6 | 7 |
| 4 | 2 | 2 | 6 | 0 |
| **合计** | — | **12** | **42** | **7** |

## 备注

- 运行时环境：Rust stable，cargo workspace
- 所有步骤必须跑通 `cargo test && cargo clippy -- -D warnings` 才能提交
- Wave 2 的 3 个任务可并行开发（3 个 @fixer 同时工作）
- Wave 3 的 3 个任务可并行开发
- Wave 4 的 2 个任务可并行开发
- 3c 删除 TaskTool 后，原来用到 TaskTool 的测试（约 7 个）需一并删除
