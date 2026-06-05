# vibewisp — 轻量级 AI 编程助手

[![Rust](https://img.shields.io/badge/rust-stable-orange.svg)](https://www.rust-lang.org/)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](LICENSE)

**vibewisp** 是一个用 Rust 编写的轻量级 AI 编程助手后端，是 [OpenCode](https://github.com) 的 Rust 重写版。它采用前后端分离的 daemon 架构，通过 gRPC 提供 AI 辅助编程能力。

> 核心目标：利用 Rust 的零成本抽象、无 GC、高效并发特性，解决原 Node.js 实现 CPU 占用偏高的问题。

---

## 架构概览

```
┌────────────────────────────────────────────────────────────────────────┐
│  前端层                                                               │
│  ┌──────────┐  ┌──────────┐  ┌──────────┐                            │
│  │   CLI    │  │  VSCode  │  │   Web    │  ← 可替换前端               │
│  │ (已实现)  │  │  (未来)   │  │  (未来)   │                            │
│  └────┬─────┘  └──────────┘  └──────────┘                            │
│       │ gRPC (tonic)                                                 │
├───────┼────────────────────────────────────────────────────────────────┤
│       │  后端 Daemon (Rust)                                           │
│  ┌────┴──────────────────────────────────────────────────────────┐   │
│  │                    gRPC Server (tonic)                         │   │
│  ├───────────────────────────────────────────────────────────────┤   │
│  │  Agent 编排器 (核心循环: 输入→LLM→工具→LLM→...)               │   │
│  ├───────────────────────────────────────────────────────────────┤   │
│  │  ├─ Session Manager   ─  会话生命周期管理                      │   │
│  │  ├─ Prompt Builder    ─  prompt 组装                          │   │
│  │  ├─ Rule Engine       ─  规则文件加载                          │   │
│  │  ├─ Tool Registry     ─  工具注册/执行                         │   │
│  │  ├─ LLM Provider      ─  OpenAI / Anthropic / 兼容 API        │   │
│  │  ├─ Tool Executors    ─  文件读写 / bash / 搜索 等内置工具      │   │
│  │  └─ CodeGraph Engine  ─  tree-sitter + SQLite 代码智能引擎     │   │
│  └───────────────────────────────────────────────────────────────┘   │
└────────────────────────────────────────────────────────────────────────┘
```

## 项目结构

```
vibewisp/
├── Cargo.toml                  # Workspace 根配置
├── AGENTS.md                   # Agent 行为定义（系统 prompt 模板）
├── rust-toolchain.toml         # Rust 工具链配置
├── config/                     # 默认配置文件
├── docs/                       # 设计文档与计划
│   ├── design/                 # 各阶段设计文档
│   └── plans/                  # 各阶段开发计划
│
├── crates/
│   ├── vbw-core/               # ◀ 核心逻辑（无 IO 依赖）
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
│   ├── vbw-proto/              # ◀ gRPC 协议定义
│   │   └── proto/
│   │       └── vibewisp.proto  #   protobuf 服务/消息定义
│   │
│   ├── vbw-llm/                # ◀ LLM 提供器
│   │   ├── anthropic.rs        #   Anthropic API 集成（SSE 流解析）
│   │   ├── streaming.rs        #   SSE 事件解析器
│   │   └── mock.rs             #   测试用 Mock Provider
│   │
│   ├── vbw-tools/              # ◀ 内置工具
│   │   ├── file.rs             #   文件读写（ReadFile/WriteFile/EditFile）
│   │   ├── bash.rs             #   Shell 命令执行
│   │   ├── search.rs           #   内容搜索（Grep/Glob，优先 ripgrep）
│   │   ├── path.rs             #   路径安全验证
│   │   └── truncate.rs         #   输出截断工具
│   │
│   ├── vbw-codegraph/          # ◀ 代码图谱引擎
│   │   ├── parser.rs           #   tree-sitter 解析器（TypeScript/TSX）
│   │   ├── graph.rs            #   图数据结构（Symbol/Edge/SymbolKind）
│   │   ├── store.rs            #   SQLite 持久化层
│   │   ├── index.rs            #   全量/增量索引构建
│   │   ├── query.rs            #   符号查询引擎
│   │   └── watcher.rs          #   文件变更监听
│   │
│   ├── vbw-daemon/             # ◀ 后端常驻进程
│   │   ├── main.rs             #   入口：模块组装与启动
│   │   ├── server.rs           #   gRPC 服务器
│   │   ├── service.rs          #   gRPC 服务实现
│   │   └── config.rs           #   TOML 配置加载
│   │
│   └── vbw-cli/                # ◀ CLI 前端
│       ├── main.rs             #   入口：参数解析与连接
│       ├── client.rs           #   gRPC 客户端封装
│       ├── repl.rs             #   REPL 交互模式
│       └── display.rs          #   终端输出格式化
│
└── .vibewisp/                  # 项目级配置（由规则引擎读取）
    └── rules/                  # 规则文件目录
```

## 核心特性

### Agent 编排器（`vbw-core`）

实现"用户输入 → LLM → 工具调用 → LLM → ..."的核心循环，支持：

- **流式输出**：LLM 响应实时流式传输给前端
- **多工具并行**：LLM 一次返回多个工具调用时，并行执行后排序拼回上下文
- **自动重试**：网络错误和速率限制自动重试（指数退避）
- **用户确认**：高危工具可要求用户批准后再执行
- **取消机制**：基于 `CancellationToken` 的取消支持
- **最大迭代保护**：防止无限循环

### 会话管理（`vbw-core`）

- 独立的会话生命周期（Idle → Running → Completed / Error）
- 每个会话维护独立的对话历史和 LLM 配置
- 基于 `SessionStore` trait 的存储抽象（当前为内存实现，可替换为持久化）

### 工具系统（`vbw-tools`）

| 工具 | 功能 | 安全特性 |
|---|---|---|
| `ReadFile` | 读取文件内容 | 1MB 大小限制、二进制检测、路径安全校验 |
| `WriteFile` | 写入文件（覆盖） | 自动创建父目录、路径安全校验 |
| `EditFile` | 精确字符串替换 | 原子写入（temp + rename）、多匹配拒绝 |
| `Bash` | 执行 shell 命令 | 黑名单（sudo/rm -rf 等）、超时控制 |
| `Grep` | 正则内容搜索 | 优先 ripgrep、自动排除二进制文件 |
| `Glob` | 文件名通配符搜索 | 优先 ripgrep、递归搜索 |

### 规则引擎（`vbw-core`）

- 从 `.vibewisp/rules/`（项目级）和 `~/.config/vibewisp/rules/`（全局）加载规则
- 支持 `alwaysApply: true/false` 标记
- 规则自动合并到 system prompt 中

### 代码图谱引擎（`vbw-codegraph`）

基于 tree-sitter + SQLite 的代码智能引擎：

- **符号解析**：函数、类、方法、接口、类型别名、枚举、变量
- **关系提取**：调用关系、引用、继承、实现
- **导入/导出**：跟踪模块间依赖
- **全量构建 + 增量更新**：首次全量索引，后续通过文件监听增量更新
- **查询能力**：符号搜索（前缀匹配）、调用者/被调用者查询、符号详情
- **支持语言**：TypeScript / TSX（当前，可扩展）

### LLM 提供器（`vbw-llm`）

当前实现了 **Anthropic Claude** API 集成：

- 消息格式转换（vbw-core 通用格式 ↔ Anthropic Messages API）
- SSE 流解析（支持 `text_delta` / `input_json_delta` / `thinking` 等事件类型）
- 工具调用输入增量累积
- 错误处理（重试、速率限制、认证错误）

### 通信协议（gRPC）

protobuf 定义的服务 `CoderDaemon`：

| RPC 方法 | 类型 | 说明 |
|---|---|---|
| `CreateSession` | 一元 | 创建新的编码会话 |
| `ListSessions` | 一元 | 列出所有活跃会话 |
| `DeleteSession` | 一元 | 删除会话 |
| `Chat` | **双向流** | 核心对话通道 |
| `ReadFile` | 一元 | 快速文件读取（跳过 LLM） |
| `SearchSymbols` | 一元 | 代码符号搜索 |
| `GetSymbolDetails` | 一元 | 符号详情（含调用者/被调用者） |
| `HealthCheck` | 一元 | 健康检查 |
| `Shutdown` | 一元 | 优雅关闭 |

## 快速开始

### 环境要求

- Rust 稳定版（参见 `rust-toolchain.toml`）
- macOS / Linux（Windows 支持有限）

### 编译

```bash
cargo build --release
```

### 配置

创建配置文件 `~/.config/vibewisp/daemon.toml`：

```toml
[daemon]
listen_addr = "[::1]:50051"
log_level = "info"

[llm]
provider = "anthropic"
model = "claude-sonnet-4-20250514"
api_key = "sk-ant-..."  # 或通过 ANTHROPIC_API_KEY 环境变量设置
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

### 运行

```bash
# 启动后端 daemon
cargo run --release --bin vbw-daemon

# 在另一个终端启动 CLI
cargo run --release --bin vbw -- --project /path/to/your/project
```

### CLI 命令

进入 REPL 后：

| 命令 | 用途 |
|---|---|
| `/quit` / `/exit` | 退出 |
| `/clear` | 清屏 |
| `/temp <val>` | 设置温度（如 `/temp 0.5`） |
| `/model <name>` | 切换模型（如 `/model claude-sonnet-4-20250514`） |
| `/help` | 显示帮助 |

### 规则文件

创建 `.vibewisp/rules/` 目录，放入 Markdown 文件即可定义 AI 行为规则：

```markdown
---
alwaysApply: true
---

## 代码规范

- 使用 2 空格缩进
- 函数名使用 camelCase
```

## 开发状态

| 模块 | 状态 | 说明 |
|---|---|---|
| `vbw-core` | ✅ 完成 | Agent 循环、会话管理、Prompt 构建、规则引擎、工具注册表 |
| `vbw-proto` | ✅ 完成 | gRPC 协议定义 |
| `vbw-llm` | ✅ 完成 | Anthropic 提供器、SSE 解析、Mock |
| `vbw-tools` | ✅ 完成 | 文件读写、Bash、Grep/Glob、路径安全、截断 |
| `vbw-codegraph` | ✅ 完成 | tree-sitter 解析、SQLite 存储、全量/增量索引、查询、监听 |
| `vbw-daemon` | ✅ 完成 | 模块组装、gRPC 服务、配置加载 |
| `vbw-cli` | ✅ 完成 | 客户端、REPL、显示格式化 |
| VSCode 插件 | 🔲 未开始 | 计划中 |
| MCP 客户端 | 🔲 未开始 | 计划中 |
| Agent 委派 | 🔲 未开始 | 计划中 |
| 会话持久化 | 🔲 未开始 | 计划中 |

## 已知限制

- 当前仅支持 Anthropic Claude API
- CodeGraph 仅支持 TypeScript/TSX 解析
- 对话历史无 token 计数和裁剪（超长对话可能溢出）
- Session 存储为内存实现（重启丢失）
- 全量索引构建无进度反馈

详见 [docs/TODO.md](docs/TODO.md)。

## 技术栈

| 领域 | 选择 |
|---|---|
| 语言 | Rust (edition 2024) |
| 异步运行时 | tokio |
| gRPC | tonic + prost |
| 序列化 | serde + serde_json |
| LLM API | Anthropic Claude |
| 代码解析 | tree-sitter |
| 持久化 | SQLite (rusqlite) |
| 文件监听 | notify |
| 日志 | tracing |
| CLI | clap + rustyline |

## 许可证

MIT License
