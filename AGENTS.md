<Role>
你是 vibewisp，一个 Rust 后端驱动的轻量级 AI 编程助手。项目已进入产品阶段，架构稳定，可以全面使用项目内置的各种工具和功能。
</Role>

<Project>

## 项目概览

**vibewisp** 是一个用 Rust 编写的轻量级 AI 编程助手后端，是 OpenCode 的 Rust 重写版。采用前后端分离的 daemon 架构，通过 gRPC (tonic) 提供 AI 辅助编程能力。

### 核心目标

利用 Rust 的零成本抽象、无 GC、高效并发特性，解决原 Node.js 实现 CPU 占用偏高的问题。

### 架构概览

```
┌──────────────────────────────────────────────────────────────┐
│                    前端层 (可替换)                            │
│  ┌──────────┐  ┌──────────┐  ┌──────────┐                   │
│  │ vbw-cli  │  │ VSCode   │  │   Web    │  ← 可替换前端      │
│  │ (已实现)  │  │ (未来)   │  │ (未来)   │                   │
│  └────┬─────┘  └──────────┘  └──────────┘                   │
│       │ gRPC (tonic)                                        │
├───────┼──────────────────────────────────────────────────────┤
│       │              后端 Daemon (vbw-daemon)               │
│  ┌────┴────────────────────────────────────────────────┐    │
│  │              gRPC Server (tonic + axum)              │    │
│  ├─────────────────────────────────────────────────────┤    │
│  │  Agent 编排器 (核心循环: 输入→LLM→工具→LLM→...)       │    │
│  │  ├─ Session Manager  — 会话生命周期管理               │    │
│  │  ├─ Prompt Builder   — prompt 组装                   │    │
│  │  ├─ Rule Engine      — 规则文件加载                   │    │
│  │  ├─ Tool Registry    — 工具注册/执行                  │    │
│  │  ├─ LLM Provider     — Anthropic API 集成            │    │
│  │  ├─ Tool Executors   — 文件读写 / bash / 搜索等      │    │
│  │  └─ CodeGraph Engine — tree-sitter + SQLite 代码索引 │    │
│  └─────────────────────────────────────────────────────┘   │
└──────────────────────────────────────────────────────────────┘
```

### 运行方式

1. 启动 `vbw-daemon`（gRPC 服务端，常驻进程）
2. 启动 `vbw-cli`（TUI 客户端，通过 gRPC 连接 daemon）

</Project>

<Modules>

## 关键模块与职责

### crates/vbw-core — 核心抽象层
*不依赖 IO，纯逻辑层。*

| 文件 | 职责 |
|------|------|
| `agent.rs` | Agent 编排器核心循环（run_agent_loop）：输入→LLM→工具→LLM→... |
| `session.rs` | 会话生命周期管理（Idle→Running→Completed/Error）、SessionStore trait |
| `message.rs` | 消息模型（Role/Message/ToolCallRequest）、消息构造辅助函数 |
| `prompt.rs` | PromptBuilder：组装 system prompt + rules + 对话历史 |
| `provider.rs` | LlmProvider trait、ChatEvent 枚举、LlmConfig |
| `tool.rs` | Tool trait（name/description/parameters/execute/requires_approval） |
| `tool_registry.rs` | 工具注册表：注册/查询/执行/导出定义给 LLM |
| `rules.rs` | RuleEngine：从 `.vibewisp/rules/` 和全局目录加载规则 |
| `error.rs` | 错误类型体系（CoreError/LlmError/SessionError/AgentErrorCode） |

### crates/vbw-proto — gRPC 协议定义
*protobuf 定义的服务 CoderDaemon。*

| RPC 方法 | 类型 | 说明 |
|---|---|---|
| `CreateSession` | 一元 | 创建新会话 |
| `ListSessions` | 一元 | 列出所有活跃会话 |
| `DeleteSession` | 一元 | 删除会话 |
| `Chat` | **双向流** | 核心对话通道（核心复杂逻辑） |
| `ReadFile` | 一元 | 快速文件读取（跳过 LLM） |
| `SearchSymbols` | 一元 | 代码符号搜索 |
| `GetSymbolDetails` | 一元 | 符号详情（调用者/被调用者） |
| `HealthCheck` | 一元 | 健康检查 |
| `Shutdown` | 一元 | 优雅关闭 |

proto 文件：`crates/vbw-proto/proto/vibewisp.proto`

### crates/vbw-llm — LLM 提供器

| 文件 | 职责 |
|------|------|
| `anthropic.rs` | Anthropic Claude API 集成：消息转换、SSE 流解析、重试逻辑 |
| `streaming.rs` | SSE 事件解析器 |
| `mock.rs` | 测试用 Mock Provider |

当前仅支持 Anthropic Claude API。消息格式转换（vbw-core 通用格式 ↔ Anthropic Messages API）。

### crates/vbw-tools — 内置工具

| 工具 | 模块 | 功能 | 安全特性 |
|------|------|------|---------|
| `ReadFile` | `file.rs` | 读取文件 | 1MB 限制、二进制检测、路径校验 |
| `WriteFile` | `file.rs` | 写入/覆盖 | 自动创建父目录、路径安全 |
| `EditFile` | `file.rs` | 精确字符串替换 | 原子写入(temp+rename)、多匹配拒绝 |
| `Bash` | `bash.rs` | shell 命令执行 | 黑名单(sudo/rm -rf)、超时控制 |
| `Grep` | `search.rs` | 正则搜索 | 优先 ripgrep、排除二进制 |
| `Glob` | `search.rs` | 文件名通配符 | 优先 ripgrep、递归搜索 |
| `CodeGraphSearch` | `codegraph.rs` | AST 符号搜索 | 包装 vbw-codegraph |
| `CodeGraphGetDetails` | `codegraph.rs` | 符号详情 | 含调用链信息 |

### crates/vbw-codegraph — 代码图谱引擎
*基于 tree-sitter + SQLite 的代码智能引擎。*

| 文件 | 职责 |
|------|------|
| `graph.rs` | 图数据结构（Symbol/Edge/SymbolKind/EdgeKind） |
| `parser.rs` | tree-sitter 解析器（TypeScript/TSX） |
| `store.rs` | SQLite 持久化层（symbols/edges/files/imports/exports 表） |
| `index.rs` | 全量/增量索引构建 |
| `query.rs` | 符号查询引擎（search/get_details） |
| `watcher.rs` | 文件变更监听（notify crate） |

当前仅支持 TypeScript / TSX。

### crates/vbw-daemon — 后端常驻进程

| 文件 | 职责 |
|------|------|
| `main.rs` | 入口：模块组装、配置加载、服务启动 |
| `server.rs` | gRPC 服务器 |
| `service.rs` | gRPC 服务实现（CoderDaemon trait） |
| `config.rs` | TOML 配置加载 |
| `command/init.rs` | `/init` 命令处理 |
| `command/mod.rs` | 命令模块 |

### crates/vbw-cli — TUI 客户端

| 文件 | 职责 |
|------|------|
| `main.rs` | 入口：参数解析、连接 daemon |
| `client.rs` | gRPC 客户端封装（VbwClient/ChatHandle） |
| `event.rs` | 事件循环：键盘输入 + gRPC 消息处理 |
| `ui.rs` | ratatui 渲染：对话区、输入区、状态栏 |
| `app.rs` | AppState 状态管理、消息缓存、Markdown 渲染 + syntect 代码高亮 |
| `theme.rs` | 颜色/样式统一管理 |

</Modules>

<Build>

## 构建与测试

### 编译

```bash
# 工作区所有 crate
cargo build

# Release 构建
cargo build --release

# 仅指定 crate
cargo build -p vbw-daemon
cargo build -p vbw-cli
```

### 运行

```bash
# 启动 daemon（必须先启动）
cargo run --release --bin vbw-daemon

# 启动 CLI（另一个终端）
cargo run --release --bin vbw -- --project /path/to/your/project
```

### 测试

```bash
# 全量测试
cargo test

# 指定 crate 测试
cargo test -p vbw-core
cargo test -p vbw-codegraph
cargo test -p vbw-cli

# 指定测试名
cargo test test_agent_config_default
```

### 代码质量检查

```bash
# Clippy（零警告标准）
cargo clippy -- -D warnings

# 格式化
cargo fmt -- --check
cargo fmt  # 自动格式化

# 完整验证
cargo test && cargo clippy -- -D warnings && cargo fmt -- --check
```

### 测试数据

- 196 个测试（全量）
- 严格 TDD：红→绿→测试→类型检查→重构→提交
- 单元测试在 src 文件中（`#[cfg(test)] mod tests`）

</Build>

<CodingConventions>

## 编码约定与设计模式

### 1. 架构原则

- **工具自包含** — 新工具不修改 ToolContext/AgentLoopContext 等核心结构
- **最小改动** — 只改必须的部分，不做预防性设计
- **先讨论后实现** — 中/复杂任务先写设计文档，确认后再动手
- **核心层无 IO** — `vbw-core` 不依赖任何 IO 操作

### 2. TDD 流程

```
1. Red   → cargo test（确认新测试失败）
2. Green → 最小实现
3. Test  → cargo test（全量测试通过）
4. Lint  → cargo clippy -- -D warnings
5. Fmt   → cargo fmt -- --check
6. Commit → git commit -m "type(scope): description"
```

### 3. 提交规范

Conventional Commits：
- `feat(scope):` — 新功能
- `fix(scope):` — 修复
- `docs(scope):` — 文档
- `refactor(scope):` — 重构
- `test(scope):` — 测试
- `chore(scope):` — 杂项

### 4. 编程风格

- **简洁优先**：变量/函数命名简短但语义明确
- **最小改动**：只改必须的部分，不顺手重构无关代码
- **简单设计**：不写未被要求的功能，50 行够就不写 200 行
- **显式依赖**：不通过全局状态或隐式传递
- **异步**：使用 tokio 异步运行时，所有 IO 操作为 async

### 5. 设计模式

- **Trait 抽象**：`LlmProvider`、`Tool`、`SessionStore` 均使用 async_trait
- **Builder 模式**：`PromptBuilder` 组装请求
- **Registry 模式**：`ToolRegistry` 管理可插拔工具
- **事件驱动**：`AgentEvent` 枚举 + mpsc channel 驱动 UI 更新
- **状态机**：Session 状态转换 Idle→Running→Completed/Error
- **Mutex<dyn Trait>**：通过 `Arc<Mutex<dyn SessionStore>>` 实现可替换存储
- **流式处理**：LLM 响应通过 `Stream<Item=ChatEvent>` 处理

### 6. 错误处理

- 使用 `thiserror` 定义枚举错误类型
- `CoreError` 作为顶层错误，包装 LlmError/SessionError
- `AgentErrorCode` 枚举用于 AgentEvent::Error 的细分
- 工具执行通过 `ToolResult { content, is_error }` 结构

### 7. 测试约定

- 测试与代码同文件（`#[cfg(test)] mod tests`）
- Mock 使用 trait + 自定义实现（如 TestProvider/MockTool）
- 测试异步函数使用 `#[tokio::test]`

</CodingConventions>

<Config>

## 配置与环境

### 配置文件

创建 `~/.config/vibewisp/daemon.toml`：

```toml
[daemon]
listen_addr = "[::1]:50051"
log_level = "info"

[llm]
provider = "anthropic"
model = "claude-sonnet-4-20250514"
# api_key 可通过环境变量 ANTHROPIC_API_KEY 设置
api_key = "sk-ant-..."
temperature = 0.7
max_tokens = 4096

[tools]
bash_timeout_secs = 120
file_max_size_bytes = 1048576

[agent]
max_iterations = 50
llm_retry_attempts = 3
llm_retry_base_delay_ms = 1000
bash_confirm_mode = true
file_max_size_bytes = 1048576
```

### 环境变量

| 变量 | 用途 |
|------|------|
| `ANTHROPIC_API_KEY` | Anthropic API 密钥（优先级低于配置文件中的 api_key） |
| `RUST_LOG` | 日志级别（如 `info`、`debug`、`vibewisp=debug`） |
| `HOME` | 用户主目录（用于查找全局配置 `~/.config/vibewisp/`） |

### 工具链

- Rust 稳定版（`rust-toolchain.toml` 指定 `channel = "stable"`）
- 组件：rustc, cargo, clippy, rustfmt
- Edition 2024

### 规则系统

规则加载路径（优先级从高到低）：
1. 项目规则：`.vibewisp/rules/`
2. 全局规则：`~/.config/vibewisp/rules/`

规则文件为 Markdown，通过 YAML frontmatter 控制：
```markdown
---
alwaysApply: true
---

## 代码规范

- 使用 2 空格缩进
- 函数名使用 snake_case
```

- `alwaysApply: true` — 始终注入 system prompt
- `alwaysApply: false` — 按需触发

### 系统 Prompt 模板

加载路径（优先级）：
1. 项目 `.vibewisp/system-prompt.md`
2. 全局 `~/.config/vibewisp/system-prompt.md`
3. 内置默认：`"You are vibewisp, a lightweight AI coding assistant running on a Rust backend."`

### CLI 命令

| 命令 | 用途 |
|---|---|
| `/quit` / `/exit` | 退出 |
| `/clear` | 清屏 |
| `/temp <val>` | 设置温度（如 `/temp 0.5`） |
| `/model <name>` | 切换模型（如 `/model claude-sonnet-4-20250514`） |
| `/init` | 初始化项目 |
| `/help` | 显示帮助 |

### 关键依赖

| crate | 用途 |
|-------|------|
| tokio | 异步运行时 |
| tonic + prost | gRPC 框架 |
| serde + serde_json | 序列化 |
| ratatui + crossterm | TUI 终端界面 |
| syntect | 代码语法高亮 |
| ratatui-markdown | Markdown 渲染 |
| reqwest | HTTP 客户端（LLM API） |
| tree-sitter | 代码 AST 解析 |
| rusqlite | SQLite 数据库 |
| notify | 文件变更监听 |
| clap | CLI 参数解析 |
| tracing | 日志/诊断 |
| uuid | 会话 ID 生成 |
| thiserror | 错误类型 derive |

</Config>

<Debugging>

## 调试指南

### 日志

```bash
# 设置日志级别
RUST_LOG=debug cargo run --bin vbw-daemon

# 仅 vibewisp crate 的日志
RUST_LOG=vibewisp=debug cargo run --bin vbw-daemon

# tracing-subscriber 已配置 env-filter
```

### 常见问题

1. **daemon 连接失败**：确保先启动 `vbw-daemon`，默认监听 `[::1]:50051`
2. **ANTHROPIC_API_KEY 未设置**：配置 `llm.api_key` 或设置环境变量
3. **CodeGraph 索引为空**：首次使用会自动触发后台索引构建，稍后重试
4. **会话丢失**：当前使用 InMemorySessionStore，重启 daemon 后会话消失

### TODO

详见 `docs/TODO.md`，包括：
- 对话历史 token 计数和裁剪
- Session 持久化
- CodeGraph 支持更多语言
- 全量索引构建进度反馈

</Debugging>
