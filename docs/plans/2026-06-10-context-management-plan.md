# visp 工作计划：Context 管理 Phase 1

## 概述

实现对话历史的 token 估算、预算计算和裁剪机制。涉及 7 个文件，按 Wave 分 4 批实现。

参考设计：`docs/design/visp-design-context-management.md`（总纲）、`docs/design/visp-design-context-management-phase1.md`（细则）

## 依赖关系总览

```
message.rs  ──→  prompt.rs（核心逻辑）──→  agent.rs
                    ↑
provider.rs ──→  service.rs
                    ↑
config.rs   ──→     ↑
proto       ────────┘
```

## Wave 并行策略

### Wave 1：基础类型（3 个并行任务）

3 个文件互不依赖，可并行执行。

| 任务 | 文件 | 改动 | 测试命令 |
|------|------|------|---------|
| A | `message.rs` | `Message` 新增 `estimated_tokens` 字段，构造器自动填充 | `cargo test -p visp-core` |
| B | `provider.rs` | `LlmConfig` 新增 `max_context_tokens`，默认 128_000 | `cargo test -p visp-core` |
| C | `config.rs` | `LlmSection` 新增 `max_context_tokens`，默认 128_000 | `cargo test -p visp-daemon` |

---

### Wave 2：配置层（1 个任务，依赖 Wave 1 B）

proto 和 service 都依赖 `LlmConfig` 的新字段。

| 任务 | 文件 | 改动 | 测试命令 |
|------|------|------|---------|
| A | `visp.proto` + `service.rs` | proto 新增字段 + `map_llm_config` 映射 + `create_session` 合并 | `cargo test -p visp-daemon` |

---

### Wave 3：核心裁剪逻辑（1 个任务，依赖 Wave 1 A）

所有新函数在 `prompt.rs` 中按顺序实现（同类函数可按 TDD 子步骤推进）。

依赖 `Message.estimated_tokens` 字段。

| 子步骤 | 内容 | 测试总数 | 测试命令 |
|--------|------|---------|---------|
| 3a | Token 估算函数 + 预算公式 + 工具截断 | ~8 | `cargo test -p visp-core` |
| 3b | 边界函数（`find_head_end`, `find_tail_start`） | ~4 | 同上 |
| 3c | `drop_old_turns` | ~5 | 同上 |
| 3d | `keep_head_and_tail` | ~4 | 同上 |
| 3e | `trim_context` 实现 | ~5 | 同上 |
| 3f | `PromptBuilder::build` 更新签名 + Tool 截断 | ~4 | 同上 |

---

### Wave 4：Agent 集成（1 个任务，依赖 Wave 3f）

| 任务 | 文件 | 改动 | 测试命令 |
|------|------|------|---------|
| A | `agent.rs` | 传递 `max_context_tokens` 和 `max_tokens` 给 `build()` | `cargo test -p visp-core` |

---

## 步骤详述

### 步骤 1a：message.rs — Message 新增 estimated_tokens

#### 🔴 红 — 测试

| # | 测试用例 | 描述 |
|---|---------|------|
| 1 | `Message::user("hello").estimated_tokens` | 简单文本估算 = ceil(5/4) = 2 |
| 2 | `Message::user("").estimated_tokens` | 空文本 = 0 |
| 3 | `Message::tool("result", "call-1").estimated_tokens` | Tool 消息含 content + call_id |
| 4 | `Message::assistant("test").estimated_tokens` | Assistant 消息同样填充 |
| 5 | 新建 `Message` 结构体默认值 | `estimated_tokens` 默认 0 |
| 6 | `Message` 序列化/反序列化 | 含 estimated_tokens 字段正常 |

#### 🟢 绿 — 实现

在 `Message` 结构体中增加 `estimated_tokens: u32` 字段（默认 0）。在各构造器（`system`, `user`, `assistant`, `tool`）末尾调用 `estimate_message_tokens(self)` 并填入。

注：`estimate_message_tokens` 本身定义在 `prompt.rs`，Message 构造器中不应直接引用。替代方案：构造器只设默认值 0，由调用方在消息创建后填充。

**修正方案：** Message 构造器设 `estimated_tokens: 0`，由调用方（PromptBuilder 等）在适当时机统一填充。或者将 `estimate_message_tokens` 函数移至 `message.rs` 中。

**推荐：** `estimate_tokens` 和 `estimate_message_tokens` 放在 `message.rs` 中（与 Message 类型同文件），这样构造器可以直接调用。`prompt.rs` 中的其他函数引用它们。

#### 🧪 测试
```bash
cargo test -p visp-core
```

#### 📦 提交
```
feat(visp-core): add estimated_tokens field to Message
```

---

### 步骤 1b：provider.rs — LlmConfig 新增 max_context_tokens

#### 🔴 红 — 测试

| # | 测试用例 | 描述 |
|---|---------|------|
| 1 | `LlmConfig::default().max_context_tokens` | 默认值 = 128_000 |
| 2 | `LlmConfig { max_context_tokens: 64000, .. }` | 可自定义设置 |
| 3 | 现有 `LlmConfig::default()` 其他字段不受影响 | model/temperature/max_tokens 不变 |

#### 🟢 绿 — 实现

```diff
 pub struct LlmConfig {
     pub model: String,
     pub temperature: f64,
     pub max_tokens: u32,
+    pub max_context_tokens: u32,   // 默认 128_000
     pub extra: HashMap<String, String>,
 }
```

`impl Default` 中设置 `max_context_tokens: 128_000`。

#### 🧪 测试
```bash
cargo test -p visp-core
```

#### 📦 提交
```
feat(visp-core): add max_context_tokens to LlmConfig
```

---

### 步骤 1c：config.rs — daemon 配置新增 max_context_tokens

#### 🔴 红 — 测试

| # | 测试用例 | 描述 |
|---|---------|------|
| 1 | `LlmSection` 未设置时默认值 | `max_context_tokens` = 128_000 |
| 2 | 配置文件显式设置 `max_context_tokens = 64000` | 正确解析 |
| 3 | 旧配置文件不包含该字段 | 向后兼容，使用默认值 |

#### 🟢 绿 — 实现

```diff
 pub struct LlmSection {
     pub model: String,
     pub temperature: f64,
     pub max_tokens: u32,
+    #[serde(default = "default_max_context_tokens")]
+    pub max_context_tokens: u32,
     ...
 }
```

新增 `fn default_max_context_tokens() -> u32 { 128_000 }`。

#### 🧪 测试
```bash
cargo test -p visp-daemon
```

#### 📦 提交
```
feat(visp-daemon): add max_context_tokens to daemon config
```

---

### 步骤 2a：proto + service.rs — 配置传递

#### 🔴 红 — 测试

| # | 测试用例 | 描述 |
|---|---------|------|
| 1 | `map_llm_config` 含 `max_context_tokens` | 正确映射 |
| 2 | `map_llm_config` 不含 `max_context_tokens` | 使用默认值 |
| 3 | `create_session` 客户端未传字段 | daemon 默认值覆盖 |
| 4 | `create_session` 客户端传了值 | 客户端的值保留 |

#### 🟢 绿 — 实现

1. `visp.proto` 中 `LlmConfig` 增加 `optional uint32 max_context_tokens = 5;`
2. 重新编译（自动触发 tonic-build）
3. `map_llm_config` 中映射新字段
4. `create_session` 增加合并逻辑：`config.max_context_tokens == LlmConfig::default().max_context_tokens` 时用 daemon 默认值覆盖
5. `CoderDaemonService::new` 中 `LlmConfig` 构建传入 `llm_section.max_context_tokens`

#### 🧪 测试
```bash
# proto 编译
cargo build -p visp-proto

# daemon 测试
cargo test -p visp-daemon
```

#### 📦 提交
```
feat(proto): add max_context_tokens field
feat(visp-daemon): wire max_context_tokens through service layer
```

---

### 步骤 3a：Token 估算函数 + 预算公式 + 工具截断

#### 🔴 红 — 测试

| # | 测试用例 | 描述 |
|---|---------|------|
| 1 | `estimate_tokens("hello")` | 5/4 = ceil(1.25) = 2 |
| 2 | `estimate_tokens("")` | 空字符串 = 0 |
| 3 | `estimate_tokens("a".repeat(10))` | 10/4 = ceil(2.5) = 3 |
| 4 | `estimate_message_tokens(Message::user("hi"))` | content=2 + 1(role) = 3 |
| 5 | `estimate_message_tokens(Message::tool("output", "call_1"))` | content + call_id + +1 role |
| 6 | `estimate_message_tokens_for_prompt(User message)` | 直接返回 estimated_tokens |
| 7 | `estimate_message_tokens_for_prompt(Tool < 2000 chars)` | 直接返回 estimated_tokens |
| 8 | `estimate_message_tokens_for_prompt(Tool > 2000 chars)` | 返回 TOOL_OUTPUT_MAX_CHARS/4 ≈ 500+1 |
| 9 | `calculate_available(128_000, 4_000)` | 128000 - 4000 = 124000 |
| 10 | `calculate_available(128_000, 8_000)` | 128000 - 8000 = 120000 |
| 11 | `truncate_tool_output("short")` | 不变 |
| 12 | `truncate_tool_output("x".repeat(3000))` | 截断到 2000 字符 + "...[truncated..." |
| 13 | `truncate_tool_output("你好".repeat(1500))` | 多字节字符不 panic |

#### 🟢 绿 — 实现

在 `prompt.rs` 中新增：
- `const TOOL_OUTPUT_MAX_CHARS: usize = 2_000`
- `pub fn estimate_tokens(text: &str) -> u32`
- `pub fn estimate_message_tokens(msg: &Message) -> u32`
- `pub fn estimate_message_tokens_for_prompt(msg: &Message) -> u32`
- `pub fn estimate_messages_tokens_for_prompt(messages: &[Message]) -> u32`
- `pub fn calculate_available(max_context_tokens: u32, output_tokens: u32) -> u32`
- `pub fn truncate_tool_output(content: &str) -> String`

`estimate_tokens` 放在 `message.rs` 中（与 Message 类型同文件），以便构造器调用。其他函数放 `prompt.rs`。

#### 🧪 测试
```bash
cargo test -p visp-core
```

#### 📦 提交
```
feat(visp-core): implement token estimation, budget formula, tool truncation
```

---

### 步骤 3b：边界函数

#### 🔴 红 — 测试

| # | 测试用例 | 描述 |
|---|---------|------|
| 1 | `find_head_end([User, Asst, User, Asst], 1)` | 返回第 2 个 User 的位置 = 2 |
| 2 | `find_head_end([User, Asst], 5)` | 不足 5 轮，返回 len = 2 |
| 3 | `find_head_end([], 2)` | 空列表，返回 0 |
| 4 | `find_tail_start([U, A, U, A, U, A], 2)` | 倒数第 2 轮起点 |
| 5 | `find_tail_start([U, A], 5)` | 不足 5 轮，返回 0 |
| 6 | `find_tail_start([], 2)` | 空列表，返回 0 |

#### 🟢 绿 — 实现

在 `prompt.rs` 中新增：
- `pub fn find_head_end(history: &[Message], n: usize) -> usize`
- `pub fn find_tail_start(history: &[Message], n: usize) -> usize`

#### 🧪 测试
```bash
cargo test -p visp-core
```

#### 📦 提交
```
feat(visp-core): add turn boundary detection functions
```

---

### 步骤 3c：drop_old_turns

#### 🔴 红 — 测试

| # | 测试用例 | 描述 |
|---|---------|------|
| 1 | 全部消息在预算内 | 返回全部 |
| 2 | 需删除 1 个完整轮次 | [U1, A1, U2, A2, U3, A3] budget=2轮 → [U3, A3] |
| 3 | 需删除多轮 | 同上，budget=1轮 → [U3, A3] |
| 4 | 不足一个完整轮次 | 保留全部 |
| 5 | 空列表 | 返回空 |

#### 🟢 绿 — 实现

在 `prompt.rs` 中新增 `fn drop_old_turns(messages: &[Message], budget: u32) -> Vec<Message>`

#### 🧪 测试
```bash
cargo test -p visp-core
```

#### 📦 提交
```
feat(visp-core): implement drop_old_turns
```

---

### 步骤 3d：keep_head_and_tail

#### 🔴 红 — 测试

| # | 测试用例 | 描述 |
|---|---------|------|
| 1 | 保留首条 User + 尾部 | 返回 [User1, tail_msg] |
| 2 | 尾部含孤立 ToolResult | ToolResult 被过滤 |
| 3 | 尾部含合法 ToolResult | ToolResult 保留 |
| 4 | 首条 User 和尾部之间有间隙 | 插入 `[... omitted ...]` 标记 |

#### 🟢 绿 — 实现

在 `prompt.rs` 中新增 `fn keep_head_and_tail(history: &[Message], budget: u32) -> Vec<Message>`

注意孤儿 ToolResult 过滤逻辑（`confirmed_tool_ids`）。

#### 🧪 测试
```bash
cargo test -p visp-core
```

#### 📦 提交
```
feat(visp-core): implement keep_head_and_tail fallback
```

---

### 步骤 3e：trim_context

#### 🔴 红 — 测试

| # | 测试用例 | 描述 |
|---|---------|------|
| 1 | 全部在预算内 | 直接返回全部 |
| 2 | 需裁剪 MIDDLE | 删除最早轮次 |
| 3 | HEAD + TAIL 已超预算 | 走 `keep_head_and_tail` |
| 4 | 空 history | 返回空 |
| 5 | 单条消息超预算 | `keep_head_and_tail` 保留首条 User |

#### 🟢 绿 — 实现

在 `prompt.rs` 中新增 `pub fn trim_context(...)`，整合 `find_head_end`、`find_tail_start`、`drop_old_turns`、`keep_head_and_tail`。

#### 🧪 测试
```bash
cargo test -p visp-core
```

#### 📦 提交
```
feat(visp-core): implement trim_context with Head/Middle/Tail strategy
```

---

### 步骤 3f：PromptBuilder::build 更新

#### 🔴 红 — 测试

| # | 测试用例 | 描述 |
|---|---------|------|
| 1 | `build` 不传 `max_context_tokens` | 行为不变（不裁剪） |
| 2 | `build` 传 `max_context_tokens` | 调用 `trim_context` |
| 3 | `build` 返回的消息中 Tool 内容已截断 | 验证 content 被 `truncate_tool_output` 处理 |
| 4 | `build` 中 `skip_context` 消息被排除 | 不参与裁剪，不出现在返回中 |
| 5 | system 消息的 `estimated_tokens` 正确 | 不影响 |

#### 🟢 绿 — 实现

修改 `PromptBuilder::build` 签名：

```rust
pub fn build(
    system_template: &str,
    rules: &str,
    history: &[Message],
    working_dir: &Path,
    date_str: &str,
    max_context_tokens: Option<u32>,
    output_tokens: u32,
) -> Vec<Message>
```

内部：
1. 构建 system 消息，获取 `system_msg.estimated_tokens`
2. 过滤 `skip_context` 消息
3. 如有 `max_context_tokens` → 调用 `trim_context`
4. 对 Tool 消息副本执行 `truncate_tool_output`
5. 组装并返回

#### 🧪 测试
```bash
cargo test -p visp-core
```

#### 📦 提交
```
feat(visp-core): update PromptBuilder::build with context trimming
```

---

### 步骤 4a：agent.rs — 传递参数

#### 🔴 红 — 测试

| # | 测试用例 | 描述 |
|---|---------|------|
| 1 | `run_agent_loop` 传递 `max_context_tokens` | 验证 `build()` 收到值 |
| 2 | `run_agent_loop` 传递 `max_tokens` | 验证 `build()` 收到值 |
| 3 | 工具结果存入完整内容 | ToolResult 不提前截断 |

测试方式：通过 TestProvider 验证传递给 build() 的参数，或检查历史中的 ToolResult 内容。

#### 🟢 绿 — 实现

修改 `agent.rs` 中调用 `PromptBuilder::build` 处：

```diff
 let messages = PromptBuilder::build(
     &enriched_template,
     &rule_engine.get_active_rules(),
     &ctx.history,
     &ctx.working_dir,
     &date_str,
+    Some(ctx.config.max_context_tokens),
+    ctx.config.max_tokens,
 );
```

#### 🧪 测试
```bash
cargo test -p visp-core
```

#### 📦 提交
```
feat(visp-core): wire max_context_tokens into agent loop
```

---

## 测试覆盖汇总

| Wave | 并行数 | 文件 | 步骤 | 测试用例数 |
|------|-------|------|------|-----------|
| 1 | 3 | `message.rs` | 1a | 6 |
| | | `provider.rs` | 1b | 3 |
| | | `config.rs` | 1c | 3 |
| 2 | 1 | `proto` + `service.rs` | 2a | 4 |
| 3 | 1（6 个子步骤串行）| `prompt.rs` | 3a-3f | ~30 |
| 4 | 1 | `agent.rs` | 4a | 3 |
| **合计** | | **7 个文件** | | **~49** |

## 提交清单

| # | 提交消息 | 涉及文件 |
|---|---------|---------|
| 1 | `feat(visp-core): add estimated_tokens field to Message` | message.rs |
| 2 | `feat(visp-core): add max_context_tokens to LlmConfig` | provider.rs |
| 3 | `feat(visp-daemon): add max_context_tokens to daemon config` | config.rs |
| 4 | `feat(proto): add max_context_tokens field` | visp.proto |
| 5 | `feat(visp-daemon): wire max_context_tokens through service layer` | service.rs |
| 6 | `feat(visp-core): implement token estimation, budget formula, tool truncation` | prompt.rs, message.rs |
| 7 | `feat(visp-core): add turn boundary detection functions` | prompt.rs |
| 8 | `feat(visp-core): implement drop_old_turns` | prompt.rs |
| 9 | `feat(visp-core): implement keep_head_and_tail fallback` | prompt.rs |
| 10 | `feat(visp-core): implement trim_context with Head/Middle/Tail strategy` | prompt.rs |
| 11 | `feat(visp-core): update PromptBuilder::build with context trimming` | prompt.rs |
| 12 | `feat(visp-core): wire max_context_tokens into agent loop` | agent.rs |

## 备注

1. **`estimate_tokens` 放 message.rs**：为了让 `Message` 构造器直接调用，`estimate_tokens` 和 `estimate_message_tokens` 放在 `message.rs` 中，避免跨文件依赖。
2. **Proto 编译**：修改 `.proto` 后 `cargo build` 自动触发代码生成（tonic-build），无需手动执行生成命令。
3. **Backward compatibility**：`config.rs` 中使用 `#[serde(default)]` 确保旧配置文件不含新字段时也能解析。
4. **不在此计划范围内**：Phase 2 的 rs-bpe 集成、Token Prefix Sum、LLM 摘要压缩。
