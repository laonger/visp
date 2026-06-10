# visp-core 代码 Review

**日期**: 2026-06-10
**审核范围**: `crates/visp-core/` 全部 11 个源文件
**总行数**: 约 3056 行
**测试状态**: 96 passed, 0 failed
**Lint 状态**: clippy 零警告

---

## 总体评价

代码质量总体良好，模块职责清晰，`README.md` 明确了「禁止 IO」的核心约束，测试覆盖率高（96 pass），error 处理使用了 `thiserror`，API 设计简洁。但存在 **1 个 Bug**、**1 个架构违规**、**1 个设计隐患**，以及若干代码组织上的改进空间。

---

## 🔴 P0 — Bug

### 1. `SessionError::AlreadyExists` 错误消息错误

**文件**: `crates/visp-core/src/error.rs` lines 58–62

```rust
#[error("Session not found: {0}")]
NotFound(String),

#[error("Session not found: {0}")]  // ← BUG: 应该是 "already exists"
AlreadyExists(String),
```

`AlreadyExists` 的 display 消息是 `"Session not found: {0}"`，跟 `NotFound` 完全一样。查找时看到 "Session not found" 但实际上 Session 已存在，会产生误导。

**修复**: 将 `AlreadyExists` 的 error 属性改为 `#[error("Session already exists: {0}")]`。

---

## 🔴 P0 — 架构违规

### 2. 「visp-core 禁止 IO」约束被破坏

根据 `README.md` 和 `AGENTS.md` 的核心约束：

> visp-core 禁止 IO：所有文件读写、网络请求、进程启动必须由其他 crate 实现。

以下文件直接进行了 IO 操作：

#### 涉及 IO 的位置

**`rules.rs`** — `RuleEngine::new()` 直接读写文件系统：

```rust
// line 22: new()
std::fs::read_to_string(&md)            // 读取项目目录 AGENTS.md
std::fs::read_dir(dir)?                 // 读取 .visp/rules/ 目录
std::fs::read_to_string(&path)?         // 读取规则文件内容

// discover_agents_md() 中的 .is_file() — 也是文件系统调用
// collect_rules() 中的 read_dir / read_to_string / is_file
```

**`session.rs`** — `load_system_prompt_template()` 和 `load_skills_from_dir()` 直接读文件：

```rust
// load_system_prompt_template()
std::fs::read_to_string(project_path.join(".visp/system-prompt.md"))
std::fs::read_to_string(home.join(".config/visp/system-prompt.md"))

// load_skills_from_dir() / load_skills()
std::fs::read_dir(dir)?
std::fs::read_to_string(&skill_file)
std::fs::create_dir_all(...)
```

#### 影响

- 违反架构设计原则
- 导致 `visp-core` 的测试依赖 tempfile 和文件系统，而非纯逻辑测试
- 无法在 wasm 或其他无文件系统环境运行
- 核心数据流依赖外部状态，推理困难

#### 修复方案（二选一）

**方案 A（推荐）**：将 IO 逻辑移到 `visp-daemon` 中。`visp-core` 只定义纯数据结构，由外部初始化时读文件并注入：

```
visp-daemon (IO)          visp-core (Pure)
───────────────           ────────────────
FileSystem                 RuleSet { content, files }
    │                            ▲
    ├─ read rules ───────────────┘
    │
FileSystem                 SkillSet { sections }
    │                            ▲
    ├─ read skills ──────────────┘
```

具体做法：
- `RuleEngine::new()` 改为接收 `RuleSet` 而非 `&Path`
- `SessionManager` 初始化时接收已加载的 system_prompt_template 和 skills 字符串
- 由 `visp-daemon/src/main.rs` 在启动时完成所有文件读取

**方案 B**：将文件 IO 封装到一个新的 crate（如 `visp-config`），但从架构上看不如方案 A 干净。

---

## 🔴 P0 — 设计隐患

### 3. `Session::created_at` 使用 `std::time::Instant`

**文件**: `session.rs` line 29

```rust
pub created_at: Instant,
```

`Instant` 是**不可序列化**的——它只是一个系统启动以来的 ticks 计数器。一旦需要：

- 将 Session 保存到磁盘 / 持久化
- 通过 gRPC 传输给 CLI（proto 消息需要序列化）
- Daemon 重启后保持会话状态

`Instant` 就完全不可用。

**修复**: 换为 `chrono::DateTime<chrono::Utc>` 或其他可持久化的时间类型。

---

## 🟡 P1 — 代码组织

### 4. `agent.rs`: 1887 行，文件过大

`agent.rs` 是目前整个项目中最大的文件之一，混合了多种职责：

| 职责 | 行数估算 |
|------|----------|
| `AgentEvent` enum（9 变种） | ~30 行 |
| `AgentLoopContext` struct | ~15 行 |
| `AgentConfig` struct + Default | ~20 行 |
| Agent 主循环 `run_agent_loop()` | ~400 行 |
| LLM error → code 转换 | ~25 行 |
| 工具参数格式化 | ~20 行 |
| `[USER_QUERY]` marker 解析 | ~100 行 |
| 测试代码 | ~800+ 行 |

**建议拆分结构**:

```
agent.rs                  ← 只保留 Agent struct + run() 对外接口
agent/
├── event.rs              ← AgentEvent + AgentLoopContext + AgentConfig
├── executor.rs           ← 工具执行循环 + ToolExecResult
├── marker.rs             ← USER_QUERY 解析/剥离
└── mod.rs                ← 重新导出
```

### 5. `session.rs`: 697 行，偏大

skills 加载、YAML frontmatter 解析相关的函数和测试可以抽到独立的 `skills.rs` 模块：

```rust
// 可以从 session.rs 抽出的函数
pub(crate) fn load_skills_from_dir(...) → String
pub(crate) fn extract_frontmatter_field(..., field) → Option<String>
fn strip_frontmatter(...) → &str   // 目前 #[cfg(test)]
```

### 6. `AgentErrorCode` 未从 `lib.rs` re-export

```rust
// lib.rs 只 re-export 了:
pub use error::{CoreError, LlmError, SessionError};
```

虽然 `AgentErrorCode` 主要通过 `AgentEvent::Error` 传递，但为了 API 完整性应对称 re-export。

---

## 🟡 P2 — 次要问题

### 7. `has_always_apply_true` 函数名可读性差

**文件**: `rules.rs` line 125

```rust
fn has_always_apply_true(content: &str) -> bool {
```

名称含义不清。实际检测的是 YAML frontmatter 中 `alwaysApply: true` 标记。建议改名：

```rust
fn has_always_apply_marker(content: &str) -> bool
// 或
fn should_always_apply(content: &str) -> bool
```

### 8. `collect_rules` 遇到不可读目录会直接失败

```rust
let mut entries: Vec<_> = std::fs::read_dir(dir)?
    .filter_map(|e| e.ok())
```

`read_dir` 使用 `?` 传播错误，如果 `.visp/rules/` 目录权限不足，整个 `RuleEngine::new()` 会失败。虽然这是边缘情况，但更健壮的做法是 `ok()` 或 `unwrap_or_default()` 处理。

### 9. Skills 加载只取 description 字段

`load_skills_from_dir()` 只提取 YAML frontmatter 的 `description` 字段，SKILL.md 的正文被丢弃。如果用户期望正文内容也被加入 LLM 上下文，可能会感到困惑。建议在文档中明确说明 SKILL.md 的行为，或提供一个配置选项控制是否包含正文。

---

## ✅ 做得好的地方

### 1. Tool trait 设计优雅

`Tool` trait 提供了 `requires_approval()` 和 `requires_approval_for()` 两个方法，后者允许按参数动态控制审批（如白名单域名），比单纯的 bool 灵活很多：

```rust
fn requires_approval(&self) -> bool { false }
fn requires_approval_for(&self, _arguments: &serde_json::Value) -> bool {
    self.requires_approval()
}
```

### 2. PromptBuilder 职责单一

只负责拼接 system prompt，不涉及 IO，不需要测试 mock。纯函数式设计，易于推理。

### 3. ToolRegistry 小而完整

- 重复注册检测
- `definitions()` 批量获取所有工具定义
- `execute()` 委托链清晰
- 无多余依赖

### 4. [USER_QUERY] 机制设计合理

LLM 通过文本中的 marker 控制是否需要用户交互，`visp-core` 解析 marker 后通过事件通道发出 `AgentEvent::UserQuery`，TUI 收到后渲染选择 UI。两端职责分明。

### 5. 测试覆盖率高

96 个测试覆盖了：
- 所有错误类型的 display 输出
- 工具注册的边界情况（重复名称、未找到）
- 规则加载的多种场景（空目录、排序、仅前 5 行检测 marker）
- YAML frontmatter 解析（有/无 frontmatter、字段缺失）
- Agent 主循环的 USER_QUERY 集成流程
- skip_context 消息过滤

### 6. `skip_context` 字段设计前瞻

允许某些消息（如 `/init` 产生的系统提示）不注入后续对话的 context window，为系统命令留了干净的扩展空间。

### 7. 错误类型分层清晰

```
CoreError
├── Llm(LlmError)      — Network, RateLimit, Auth, Api, Stream
├── Tool(String)
├── Session(String)
├── Config(String)
├── Io(std::io::Error)
└── Other(String)
```

附属枚举 `SessionError` 和 `AgentErrorCode` 各自覆盖领域内的错误场景。

---

## 修复优先级汇总

| 优先级 | 问题 | 影响 | 文件 |
|--------|------|------|------|
| 🔴 P0 | `SessionError::AlreadyExists` 错误消息错误 | 调试误导 | `error.rs` |
| 🔴 P0 | visp-core 包含 IO（rules.rs + session.rs） | 架构违规 | `rules.rs`, `session.rs` |
| 🔴 P0 | `Session.created_at` 用 `Instant` 不可持久化 | 序列化阻塞 | `session.rs` |
| 🟡 P1 | agent.rs 1887 行需要拆分 | 可维护性 | `agent.rs` |
| 🟡 P1 | session.rs 697 行可以拆分 skills 模块 | 代码组织 | `session.rs` |
| 🟡 P1 | `AgentErrorCode` 未 re-export | API 不对称 | `lib.rs` |
| 🟡 P2 | `has_always_apply_true` 命名含糊 | 可读性 | `rules.rs` |
| 🟢 P3 | `collect_rules` 不可读目录直接失败 | 健壮性 | `rules.rs` |
| 🟢 P3 | Skills 加载只取 description | 用户预期 | `session.rs` |
