# visp-core — 核心抽象层

纯逻辑层，定义 Agent 编排、消息模型、Tool trait、规则引擎、Prompt 构建等核心抽象，不依赖任何 IO 操作。

## 关键文件

- `agent.rs` — Agent 编排循环
- `session.rs` — 会话管理
- `message.rs` — 消息模型
- `tool.rs` — Tool trait 定义
- `provider.rs` — LlmProvider trait 定义

## 依赖

无内部 crate 依赖。

## 核心约束

**禁止 IO**：所有文件读写、网络请求、进程启动必须由其他 crate 实现。

## 测试

```bash
cargo test -p visp-core
```
