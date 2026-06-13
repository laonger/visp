# vibe + wisp == VISP ==  轻量级 AI 编程助手

[![Rust](https://img.shields.io/badge/rust-stable-orange.svg)](https://www.rust-lang.org/)
[![License: MPL-2.0](https://img.shields.io/badge/License-MPL_2.0-brightgreen.svg)](LICENSE)

**visp** 是一个用 Rust 编写的轻量级 AI 编程助手后端，采用前后端分离的 daemon 架构，通过 gRPC (tonic) 提供 AI 辅助编程能力。

> 核心目标：利用 Rust 的零成本抽象、无 GC、高效并发特性，解决原 Node.js 实现 CPU 占用偏高的问题。

---

## 架构概览

```
                           visp launcher
                  ① start daemon → ③ start CLI
                        ② health check

┌─────────────────────────────────────────────────────────────┐
│  前端层 (gRPC 客户端)                                       │
│  ┌──────────┐  ┌──────────┐  ┌──────────┐                   │
│  │   CLI    │  │  VSCode  │  │   Web    │  ← 可替换前端     │
│  │ (TUI)    │  │  (未来)  │  │  (未来)  │                   │
│  └────┬─────┘  └──────────┘  └──────────┘                   │
│       │ gRPC                                                │
├───────┴─────────────────────────────────────────────────────┤
│  后端 Daemon                                                │
│  ┌───────────────────────────────────────────────────────┐  │
│  │  Agent 编排器 → LLM Provider → 工具执行 → CodeGraph   │  │
│  │  规则引擎 · Skills · 上下文裁剪 · 会话管理            │  │
│  └───────────────────────────────────────────────────────┘  │
└─────────────────────────────────────────────────────────────┘
```

### 设计思路

visp 采用 **前后端分离的 daemon 架构**，核心决策是让后端（Daemon）保持独立运行，前端（CLI、编辑器插件、Web 界面）通过 gRPC 与之通信。这种分离的好处：

- **语言无关**：前端可以是任何支持 gRPC 的语言，不限于 Rust
- **状态持久**：Daemon 后台常驻，Agent 循环和会话不会因终端关闭中断
- **进程隔离**：UI 卡顿不影响后端工作，后端 OOM 不拖垮编辑器

三个二进制文件的关系：

**Launcher（`visp`）** — 一键式入口。不参与运行时架构，只做启动编排：start daemon → health check → start CLI → CLI 退出后 shutdown daemon。本身启动完即退出。

**Daemon（`visp-daemon`）** — 核心服务进程，常驻后台。负责 Agent 编排、LLM 调用、工具执行、CodeGraph、会话管理、上下文裁剪、规则引擎、Skills 等全部 AI 能力。这是唯一直接使用 AI 模型和系统资源的进程。

**CLI（`visp-cli`）** — TUI 前端（ratatui），通过 gRPC 连接 Daemon，提供聊天界面、审批弹窗、命令系统（`/model`、`/init` 等）。CLI 是默认前端，gRPC 接口同样可被 VSCode 插件、Web 界面等其他前端复用。

## Crate 列表

每个 crate 有独立的 `README.md`，点击查看详情：

| Crate | 职责 | 详情 |
|-------|------|------|
| [visp](crates/visp/) | Launcher — 一键启动 daemon + CLI | [README](crates/visp/README.md) |
| [visp-core](crates/visp-core/) | 核心抽象层 — Agent/Session/Tool/Prompt/Rules | [README](crates/visp-core/README.md) |
| [visp-proto](crates/visp-proto/) | gRPC 协议定义 + 代码生成 | [README](crates/visp-proto/README.md) |
| [visp-llm](crates/visp-llm/) | LLM 提供器 — Anthropic/OpenAI API 集成 | [README](crates/visp-llm/README.md) |
| [visp-tools](crates/visp-tools/) | 内置工具 — 文件/Bash/搜索/WebFetch/CodeGraph | [README](crates/visp-tools/README.md) |
| [visp-codegraph](crates/visp-codegraph/) | 代码图谱引擎 — tree-sitter + SQLite | [README](crates/visp-codegraph/README.md) |
| [visp-context](crates/visp-context/) | 上下文裁剪器 — token 预算 + 轮次剪枝 + 工具输出压缩 | [README](crates/visp-context/README.md) |
| [visp-daemon](crates/visp-daemon/) | gRPC 服务端 — 组装所有模块 | [README](crates/visp-daemon/README.md) |
| [visp-cli](crates/visp-cli/) | TUI 客户端 — ratatui 终端界面 | [README](crates/visp-cli/README.md) |
| [visp-mcp](crates/visp-mcp/) | MCP 客户端 — 连接外部 MCP 服务器获取动态工具 | [README](crates/visp-mcp/README.md) |

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
独立会话生命周期（Idle → Running → Completed/Error），每个会话维护独立的对话历史和 LLM 配置。支持通过 SQLite 持久化保存和恢复会话，`-s <short-id>` 前缀匹配恢复，`/list` 和 `/sessions` 交互式选择。

### 多模型配置
支持在配置文件中配置多个 LLM 模型，每个模型可独立指定 provider、api_key、base_url、temperature、max_tokens 等参数。通过 `/model` 交互式选择器实时切换，切换时自动更新 provider 驱动和全部参数。详见 [`docs/daemon.example.toml`](docs/daemon.example.toml)。

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

### MCP 支持（动态工具扩展）

visp 支持 [Model Context Protocol (MCP)](https://modelcontextprotocol.io/)，可连接外部 MCP 服务器，将其提供的工具动态集成到 Tool Registry 中，与内置工具一视同仁。

**支持的传输方式**：
- **stdio**：daemon 以子进程方式启动 MCP 服务器，通过 stdin/stdout 通信
- **SSE**：通过 HTTP Server-Sent Events 连接已运行的 MCP 服务器

**配置示例**（`~/.config/visp/daemon.toml`）：

```toml
[mcp]

[[mcp.servers]]
name = "playwright"
transport = { type = "stdio", command = "npx", args = ["@anthropic-ai/mcp-playwright"] }

[[mcp.servers]]
name = "filesystem"
transport = { type = "stdio", command = "npx", args = ["@anthropic-ai/mcp-filesystem"] }

[[mcp.servers]]
name = "custom-sse"
transport = { type = "sse", url = "http://localhost:3000/mcp" }
enabled = false  # 可通过 restart API 按需启用
tool_prefix = "custom_"  # 防止工具名冲突
tool_timeout_secs = 120  # 覆盖默认 60s 超时
```

| 配置项 | 说明 | 默认值 |
|--------|------|--------|
| `enabled` | 是否自动连接 | `true` |
| `tool_prefix` | 工具名称前缀，防冲突 | 无 |
| `tool_timeout_secs` | 单次工具调用超时 | `60` |

MCP 工具与内置工具同属一个 Registry，Agent 循环自动识别与调用。工具名冲突时 MCP 工具会被跳过（内置工具优先级更高）。

## 快速开始

### 环境要求

- Rust 稳定版（`rust-toolchain.toml`）
- macOS / Linux
- `ANTHROPIC_API_KEY` 环境变量

### 下载

从 [GitHub Releases](https://github.com/laonger/visp/releases) 下载预编译二进制包（tar.gz）：

| 平台 | 包名 |
|------|------|
| Linux x86_64 | `visp-x86_64-unknown-linux-gnu.tar.gz` |
| macOS ARM | `visp-aarch64-apple-darwin.tar.gz` |

解压后包含 `visp`（启动器）、`visp-daemon`（后台服务）、`visp-cli`（终端界面）三个二进制文件，可直接运行。

### 编译

```bash
cargo build --release
```

### 配置（可选）

配置文件位于 `~/.config/visp/daemon.toml`，所有字段均有默认值，只需填写需要覆盖的项：

```toml
[llm]
model = "claude-sonnet-4-20250514"
api_key = "sk-ant-..."          # 或设置 ANTHROPIC_API_KEY 环境变量
```

不创建也能运行，仅配 `api_key` 和 `model` 即可。完整的注解模板见 [`docs/daemon.example.toml`](docs/daemon.example.toml)，完整结构见 [`config.rs`](crates/visp-daemon/src/config.rs)。

### 规则文件（rules）

项目级规则放在 `.visp/rules/`，全局规则放在 `~/.config/visp/rules/`，Markdown 格式：

```markdown
---
alwaysApply: true
---

## 代码规范

- 使用 4 空格缩进
- 函数名使用 snake_case
```

`alwaysApply: true` 表示无条件注入 prompt，`false` 则按需注入。也支持 `AGENTS.md` 从项目目录向上查找。

### 技能文件（skills）

领域知识或工作流指令放在 `.visp/skills/` 下，每个技能一个子目录，包含 `SKILL.md`：

```
.visp/skills/
├── my-workflow/
│   └── SKILL.md     # YAML frontmatter + Markdown
└── another-skill/
    └── SKILL.md
```

内容格式：`---` 分隔的 YAML frontmatter（`name`、`description`）后接 Markdown 正文，自动合并到 system prompt。详见 [visp-core](crates/visp-core/README.md)。

### 运行

```bash
# 编译
cargo build --release

# 一键启动（推荐）
./target/release/visp -p /path/to/project

# 手动分别启动（调试用）
./target/release/visp-daemon              # 终端 1
./target/release/visp-cli -p /path        # 终端 2

# 恢复 Session（支持 short-id 前缀匹配）
./target/release/visp -p /path -s <session-id-or-prefix>

# 列出所有 Session
./target/release/visp -p /path --list
```

### CLI 参数

| 参数 | 说明 |
|---|---|---|
| `-p, --project` | 项目路径（默认 `.`） |
| `-a, --addr` | daemon 地址（默认 `[::1]:50051`） |
| `-s, --session` | 恢复指定 session（支持 short-id 前缀匹配） |
| `--list` | 列出所有 session |
| `--model` / `--temperature` / `--thinking-budget` | LLM 配置覆盖 |

### TUI 内快捷键

| 按键 | 功能 |
|------|------|
| `Enter` | 发送消息 / 确认审批 |
| `Alt+Enter` | 插入换行（不发送） |
| `Alt+M` / `Ctrl+M` | 切换鼠标捕获模式（任何时候生效） |
| `↑` / `↓` | 上/下一条历史输入 |
| `PgUp` / `PgDn` | 向上/下滚动 10 行 |
| `Tab` | 命令自动补全 |
| `Ctrl+C` | 中断正在生成的请求 |
| `Ctrl+D` | 无条件退出程序 |
| `←` / `→` | 审批弹窗中切换选项 |
| `Esc` | 取消/拒绝审批 |
| 鼠标滚轮 | 滚动对话区域 |

### TUI 内命令

| 命令 | 用途 |
|------|------|
| `/model` | 交互式模型选择器（↑↓ 选择，Enter 切换） |
| `/model <name>` | 直接切换模型 |
| `/temp <val>` | 设置 LLM 温度 |
| `/list` | 交互式 session 选择器 |
| `/sessions` | 列出所有 session（同 `/list`） |
| `/sessions <id>` | 切换到指定 session（支持 short-id） |
| `/new` | 创建新 session |
| `/init` | 初始化项目配置并生成 AGENTS.md |
| `/mouse` | 切换鼠标捕获模式 |
| `/clear` | 清屏 |
| `/help` | 显示帮助 |

## 质量门禁

```bash
cargo test && cargo clippy -- -D warnings && cargo fmt -- --check
```

## 技术栈

Rust (edition 2024) · tokio · tonic + prost · serde · ratatui + crossterm · tree-sitter · SQLite (rusqlite) · notify · tracing · reqwest · clap · rmcp (MCP 协议)

## 已知限制

- Session 存储为 SQLite，重启 daemon 后可恢复（需使用 `-s` 参数）
- 多模型配置下切换模型时动态创建 provider，切换后新 agent loop 生效

详见 [docs/TODO.md](docs/TODO.md).

## 许可证

Mozilla Public License 2.0 © [laonger](https://github.com/laonger)
