# vibe + wisp == VISP ==  轻量级 AI 编程助手

[![Rust](https://img.shields.io/badge/rust-stable-orange.svg)](https://www.rust-lang.org/)
[![License: MPL-2.0](https://img.shields.io/badge/License-MPL_2.0-brightgreen.svg)](LICENSE)

**visp** 是一个用 Rust 编写的轻量级 AI 编程助手后端，采用前后端分离的 daemon 架构，通过 gRPC (tonic) 提供 AI 辅助编程能力。

> 核心目标：利用 Rust 的零成本抽象、无 GC、高效并发特性，解决原 Node.js 实现 CPU 占用偏高的问题。

---

## 架构概览

```
  visp launcher
    │
    ├─ ① start daemon → ② health check → ③ start CLI → ④ exit
    ▼
┌────────────────┐                    ┌────────────────────────────┐
│  前端层 (gRPC)  │                    │  Daemon                    │
├────────────────┤                    │                            │
│  CLI (TUI)     │◄─── CoderDaemon ──►│  Agent 编排器              │
│  VSCode (未来)  │    service        │  → LLM Provider            │
│  Web (未来)     │                    │  → 工具执行                │
│                 │                    │  → CodeGraph 代码图谱      │
│                 │                    │  → 规则引擎 + Skills      │
│                 │                    │  → 上下文裁剪             │
│                 │                    │  → 会话管理               │
└────────────────┘                    └────────────────────────────┘
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
| [visp-context](crates/visp-context/) | 上下文裁剪器 — token 预算 + 轮次剪枝 + 工具输出压缩 | [README](crates/visp-context/README.md) |
| [visp-daemon](crates/visp-daemon/) | gRPC 服务端 — 组装所有模块 | [README](crates/visp-daemon/README.md) |
| [visp-cli](crates/visp-cli/) | TUI 客户端 — ratatui 终端界面 | [README](crates/visp-cli/README.md) |

## 核心特性

### Agent 编排循环
用户输入 → LLM → 工具调用 → LLM → ... 的完整循环，支持流式输出、多工具并行、自动重试、Thinking 模式、迭代保护。详见 [visp-core](crates/visp-core/README.md)。

### 工具系统
9 个内置工具（文件读写、bash 执行、搜索、网页获取、代码图谱），详见 [visp-tools](crates/visp-tools/README.md)。

### 权限系统
工具执行前经过多级审批检查，用户通过弹窗审批，支持"允许/拒绝/始终允许"：

- **工具级审批**：每个工具可定义是否需要审批。`WriteFile`/`EditFile` 始终弹窗，`Bash` 仅对危险命令（`rm`、`dd`、`>` 等）弹窗，`WebFetch` 对非白名单域名弹窗，搜索/读取类工具默认放行
- **Always Allow**：审批时选择"始终允许"后，该工具在当前会话内不再弹窗
- **Bash 确认模式**：配置 `bash_confirm_mode = true/false`（默认 `true`）控制是否对危险命令审批
- **WebFetch 域名白名单**：支持两层白名单——daemon 级（`[tool.webfetch].allow_domains`）和项目级（`.visp/webfetch.toml`），命中白名单自动放行

**审批流程**：LLM 请求执行工具 → `requires_approval_for(args)` → 已始终允许？→ 执行；否则弹出对话框 → 用户选择【允许 / 拒绝 / 始终允许】。

### 会话管理
独立会话生命周期（Idle → Running → Completed/Error），每个会话维护独立的对话历史和 LLM 配置。当前为内存存储，可通过 `SessionStore` trait 替换为持久化实现。

### 上下文裁剪
长对话自动管理 context window，通过三段式剪枝（HEAD/MIDDLE/TAIL）和工具输出压缩控制 token 用量。详见 [visp-context](crates/visp-context/README.md)。

### Skills 技能系统
从 `.visp/skills/` 加载技能定义（YAML frontmatter + Markdown），自动合并到 system prompt 中，用于注入领域知识或工作流指令。详见 [visp-core](crates/visp-core/README.md)。

### 规则引擎
从 `.visp/rules/`（项目级）和 `~/.config/visp/rules/`（全局）加载 Markdown 规则，通过 `alwaysApply: true/false` 控制注入时机。同时支持 AGENTS.md 从项目目录向上查找。

### 代码图谱
tree-sitter 解析 + SQLite 索引，支持符号搜索、调用者/被调用者查询、调用路径追踪。支持 TS/TSX、Rust、Python、C/C++、Go。文件监听器自动触发增量索引更新。

### gRPC 通信
`CoderDaemon` 服务基于 tonic，提供 Chat（双向流）、会话管理、文件读取、符号查询、健康检查等 RPC。详见 [visp-proto](crates/visp-proto/README.md)。

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
max_context_tokens = 128000       # 可选：上下文窗口大小，默认 128K
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
- Session 存储为内存实现（重启丢失）

详见 [docs/TODO.md](docs/TODO.md).

## 许可证

Mozilla Public License 2.0 © [laonger](https://github.com/laonger)
