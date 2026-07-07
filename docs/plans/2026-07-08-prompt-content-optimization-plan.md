# DEFAULT_SYSTEM_PROMPT 内容质量优化 — 实施计划

> 对应设计文档：`docs/design/visp-design-prompt-content-optimization.md`
> 实施文件：`crates/visp-core/src/prompt.rs`
> 日期：2026-07-08

## 任务概述

将 `DEFAULT_SYSTEM_PROMPT` 从当前 173 行（~1000 tokens）重写为约 75 行（~560 tokens），5 段结构。解决设计文档指出的 6 个严重问题和 5 个建议改进。

## 涉及范围

| 文件 | 改动类型 |
|------|---------|
| `crates/visp-core/src/prompt.rs` (89-261) | 重写常量 + 更新/新增测试 |
| `crates/visp-core/src/prompt.rs` (268-576) | 更新/新增测试 |

不涉及：`USER_QUERY_INSTRUCTION` 常量、`PromptBuilder::build` 方法、orchestrator.rs。

## 测试策略（TDD 红-绿）

### 红：先更新测试

| 测试 | 动作 | 验证内容 |
|------|------|---------|
| `test_default_prompt_contains_role` | 保留 | 包含 "visp" |
| `test_default_prompt_no_project_specific_content` | 保留 | 不含 "Conventional Commits"、"简洁优先"、"TDD" |
| `test_default_prompt_no_hardcoded_tools` | 保留 | 不含 "ReadFile"、"Bash" |
| `test_default_prompt_contains_interaction_rules` | **更新** | 拆分为新结构对应的测试（见下方新增） |
| `test_default_prompt_has_core_principle` | **新增** | 包含 "Core Principle" 段 + 优先级声明 + 委托触发条件 |
| `test_default_prompt_has_execution_workflow` | **新增** | 包含 "Execution Workflow" 段 + Plan/Dispatch/Verify/Failure 子节 |
| `test_default_prompt_has_code_quality` | **新增** | 包含 "Code Quality" 段 |
| `test_default_prompt_has_communication` | **新增** | 包含 "Communication" 段 |
| `test_default_prompt_has_constraints` | **新增** | 包含 "Constraints" 段 |
| `test_default_prompt_no_contradictions` | **新增** | 不含 "Record task IDs"（不可执行指令）、不含 "Result Contract"（已移除段落） |
| `test_default_prompt_contains_user_query_ref` | **新增** | 包含 "[USER_QUERY]" 引用 + 指向详细说明 |

### 绿：重写 DEFAULT_SYSTEM_PROMPT

按设计文档第 8 节结构大纲实现 5 段内容：

1. **I. Core Principle** — 角色声明 + 优先级 + 委托触发条件 + 主 Agent 职责
2. **II. Execution Workflow** — 探索停止规则 + 计划路由 + 派发格式 + 整合验证 + 失败恢复
3. **III. Code Quality** — 最小代码 + 简洁优先 + 含测试 + 验证命令
4. **IV. Communication** — 简洁规则 + 反推格式 + 不恭维 + 何时询问
5. **V. Constraints** — 引用方式 + 不重复 + token 限制

### 验收标准（来自设计文档第 11 节）

1. **token 数下降**：主 prompt 从 ~1000 tokens 降至 ~560 tokens
2. **指令矛盾消除**：不存在互斥指令
3. **可执行性提升**：每条指令都是模型可照做的行为指令
4. **测试通过**：`cargo test -p visp-core` 全量通过
5. **Clippy 零警告**：`cargo clippy -- -D warnings`
6. **格式检查通过**：`cargo fmt -- --check`

## 执行步骤

### 步骤 1：更新测试（红）
- 更新 `test_default_prompt_contains_interaction_rules` 适配新结构
- 新增 7 个测试覆盖新 prompt 的各段落和约束
- 运行 `cargo test -p visp-core` — 预期新测试 FAIL

### 步骤 2：重写 DEFAULT_SYSTEM_PROMPT（绿）
- 按设计文档第 8 节大纲 + 第 5 节逐条改写方案撰写新 prompt
- 保留 USER_QUERY 引用（功能依赖）
- 保留工具调用规则（交互基础）
- 运行 `cargo test -p visp-core` — 预期全部 PASS

### 步骤 3：质量门
- `cargo test -p visp-core`
- `cargo clippy -- -D warnings`
- `cargo fmt -- --check`

### 步骤 4：提交
- `git commit -m "refactor(visp-core): optimize DEFAULT_SYSTEM_PROMPT content quality"`
