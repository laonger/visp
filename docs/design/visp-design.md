# visp 轻量设计文档

## 1. 动机与目标

### 1.1 问题

现有 OpenCode（Node.js/TypeScript 实现）存在持续偏高的 CPU 占用，影响开发体验。

### 1.2 目标

用 Rust 重写 OpenCode 核心后端，实现：

- **降低 CPU 占用**：利用 Rust 的零成本抽象、无 GC、高效并发，显著减少 CPU 开销
- **架构解耦**：前后端分离，后端作为独立 daemon 常驻，前端可替换（CLI / VSCode / Web）
- **性能优势模块**：CodeGraph（tree-sitter 解析和索引）在 Rust 中可获得最大的性能提升

### 1.3 非目标（MVP 不做）

- VSCode 插件前端
- Agent 委派（多 specialist 调度）
- Web UI
- 完整的插件/扩展市场

---

## 2. 整体架构

### 2.1 架构图

```
┌──────────────────────────────────────┐
│           前端层 (Frontend)           │
│                                      │
│  ┌──────────┐  ┌──────────┐         │
│  │   CLI    │  │  VSCode  │  ...    │
│  │ (MVP)    │  │ (未来)    │         │
│  └────┬─────┘  └──────────┘         │
│       │ gRPC (tonic)                 │
└───────┼──────────────────────────────┘
        │
┌───────┴──────────────────────────────┐
│          后端层 (Rust Daemon)        │
│                                      │
│  ┌────────────────────────────┐     │
│  │     gRPC Server (tonic)    │     │
│  └────────────┬───────────────┘     │
│               │                      │
│  ┌────────────┴───────────────┐     │
│  │     Agent Orchestrator     │     │
│  │  (核心循环: 输入→LLM→工具→LLM) │ │
│  └───┬──────┬──────┬──────┬───┘     │
│      │      │      │      │         │
│  ┌───┴──┐┌──┴───┐┌─┴───┐┌┴──────┐ │
│  │Session││Prompt││Rule ││Tool   │ │
│  │Manager││Builder││Engine││Registry│ │
│  └──────┘└──────┘└─────┘└───┬───┘ │
│                             │      │
│  ┌──────────────┬───────────┴──┐   │
│  │  LLM Provider│  Tool Exec   │   │
│  │  (OpenAI etc)│  (file/bash/ │   │
│  │              │   search/MCP)│   │
│  └──────────────┴──────────────┘   │
│                                     │
│  ┌─────────────────────────────┐   │
│  │  CodeGraph Engine           │   │
│  │  (tree-sitter + SQLite)     │   │
│  └─────────────────────────────┘   │
│                                     │
│  ┌─────────────────────────────┐   │
│  │  File Watcher (notify)      │   │
│  └─────────────────────────────┘   │
└─────────────────────────────────────┘
```

### 2.2 Crate 依赖关系

```
visp-cli ──────► visp-proto ──────► visp-core
                     ▲                   │
visp-daemon ──────────┘      ┌────────────┼────────────────┐
                            ▼            ▼                ▼
                        visp-llm    visp-tools    visp-codegraph
                                                      │
                                                 visp-mcp
```

- **visp-core**：纯逻辑 crate，无 IO 依赖。定义核心 trait 和类型。
- **visp-proto**：gRPC 协议定义，生成的客户端/服务端代码。
- **visp-llm**：LLM 提供商抽象和实现。
- **visp-tools**：内置工具的具体实现。
- **visp-codegraph**：代码智能引擎，依赖 tree-sitter。
- **visp-mcp**：MCP 协议客户端实现。
- **visp-daemon**：daemon 二进制，组装所有模块。
- **visp-cli**：CLI 二进制，gRPC 客户端 + TUI。

---

## 3. 核心模块职责

### 3.1 Agent Orchestrator（代理编排器）

**职责**：实现 "用户输入 → LLM → 工具调用 → LLM → ..." 的核心循环。

**关键行为**：

1. 接收用户消息（来自 gRPC 流）
2. 从 Rule Engine 获取系统规则，从 Tool Registry 获取可用工具定义
3. 构建完整 prompt（system + rules + tools + conversation history）
4. 调用 LLM Provider，流式接收响应
5. 如 LLM 返回工具调用：通过 Tool Registry 执行工具，将结果拼回上下文，回到步骤 3
6. 如 LLM 返回文本：流式传回前端
7. 循环直到 LLM 返回最终响应（无工具调用）

**状态管理**：每个会话独立运行一个 Agent 实例，维护自己的对话历史和上下文状态。

**可配置项**：
- 最大迭代次数（防无限循环）
- 超时时间
- 温度等 LLM 参数
- 可用工具白名单

### 3.2 Session Manager（会话管理器）

**职责**：管理多个并发会话的生命周期。

**关键行为**：

- 创建会话：生成唯一 ID，初始化 Agent 实例，加载项目配置
- 列出会话：返回所有活跃会话及其状态
- 删除会话：清理资源，终止正在运行的 Agent
- 会话持久化：将会话状态写入 SQLite，支持 daemon 重启后恢复

**会话状态机**：`Idle → Running → Completed / Error`

### 3.3 Prompt Builder（提示构建器）

**职责**：组装发送给 LLM 的完整 prompt。

**输入**：
- 系统 prompt 模板（内置 + 用户自定义）
- 规则文件内容（来自 `~/.config/visp/rules/` 等路径）
- 可用工具列表（名称、描述、参数定义，转为 function calling 格式）
- 对话历史（用户消息 + LLM 响应 + 工具调用/结果）

**输出**：结构化的消息数组，可直接发送给 LLM API。

### 3.4 Rule Engine（规则引擎）

**职责**：加载和管理规则文件。

**规则来源**：
- 全局规则：`~/.config/visp/rules/`
- 项目规则：`.visp/rules/`
- 用户自定义路径

**规则类型**：
- `alwaysApply: true` — 总是注入到 system prompt
- `alwaysApply: false` — 按需或条件触发

**关键行为**：
- 启动时扫描规则目录
- 监听规则文件变更，热重载
- 将活跃规则拼接为 system prompt 的一部分

### 3.5 Tool Registry（工具注册表）

**职责**：管理所有可用工具的生命周期和发现。

**工具来源**：
- 内置工具：文件读写、bash、搜索（grep/glob）、网络请求
- MCP 工具：通过 MCP 客户端从已连接的 MCP 服务器动态发现

**工具描述格式**：每个工具提供名称、描述、JSON Schema 参数定义，转为 LLM function calling 格式。

**工具执行**：
- 同步工具（文件读写）：直接执行，阻塞等待结果
- 异步工具（bash、网络请求）：异步执行，可设置超时
- 流式工具：支持将执行过程流式返回

**权限控制**：可配置工具白名单，限制某些工具在特定项目中的使用。

**多 Agent 预留**（Phase 3+）：ToolRegistry 后续需支持按 Agent 角色分配工具子集——orchestrator 能 spawn sub-agent，sub-agent 不能 spawn 别人的 sub-agent。工具权限粒度从"项目级"扩展到"Agent 角色级"。当前 Tool trait 的 Send + Sync + 无状态设计已为此预留，ToolContext 中的 session_id 字段也为此提供了调用链追踪基础。

### 3.6 LLM Provider（LLM 提供器）

**职责**：抽象不同 LLM 服务的调用接口。

**支持的 provider trait 方法**：
- 同步调用：发送消息，等待完整响应
- 流式调用：发送消息，流式接收 token

**错误处理**：网络错误重试、速率限制退避、API 错误分类。

**Provider 实现**：
- OpenAI（GPT-4 系列）
- Anthropic（Claude 系列）
- 兼容 OpenAI API 的本地模型（Ollama 等）

### 3.7 CodeGraph Engine（代码图谱引擎）

**职责**：基于 tree-sitter 的代码解析、索引和查询。

#### 3.7.1 解析与索引

- **解析器层**：使用 tree-sitter 库解析源代码文件，生成 AST
- **符号提取**：从 AST 中提取符号（函数、类、变量、接口等）及其位置信息
- **关系提取**：分析符号间的调用、引用、继承关系
- **增量更新**：文件变更时，仅重新解析变更的文件并更新索引

#### 3.7.2 数据模型（逻辑描述）

数据模型包含三个核心实体：
- **符号节点**：名称、类型（函数/类/变量/接口/路由/组件）、所在文件、位置范围、签名、文档注释
- **符号边**：源符号、目标符号、关系类型（调用、引用、继承、实现）
- **文件信息**：路径、语言类型、符号数量、最后修改时间

索引结构：
- 符号名到 ID 的倒排索引（支持前缀搜索）
- 符号 ID 间的有向图（支持调用链追踪）

#### 3.7.3 查询能力

| 查询操作 | 描述 | 实现方式 |
|---|---|---|
| 符号搜索 | 按名称前缀搜索符号 | 倒排索引前缀匹配 |
| 调用者查询 | 查找调用某符号的所有符号 | 图反向边遍历 |
| 被调用者查询 | 查找某符号调用的所有符号 | 图正向边遍历 |
| 路径追踪 | 查找两个符号间的调用路径 | 图上的 BFS/最短路径搜索 |
| 影响分析 | 分析修改某符号的影响范围 | 多跳正向边扩展 |
| 上下文查询 | 组合搜索、调用者、被调用者 | 复合操作 |
| 文件浏览 | 获取项目文件树 | 文件元数据遍历 |

#### 3.7.4 持久化

使用 SQLite 存储索引数据：
- 符号表、边表、文件表、倒排索引表
- 首次启动全量构建索引，后续增量更新
- 文件监听器触发增量更新

### 3.8 File Watcher（文件监听器）

**职责**：监听项目文件的增删改事件，触发 CodeGraph 增量索引。

**关键行为**：
- 递归监听项目目录
- 防抖处理（合并短时间内的多次变更）
- 按文件类型过滤（仅监听 tree-sitter 支持的语言）
- 触发解析队列，由 CodeGraph Engine 消费

### 3.9 MCP Client（MCP 客户端）

**职责**：连接和管理 MCP 服务器，将外部工具集成到 Tool Registry。

**关键行为**：
- 启动 MCP 服务器子进程（通过 stdio 或 HTTP 传输）
- 协商协议版本和能力
- 发现服务器提供的工具列表
- 接收工具调用请求并路由到对应 MCP 服务器
- 管理多个 MCP 服务器连接的生命周期

**实施策略**：优先使用 `rmcp` crate，对其不完善的功能进行补充实现。

---

## 4. 通信协议

### 4.1 协议选型

使用 **gRPC** 基于以下理由：
- 强类型接口定义（protobuf），前后端契约清晰
- 原生双向流支持，适合 agent 对话场景
- 高性能二进制协议
- 成熟的 Rust 生态（tonic）

### 4.2 服务定义（逻辑描述）

**Vibewisp 服务**提供以下 RPC 方法：

#### 会话管理
- `CreateSession`：创建新会话，返回会话 ID 和初始状态
- `ListSessions`：列出所有活跃会话
- `DeleteSession`：终止并清理指定会话

#### 核心对话
- `Chat`（双向流）：主要交互通道
  - **客户端→服务端**：用户文本消息、配置更新
  - **服务端→客户端**：文本增量（流式输出）、工具调用通知、工具执行结果、状态更新、错误信息、完成信号

#### 快速通道
- `ReadFile`：直接读取文件内容（跳过 LLM 循环，用于前端快速预览）

#### 代码智能
- `SearchSymbols`：按名称搜索符号
- `GetSymbolDetails`：获取单个符号的详细信息

#### Daemon 控制
- `HealthCheck`：检查 daemon 健康状态
- `Shutdown`：优雅关闭 daemon

### 4.3 消息流示例

```
客户端                            Daemon
  │                                │
  │── CreateSession ──────────────▶│ 创建会话
  │◀─────── Session(id, status) ──│
  │                                │
  │── Chat(UserInput("重构X")) ──▶│ 启动 Agent 循环
  │                                │── 调用 LLM
  │◀── TextDelta("好的...") ──────│ 流式文本
  │◀── StatusUpdate("执行工具") ──│ 状态更新
  │◀── ToolCall("read_file") ────│ 工具调用
  │◀── ToolResult(...) ──────────│ 工具结果
  │◀── TextDelta("已完成...") ───│ 继续流式
  │◀── Done ─────────────────────│ 本轮完成
```

---

## 5. 数据流

### 5.1 Agent 核心循环流程

```
┌──────────────────────────────────────────────────────┐
│                                                      │
│  1. 接收用户消息                                      │
│       │                                              │
│       ▼                                              │
│  2. 加载 Rules（从 Rule Engine）                      │
│  3. 获取可用工具列表（从 Tool Registry）               │
│  4. 构建 Prompt（Prompt Builder）                     │
│       │                                              │
│       ▼                                              │
│  5. 发送给 LLM（流式）                                │
│       │                                              │
│       ▼                                              │
│  6. 解析 LLM 响应                                     │
│       │                                              │
│   ┌───┴───────────┐                                  │
│   │               │                                  │
│  文本            工具调用                              │
│   │               │                                  │
│   ▼               ▼                                  │
│  流式输出      7. 执行工具（Tool Registry）            │
│  给前端            │                                  │
│                   ▼                                  │
│               8. 工具结果拼回上下文                     │
│                   │                                  │
│                   ▼                                  │
│               9. 检查终止条件                          │
│                   │                                  │
│            ┌──────┴──────┐                           │
│            │             │                           │
│          继续            终止                          │
│            │             │                           │
│            ▼             ▼                           │
│        回到步骤4     返回 Done                        │
│                                                      │
└──────────────────────────────────────────────────────┘
```

**终止条件**：
- LLM 返回纯文本（无工具调用）且 `stop_reason` 为正常结束
- 达到最大迭代次数（可配置，默认 50）
- 用户主动取消
- 发生不可恢复的错误

### 5.2 CodeGraph 索引流程

```
文件变更事件 (notify)
       │
       ▼
  防抖队列（合并 500ms 内变更）
       │
       ▼
  判断文件类型（是否支持的语言）
       │
       ▼
  tree-sitter 解析 → 提取符号 → 提取关系
       │
       ▼
  更新 SQLite 索引（增量 upsert）
       │
       ▼
  清理已删除文件的索引条目
```

---

## 6. 技术选型理由

| 领域 | 选择 | 理由 | 备选 |
|---|---|---|---|
| 语言 | Rust | 零成本抽象、无 GC、内存安全、高性能并发 | 无（需求本身） |
| 异步运行时 | tokio | 生态最成熟、文档最完善、事实标准 | async-std |
| gRPC | tonic 0.13 + prost 0.13 | 社区标准组合，prost 版本需与 tonic 对齐 | grpc-rust |
| HTTP 客户端 | reqwest | 功能完整、支持 streaming、TLS 可选 | hyper（太底层） |
| 序列化 | serde + serde_json | 无可争议的标准 | 无 |
| 错误处理 | thiserror + anyhow | 分层策略：库用 thiserror，二进制用 anyhow | eyre |
| Tree-sitter | tree-sitter | C 写的解析器，Rust 直接 FFI，零开销 | 无 |
| 文件监听 | notify | 跨平台、支持 tokio | inotify（仅 Linux） |
| MCP 客户端 | rmcp | 唯一活跃的 Rust MCP 实现 | 自行实现 |
| 日志 | tracing | 结构化、异步友好、支持 span | log |
| CLI | clap (derive) + ratatui | 声明式参数解析 + 终端 UI | structopt（已合并进 clap） |
| 配置格式 | TOML | 可读性好、serde 原生支持 | YAML、JSON |
| 持久化 | rusqlite (SQLite) | 成熟稳定、增量更新方便、查询灵活 | sled |

### 风险项

| 风险 | 等级 | 缓解措施 |
|---|---|---|
| rmcp 成熟度不足 | 中 | 优先评估；如不满足需求，自行实现 MCP 协议子集 |
| tree-sitter 语法库覆盖不全 | 低 | 主流语言（TS/JS/Python/Rust/Go）均有官方语法 |
| Rust 开发速度慢于 TS | 中 | 通过模块化设计和清晰接口约束范围，MVP 功能最小化 |
| gRPC CLI 客户端复杂度 | 低 | tonic 自动生成客户端代码，复杂度可控 |

---

## 7. 项目结构

```
visp/
├── Cargo.toml                  # Workspace 根配置
├── AGENTS.md                   # Agent 行为定义（系统 prompt 模板）
├── rust-toolchain.toml         # 稳定版工具链配置
├── crates/
│   ├── visp-core/               # 核心逻辑（无 IO 依赖）
│   │   └── src/
│   │       ├── agent.rs        # Agent 编排器
│   │       ├── session.rs      # 会话管理器
│   │       ├── prompt.rs       # 提示构建器
│   │       ├── rules.rs        # 规则引擎
│   │       ├── tool.rs         # Tool trait 定义
│   │       └── error.rs        # 核心错误类型
│   │
│   ├── visp-proto/              # gRPC 协议
│   │   ├── proto/
│   │   │   └── visp.proto  # 服务与消息定义
│   │   └── src/                # prost/tonic 生成代码
│   │
│   ├── visp-llm/                # LLM 提供商
│   │   └── src/
│   │       ├── provider.rs     # LlmProvider trait
│   │       ├── openai.rs       # OpenAI 实现
│   │       ├── anthropic.rs    # Anthropic 实现
│   │       └── streaming.rs    # SSE 流解析
│   │
│   ├── visp-tools/              # 内置工具
│   │   └── src/
│   │       ├── file.rs         # 文件读写
│   │       ├── bash.rs         # Shell 执行
│   │       ├── search.rs       # Grep/Glob 搜索
│   │       └── web.rs          # 网络请求
│   │
│   ├── visp-codegraph/          # 代码智能
│   │   └── src/
│   │       ├── index.rs        # 索引构建
│   │       ├── graph.rs        # 符号图数据结构
│   │       ├── query.rs        # 查询引擎
│   │       ├── watcher.rs      # 文件监听
│   │       └── parser.rs       # Tree-sitter 集成
│   │
│   ├── visp-mcp/                # MCP 客户端
│   │   └── src/
│   │       ├── client.rs       # MCP 客户端实现
│   │       └── transport.rs    # 传输层（stdio/HTTP）
│   │
│   ├── visp-daemon/             # Daemon 二进制
│   │   └── src/
│   │       ├── main.rs         # 入口
│   │       ├── server.rs       # gRPC 服务端
│   │       ├── config.rs       # 配置加载
│   │       └── service.rs      # Service 实现
│   │
│   └── visp-cli/                # CLI 二进制
│       └── src/
│           ├── main.rs         # 入口
│           ├── repl.rs         # REPL 模式
│           ├── stream.rs       # 流式模式
│           ├── batch.rs        # 批处理模式
│           └── display.rs      # 终端显示
│
├── config/
│   └── default.toml            # 默认配置
│
└── tests/                      # 集成测试
```

---

## 8. 配置系统

### 8.1 配置层级（优先级从高到低）

1. 命令行参数（会话级）
2. 环境变量
3. 项目配置（`.visp/config.toml`）
4. 全局配置（`~/.config/visp/config.toml`）
5. 内置默认值

### 8.2 关键配置项

- **LLM 配置**：provider 类型、API key、模型名称、温度、最大 token
- **工具配置**：可用工具白名单、bash 超时、文件大小限制
- **Agent 配置**：最大迭代次数、单次请求超时
- **CodeGraph 配置**：监听路径、排除模式、支持的语言列表
- **MCP 配置**：服务器列表及其启动参数
- **Daemon 配置**：监听地址、日志级别、数据库路径

---

## 9. MVP 范围

### 9.1 包含

- [ ] Rust daemon 基础框架（tonic gRPC server）
- [ ] 单 Agent 核心循环（无委派）
- [ ] OpenAI + Anthropic LLM provider
- [ ] 内置工具：文件读写、bash、grep/glob 搜索
- [ ] Rule Engine（加载 markdown 规则文件）
- [ ] Session Manager（内存存储，不持久化）
- [ ] CLI 前端（流式输出 + REPL 两种模式）
- [ ] CodeGraph 基础：tree-sitter 解析 + 符号搜索 + SQLite 持久化
- [ ] 文件监听 + 增量索引更新

### 9.2 不包含

- Agent 委派（多 specialist）
- MCP 客户端
- VSCode 前端
- 批处理模式 CLI
- 会话持久化（daemon 重启恢复）
- 完整的测试覆盖（仅核心路径的单元测试）
- 多语言 tree-sitter（仅 TypeScript/JavaScript）
- 网络搜索工具

---

## 10. 边界情况与约束

### 10.1 并发安全

- daemon 需支持多个会话同时运行
- 每个会话的 Agent 实例相互独立
- CodeGraph 索引读写需要读写锁保护
- 文件监听器的索引更新不应阻塞 Agent 循环

### 10.2 错误恢复

- LLM API 调用失败：自动重试（指数退避，最多 3 次）
- 工具执行失败：将错误信息作为工具结果返回给 LLM，由 LLM 决定如何响应
- Daemon 崩溃：会话数据丢失（MVP 不做持久化），用户需重新开始
- 解析失败：tree-sitter 解析错误不中断索引构建，跳过问题文件

### 10.3 资源限制

- 最大日志文件大小
- 最大会话数（可配置）
- bash 执行超时（默认 120 秒）
- 单文件读取大小上限（默认 1MB）
- CodeGraph 索引内存上限

### 10.4 跨平台

- 目标平台：macOS（优先）、Linux、Windows
- bash 工具在 Windows 上使用 PowerShell 或 WSL
- 文件路径处理使用跨平台路径库
- 文件监听使用 notify（已跨平台）

---

## 附录：与现有 OpenCode 的差异

| 维度 | 现有 OpenCode (Node.js) | visp (Rust) |
|---|---|---|
| 架构 | 单体 VSCode 插件 + MCP | 前后端分离 daemon + gRPC |
| 并发模型 | 单线程事件循环 | 多线程异步 (tokio) |
| CodeGraph | tree-sitter wasm/原生 | tree-sitter 直接 FFI |
| 前端 | VSCode only | CLI → VSCode/Web（可扩展） |
| 插件 | 动态 require/import | 静态编译 + MCP 扩展 |
| 内存管理 | GC | 所有权系统 |
| 配置格式 | JSON | TOML |
