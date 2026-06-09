# 工作计划：System Prompt 与工具描述优化

## 概述

优化默认 system prompt 的内容密度和结构清晰度，让 LLM 清楚自己的角色、编码规范、可用工具和交互方式。同时改进工具描述和参数文档，实现动态工具指南。仅涉及 vbw-core 和 vbw-daemon 两个 crate。

## Wave 并行策略

```
Wave 1 (串行)          Wave 2 (串行)
┌─────────────┐       ┌───────────────────┐
│ 1a. 默认 prompt    │       │ 2a. Tool category() │
│ 1b. build() 签名    │       │ 2b. 工具描述改进     │
│ 1c. agent 调用方    │       │ 2c. 动态工具指南     │
│ 1d. 测试更新        │       │ 2d. 注册顺序 + 测试  │
└─────────────┘       └───────────────────┘
       │                       │
       └── 都完成后 ────────────┘
           合并测试验证
```

Wave 1 和 Wave 2 修改的是 vbw-core 中不同的文件集合，但 agent.rs 被两个 Wave 都修改（不同位置），所以串行执行。

## 步骤 1：System Prompt + 上下文注入

### 1a：重写默认 System Prompt

#### 🔴 红 — 测试

| # | 测试用例 | 说明 |
|---|---------|------|
| 1.1 | 默认 prompt 包含角色定义 | 含 "vibewisp" 和 "Rust" |
| 1.2 | 默认 prompt 包含编码规范 | 含 "TDD" 或 "Conventional Commits" |
| 1.3 | 默认 prompt 包含交互规范 | 含 "`[USER_QUERY]`" |
| 1.4 | 默认 prompt 不包含工具名 | 工具名由动态指南生成，文字部分不硬编码 |
| 1.5 | `DEFAULT_SYSTEM_PROMPT` 类型保持 `&str` | 不改变类型 |

#### 🟢 绿 — 实现

在 `crates/vbw-core/src/session.rs` 中，用 `concat!()` 宏重写 `DEFAULT_SYSTEM_PROMPT`：

```
concat!(
    "You are vibewisp, ...",
    "\n\n## Coding Conventions\n",
    "- 简洁优先...",
    "- 手术刀式修改...",
    ...
    "\n\n## Interaction Rules\n",
    "- 工具调用后必须等待结果...",
    ...
)
```

保留原常量名和类型，只改内容。

#### 🧪 测试 → 🔍 类型检查

```bash
cargo test -p vbw-core
cargo clippy -p vbw-core -- -D warnings
```

#### ♻️ 重构

无

#### 📦 提交

`feat(core): enrich default system prompt with role, conventions, and interaction rules`

---

### 1b：PromptBuilder::build() 签名扩展 + 上下文注入

#### 🔴 红 — 测试

| # | 测试用例 | 说明 |
|---|---------|------|
| 2.1 | `build()` 接受 5 参数，编译通过 | 新签名 `(template, rules, history, working_dir, date_str)` |
| 2.2 | system content 末尾包含当前环境段 | 含 "Current Date" 和 "Working Directory" |
| 2.3 | working_dir 透传到输出 | 传入路径出现在 system content 中 |
| 2.4 | date_str 透传到输出 | 传入日期字符串出现在 system content 中 |
| 2.5 | 当 working_dir 为空路径时正确处理 | 不 panic |
| 2.6 | `[USER_QUERY]` 指令文案含使用示例 | 含 "allow_other=true" 说明 |
| 2.7 | 现有测试仍通过（签名变更后调用方更新） | 回归验证 |

#### 🟢 绿 — 实现

**修改 `PromptBuilder::build()` 签名**：
```rust
pub fn build(
    system_template: &str,
    rules: &str,
    history: &[Message],
    working_dir: &Path,
    date_str: &str,
) -> Vec<Message>
```

**注入运行时上下文**：在 system content 末尾追加：
```
## Current Context

Date: {date_str}
Working Directory: {working_dir}
```

**`[USER_QUERY]` 指令优化**：更新 `USER_QUERY_INSTRUCTION` 常量文案，包含：
- 使用示例（展示多选项格式）
- `allow_other=true` 的效果说明
- 强调仅在输出末尾使用

#### 🧪 测试 → 🔍 类型检查

```bash
cargo test -p vbw-core
cargo clippy -p vbw-core -- -D warnings
```

#### ♻️ 重构

无

#### 📦 提交

`feat(core): extend PromptBuilder::build() with working_dir, date context, improve [USER_QUERY] instruction`

---

### 1c：agent.rs 调用方更新

#### 🔴 红 — 测试

| # | 测试用例 | 说明 |
|---|---------|------|
| 3.1 | `run_agent_loop` 调用 `build()` 时传入 `ctx.working_dir` | 编译检查 |
| 3.2 | 日期字符串格式正确 | `"2026-06-09"` 格式 |
| 3.3 | agent 循环中每轮迭代日期更新 | 跨天场景（午夜跨天时日期变化） |
| 3.4 | 所有 `build()` 调用点都更新（含测试中的调用） | 编译通过 |

#### 🟢 绿 — 实现

在 `crates/vbw-core/src/agent.rs` 中：

1. 在调用 `build()` 之前（约 298 行），生成日期字符串：
```rust
let now = std::time::SystemTime::now()
    .duration_since(std::time::UNIX_EPOCH)
    .unwrap();
// 简单的日期格式化（天级别精度足够）
let date_secs = now.as_secs();
let days = date_secs / 86400;
// ... 1970-01-01 基础上计算年月日
let date_str = format!("{}-{:02}-{:02}", year, month, day);
```

2. 更新调用：
```rust
let messages = PromptBuilder::build(
    &enriched_template,
    &rule_engine.get_active_rules(),
    &ctx.history,
    &ctx.working_dir,
    &date_str,
);
```

3. 更新 `session.rs` 或其他地方所有对 `PromptBuilder::build()` 的测试调用。

#### 🧪 测试 → 🔍 类型检查

```bash
cargo test -p vbw-core
cargo clippy -p vbw-core -- -D warnings
```

#### ♻️ 重构

考虑将日期格式化提取为一个辅助函数 `fn today_date_string() -> String`，方便测试。

#### 📦 提交

`feat(core): update agent loop to pass working_dir and date to PromptBuilder`

---

### 1d：测试更新

#### 🔴 红 — 测试

| # | 测试用例 | 说明 |
|---|---------|------|
| 4.1 | 所有 `PromptBuilder::build()` 测试使用新签名 | 全部更新 |
| 4.2 | 新测试覆盖运行时上下文渲染 | 日期和路径正确 |
| 4.3 | `[USER_QUERY]` 指令包含优化后文案 | 含示例 |

#### 🟢 绿 — 实现

更新 `crates/vbw-core/src/prompt.rs` 中的测试：
- 所有 `PromptBuilder::build(...)` 调用增加 `working_dir` 和 `date_str` 参数
- 现有 `test_system_message` 等测试验证上下文段出现
- 新增测试验证日期格式

#### 🧪 测试 → 🔍 类型检查

```bash
cargo test -p vbw-core
cargo clippy -p vbw-core -- -D warnings
```

#### ♻️ 重构

无

#### 📦 提交

`test(core): update PromptBuilder tests for new signature and context injection`

---

## 步骤 2：工具描述优化 + 动态工具指南

### 2a：Tool trait 新增 category() 方法

#### 🔴 红 — 测试

| # | 测试用例 | 说明 |
|---|---------|------|
| 5.1 | Tool trait 包含 `category()` 方法 | 默认返回 "other" |
| 5.2 | 已有工具不 override，编译通过 | 默认实现兼容 |
| 5.3 | Mock 工具可 override category | 用于测试 |

#### 🟢 绿 — 实现

在 `crates/vbw-core/src/tool.rs` 的 `Tool` trait 中增加：

```rust
fn category(&self) -> &str {
    "other"
}
```

注意：`Tool` trait 有 `#[async_trait]`，已有 test mock 工具通过宏生成。新增方法需要同步更新测试中的 mock 宏（如果有）。

#### 🧪 测试 → 🔍 类型检查

```bash
cargo test -p vbw-core
cargo clippy -p vbw-core -- -D warnings
```

#### ♻️ 重构

无

#### 📦 提交

`feat(core): add category() method to Tool trait for dynamic tool grouping`

---

### 2b：各工具 override category + 改进 description/parameters

#### 🔴 红 — 测试

| # | 测试用例 | 说明 |
|---|---------|------|
| 6.1 | Bash category 返回 "common" | |
| 6.2 | ReadFile / WriteFile / EditFile category 返回 "common" | |
| 6.3 | Grep / Glob category 返回 "common" | |
| 6.4 | CodeGraphSearch / CodeGraphGetDetails category 返回 "analyze" | |
| 6.5 | WebFetch category 返回 "network" | |
| 6.6 | 各工具 description() 至少 2 句 | 不再是一句话 |
| 6.7 | 各工具 parameters() 中关键参数有 description | JSON Schema description 字段 |
| 6.8 | 现有 tool 测试不受影响 | 回归 |

#### 🟢 绿 — 实现

逐个修改以下文件（可并行）：

**Bash** (`bash.rs`):
```rust
fn category(&self) -> &str { "common" }
fn description(&self) -> &str {
    "Execute a shell command on the host system with the user's permissions. \
     Use this for running scripts, build tools, git operations, and other CLI tasks. \
     Not suitable for interactive programs (no stdin/stdout). \
     Timeout: {timeout_secs}s. Blocked commands: sudo, rm -rf (top-level)."
}
// parameters 中 command 参数加 description
```

**ReadFile** (`file.rs`):
```rust
fn category(&self) -> &str { "common" }
// description 说明 1MB 限制、二进制检测、路径安全
// parameters 中 path 参数加 description
```

**WriteFile / EditFile**: 同上，各自说明用途和安全机制。

**Grep / Glob** (`search.rs`):
```rust
fn category(&self) -> &str { "common" }
// description 说明正则支持、ripgrep 优先、排除二进制
```

**CodeGraphSearch / CodeGraphGetDetails** (`codegraph.rs`):
```rust
fn category(&self) -> &str { "analyze" }
// description 说明 AST 查询、符号搜索、调用链
```

**WebFetch** (`fetch.rs`):
```rust
fn category(&self) -> &str { "network" }
// description 说明 URL fetch、内容提取为 markdown
```

#### 🧪 测试 → 🔍 类型检查

```bash
cargo test -p vbw-core -p vbw-tools
cargo clippy -p vbw-core -p vbw-tools -- -D warnings
```

#### ♻️ 重构

无

#### 📦 提交

`feat(tools): add category overrides and improve description/parameters for all tools`

---

### 2c：动态工具指南渲染逻辑

#### 🔴 红 — 测试

| # | 测试用例 | 说明 |
|---|---------|------|
| 7.1 | 从 ToolRegistry 遍历 definitions，按 category 分组 | |
| 7.2 | 渲染结果为有效 markdown 格式 | 含 "## Available Tools" |
| 7.3 | common 组标注 "(prefer these first)" | |
| 7.4 | 空 registry 返回空字符串 | 不 panic |
| 7.5 | 渲染结果拼接到 system_template 末尾后传入 build() | |

#### 🟢 绿 — 实现

在 `agent.rs` 中（调用 `build()` 之前）添加 `render_tool_guide()` 辅助函数：

```rust
fn render_tool_guide(registry: &ToolRegistry) -> String {
    let defs = registry.definitions();
    if defs.is_empty() { return String::new(); }
    
    let mut grouped: HashMap<&str, Vec<&str>> = HashMap::new();
    for def in &defs {
        grouped.entry(def.category.as_str()).or_default().push(def.name.as_str());
    }
    
    let mut parts = vec!["\n\n## Available Tools".to_string()];
    // 按 category 优先级输出：common → analyze → network → other
    for (label, cat) in [("Common (prefer these first)", "common"),
                          ("Analyze", "analyze"),
                          ("Network", "network"),
                          ("Other", "other")] {
        if let Some(tools) = grouped.remove(cat) {
            if !tools.is_empty() {
                parts.push(format!("\n{label}:\n  {}", tools.join(", ")));
            }
        }
    }
    // 剩余未识别的 category
    for (cat, tools) in grouped {
        parts.push(format!("\n{cat}:\n  {}", tools.join(", ")));
    }
    
    parts.join("")
}
```

**注意**：`ToolRegistry::definitions()` 返回的 `ToolDefinition` 结构需要包含 category 信息。检查当前是否有 `ToolDefinition` 结构及其定义，可能需要同步更新。

#### 🧪 测试 → 🔍 类型检查

```bash
cargo test -p vbw-core
cargo clippy -p vbw-core -- -D warnings
```

#### ♻️ 重构

如果 `ToolDefinition` 没有 `category` 字段，需要新增。

#### 📦 提交

`feat(core): dynamic tool guide rendering with category-based grouping`

---

### 2d：注册顺序调整 + 测试

#### 🔴 红 — 测试

| # | 测试用例 | 说明 |
|---|---------|------|
| 8.1 | `main.rs` 中各工具注册顺序按新顺序 | Bash → 文件 → 搜索 → 网络 → 代码分析 |
| 8.2 | 回归：所有工具仍可正常注册 | 逻辑不变，只调顺序 |

#### 🟢 绿 — 实现

在 `crates/vbw-daemon/src/main.rs` 中调整 register 调用顺序：

```
Bash → ReadFile → WriteFile → EditFile → Grep → Glob → WebFetch → CodeGraphSearch → CodeGraphGetDetails
```

这个顺序与 `category()` 的渲染无关（渲染时从 registry 遍历），但影响 LLM 看到的 tool definitions 列表顺序。

#### 🧪 测试 → 🔍 类型检查

```bash
cargo test -p vbw-daemon
cargo clippy -p vbw-daemon -- -D warnings
```

#### ♻️ 重构

无

#### 📦 提交

`chore(daemon): reorder tool registration for better LLM visibility`

---

## 步骤 3：集成验证

```bash
cargo test
cargo clippy -- -D warnings
cargo fmt -- --check
```

### 📦 提交

无（已在各子步骤中提交）

---

## 测试覆盖汇总

| Wave | 并行数 | 子步骤 | 测试用例数 | 涉及文件 |
|------|--------|--------|-----------|---------|
| Wave 1 | 串行 | 1a-1d | ~15 | session.rs, prompt.rs, agent.rs |
| Wave 2 | 串行 | 2a-2d | ~20 | tool.rs, bash.rs, file.rs, search.rs, codegraph.rs, fetch.rs, agent.rs, main.rs |

## 备注

- **ToolDefinition 结构**：需要检查 `tool_registry` 中的 `definitions()` 返回类型，确保包含 `category` 字段。可能需要同步更新。
- **日期格式化**：简单的 SystemTime 计算足够，不用加 chrono 依赖。
- **测试中的 build() 调用**：改签名后所有测试调用点都需要更新，注意检查。
- **Mock 工具宏**：如果 Tool trait 的 mock 测试宏自动生成所有方法，需要确保新增的 `category()` 默认方法被正确处理。
