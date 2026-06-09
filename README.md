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

- **Agent 编排循环**：用户输入 → LLM → 工具调用 → LLM → ...，支持流式输出、多工具并行、自动重试、用户确认、取消机制
- **会话管理**：独立会话生命周期（Idle → Running → Completed/Error），内存存储（可替换持久化）
- **内置工具**：文件读写、Bash 执行、Grep/Glob 搜索、WebFetch 网页获取、CodeGraph 代码分析
- **规则引擎**：从 `.visp/rules/` 和 `~/.config/visp/rules/` 加载 Markdown 规则，自动注入 system prompt
- **代码图谱**：tree-sitter 解析 + SQLite 索引，支持 TS/TSX、Rust、Python、C/C++、Go
- **gRPC 协议**：`CoderDaemon` 服务，提供 Chat 双向流、会话管理、文件读取、符号搜索等 RPC

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
