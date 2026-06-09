# visp — 轻量级 AI 编程助手

[![Rust](https://img.shields.io/badge/rust-stable-orange.svg)](https://www.rust-lang.org/)
[![License: MPL-2.0](https://img.shields.io/badge/License-MPL_2.0-brightgreen.svg)](LICENSE)

**visp** 是一个用 Rust 编写的轻量级 AI 编程助手后端，是 OpenCode 的 Rust 重写版。采用前后端分离的 daemon 架构，通过 gRPC (tonic) 提供 AI 辅助编程能力。

> 核心目标：利用 Rust 的零成本抽象、无 GC、高效并发特性，解决原 Node.js 实现 CPU 占用偏高的问题。

---

## 架构概览

```
┌──────────────────────────────────────────────────────────────────┐
│  前端层（可替换）                                                  │
│  ┌──────────┐  ┌──────────┐  ┌──────────┐                       │
│  │   CLI    │  │  VSCode  │  │   Web    │  ← 可替换前端           │
│  │ (TUI)    │  │  (未来)   │  │  (未来)   │                       │
│  └────┬─────┘  └──────────┘  └──────────┘                       │
│       │ gRPC (tonic)                                            │
├───────┴──────────────────────────────────────────────────────────┤
│  后端 Daemon (visp-daemon)                                       │
│  ┌────────────────────────────────────────────────────────────┐  │
│  │                  gRPC Server (tonic)                        │  │
│  ├────────────────────────────────────────────────────────────┤  │
│  │  Agent 编排器（核心循环: 输入→LLM→工具→LLM→...）             │  │
│  ├────────────────────────────────────────────────────────────┤  │
│  │  ├─ Session Manager   ─  会话生命周期管理                   │  │
│  │  ├─ Prompt Builder    ─  prompt 组装                       │  │
│  │  ├─ Rule Engine       ─  规则文件加载                       │  │
│  │  ├─ Tool Registry     ─  工具注册/执行                      │  │
│  │  ├─ LLM Provider      ─  Anthropic API 集成                │  │
│  │  ├─ Tool Executors    ─  文件读写 / bash / 搜索 / 网络获取  │  │
│  │  └─ CodeGraph Engine  ─  tree-sitter + SQLite 代码智能引擎  │  │
│  └────────────────────────────────────────────────────────────┘  │
│                                                                   │
│  Launcher（visp）                                                  │
│  └─ 一键启动: 启动 daemon → 等待就绪 → 启动 CLI                   │
└──────────────────────────────────────────────────────────────────┘
```

## 项目结构

```
vibewisp/
├── Cargo.toml                  # Workspace 根配置
├── AGENTS.md                   # Agent 行为指南
├── rust-toolchain.toml         # Rust 工具链配置
├── config/                     # 默认配置文件
├── docs/                       # 设计文档与计划
│   ├── design/                 # 各阶段设计文档
│   └── plans/                  # 各阶段开发计划
│
├── crates/
│   ├── visp/                   # ◀ Launcher（一键启动 daemon + CLI）
│   │   └── main.rs             #   启 daemon → 健康检查 → 启 CLI
│   │
│   ├── visp-core/              # ◀ 核心逻辑（无 IO 依赖）
│   │   ├── agent.rs            #   Agent 编排器核心循环
│   │   ├── session.rs          #   会话管理与状态机
│   │   ├── message.rs          #   消息模型（Role/Message/ToolCall）
│   │   ├── prompt.rs           #   Prompt 构建器
│   │   ├── provider.rs         #   LlmProvider trait 定义
│   │   ├── tool.rs             #   Tool trait 定义
│   │   ├── tool_registry.rs    #   工具注册表
│   │   ├── rules.rs            #   规则引擎
│   │   └── error.rs            #   错误类型体系
│   │
│   ├── visp-proto/             # ◀ gRPC 协议定义
│   │   └── proto/
│   │       └── visp.proto      #   protobuf 服务/消息定义
│   │
│   ├── visp-llm/               # ◀ LLM 提供器
│   │   ├── anthropic.rs        #   Anthropic API 集成（SSE 流解析）
│   │   ├── streaming.rs        #   SSE 事件解析器
│   │   └── mock.rs             #   测试用 Mock Provider
│   │
│   ├── visp-tools/             # ◀ 内置工具
│   │   ├── file.rs             #   文件读写（ReadFile/WriteFile/EditFile）
│   │   ├── bash.rs             #   Shell 命令执行
│   │   ├── search.rs           #   内容搜索（Grep/Glob，优先 ripgrep）
│   │   ├── codegraph.rs        #   代码符号搜索与详情
│   │   ├── fetch.rs            #   网页内容获取（WebFetch）
│   │   ├── path.rs             #   路径安全验证
│   │   └── truncate.rs         #   输出截断工具
│   │
│   ├── visp-codegraph/         # ◀ 代码图谱引擎
│   │   ├── parser.rs           #   tree-sitter 解析器（TS/TSX、Rust、Python、C/C++、Go）
│   │   ├── graph.rs            #   图数据结构（Symbol/Edge/SymbolKind）
│   │   ├── store.rs            #   SQLite 持久化层
│   │   ├── index.rs            #   全量/增量索引构建
│   │   ├── query.rs            #   符号查询引擎
│   │   └── watcher.rs          #   文件变更监听
│   │
│   ├── visp-daemon/            # ◀ 后端常驻进程
│   │   ├── main.rs             #   入口：模块组装与启动
│   │   ├── server.rs           #   gRPC 服务器
│   │   ├── service.rs          #   gRPC 服务实现
│   │   ├── config.rs           #   TOML 配置加载
│   │   └── command/            #   内置命令处理
│   │
│   └── visp-cli/               # ◀ TUI 前端
│       ├── main.rs             #   入口：参数解析与连接
│       ├── client.rs           #   gRPC 客户端封装
│       ├── app.rs              #   应用状态管理 + Markdown 渲染
│       ├── event.rs            #   事件循环（键盘 + gRPC 消息）
│       ├── ui.rs               #   ratatui 渲染（对话区、输入区、状态栏）
│       └── theme.rs            #   颜色/样式管理
│
└── .visp/                      # 项目级配置（由规则引擎读取）
    └── rules/                  # 规则文件目录
```

## 核心特性

### Agent 编排器（`visp-core`）

"用户输入 → LLM → 工具调用 → LLM → ..."核心循环：

- **流式输出**：LLM 响应实时流式传输给前端
- **多工具并行**：LLM 一次返回多个工具调用时，并行执行后排序拼回上下文
- **自动重试**：网络错误和速率限制自动重试（指数退避）
- **用户确认**：高危工具可要求用户批准后再执行
- **取消机制**：支持取消正在运行的 Agent 循环
- **最大迭代保护**：防止无限循环

### 会话管理（`visp-core`）

- 独立的会话生命周期（Idle → Running → Completed / Error）
- 每个会话维护独立的对话历史和 LLM 配置
- 基于 `SessionStore` trait 的存储抽象（当前为内存实现，可替换为持久化）

### 工具系统（`visp-tools`）

| 工具 | 功能 | 安全特性 |
|---|---|---|
| `ReadFile` | 读取文件内容 | 大小限制、二进制检测、路径校验 |
| `WriteFile` | 写入文件（覆盖） | 自动创建父目录、路径校验 |
| `EditFile` | 精确字符串替换 | 原子写入（temp + rename）、多匹配拒绝 |
| `Bash` | 执行 shell 命令 | 黑名单（sudo/rm -rf）、超时控制 |
| `Grep` | 正则内容搜索 | 优先 ripgrep、排除二进制文件 |
| `Glob` | 文件名通配符搜索 | 优先 ripgrep、递归搜索 |
| `WebFetch` | 获取网页内容并提取 | 协议白名单、域名白名单、响应大小限制 |
| `CodeGraphSearch` | AST 符号搜索 | 基于 tree-sitter 解析 |
| `CodeGraphGetDetails` | 符号详情查询 | 含调用者/被调用者关系 |

### 规则引擎（`visp-core`）

- 从 `.visp/rules/`（项目级）和 `~/.config/visp/rules/`（全局）加载规则
- 同时支持 AGENTS.md（从项目目录向上寻找）
- 支持 `alwaysApply: true/false` 标记
- 规则自动合并到 system prompt

### 代码图谱引擎（`visp-codegraph`）

基于 tree-sitter + SQLite 的代码智能引擎：

- 符号解析、关系提取、导入/导出跟踪
- 全量构建 + 文件监听增量更新
- 符号搜索、调用者/被调用者查询、符号详情
- 支持语言：TypeScript / TSX、Rust、Python、C / C++、Go

### LLM 提供器（`visp-llm`）

Anthropic Claude API 集成：

- 消息格式转换（visp-core ↔ Anthropic Messages API）
- SSE 流解析（text_delta / input_json_delta / thinking 等事件）
- 工具调用输入增量累积
- 支持自定义 `base_url`（兼容 OpenAI 兼容 API）

### 通信协议（gRPC）

`CoderDaemon` 服务：

| RPC 方法 | 类型 | 说明 |
|---|---|---|
| `CreateSession` | 一元 | 创建新的编码会话 |
| `ListSessions` | 一元 | 列出所有活跃会话 |
| `DeleteSession` | 一元 | 删除会话 |
| `Chat` | **双向流** | 核心对话通道（流式输入/输出） |
| `ReadFile` | 一元 | 快速文件读取（跳过 LLM） |
| `SearchSymbols` | 一元 | 代码符号搜索 |
| `GetSymbolDetails` | 一元 | 符号详情（含调用者/被调用者） |
| `HealthCheck` | 一元 | 健康检查 |
| `Shutdown` | 一元 | 优雅关闭 |

## 快速开始

### 环境要求

- Rust 稳定版（参见 `rust-toolchain.toml`）
- macOS / Linux
- `ANTHROPIC_API_KEY` 环境变量（或配置文件中设置）

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
provider = "anthropic"
model = "claude-sonnet-4-20250514"
api_key = "sk-ant-..."        # 或设置 ANTHROPIC_API_KEY 环境变量
base_url = ""                  # 可选：自定义 API 地址
temperature = 0.7
max_tokens = 4096
thinking_budget_tokens = 2048  # 可选：Claude thinking 模式

[tools]
bash_timeout_secs = 120
file_max_size_bytes = 1048576

[agent]
max_iterations = 50
llm_retry_attempts = 3
bash_confirm_mode = true

[tool.webfetch]                # 可选：WebFetch 额外配置
allow_domains = ["github.com", "docs.rs"]
```

### 运行

```bash
# 推荐：一键启动
cargo run --bin visp -- -p /path/to/project

# 或手动分别启动（调试用）
cargo run --bin visp-daemon              # 终端 1
cargo run --bin visp-cli -- -p <path>    # 终端 2
```

### CLI 参数

| 参数 | 说明 |
|---|---|
| `-p, --project` | 项目路径（默认当前目录） |
| `-a, --addr` | daemon 地址（默认 `[::1]:50051`） |
| `--model` | 指定模型 |
| `--temperature` | 设置温度 |
| `--thinking-budget` | Claude thinking 模式预算 token 数 |

### TUI 内命令

| 命令 | 用途 |
|---|---|
| `/quit` / `/exit` | 退出 |
| `/clear` | 清屏 |
| `/temp <val>` | 设置温度 |
| `/model <name>` | 切换模型 |
| `/init` | 初始化项目配置 |
| `/help` | 显示帮助 |

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

## 技术栈

| 领域 | 选择 |
|---|---|
| 语言 | Rust (edition 2024) |
| 异步运行时 | tokio |
| gRPC | tonic + prost |
| 序列化 | serde + serde_json |
| LLM API | Anthropic Claude |
| TUI | ratatui + crossterm + syntect |
| 代码解析 | tree-sitter |
| 持久化 | SQLite (rusqlite) |
| 文件监听 | notify |
| 日志 | tracing |
| HTTP 客户端 | reqwest |
| CLI 参数 | clap |

## 已知限制

- 当前仅支持 Anthropic Claude API（可通过 `base_url` 配置 OpenAI 兼容接口）
- 对话历史无 token 计数和裁剪（超长对话可能溢出）
- Session 存储为内存实现（重启丢失）

详见 [docs/TODO.md](docs/TODO.md)。

## 许可证

Mozilla Public License 2.0 © [laonger](https://github.com/laonger)
