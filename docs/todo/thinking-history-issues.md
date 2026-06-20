# Thinking 数据在对话历史中的机制问题与方案

## 现状

visp 每次对话使用历史时都包含 thinking 数据，且完整 round-trip 保留：

```
LLM 响应
  │  Anthropic: thinking_delta / thinking block start → ChatEvent::ThinkingBlock
  │  OpenAI: reasoning_content / reasoning field → ChatEvent::ThinkingBlock
  ▼
agent_loop.rs:363-369  collect_stream_events
  │  thinking_blocks: Vec<serde_json::Value>（只保留最后一块）
  ▼
agent_loop.rs:585-633  handle_stream_result  ← 关键：存为两条消息
  ├─ Message::thinking(content=thinking文本)              ← kind=Thinking 独立消息
  └─ Message::assistant(content=响应文本, extra_blocks=Some(thinking_blocks))
  ▼
DB 持久化（message_repo.rs:48-51）
  │  type='thinking' / extra_blocks TEXT 列
  ▼
下次对话加载历史（message_repo.rs:87-184 完整恢复）
  │  context_trimmer.trim() ← 不特殊处理，按轮次保留
  │  retain(!skip_context)  ← 不影响 thinking
  ▼
Provider 转换
  ├─ Anthropic: extra_blocks → content blocks（thinking block 原样发送，含 signature）
  └─ OpenAI: extra_blocks → 顶层字段合并（"thinking":"..."）
  ▼
发送给 LLM
```

关键代码位置：

| 环节 | 文件 | 行号 |
|------|------|------|
| ChatEvent::ThinkingBlock 定义 | crates/visp-core/src/provider.rs | 84 |
| Anthropic thinking 请求配置 | crates/visp-llm/src/anthropic.rs | 72-84 |
| Anthropic thinking delta 解析 | crates/visp-llm/src/anthropic.rs | 328-335, 378-403 |
| Anthropic thinking 流累积+发射 | crates/visp-llm/src/anthropic.rs | 623-647, 669-677 |
| OpenAI reasoning 解析 | crates/visp-llm/src/openai.rs | 330-340 |
| OpenAI reasoning 流累积+发射 | crates/visp-llm/src/openai.rs | 469-476, 414 |
| Agent 流收集（clear+push） | crates/visp-core/src/agent_loop.rs | 363-369 |
| Thinking → Message::thinking() | crates/visp-core/src/agent_loop.rs | 475-477, 585-607 |
| extra_blocks 挂载到 assistant 消息 | crates/visp-core/src/agent_loop.rs | 499-503, 611-614 |
| extra_blocks 挂载到 tool_call 消息 | crates/visp-core/src/agent_loop.rs | 695-698 |
| extract_thinking_text 辅助函数 | crates/visp-core/src/agent.rs | 336-342 |
| Message::thinking() 构造函数 | crates/visp-core/src/message.rs | 213-233 |
| extra_blocks 字段定义 | crates/visp-core/src/message.rs | 57-58 |
| DB extra_blocks 列 schema | crates/visp-db/src/schema.rs | 44 |
| DB thinking 序列化 | crates/visp-db/src/message_repo.rs | 48-51 |
| DB thinking 反序列化 | crates/visp-db/src/message_repo.rs | 124, 134-135 |
| Prompt 构建（传入 trimmer） | crates/visp-core/src/prompt.rs | 44-88 |
| Anthropic 消息转换（含 extra_blocks） | crates/visp-llm/src/anthropic.rs | 115-228 |
| OpenAI 消息转换（含 extra_blocks） | crates/visp-llm/src/openai.rs | 140-221 |
| 上下文裁剪（不特殊处理 thinking） | crates/visp-context/src/lib.rs | 31-98 |

## 发现的问题

### 🔴 问题 1：双存储导致 thinking 被重复发送（疑似 bug）

最严重的问题。一次 thinking 被存成两条消息：

```
Message 1: kind=Thinking,  content="我来分析这个..."        ← 独立消息
Message 2: kind=Assistant, content="答案是...", extra_blocks=[thinking block]  ← 挂载
```

构建 LLM 请求时（`build_anthropic_messages`）：
- Message 1 → assistant role + `{"type":"text","text":"我来分析这个..."}`（thinking 文本被当作普通 text）
- Message 2 → assistant role + `{"type":"thinking","thinking":"我来分析这个...","signature":"..."}` + `{"type":"text","text":"答案是..."}`

结果：Anthropic 收到两个连续 assistant 消息，第一个把 thinking 当普通输出，第二个才是真正的 thinking block。同一份 thinking 内容发了两次，且第一次语义错误（thinking 冒充 assistant 回复）。

这会污染模型对历史的理解——模型会以为"我之前说过这些话"，而实际上是它的内部思考。

待验证项：需读 `build_anthropic_messages` 的确切实现，确认是否真的把 kind=Thinking 消息也作为 assistant 消息发送（而非过滤）。

### 🟡 问题 2：只保留最后一块 thinking block

`agent_loop.rs:363-369`：

```rust
thinking_blocks.clear();  // 清空
thinking_blocks.push(block.clone());  // 只留最后一块
```

如果一轮响应中产生多个 thinking block（Anthropic 在 tool use 场景可能产生），前面的全丢。这是信息丢失。

### 🟡 问题 3：Anthropic signature / block 顺序约束未验证

Anthropic extended thinking 有严格约束：
- thinking block 必须带 signature 才能被服务端接受为有效历史
- thinking block 在 assistant turn 中有位置要求（通常在 text/tool_use 之前）

当前机制把 thinking 存成独立消息再重组，是否破坏了 Anthropic 要求的 block 顺序？比如裁剪后 thinking 消息和 assistant 消息是否还能正确配对？这需要验证。

### 🟡 问题 4：OpenAI 路径产生脏字段

`build_openai_messages` 把 `extra_blocks` 合并为 assistant 消息顶层字段（`"thinking":"..."`）。对标准 OpenAI API（GPT-4o 等）这是未定义字段，strict 模式可能报错；仅 DeepSeek 等兼容模型能识别。

### 🟢 问题 5：裁剪粒度可能产生 orphan thinking

`DefaultContextTrimmer` 按 User 消息划分轮次。如果 thinking 消息和对应 assistant 消息在边界附近，裁剪是否会让 thinking 成为孤儿？调查说"按轮次整体保留"，但 thinking 消息是否被正确归入同一轮次取决于轮次划分逻辑——这点没完全验证。

## 如果发送 history 不带 thinking 数据，会怎样

### 对 Anthropic

完全可行，且是官方支持的模式。Anthropic 文档明确：历史中的 thinking block 是可选的，不带的话模型每轮重新思考。

- 损失：失去"跨轮连续思考"的上下文。对多步推理任务，模型无法引用之前思考过的中间结论。
- 收益：避免上述所有 bug；显著节省 token（thinking 常占单轮 50%+ token）。

实际影响：对绝大多数任务可忽略。thinking 本质是模型的"草稿纸"，模型从最终回复中已经提炼了结论，不需要回看草稿。

### 对 OpenAI

无负面影响，反而是清理脏数据。标准 OpenAI reasoning（o1/o3）的思考过程是模型内部状态，不在 messages 中传递。当前发的 `"thinking":"..."` 顶层字段对标准 OpenAI 是无效字段，去掉更干净。

### 净效果对比

| 维度 | 带 thinking（当前） | 不带 thinking |
|------|---------------------|---------------|
| Token 成本 | 高（thinking 占 50%+） | 低 |
| 连续推理能力 | 理论上有，实际受 bug 影响 | 损失，但影响小 |
| API 兼容性 | Anthropic 有顺序/signature 风险，OpenAI 有脏字段 | 干净 |
| 实现复杂度 | 双存储 + extra_blocks + 裁剪关联 | 简单 |
| Bug 风险 | 高（重复发送、信息丢失、顺序破坏） | 低 |

## 方案

### 方向 A：修复 thinking 保留机制

- 去掉独立 `kind=Thinking` 消息，只保留 `extra_blocks`
- 修复只保留最后一块的问题
- 验证 Anthropic signature / 顺序约束
- 复杂度高，收益是保留连续思考能力

### 方向 B：发送历史时丢弃 thinking（推荐）

- 构建 LLM 请求时过滤掉 `kind=Thinking` 消息和 `extra_blocks`
- 仍可保留 DB 持久化（用于 TUI 展示和审计）
- 简单、干净、避免所有 bug
- 损失的连续思考能力对实际任务影响很小

推荐方向 B，因为 thinking 的本质是"一次性草稿"，跨轮保留的收益远不如它带来的复杂度和 bug 风险。但这是产品决策，需要用户确认。

## 待验证项

1. 问题 1 的确切行为：读 `build_anthropic_messages` 确认 kind=Thinking 消息是否被当作 assistant text 发送
2. 问题 3 的实际影响：实测带 thinking 的历史是否被 Anthropic 拒绝或产生异常输出
3. 问题 5 的裁剪边界：构造 thinking+assistant 在裁剪边界的用例，验证是否产生孤儿
4. Anthropic 官方对历史 thinking 的确切要求（signature 必须性、block 顺序约束）
