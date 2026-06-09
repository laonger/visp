# visp — 轻量级 AI 编程助手

[![Rust](https://img.shields.io/badge/rust-stable-orange.svg)](https://www.rust-lang.org/)
[![License: MPL-2.0](https://img.shields.io/badge/License-MPL_2.0-brightgreen.svg)](LICENSE)

**visp** 是一个用 Rust 编写的轻量级 AI 编程助手后端，采用前后端分离的 daemon 架构，通过 gRPC (tonic) 提供 AI 辅助编程能力。

> 核心目标：利用 Rust 的零成本抽象、无 GC、高效并发特性，解决原 Node.js 实现 CPU 占用偏高的问题。

---

## 架构概览

```
┌─────────────────────────────────────────────────────────────┐
│  前端层                                                      │
│  ┌──────────┐  ┌──────────┐  ┌──────────┐                  │
│  │   CLI    │  │  VSCode  │  │   Web    │  ← 可替换前端      │
│  │ (TUI)    │  │  (未来)   │  │  (未来)   │                  │
│  └────┬─────┘  └──────────┘  └──────────┘                  │
│       │ gRPC                                                │
├───────┴──────────────────────────────────────────────────────┤
│  后端 Daemon                                                │
│  ┌───────────────────────────────────────────────────────┐  │
│  │  Agent 编排器 → LLM Provider → 工具执行 → CodeGraph    │  │
│  └───────────────────────────────────────────────────────┘  │
│                                                              │
│  Launcher: visp daemon → 健康检查 → visp cli               │
└─────────────────────────────────────────────────────────────┘
```

## Crate 列表

每个 crate 有独立的 `README.md`，点击查看详情：

| Crate | 职责 | 详情 |
|-------|------|------|
| [visp](crates/visp/) | Launcher — 一键启动 daemon + CLI | [README](crates/visp/README.md) |
| [visp-core](crates/visp-core/) | 核心抽象层 — Agent/Session/Tool/Prompt/Rules | [README](crates/visp-core/README.md) |
| [visp-proto](crates/visp-proto/) | gRPC 协议定义 + 代码生成 | [README](crates/visp-proto/README.md) |
| [visp-llm](crates/visp-llm/) | LLM 提供器 — Anthropic API 集成 | [README](crates/visp-llm/README.md) |
| [visp-tools](crates/visp-tools/) | 内置工具 — 文件/Bash/搜索/WebFetch/CodeGraph | [README](crates/visp-tools/README.md) |
| [visp-codegraph](crates/visp-codegraph/) | 代码图谱引擎 — tree-sitter + SQLite | [README](crates/visp-codegraph/README.md) |
| [visp-daemon](crates/visp-daemon/) | gRPC 服务端 — 组装所有模块 | [README](crates/visp-daemon/README.md) |
| [visp-cli](crates/visp-cli/) | TUI 客户端 — ratatui 终端界面 | [README](crates/visp-cli/README.md) |

## 核心特性

### Agent 编排循环
用户输入 → LLM → 工具调用 → LLM → ... 的完整循环，支持：
- 流式输出（实时推送 text delta）
- 多工具并行执行（LLM 一次返回多个 tool_use 时并行跑，结果排序拼回上下文）
- 自动重试（网络错误/速率限制，指数退避）
- Thinking 模式（整合 Claude thinking blocks，配置 `thinking_budget_tokens` 控制预算）
- Token 用量统计（每轮对话返回 input/output token 数）
- 最大迭代保护（防止无限循环）

### 工具系统
9 个内置工具，详见 [visp-tools](crates/visp-tools/README.md)：

| 工具 | 功能 |
|------|------|
| `ReadFile` / `WriteFile` / `EditFile` | 文件读写与精确替换 |
| `Bash` | Shell 命令执行（安全黑名单 + 超时控制） |
| `Grep` / `Glob` | 正则搜索 / 文件名搜索 |
| `WebFetch` | 网页内容获取与提取 |
| `CodeGraphSearch` / `CodeGraphGetDetails` | AST 符号搜索与调用链查询 |

### 用户确认
高危工具（如 `Bash`、`WebFetch` 非白名单域名）可通过 `[USER_QUERY]` 机制让用户逐条审批后再执行，支持单条允许/拒绝或一键全部通过。

### 取消机制
基于 `CancellationToken`，用户可随时取消运行中的 Agent 循环，清理进行中的工具调用。

### 会话管理
独立会话生命周期（Idle → Running → Completed/Error），每个会话维护独立的对话历史和 LLM 配置。当前为内存存储，可通过 `SessionStore` trait 替换为持久化实现。

### Skills 技能系统
从 `.visp/skills/` 加载技能定义，用于注入领域知识或工作流指令。每个技能是一个子目录，包含 `SKILL.md`（YAML frontmatter + Markdown 内容），自动合并到 system prompt 中：

```
.visp/skills/
├── my-workflow/
│   └── SKILL.md     # ---\nname: my-workflow\ndescription: ...\n---\n具体指令内容
└── another-skill/
    └── SKILL.md
```

### 规则引擎
从 `.visp/rules/`（项目级）和 `~/.config/visp/rules/`（全局）加载 Markdown 规则，通过 `alwaysApply: true/false` 控制注入时机。同时支持 AGENTS.md 从项目目录向上查找。

### 代码图谱
tree-sitter 解析 + SQLite 索引，支持符号搜索、调用者/被调用者查询、调用路径追踪。支持 TS/TSX、Rust、Python、C/C++、Go。文件监听器自动触发增量索引更新。

### gRPC 通信
`CoderDaemon` 服务，基于 tonic：

| RPC | 类型 | 说明 |
|-----|------|------|
| `Chat` | 双向流 | 核心对话通道 |
| `CreateSession` / `ListSessions` / `DeleteSession` | 一元 | 会话管理 |
| `ReadFile` | 一元 | 快速文件读取（跳过 LLM） |
| `SearchSymbols` / `GetSymbolDetails` | 一元 | 代码符号查询 |
| `HealthCheck` / `Shutdown` | 一元 | 健康检查 / 优雅关闭 |

## 快速开始

### 环境要求

- Rust 稳定版（`rust-toolchain.toml`）
- macOS / Linux
- `ANTHROPIC_API_KEY` 环境变量

### 编译

```bash
cargo build --release
```

### 配置（可选）

创建 `~/.config/visp/daemon.toml`，所有字段均有默认值：

```toml
[daemon]
listen_addr = "[::1]:50051"

[llm]
model = "claude-sonnet-4-20250514"
api_key = "sk-ant-..."          # 或设置 ANTHROPIC_API_KEY 环境变量
base_url = ""                    # 可选：自定义 API 地址
temperature = 0.7
max_tokens = 4096
thinking_budget_tokens = 2048    # 可选：Claude thinking 模式

[tools]
bash_timeout_secs = 120
file_max_size_bytes = 1048576

[agent]
max_iterations = 50
bash_confirm_mode = true

[tool.webfetch]
allow_domains = ["github.com", "docs.rs"]
```

### 运行

```bash
# 一键启动（推荐）
cargo run --bin visp -- -p /path/to/project

# 手动分别启动（调试用）
cargo run --bin visp-daemon              # 终端 1
cargo run --bin visp-cli -- -p <path>    # 终端 2
```

### CLI 参数

| 参数 | 说明 |
|---|---|
| `-p, --project` | 项目路径（默认 `.`） |
| `-a, --addr` | daemon 地址（默认 `[::1]:50051`） |
| `--model` / `--temperature` / `--thinking-budget` | LLM 配置覆盖 |

### TUI 内命令

| 命令 | 用途 |
|---|---|
| `/temp <val>` / `/model <name>` | 设置 LLM 参数 |
| `/init` | 初始化项目配置 |
| `/clear` | 清屏 |
| `/quit` | 退出 |

### 规则文件

创建 `.visp/rules/` 目录，放入 Markdown 文件即可定义 AI 行为规则：

```markdown
---
alwaysApply: true
---

## 代码规范

- 使用 4 空格缩进
- 函数名使用 snake_case
- 结构体使用 CamelCase
```

## 质量门禁

```bash
cargo test && cargo clippy -- -D warnings && cargo fmt -- --check
```

## 技术栈

Rust (edition 2024) · tokio · tonic + prost · serde · ratatui + crossterm · tree-sitter · SQLite (rusqlite) · notify · tracing · reqwest · clap

## 已知限制

- 当前仅支持 Anthropic Claude API（可通过 `base_url` 兼容 OpenAI 接口）
- 对话历史无 token 计数和裁剪
- Session 存储为内存实现（重启丢失）

详见 [docs/TODO.md](docs/TODO.md).

## 许可证

Mozilla Public License 2.0 © [laonger](https://github.com/laonger)
