# Phase 2：Context Trimmer 独立 crate

## 1. 目标

将 context 裁剪逻辑从 `visp-core` 中抽离为独立 crate `visp-context`，core 只定义接口，context 实现具体策略。

## 2. 动机

Phase 1 的实现中，所有预算计算、轮次裁剪、极端保底、工具输出截断逻辑都集中在 `visp-core/src/prompt.rs`，导致该文件承担了三个不同层次的责任：

| 层次 | 职责 | 是否属于 core？ |
|------|------|:---:|
| System prompt 组装 | 拼 system template + rules + env context | ✅ 是 |
| 消息标记过滤 | skip_context 过滤 | ✅ 是 |
| Token 预算 + 裁剪 + 截断 | context engineering | ❌ 不是 |

将裁剪逻辑独立为 crate 后：

- **visp-core** 回归纯拼装角色，不再包含裁剪策略
- **visp-context** 可独立演进裁剪算法、token 估算精度、压缩策略等
- 未来 Phase 3 引入 LLM 摘要、语义去重等高级策略时，改 visp-context 即可，不影响 core
- 依赖方向干净（依赖倒置），无循环依赖

## 3. 模块划分

### 3.1 visp-core（修改）

**新增**：`ContextTrimmer` trait 定义（新文件 `crates/visp-core/src/context.rs`）

**修改**：
- `AgentLoopContext` 增加 `context_trimmer: Arc<dyn ContextTrimmer + Send + Sync>` 字段（trait 要求 `Send + Sync`，因为 agent 循环在 tokio::spawn 中运行）
- `PromptBuilder::build` 签名新增 `trimmer: &dyn ContextTrimmer` 参数，内部通过 `trimmer.trim()` 替代直接调用 `trim_context`
- `SessionManager::start_loop` 签名新增 `context_trimmer: &Arc<dyn ContextTrimmer + Send + Sync>` 参数，注入 `AgentLoopContext`

**不动**：
- `PromptBuilder` 保持 unit struct，不持有状态
- `SessionManager` 自身不持有 trimmer（由 daemon 服务层持有并传入 start_loop）

**删除**：`prompt.rs` 中移走以下函数/常量：
- `trim_context`、`drop_old_turns`、`keep_head_and_tail`、`find_head_end`、`find_tail_start`
- `estimate_message_tokens_for_prompt`、`estimate_messages_tokens_for_prompt`
- `calculate_available`
- `truncate_tool_output`
- `TOOL_OUTPUT_MAX_CHARS`、`PROTECTED_HEAD_TURNS`、`PROTECTED_TAIL_TURNS`

**保留**：
- `PromptBuilder` unit struct（无字段，不变）
- `build()` 方法（签名增加 `trimmer: &dyn ContextTrimmer` 参数；内部简化为：skip_context 过滤 + 调 trimmer.trim() + 拼接 system_msg）
- `user_query_instruction()` 函数

**保留不动**：
- `message.rs` 中 `Message` 类型、`estimate_tokens`、`estimate_message_tokens` — 这些是消息类型自身的基础方法
- `provider.rs` 中 `LlmConfig` — 配置定义属于 core

### 3.2 visp-context（新建）

新 crate，位于 `crates/visp-context/`。

**职责**：context 裁剪的全部工程逻辑，包括：
- Token 预算计算（`calculate_available`）
- 对话历史裁剪策略（三段式 HEAD/MIDDLE/TAIL）
- 极端保底策略（keep_head_and_tail）
- 工具输出截断（`truncate_tool_output`）
- Prompt 版本 token 估算（`estimate_message_tokens_for_prompt`、`estimate_messages_tokens_for_prompt`）

**对外暴露**：
- `DefaultContextTrimmer` 结构体 — 实现 `ContextTrimmer` trait
- 实现 `Default` trait，默认值：
  - `head_turns: 5`（对应 `PROTECTED_HEAD_TURNS`）
  - `tail_turns: 10`（对应 `PROTECTED_TAIL_TURNS`）
  - `tool_output_max_chars: 2000`（对应 `TOOL_OUTPUT_MAX_CHARS`）
- daemon 侧只需 `DefaultContextTrimmer::default()` 即可构造，无需传递具体数值

**依赖**：
- `visp-core` — 使用 `Message`、`LlmConfig`、`Role`、`ContextTrimmer` trait

### 3.3 visp-daemon（修改）

**修改**：
- `Cargo.toml` 新增 `visp-context` 依赖
- 服务启动时创建 `Arc::new(DefaultContextTrimmer::default())`（一次创建，全局共享——DefaultContextTrimmer 是无状态的，所有参数由内部常量决定）
- 每次调用 `start_loop` 时传入 `&Arc<dyn ContextTrimmer>`，注入 `AgentLoopContext`

**生命周期**：trimmer 与 daemon 进程同生命周期，所有 session 共享同一实例。

### 3.4 不涉及的模块

- `visp-llm`、`visp-tools`、`visp-codegraph`、`visp-cli`、`visp-proto`、`visp`（launcher）— 无改动
- `visp-proto` 无改动 — `LlmConfig` proto 消息不变，只是 core 中的使用方式变了

## 4. ContextTrimmer trait 设计

定义在 `visp-core`。

**输入**：
- `history` — 待裁剪的对话历史（不含 system message，已过滤 skip_context）
- `max_context_tokens` — LLM 上下文窗口总大小
- `system_overhead` — system prompt + rules + env context 等非历史的 token 开销
- `output_tokens` — 期望的输出 token 数

**输出**：
- 裁剪后的 `Vec<Message>`，总 token 数 ≤ 可用预算

**内部自主计算**：
- 可用预算 = `max_context_tokens − max(output_tokens, 4000)`
- 对话历史预算 = 可用预算 − system_overhead
- 裁剪策略（三段式 / 极端保底）
- Tool 输出截断（在裁剪后执行）

**设计决策**：预算计算放在 trait 实现内部而非 caller 侧。理由：
- 预算公式是 context 工程的策略细节（保留 4000 底线、三段式分配等）
- 未来不同模型可能有不同预算策略（如 Claude 和 GPT 输出 token 预留不同）
- caller 不需要理解内部分配逻辑

## 5. PromptBuilder 变化

### 5.1 签名变化

`PromptBuilder` 保持 unit struct，`build` 方法新增 `trimmer: &dyn ContextTrimmer` 参数，由调用方（agent 循环）从 `AgentLoopContext` 中取出传入。

### 5.2 build 流程变化

```
build(system_template, rules, history, working_dir, date_str, max_ctx, output_tokens):
  1. 拼 system prompt → system_msg，计算 system_overhead = system_msg.estimated_tokens
  2. 过滤 skip_context 标记的消息
   3. trimmed = trimmer.trim(history, max_ctx, system_overhead, output_tokens)
  4. 拼接 [system_msg] + trimmed → 返回
```

不再包含：
- ❌ budget 计算
- ❌ 轮次边界识别
- ❌ 裁剪算法
- ❌ Tool 输出截断
- ❌ 极端保底逻辑

## 6. 数据流

```
daemon 启动
  │
  ├─ 创建 Arc<DefaultContextTrimmer>（via Default::default()，内部使用常量 head_turns=5, tail_turns=10, tool_output_max_chars=2000）
  │
  └─→ 每次 start_loop(&self, id, &context_trimmer)
        │
        └─→ AgentLoopContext { ..., context_trimmer: Arc::clone(context_trimmer) }
              │
              └─→ Agent 循环每轮:
                    │
                    PromptBuilder::build(..., ctx.context_trimmer.as_ref())
                      ├─ 拼 system prompt
                      ├─ 过滤 skip_context
                      ├─ trimmer.trim(history, max_ctx, system_overhead, output_tokens)
                      │    ├─ budget = max_ctx − max(output_tokens, 4000)
                      │    ├─ history_budget = budget − system_overhead
                      │    ├─ 总 token ≤ history_budget？ → 直接返回
                      │    ├─ HEAD(5) + MIDDLE(drop_old_turns) + TAIL(10)
                      │    ├─ HEAD+TAIL 超预算？ → keep_head_and_tail 回退
                      │    └─ 返回裁剪后消息（Tool 输出已截断）
                      └─ 拼接 [system_msg] + trimmed → 返回 Vec<Message>
```

## 7. 依赖关系

### 7.1 当前

```
visp-core (含全部 context 逻辑)
    ↑
    ├── visp-llm
    ├── visp-tools
    ├── visp-codegraph
    └── visp-daemon
```

### 7.2 改动后

```
visp-core (ContextTrimmer trait + Message + LlmConfig + PromptBuilder 协调)
    ↑                    ↑
    |           visp-context (impl ContextTrimmer for DefaultContextTrimmer)
    |                    ↑
    +─── visp-daemon ────┘
    |
    ├── visp-llm
    ├── visp-tools
    └── visp-codegraph
```

无循环依赖。`visp-context` 依赖 `visp-core`（使用 trait 和类型），`visp-daemon` 依赖两者进行组装。

## 8. 文件改动清单

| 文件 | 操作 | 说明 |
|------|:---:|------|
| `crates/visp-context/Cargo.toml` | 新建 | 依赖 `visp-core` |
| `crates/visp-context/src/lib.rs` | 新建 | `DefaultContextTrimmer` 实现 + 所有裁剪函数 |
| `crates/visp-core/src/context.rs` | 新建 | `ContextTrimmer` trait 定义 |
| `crates/visp-core/src/lib.rs` | 修改 | 导出 `context` 模块 |
| `crates/visp-core/src/prompt.rs` | 修改 | 删除裁剪逻辑；`build` 签名增加 `trimmer` 参数；内部简化为组装 |
| `crates/visp-core/src/agent.rs` | 修改 | `AgentLoopContext` 加 `context_trimmer: Arc<dyn ContextTrimmer + Send + Sync>` 字段 |
| `crates/visp-core/src/session.rs` | 修改 | `start_loop` 增加 `context_trimmer` 参数，注入 AgentLoopContext |
| `crates/visp-daemon/Cargo.toml` | 修改 | 新增 `visp-context` 依赖 |
| `crates/visp-daemon/src/service.rs` | 修改 | `Arc::new(DefaultContextTrimmer::default())`；每次 `start_loop` 时传入引用 |
| 根 `Cargo.toml` | 修改 | workspace members 加 `crates/visp-context` |
| `crates/visp-core/Cargo.toml` | 无改动 | core 不新增依赖 |

## 9. 边界情况

| 情况 | 处理 |
|------|------|
| 空历史 | trim 返回空 Vec |
| 单条消息超总预算 | keep_head_and_tail 保留首条 User |
| 历史不足 HEAD+TAIL 轮次 | HEAD 和 TAIL 重叠，MIDDLE 为空，不裁剪 |
| HEAD+TAIL 超预算 | 保留首条 User + 尾部最近消息 + 插入省略标记 |
| skip_context 消息 | 在 trim 调用前由 PromptBuilder 过滤，trim 不感知 skip_context |
| Tool 输出低于截断阈值 | 不截断 |
| max_context_tokens 未配置 | PromptBuilder 在 build 调用时传 None，不触发裁剪 |
| trimmer 未配置 | `AgentLoopContext` 的 `context_trimmer` 非 Option，由 daemon 保证注入 |

## 10. 不做什么

- **不引入新 tokenizer**（rs-bpe 等精确计数）— 属于 Phase 3
- **不实现 LLM 摘要策略** — 属于 Phase 3
- **不分拆 Message 类型** — Message、LlmConfig 等基础类型留在 core
- **不改变 proto 协议** — 接口不变，只是实现位置变了
- **不修改 CLI 侧** — CLI 不感知裁剪逻辑变化
- **不对外暴露 DefaultContextTrimmer 的裁剪参数给用户配置** — 本期保持硬编码默认值

## 11. 验收标准

1. 所有现有 `prompt.rs` 中 context 裁剪相关测试迁移至 `visp-context` 并通过
2. `PromptBuilder` 的 `build` 方法不再包含裁剪/截断逻辑
3. `cargo test -p visp-context` 全部通过
4. 全量 `cargo test` 全部通过
5. `cargo clippy -- -D warnings` 零警告
6. `cargo fmt -- --check` 通过
7. Agent 循环运行时行为与拆分前完全一致
