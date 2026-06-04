# vibewisp Phase 1 工作计划：项目骨架 + 核心抽象

## 概述

Phase 1 创建项目骨架，定义 `vbw-core` 的核心 trait 和 `vbw-proto` 的 gRPC 协议。

**关于测试**：Phase 1 只包含 trait 定义、错误枚举、数据结构，没有可执行的实现代码。编译通过 + type check 通过即为验收，不需要编写 #[test] 用例。

## 执行步骤

### 步骤 1：创建 Workspace 骨架

**操作**：
- 创建 `vibewisp/Cargo.toml`（workspace 根，声明 8 个 member）
- 创建 \`vibewisp/rust-toolchain.toml\`（channel = "stable"）
- 创建 `vibewisp/crates/` 目录
- 创建 `vibewisp/vbw-core/Cargo.toml`（依赖：serde, serde_json, async-trait, thiserror, uuid）
- 创建 `vibewisp/vbw-core/src/lib.rs`（空模块声明）
- 创建 `vibewisp/vbw-proto/Cargo.toml`（依赖：tonic, prost, prost-types）
- 创建 `vibewisp/vbw-proto/build.rs`（prost 编译配置）
- 创建 `vibewisp/vbw-proto/src/lib.rs`（include! 生成代码）

**验证**：`cargo check --workspace` 编译通过（此时代码为空，无错误即可）

### 步骤 2：定义 vibewisp.proto

**操作**：
- 创建 `vibewisp/vbw-proto/proto/vibewisp.proto`
- 定义服务 `CoderDaemon` 和所有 RPC 方法
- 定义全部消息类型
- 需要 `google.protobuf.Timestamp` 依赖

**验证**：`cargo build -p vbw-proto` 编译通过，proto 生成 Rust 代码无误

### 步骤 3：定义 vbw-core 核心类型

**操作**：

**3a：错误类型** (`error.rs`)
- `CoreError` enum：顶层错误，含 `Llm`、`Tool`、`Session` 变体
- `SessionError` enum：会话相关错误
- 预留 `LlmError` 和 `ToolError` 类型（Phase 2 会在对应 crate 定义，Phase 1 先占位）

**3b：消息类型** (`message.rs`)
- `Message` struct：角色（system/user/assistant/tool）+ 内容 + 可选的 tool_call_id 和 tool_calls
- `ToolDefinition` struct：名称 + 描述 + JSON Schema 参数
- `ChatEvent` enum：TextDelta / ToolCall / Done

**3c：Tool trait** (`tool.rs`)
- `Tool` trait：name() / description() / parameters() / execute()
- `ToolContext` struct：working_dir
- `ToolResult` struct：content + is_error

**3d：LlmProvider trait** (`provider.rs`)
- `LlmProvider` trait：chat_stream() 方法
- `LlmConfig` struct：model / temperature / max_tokens
- `ChatEvent` 流类型定义

**验证**：`cargo build -p vbw-core` 编译通过

### 步骤 4：验证

**操作**：运行完整的质量门

- `cargo build --workspace` — 全 workspace 编译通过
- `cargo clippy --workspace -- -D warnings` — 0 警告
- `cargo fmt --check --all` — 格式检查通过

### 步骤 5：提交

**操作**：`git commit`，conventional commit 格式

## 依赖关系

```
步骤 1 → 步骤 2 + 步骤 3（可并行）
                ↓
              步骤 4
                ↓
              步骤 5
```

步骤 2 和 3 互不依赖，可以并行执行。

## 备注

- Phase 1 不创建 `vbw-llm`、`vbw-tools`、`vbw-daemon`、`vbw-cli`、`vbw-codegraph`、`vbw-mcp` 的目录和源码。它们只在 workspace Cargo.toml 中声明为 member（路径存在即可），但 Phase 2-5 才会创建实际代码。
- `Cargo.toml` 的 member 声明需要指向 `crates/vbw-*`，Phase 1 仅创建存在的 crate 目录，其他 member 声明加注释或直接省略。
- 实际做法：workspace Cargo.toml 的 members 字段只列出 Phase 1 实际存在的 crate，后续阶段再追加。
