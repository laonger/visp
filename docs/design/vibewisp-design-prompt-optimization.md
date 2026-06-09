# System Prompt 与工具描述优化设计

## 1. 目标

当前 vibewisp 的默认 system prompt 只有一句 `"You are vibewisp, a lightweight AI coding assistant running on a Rust backend."`，LLM 对自己的能力、所在环境、可用工具、交互规范几乎一无所知。这导致：

- **理解能力弱**：LLM 不知道项目结构、语言、框架，回答缺乏上下文
- **产出质量低**：没有编码规范指导、没有输出格式约束
- **工具使用差**：不知道什么时候该用什么工具、参数怎么填

参照 OpenCode 的 system context 架构，优化 system prompt 的**内容密度**和**结构清晰度**，在不改动架构的前提下最大提升 LLM 能力。

## 2. 改动范围

| 层次 | 改动内容 |
|------|---------|
| **vbw-core** | 重写默认 system prompt 模板（`DEFAULT_SYSTEM_PROMPT`） |
| **vbw-core** | PromptBuilder 构建时注入更多运行时上下文（环境、日期、项目信息） |
| **vbw-core** | 改进各 Tool 的 `description()` 和 `parameters()` 输出 |
| **vbw-core** | `[USER_QUERY]` 指令格式优化 |
| **vbw-proto** | 无改动 |
| **vbw-daemon** | 无改动 |
| **vbw-cli** | 无改动 |

## 3. 模块详细设计

### 3.1 默认 System Prompt 重写

当前：静态常量 `DEFAULT_SYSTEM_PROMPT`。

改造后：仍然是一个静态常量字符串，但内容大幅扩展。包含以下区块：

#### 角色定义
- 名称：vibewisp
- 定位：轻量级 AI 编程助手
- 后端技术栈：Rust + gRPC

#### 编码规范指南
- 简洁优先：只写最少代码，不做预防性设计
- 手术刀式修改：只改必须改的部分
- TDD 流程：红 → 绿 → 测试 → 类型检查 → 重构 → 提交
- 命名风格：变量/函数简洁语义明确
- 错误处理：使用 `thiserror`，返回清晰错误信息
- 提交格式：Conventional Commits

#### 可用工具列表
每个工具列出：名称、用途、使用场景、重要参数说明。按使用频率分组：

**常用工具**：
- ReadFile / WriteFile / EditFile：文件操作
- Bash：执行 shell 命令（含安全限制说明）
- Grep / Glob：搜索代码和文件

**低频工具**：
- CodeGraphSearch / CodeGraphGetDetails：代码智能搜索
- WebFetch：获取网页内容

#### 交互规范
- 工具调用后必须等待结果
- 一个回复里可以同时调用多个工具（并行执行）
- 需要用户确认时会弹出确认栏
- 可通过 `[USER_QUERY]` 向用户提问

#### 当前上下文
- 当前日期
- 工作目录
- 项目语言（如已知）
- 可用技能列表（从 `.vibewisp/skills/` 加载）

### 3.2 运行时上下文注入

在 `PromptBuilder::build()` 中，除了拼接 system_template + rules + USER_QUERY_INSTRUCTION 外，还应在 system content 末尾注入运行时上下文：

```
## 当前环境

日期：{current_date}
工作目录：{working_dir}
```

来源：
- 日期：`chrono::Local::now().format("%Y-%m-%d")`（或 `std::time`）
- 工作目录：从 `AgentLoopContext.working_dir` 获取
- 技能：已从 `load_skills()` 加载到 system_prompt_template 中

### 3.3 工具描述优化

每个 Tool 的 `description()` 应改为多句描述，包含：

| 工具 | 当前描述 | 优化方向 |
|------|---------|---------|
| ReadFile | 简短一句话 | 说明用途、文件大小限制、二进制检测行为 |
| WriteFile | 简短一句话 | 说明支持自动创建父目录、路径安全机制 |
| EditFile | 简短一句话 | 说明精确字符串替换、原子写入、多匹配拒绝规则 |
| Bash | 简短一句话 | 说明执行环境、超时控制、危险命令黑名单 |
| Grep | 简短一句话 | 说明支持正则、优先 ripgrep、排除二进制文件 |
| Glob | 简短一句话 | 说明文件名通配符搜索、递归行为 |
| CodeGraphSearch | 简短一句话 | 说明 AST 符号搜索、适用范围 |
| CodeGraphGetDetails | 简短一句话 | 说明返回调用链信息 |
| WebFetch | 简短一句话 | 说明 URL 获取、内容提取 |

每个描述应包含：
- 工具做什么
- 适合什么场景
- 不适合什么场景
- 重要参数说明
- 安全/限制信息

### 3.4 `[USER_QUERY]` 指令优化

当前指令：
```
When you need the user to make a choice, append the following at the end of your response:
[USER_QUERY]
...
```

优化方向：
- 添加使用示例，展示多选项的格式
- 强调**只在输出末尾**使用此标记
- 说明 `allow_other=true` 的效果
- 提示不要滥用（只在确实需要用户决策时使用）

## 4. 影响范围

- **默认 system prompt** 改动影响所有使用默认模板的会话。如果项目有自定义 `.vibewisp/system-prompt.md`，则不受影响（自定义模板优先级更高）。
- **工具描述** 改动影响 LLM 看到的工具定义，不需要改协议。
- **运行时上下文注入** 需要修改 `PromptBuilder::build()` 签名，增加 `working_dir` 参数。

## 5. 不做什么

- ❌ 不改动提示组合架构（保持当前的 `build(system_template, rules, history)` 签名）
- ❌ 不引入持久化上下文纪元（OpenCode 的 context epoch 机制）
- ❌ 不增加多代理支持
- ❌ 不改工具注册/执行流程，只改描述文本

## 6. 验收标准

1. **角色清晰**：LLM 知道自己是 vibewisp，后端是 Rust
2. **工具使用准确**：LLM 能准确选择工具完成用户请求（验证：多个典型场景的 tool call 正确率）
3. **问答有上下文**：LLM 回答中体现项目环境信息（日期、路径等）
4. **编码规范体现**：输出的代码和 commit message 符合规范
5. **`[USER_QUERY]` 正确使用**：LLM 知道何时以及如何使用此功能
6. **兼容性**：自定义 `.vibewisp/system-prompt.md` 项目不受影响
7. **测试通过**：`cargo test` 全量通过
8. **Clippy 零警告**

## 7. 拆分策略

分两步执行：

**步骤 1：System Prompt + 上下文注入**
- 重写 `DEFAULT_SYSTEM_PROMPT` 常量
- 修改 `PromptBuilder::build()` 增加 `working_dir` 参数、注入日期
- 更新 `[USER_QUERY]` 指令文案
- 更新测试

**步骤 2：工具描述优化**
- 逐个 review 各 Tool 的 `description()`，改为多句描述
- 确保每个工具的使用场景、限制、参数说明清晰
- 不需要改测试（描述文本变化不影响逻辑测试）
