# Phase 1：Token 计数与上下文裁剪

## 范围

Phase 1 解决最紧迫的问题：对话历史无 token 计数和裁剪。实现轻量级方案，不引入新依赖。

> 参考：`docs/design/visp-design-context-management.md` 总纲中的 Phase 1 定义。

## 1. Token 估算

参考 opencode 实现：`chars ÷ 4` 的简单估算，无外部依赖。

### 1.1 Message 新增字段

```rust
pub struct Message {
    pub role: Role,
    pub content: String,
    pub tool_call_id: Option<String>,
    pub tool_calls: Option<Vec<ToolCallRequest>>,
    pub extra_blocks: Option<Vec<serde_json::Value>>,
    pub skip_context: bool,
    /// NEW: 预计算的 token 估算值，在消息创建时填充
    pub estimated_tokens: u32,
}
```

值在构造时计算：`Message::user("...")` → 自动调用 `estimate_message_tokens()` 填入。Phase 2 替换为 `rs-bpe` 时，只需更新构造逻辑中的估算函数，Pruner / Budget / PromptBuilder 无需改动。

### 1.2 基础估算函数（消息构造时调用）

```rust
fn estimate_tokens(text: &str) -> u32 {
    if text.is_empty() { return 0; }
    ((text.len() as f64) / 4.0).ceil() as u32
}

fn estimate_message_tokens(msg: &Message) -> u32 {
    let mut total = estimate_tokens(&msg.content) + 1; // +1 role overhead
    if let Some(ref id) = msg.tool_call_id {
        total += estimate_tokens(id);
    }
    if let Some(ref calls) = msg.tool_calls {
        for call in calls {
            total += estimate_tokens(&call.id)
                + estimate_tokens(&call.name)
                + estimate_tokens(&call.arguments);
        }
    }
    total
}
```

`Message` 构造时调用 `estimate_message_tokens()` 填充 `estimated_tokens` 字段。后续所有裁剪决策使用该预计算值。

### 1.3 Prompt 版估算（裁剪时使用）

裁剪决策需要使用截断后的大小。Tool 消息在 prompt 中只保留前 2000 字符，但 `estimated_tokens` 存的是原始内容大小。

```rust
const TOOL_OUTPUT_MAX_CHARS: usize = 2_000;

fn estimate_message_tokens_for_prompt(msg: &Message) -> u32 {
    if msg.role == Role::Tool && msg.content.chars().count() > TOOL_OUTPUT_MAX_CHARS {
        // Tool 消息将被截断到 TOOL_OUTPUT_MAX_CHARS 个字符
        // 粗略估算：TOOL_OUTPUT_MAX_CHARS 字符 ≈ TOOL_OUTPUT_MAX_CHARS / 4 tokens
        ((TOOL_OUTPUT_MAX_CHARS as f64) / 4.0).ceil() as u32 + 1
    } else {
        msg.estimated_tokens
    }
}

fn estimate_messages_tokens_for_prompt(messages: &[Message]) -> u32 {
    messages.iter().map(|m| estimate_message_tokens_for_prompt(m)).sum()
}
```

- `estimate_message_tokens` — 构造时调用，填充 `estimated_tokens`，基于存储内容
- `estimate_message_tokens_for_prompt` — 裁剪时调用，使用预计算值，Tool 消息按截断调整
- 所有裁剪决策函数使用 `for_prompt` 版本

> **Phase 2 升级路径：** 只替换 `estimate_message_tokens`（用 `rs-bpe`），重新填充 `estimated_tokens`。PromptBuilder / Pruner / BudgetManager 无需改动。

## 2. 配置

### 2.1 LlmConfig 新增字段

```rust
pub struct LlmConfig {
    pub model: String,
    pub temperature: f64,
    pub max_tokens: u32,            // 输出限制
    pub max_context_tokens: u32,    // NEW: effective limit，默认 128_000
    pub extra: HashMap<String, String>,
}
```

**语义：** `max_context_tokens` 是 **effective limit**——用户/配置文件写多少就用多少，不参与运行时二次预留。

### 2.2 daemon.toml

```toml
[llm]
max_context_tokens = 128000   # 未配置时默认为 128000
```

### 2.3 Proto

```protobuf
message LlmConfig {
    optional string model = 1;
    optional double temperature = 2;
    optional uint32 max_tokens = 3;
    map<string, string> extra = 4;
    optional uint32 max_context_tokens = 5;  // NEW
}
```

### 2.4 默认值推算（配置加载时）

当 `daemon.toml` 未配置且 proto 也未传入时，由内置对照表根据 model 名推算：

| 模型 | context_window | 推荐 max_context_tokens |
|------|---------------|------------------------|
| Claude Sonnet 3.7 | 200K | 128K |
| GPT-5 | 128K | 102K |
| DeepSeek-V3 | 128K | 102K |
| Qwen3 | 128K | 102K |
| Llama3 | 128K | 102K |
| 未识别 | — | 128K |

这些推荐值仅在配置加载时计算一次，存入 `LlmConfig.max_context_tokens` 后不再参与运行时计算。

## 3. 预算公式

```rust
fn calculate_available(max_context_tokens: u32, output_tokens: u32) -> u32 {
    max_context_tokens.saturating_sub(output_tokens.max(4_000))
}
```

- `max_context_tokens` 已经是 effective limit（含预留）
- 只减去 `max(output_tokens, 4000)` 作为本轮输出的保留空间
- 返回值为本轮输入消息可用的 token 数

## 4. 工具输出：存储与 Prompt 分离

### 核心原则：Storage ≠ Prompt Context

```
存储:   ToolResult("5000 行 cargo build 输出...")     ← 完整保留原始内容
Prompt: ToolResult("前 2000 字符...[truncated]")       ← 临时截断
```

- `Message.content` 始终存储完整原始内容（不修改存储）
- 截断只在 `PromptBuilder::build()` 内部进行——创建临时的浅拷贝，message.content 替换为截断版本
- 未来支持 Summary / Memory / Replay / Debug 时可以访问完整原始内容

### 截断函数

```rust
const TOOL_OUTPUT_MAX_CHARS: usize = 2_000;

fn truncate_tool_output(content: &str) -> String {
    if content.chars().count() <= TOOL_OUTPUT_MAX_CHARS {
        content.to_string()
    } else {
        let truncated: String = content.chars().take(TOOL_OUTPUT_MAX_CHARS).collect();
        format!(
            "{}...\n[truncated {} chars]",
            truncated,
            content.chars().count() - TOOL_OUTPUT_MAX_CHARS
        )
    }
}
```

### Token 估算差异

裁剪决策使用 `estimate_messages_tokens_for_prompt`（见第 1.3 节），Tool 消息按截断后长度估算，确保预算计算准确。

## 5. 对话历史裁剪（唯一触发点）

每次 LLM 调用前，从完整历史中选择一个子集，并对 Tool 消息临时截断。**不修改存储**，只影响本次 prompt。

### 5.1 轮次定义

一个"轮次"是原子单位，定义为：

> 从 `User` 消息开始，到下一个 `User` 消息之前结束的所有消息。

```
轮次1: User → Assistant(tool_use) → ToolResult → Assistant
轮次2: User → Assistant
轮次3: User → Assistant(tool_use) → ToolResult → Assistant(tool_use) → ToolResult → Assistant
```

**拆除原则：** 不做部分删除。不单独删除 ToolResult，不剥离 tool_calls。保证保留的每一段对话都是自洽的。

### 5.2 Protect Head / Tail

```rust
const PROTECTED_HEAD_TURNS: usize = 5;   // 开头 N 轮（保护目标/约束/架构建立过程）
const PROTECTED_TAIL_TURNS: usize = 10;  // 末尾 N 轮（保护）
```

```
HEAD (5 turns) | MIDDLE (可裁剪) | TAIL (10 turns)
```

> Phase 2 自适应：`max(5, total_turns × 10%)` 实现动态头尾保护比例。

历史不足 `HEAD + TAIL` 轮次时，HEAD 和 TAIL 自然重叠，MIDDLE 为空，裁剪无操作。

### 5.3 边界函数定义

```rust
/// 返回第 n+1 轮起点索引。如果不足 n 轮，返回 history.len()。
fn find_head_end(history: &[Message], n: usize) -> usize {
    let mut turns = 0;
    for (i, msg) in history.iter().enumerate() {
        if msg.role == Role::User {
            turns += 1;
            if turns > n {
                return i;
            }
        }
    }
    history.len()
}

/// 返回倒数第 n 轮起点索引。如果不足 n 轮，返回 0。
fn find_tail_start(history: &[Message], n: usize) -> usize {
    let total = history.iter().filter(|m| m.role == Role::User).count();
    if total <= n {
        return 0;
    }
    let target = total - n + 1;
    let mut seen = 0;
    for (i, msg) in history.iter().enumerate() {
        if msg.role == Role::User {
            seen += 1;
            if seen == target {
                return i;
        }
        }
    }
    0
}
```

### 5.4 drop_old_turns

从 MIDDLE 区域中删除最早完整轮次，直到预算满足或无可删除。

```rust
fn drop_old_turns(messages: &[Message], budget: u32) -> Vec<Message> {
    if messages.is_empty() {
        return vec![];
    }

    let mut tokens = estimate_messages_tokens_for_prompt(messages);
    if tokens <= budget {
        return messages.to_vec();
    }

    let mut start = 0;
    while start < messages.len() && tokens > budget {
        // 找到下一个 User 消息（当前轮次结尾 / 下一轮次起点）
        let first_user = messages[start..]
            .iter()
            .position(|m| m.role == Role::User);
        let second_user = first_user.and_then(|after_first| {
            messages[start + after_first + 1..]
                .iter()
                .position(|m| m.role == Role::User)
                .map(|p| start + after_first + 1 + p)
        });

        match second_user {
            Some(boundary) => {
                let dropped = &messages[start..boundary];
                tokens = tokens.saturating_sub(estimate_messages_tokens_for_prompt(dropped));
                start = boundary;
            }
            None => {
                break;
            }
        }
    }

    messages[start..].to_vec()
}
```

### 5.5 trim_context 主流程

```rust
pub fn trim_context(
    history: &[Message],
    system_tokens: u32,
    max_context_tokens: u32,
    output_tokens: u32,
) -> Vec<Message> {
    let available = calculate_available(max_context_tokens, output_tokens);
    let budget = available.saturating_sub(system_tokens);

    let total = estimate_messages_tokens_for_prompt(history);
    if total <= budget || history.is_empty() {
        return history.to_vec();
    }

    // HEAD / TAIL 边界
    let head_end = find_head_end(history, PROTECTED_HEAD_TURNS);
    let tail_start = find_tail_start(history, PROTECTED_TAIL_TURNS);

    // HEAD + TAIL 已超预算 → 保留首条 User + 尾部最近消息
    let head_tokens = estimate_messages_tokens_for_prompt(&history[..head_end]);
    let tail_tokens = estimate_messages_tokens_for_prompt(&history[tail_start..]);
    if head_tokens + tail_tokens > budget {
        return keep_head_and_tail(history, budget);
    }

    // 裁剪 MIDDLE
    let middle_budget = budget - head_tokens - tail_tokens;
    let middle = &history[head_end..tail_start];
    let pruned = drop_old_turns(middle, middle_budget);

    // 组装
    let mut result = Vec::with_capacity(
        head_end + pruned.len() + (history.len() - tail_start),
    );
    result.extend_from_slice(&history[..head_end]);
    result.extend(pruned);
    result.extend_from_slice(&history[tail_start..]);
    result
}

/// 极端情况：保留首条 User + 尾部最近消息，确保任务锚点不丢失
fn keep_head_and_tail(history: &[Message], budget: u32) -> Vec<Message> {
    // 1. 第一条 User 消息（任务锚点）
    let first_user_idx = history.iter().position(|m| m.role == Role::User);
    let first_user_tokens = first_user_idx
        .map(|i| estimate_message_tokens_for_prompt(&history[i]))
        .unwrap_or(0);

    if first_user_tokens > budget {
        return first_user_idx.map(|i| vec![history[i].clone()]).unwrap_or_default();
    }

    let remaining = budget.saturating_sub(first_user_tokens);

    // 2. 从尾往前收集消息
    let mut result = Vec::new();
    if let Some(idx) = first_user_idx {
        result.push(history[idx].clone());
    }

    let mut tokens = 0u32;
    let mut tail_msgs: Vec<Message> = Vec::new();
    // 跟踪已确认的 tool_call_ids（其 Assistant(tool_use) 已被收集）
    let mut confirmed_tool_ids: HashSet<String> = HashSet::new();

    for (i, msg) in history.iter().enumerate().rev() {
        if Some(i) == first_user_idx {
            continue;
        }

        let t = estimate_message_tokens_for_prompt(msg);
        if tokens + t > remaining && !tail_msgs.is_empty() {
            break;
        }
        tokens += t;

        // 记录已确认的 tool_use
        if msg.role == Role::Assistant {
            if let Some(ref calls) = msg.tool_calls {
                for call in calls {
                    confirmed_tool_ids.insert(call.id.clone());
                }
            }
        }

        tail_msgs.push(msg.clone());
    }

    tail_msgs.reverse();

    // 3. 过滤孤立的 ToolResult（tool_call_id 在 confirmed_tool_ids 中找不到对应项）
    let filtered_tail: Vec<Message> = tail_msgs
        .into_iter()
        .filter(|msg| {
            if msg.role == Role::Tool {
                if let Some(ref call_id) = msg.tool_call_id {
                    confirmed_tool_ids.contains(call_id.as_str())
                } else {
                    true
                }
            } else {
                true
            }
        })
        .collect();

    // 4. 如果首条 User 和尾部之间有被跳过的消息，插入标记
    if let Some(fi) = first_user_idx {
        let total_after_first = history.len().saturating_sub(fi + 1);
        let has_gap = filtered_tail.len() < total_after_first;
        if has_gap {
            result.push(Message::system(
                "[... earlier messages omitted due to context limit ...]".into()
            ));
        }
    }

    result.extend(filtered_tail);
    result
}
```

### 5.6 处理顺序：先剪枝，后截断

`build()` 内部严格按照以下顺序执行：

```
① trim_context()  →  决定保留哪些消息（可能删除完整轮次）
② truncate_tool_output()  →  对保留的 Tool 消息副本截断到 2000 字符
```

**顺序不能颠倒的原因：** 剪枝决策依赖准确的 token 预算。`trim_context` 内部使用的 `estimate_messages_tokens_for_prompt` 已经按截断后长度估算 Tool 消息的 token，所以预算计算是准的。如果先截断再剪枝，截断后的消息虽然更短，但截断是纯缩减操作——不改变"该保留哪些消息"的决策，反而提前截断会丢失未来可能需要的信息。

### 5.7 PromptBuilder::build 集成

```rust
impl PromptBuilder {
    pub fn build(
        system_template: &str,
        rules: &str,
        history: &[Message],
        working_dir: &Path,
        date_str: &str,
        max_context_tokens: Option<u32>,  // NEW
        output_tokens: u32,                // NEW
    ) -> Vec<Message> {
        let system_msg = build_system(system_template, rules, working_dir, date_str);
        let system_tokens = system_msg.estimated_tokens;

        let filtered: Vec<Message> = history.iter().filter(|m| !m.skip_context).cloned().collect();

        let final_history = if let Some(max_ctx) = max_context_tokens {
            trim_context(&filtered, system_tokens, max_ctx, output_tokens)
        } else {
            filtered
        };

        // 对 Tool 消息截断（仅在发送给 LLM 的副本中，不修改存储）
        let prompt_history: Vec<Message> = final_history
            .into_iter()
            .map(|m| {
                if m.role == Role::Tool {
                    let mut msg = m;
                    msg.content = truncate_tool_output(&msg.content);
                    msg.estimated_tokens = estimate_message_tokens(&msg);
                    msg
                } else {
                    m
                }
            })
            .collect();

        let mut messages = vec![system_msg];
        messages.extend(prompt_history);
        messages
    }
}
```

## 6. run_agent_loop 集成

agent.rs 中一处改动：

### 6.1 追加 ToolResult（完整原始内容）

```rust
// agent.rs: 追加 ToolResult，不截断——存储原始内容
let tool_msg = Message::tool(tr.result.content, &tr.call_id);
```

截断由 `PromptBuilder::build()` 在发送给 LLM 前临时执行。

### 6.2 传递裁剪参数到 PromptBuilder

```rust
// agent.rs: 每次 LLM 调用前
let messages = PromptBuilder::build(
    &enriched_template,
    &rule_engine.get_active_rules(),
    &ctx.history,
    &ctx.working_dir,
    &date_str,
    Some(ctx.config.max_context_tokens),
    ctx.config.max_tokens,
);
```

## 7. 文件改动清单

| 文件 | 改动 |
|------|------|
| `crates/visp-core/src/message.rs` | `Message` 新增 `estimated_tokens: u32`（默认 0），构造时自动填充 |
| `crates/visp-core/src/prompt.rs` | 新增 `estimate_tokens`, `estimate_message_tokens`, `estimate_message_tokens_for_prompt`, `estimate_messages_tokens_for_prompt`, `calculate_available`, `find_head_end`, `find_tail_start`, `drop_old_turns`, `keep_head_and_tail`, `trim_context`, `truncate_tool_output`；修改 `build` 签名增加 `max_context_tokens` 和 `output_tokens`，末尾对 Tool 消息截断 |
| `crates/visp-core/src/provider.rs` | `LlmConfig` 新增 `max_context_tokens: u32`（默认 128_000） |
| `crates/visp-core/src/agent.rs` | 传递 `max_context_tokens` 和 `max_tokens` 给 `build()` |
| `crates/visp-proto/proto/visp.proto` | `LlmConfig` 新增 `optional uint32 max_context_tokens = 5` |
| `crates/visp-daemon/src/config.rs` | `LlmSection` 新增 `max_context_tokens: u32` |
| `crates/visp-daemon/src/service.rs` | `map_llm_config` + `create_session` 合并逻辑处理新字段 |

## 8. 边界情况

| 情况 | 处理 |
|------|------|
| 空历史 | `trim_context` 返回空 Vec |
| 单条消息超过总预算 | `keep_head_and_tail` 保留首条 User（任务锚点） |
| System 消息本身超限 | 不裁剪 system |
| 历史不足 HEAD+TAIL 轮次 | HEAD 和 TAIL 重叠，MIDDLE 空，不裁剪 |
| HEAD+TAIL 已超预算 | 保留首条 User（任务锚点）+ 尾部最近消息，插入 `[... N messages omitted ...]` 标记 |
| MIDDLE 不足一个完整轮次 | 保留 MIDDLE 所有消息 |
| 用户未配置 max_context_tokens | 内置对照表根据 model 名推算，推算失败默认 128K |
| `skip_context` 消息 | `build` 中先过滤，不参与裁剪决策 |
| 工具输出低于 2000 字符 | 不截断 |
| phase 1 token 估算不准 | 仅影响裁剪决策（宁保留多不少删），Phase 2 用 rs-bpe 替换 |
| 工具输出被截断 | 截断仅影响 prompt，`Session.history` 中保留完整原始内容 |
