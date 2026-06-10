# visp-core — 核心抽象层

纯逻辑层，定义 Agent 编排、消息模型、Tool trait、规则引擎、Prompt 构建等核心抽象，不依赖任何 IO 操作。

## Agent 编排循环

用户输入 → LLM → 工具调用 → LLM → ... 的完整循环：

- **流式输出**：实时推送 text delta 到客户端
- **多工具并行**：LLM 一次返回多个 tool_use 时并行执行，结果排序后拼回上下文
- **自动重试**：网络错误/速率限制时指数退避重试
- **Thinking 模式**：整合 Claude thinking blocks，通过 `thinking_budget_tokens` 控制预算
- **Token 统计**：每轮返回 input/output token 数
- **迭代保护**：`max_iterations` 防止无限循环

## 关键文件

- `agent.rs` — Agent 编排循环
- `session.rs` — 会话管理（含 Skills 加载）
- `message.rs` — 消息模型
- `tool.rs` — Tool trait 定义
- `provider.rs` — LlmProvider trait 定义

## Skills 技能系统

从 `.visp/skills/` 加载技能定义，自动合并到 system prompt 中。每个技能是一个子目录，包含 `SKILL.md`：

```
.visp/skills/
├── my-workflow/
│   └── SKILL.md     # ---\nname: my-workflow\ndescription: ...\n---\n具体指令内容
└── another-skill/
    └── SKILL.md
```

## 依赖

无内部 crate 依赖。

## 核心约束

**禁止 IO**：所有文件读写、网络请求、进程启动必须由其他 crate 实现。

## 测试

```bash
cargo test -p visp-core
```
