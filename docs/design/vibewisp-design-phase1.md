# vibewisp Phase 1 阶段设计：项目骨架 + 核心抽象

## 1. 阶段目标

搭建 Cargo workspace 骨架，定义所有后续模块依赖的核心 trait 和 gRPC 协议。

**一句话总结**：让 `cargo build --workspace` 能过，所有 crate 的公共接口定义完毕。

## 2. 模块划分

Phase 1 涉及两个 crate：

| Crate | 职责 | 依赖 |
|---|---|---|
| **vbw-core** | 核心 trait、错误类型、消息类型 | 无（纯逻辑） |
| **vbw-proto** | gRPC 协议定义，prost/tonic 代码生成 | 无 |

### 2.1 vbw-core

这是整个项目的基石。所有其他 crate 通过实现 vbw-core 的 trait 来接入系统。Phase 1 只定义接口，不实现。

#### 2.1.1 Tool trait

定义工具的抽象接口，所有工具（内置 + MCP）都实现此 trait。

**能力要求**：
- **身份**：提供名称和描述，用于生成 LLM function calling 的 tool definition
- **参数定义**：提供 JSON Schema 格式的参数描述，用于 LLM 理解如何调用
- **执行**：异步执行，接受参数（`serde_json::Value`）和上下文（ToolContext），返回结果

**上下文（ToolContext）**：
- MVP 阶段仅包含当前工作目录路径
- 后续可扩展：session_id、配置引用等

**结果（ToolResult）**：
- 简单结构：文本内容 + 是否错误标记
- MVP 不包含流式输出变体，后续通过新增 variant 扩展

**设计决策**：
- 参数类型用 `serde_json::Value` 而非泛型——LLM 输出的 JSON 无法在编译期确定类型，运行时校验更自然
- 执行方法标记为 async——bash 执行和后续 MCP 调用都是异步场景
- 不定义生命周期方法（init/shutdown）——MVP 的 Tool Registry 不做生命周期管理

#### 2.1.2 LlmProvider trait

定义 LLM 服务的调用接口。每个外部 LLM 服务（OpenAI、Anthropic 等）提供独立实现。

**能力要求**：
- **流式调用**：发送消息列表，流式接收 delta。返回区分文本块和工具调用请求
- **配置参数**：接受模型名称、温度、最大 token 等 LLM 参数
- **工具定义**：接受工具列表（从 Tool Registry 获取），转为 function calling 格式传给 LLM

**流式响应中的事件类型**：
- 文本增量（token 级别 delta）
- 工具调用请求（函数名 + 参数 JSON）
- 流结束信号

**错误处理**：
- 网络错误（可重试）
- 速率限制（需退避重试）
- API 错误（不可重试，如无效 API key）
- 流解析错误

**设计决策**：
- 先做流式调用（chat_stream），同步调用可基于流式调用封装
- 返回类型用 Rust stream（futures::Stream），而非回调
- 错误类型需要细粒度分类，方便上层决定重试策略

#### 2.1.3 错误类型

采用分级枚举设计，各 crate 定义自己的错误并通过 From trait 向上传播。

**vbw-core 定义**：
- **CoreError**：顶层错误，包含工具错误、LLM 错误、会话错误的变体
- **SessionError**：会话相关错误（不存在、已结束等）

**vbw-llm 定义（Phase 2 实现，Phase 1 预留类型占位）**：
- **LlmError**：网络错误、速率限制、API 错误、流错误

**vbw-tools 定义（Phase 2 实现，Phase 1 预留类型占位）**：
- **ToolError**：工具未找到、执行失败、超时、权限不足

**传播链**：子 crate 错误 → `impl From<LlmError> for CoreError` → daemon 层 `anyhow` 收口 → gRPC `tonic::Status`

#### 2.1.4 消息类型

定义 Agent 循环中流转的核心数据结构：

- **Message**：对话中的一条消息。包含角色（system / user / assistant / tool）和内容
- **ToolDefinition**：工具描述，用于生成 LLM function calling 参数。包含名称、描述、JSON Schema
- **ChatEvent**：LLM 流式响应中的单个事件。可能是文本增量、工具调用请求、或流结束

### 2.2 vbw-proto

定义 vibewisp daemon 与前端之间通信的 gRPC 协议。

#### 2.2.1 服务定义

**CoderDaemon 服务**（proto package: `vibewisp`）提供以下 RPC 方法：

**会话管理**：
- `CreateSession`：创建新会话。输入：项目路径、配置参数。输出：会话 ID + 初始状态
- `ListSessions`：列出所有活跃会话。无输入。输出：会话列表
- `DeleteSession`：终止指定会话。输入：会话 ID。输出：空

**核心对话**：
- `Chat`：双向流。客户端发送用户消息和配置更新，服务端流式返回文本增量、工具调用通知、工具结果、状态更新、完成信号

**Daemon 控制**：
- `HealthCheck`：检查 daemon 存活状态。无输入。输出：健康状态 + 版本号
- `Shutdown`：优雅关闭 daemon。输入：是否强制。输出：空

**快速通道**（Phase 3 实现，Phase 1 定义接口）：
- `ReadFile`：跳过 LLM 循环直接读取文件。输入：文件路径。输出：文件内容

**代码智能**（Phase 5 实现，Phase 1 定义接口）：
- `SearchSymbols`：按名称搜索符号。输入：查询字符串 + 项目路径。输出：匹配的符号列表
- `GetSymbolDetails`：获取单个符号详情。输入：符号标识。输出：符号信息

#### 2.2.2 关键消息类型

**会话相关**：
- `Session`：会话 ID、状态（空闲/运行/完成/错误）、项目路径、创建时间
- `CreateSessionRequest`：项目路径、初始配置
- `ListSessionsResponse`：会话列表

**Chat 双向流**：
- `ClientMessage`：用户文本输入 或 配置更新请求
- `ServerMessage`：文本增量（TextDelta）、工具调用通知（ToolCall）、工具结果（ToolResult）、状态更新（StatusUpdate）、错误（Error）、完成信号（Done）

**通用**：
- `HealthStatus`：daemon 存活状态、版本号、运行时间
- `Empty`：空消息，用于无参数或无返回值的 RPC

#### 2.2.3 设计决策

- gRPC 版本用 proto3（tonic 默认）
- 服务名在 proto 文件中定义为 `vibewisp.CoderDaemon`
- 双向流 `Chat` 是整个系统的核心通信通道，承载 Agent 循环的完整生命周期
- 所有时间戳使用 `google.protobuf.Timestamp`
- 消息中的路径字段使用字符串类型，由调用方保证跨平台兼容

## 3. Workspace 结构

Phase 1 完成后的目录结构：

```
vibewisp/
├── Cargo.toml                  # Workspace 根，members 列出所有 crate
├── rust-toolchain.toml         # 固定 Rust 稳定版
├── rustfmt.toml                # 代码格式化配置（可选）
├── crates/
│   ├── vbw-core/
│   │   ├── Cargo.toml          # 依赖：serde, serde_json, async-trait, thiserror
│   │   └── src/
│   │       ├── lib.rs          # 模块声明
│   │       ├── tool.rs         # Tool trait + ToolContext + ToolResult
│   │       ├── provider.rs     # LlmProvider trait + ChatEvent
│   │       ├── message.rs      # Message + ToolDefinition
│   │       ├── error.rs        # CoreError + SessionError
│   │       └── session.rs      # Session 状态类型
│   │
│   └── vbw-proto/
│       ├── Cargo.toml          # 依赖：tonic, prost
│       ├── build.rs            # prost 编译配置
│       ├── proto/
│       │   └── vibewisp.proto  # 服务 + 消息定义
│       └── src/
│           └── lib.rs          # include! 生成的代码
│
├── AGENTS.md                   # ✅ 已创建（项目根目录）
├── config/
│   └── default.toml            # 默认配置（Phase 1 可选）
│
└── docs/
    ├── design/
    │   ├── vibewisp-design.md      # ✅ 总设计文档
    │   └── vibewisp-design-phase1.md # ✅ 当前文件
    └── plans/
        ├── vibewisp-master-plan.md  # ✅ 总计划
        └── vibewisp-plan-phase1.md  # Phase 1 计划（下一步）
```

## 4. 依赖关系

```
vbw-core (无外部依赖)
    ↑ 依赖
vbw-proto (仅依赖 prost/tonic，不依赖 vbw-core)
```

Phase 1 的两个 crate 相互独立。vbw-proto 不依赖 vbw-core，因为 protobuf 生成的类型是独立的数据结构，上层使用时再做类型转换。

## 5. Phase 1 不做什么

明确边界：

- ❌ 不实现任何 Tool（没有 file/bah/search 工具）
- ❌ 不实现任何 LlmProvider（没有 OpenAI/Anthropic 调用）
- ❌ 不编写测试用例（trait 和类型定义不需要测试——但 proto 编译需要验证）
- ❌ 不实现 gRPC server 或 daemon
- ❌ 不编写构建脚本（build.rs）逻辑——proto 编译除外
- ❌ 不创建 `vbw-llm`、`vbw-tools`、`vbw-daemon` 等 crate 目录（它们只需要在 workspace Cargo.toml 中声明为 member）

## 6. 验收标准

- `cargo build --workspace` 编译通过（0 错误，0 警告）
- `cargo clippy --workspace -- -D warnings` 通过
- `cargo fmt --check --all` 通过
- proto 文件能正确编译生成 Rust 代码
- vbw-core 暴露的所有 public 类型和 trait 已在文档注释中说明用途
