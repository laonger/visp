# visp-context — 上下文裁剪器

## 职责

管理 LLM 对话历史的 context window，防止超出 token 限制。

## 核心能力

### Token 预算计算

根据 `max_context_tokens` 和 `output_tokens` 计算本轮对话历史的可用 token 预算：

```
available = max_context_tokens − max(output_tokens, 4000)
history_budget = available − system_overhead
```

### 三段式裁剪策略

```
HEAD (5 turns) | MIDDLE (可裁剪) | TAIL (10 turns)
```

- **HEAD**：对话开头的轮次，保护不裁剪（保留任务目标、上下文建立过程）
- **TAIL**：对话末尾的轮次，保护不裁剪（保留最近的对话流）
- **MIDDLE**：可裁剪区域，从最旧轮次开始按完整轮次删除直到满足预算

### 极端保底

当 HEAD + TAIL 本身已超出预算时：
- 保留第一条 User 消息（任务锚点）
- 从尾部往前填充最近消息
- 过滤孤立的 ToolResult（对应 tool_use 已被裁剪）
- 插入 `[... earlier messages omitted ...]` 标记

### 工具输出压缩

Tool 消息输出超过 2000 字符时自动截断，附加 `[truncated N chars]` 标记。仅影响发给 LLM 的 prompt 副本，存储中的原始内容完整保留。

## 架构

```
visp-core (ContextTrimmer trait)
    ↑
visp-context (DefaultContextTrimmer — 实现 trait)
    ↑
visp-daemon (创建实例，注入 Agent 循环)
```

`ContextTrimmer` trait 定义在 `visp-core` 中，`visp-context` 实现默认策略。Daemon 通过 `Arc<dyn ContextTrimmer>` 依赖注入，core 不依赖具体实现。未来可替换为其他裁剪策略（如 LLM 摘要压缩）。

## 使用

```rust
use visp_context::DefaultContextTrimmer;
use visp_core::context::ContextTrimmer;

// 默认配置：head=5, tail=10, tool_output_max_chars=2000
let trimmer = DefaultContextTrimmer::default();

// 裁剪对话历史
let trimmed = trimmer.trim(
    &history,           // 对话历史（不含 system message）
    max_context_tokens,  // LLM 上下文窗口大小
    system_overhead,     // system prompt 等开销
    output_tokens,       // 期望输出 token 数
);
```

## 配置

裁剪参数当前为内部常量，不对外暴露配置：

| 参数 | 默认值 | 说明 |
|------|--------|------|
| `head_turns` | 5 | 保护的对话开头轮数 |
| `tail_turns` | 10 | 保护的对话末尾轮数 |
| `tool_output_max_chars` | 2000 | Tool 输出截断阈值 |

## Token 估算

基于 `chars/4` 的简单估算（1 token ≈ 4 字符），消息构造时预计算存入 `Message.estimated_tokens` 字段。裁剪决策使用预计算值，Tool 消息按截断后长度调整。
