# visp 工作计划：Context Trimmer 独立 crate（Phase 2）

## 概述

将 context 裁剪逻辑从 `visp-core/src/prompt.rs` 抽离为独立 crate `visp-context`。core 定义 `ContextTrimmer` trait，visp-context 实现具体策略，daemon 组装注入。

基于设计文档：[visp-design-context-management-phase2.md](../design/visp-design-context-management-phase2.md)

---

## Wave 1：Trait 定义（串行，1 个任务）

### 步骤 1：定义 `ContextTrimmer` trait

**文件**：`crates/visp-core/src/context.rs`（新建）、`crates/visp-core/src/lib.rs`（修改）

#### 🔴 红 — 测试

| # | 测试用例 | 描述 |
|---|---------|------|
| 1.1 | `test_context_trimmer_trait_object` | 验证 trait 可作为 `Box<dyn ContextTrimmer + Send + Sync>` 使用 |
| 1.2 | `test_context_trimmer_send_sync` | 编译期验证 trait 满足 Send + Sync |

#### 🟢 绿 — 实现

- 新建 `crates/visp-core/src/context.rs`
- 定义 `ContextTrimmer` trait（`Send + Sync`），包含 `trim()` 方法
  - 输入：`&self`, `&[Message]`, `u32`(max_ctx), `u32`(system_overhead), `u32`(output_tokens)
  - 输出：`Vec<Message>`
- 在 `lib.rs` 中导出 `pub mod context`

#### 🧪 测试 → 🔍 类型检查

```bash
cargo test -p visp-core -- context
cargo clippy -p visp-core -- -D warnings
```

#### 📦 提交

```
feat(core): add ContextTrimmer trait for context pruning abstraction
```

---

## Wave 2：并行实现（4 个并行任务）

### 步骤 2a：新建 visp-context crate

**文件**：`crates/visp-context/Cargo.toml`（新建）、`crates/visp-context/src/lib.rs`（新建）、根 `Cargo.toml`（修改）

#### 🔴 红 — 测试（迁移自 prompt.rs）

所有以下测试从 `crates/visp-core/src/prompt.rs` 迁移至 `crates/visp-context/src/lib.rs`：

| # | 测试用例 | 描述 |
|---|---------|------|
| 2a.1 | `test_trim_all_fit` | 全部历史在预算内时不裁剪 |
| 2a.2 | `test_trim_head_middle_tail` | 三段式裁剪：HEAD 保留 + MIDDLE 裁剪 + TAIL 保留 |
| 2a.3 | `test_trim_empty` | 空历史返回空 |
| 2a.4 | `test_trim_zero_budget` | 零预算时触发 keep_head_and_tail 回退 |
| 2a.5 | `test_trim_single_turn_exceeds_budget` | 单轮（User+Assistant+Tool）超过预算 |
| 2a.6 | `test_drop_old_turns_empty` | 空消息 drop 返回空 |
| 2a.7 | `test_drop_old_turns_all_fit` | 全部在预算内则不删除 |
| 2a.8 | `test_drop_old_turns_removes_full_turns` | 验证按完整轮次删除 |
| 2a.9 | `test_head_and_tail_overlap` | HEAD+TAIL 覆盖全部历史时 MIDDLE 为空 |
| 2a.10 | `test_keep_head_and_tail_first_user` | 极端保底保留首条 User（任务锚点） |
| 2a.11 | `test_keep_head_and_tail_omitted_marker` | 保底模式插入省略标记 |
| 2a.12 | `test_keep_head_and_tail_filters_orphan_toolresults` | 过滤孤立 ToolResult |
| 2a.13 | `test_estimate_tokens_for_prompt_normal` | 非 Tool 消息直接返回 estimated_tokens |
| 2a.14 | `test_estimate_tokens_for_prompt_tool_within_limit` | Tool 消息未超 2000 字符 |
| 2a.15 | `test_estimate_tokens_for_prompt_tool_truncated` | Tool 消息超过 2000 字符按截断估算 |
| 2a.16 | `test_calculate_available` | 预算计算公式验证 |
| 2a.17 | `test_truncate_tool_output_within_limit` | 2000 字符以内不截断 |
| 2a.18 | `test_truncate_tool_output_exceeds_limit` | 超过 2000 字符正确截断并附加标记 |
| 2a.19 | `test_default_context_trimmer_implements_trait` | 验证 DefaultContextTrimmer 实现 ContextTrimmer |
| 2a.20 | `test_default_trimmer_default_values` | Default 使用正确的 5/10/2000 |

#### 🟢 绿 — 实现

- 新建 `crates/visp-context/` crate
- `Cargo.toml`：依赖 `visp-core`
- `lib.rs`：
  - `DefaultContextTrimmer` struct（字段：`head_turns`, `tail_turns`, `tool_output_max_chars`）
  - `Default` trait 实现（默认值 5/10/2000，对应原 `PROTECTED_HEAD_TURNS`/`PROTECTED_TAIL_TURNS`/`TOOL_OUTPUT_MAX_CHARS`）
  - `impl ContextTrimmer for DefaultContextTrimmer`
  - 内部函数：`calculate_available`, `drop_old_turns`, `keep_head_and_tail`, `find_head_end`, `find_tail_start`, `truncate_tool_output`, `estimate_message_tokens_for_prompt`, `estimate_messages_tokens_for_prompt`
- 根 `Cargo.toml`：workspace members 增加 `"crates/visp-context"`

#### 🧪 测试 → 🔍 类型检查

```bash
cargo test -p visp-context
cargo clippy -p visp-context -- -D warnings
cargo fmt -p visp-context -- --check
```

#### 📦 提交

```
feat(context): add visp-context crate with DefaultContextTrimmer
```

---

### 步骤 2b：简化 PromptBuilder（prompt.rs）

**文件**：`crates/visp-core/src/prompt.rs`（修改）

#### 🔴 红 — 测试（更新现有 + 新增）

| # | 测试用例 | 描述 |
|---|---------|------|
| 2b.1 | `test_build_no_context_trimming` | max_context_tokens 为 None 时不调用 trimmer |
| 2b.2 | `test_build_with_context_trimming` | 有 max_context_tokens 时调用 trimmer.trim() |
| 2b.3 | `test_build_skip_context_excluded` | skip_context 消息在 trim 之前过滤，不传给 trimmer |
| 2b.4 | `test_build_trimmer_not_called_when_none` | max_ctx=None 时 trimmer 的 trim 不会被调用 |
| 2b.5 | `test_build_passes_correct_params_to_trimmer` | 验证传给 trimmer.trim() 的参数正确（history, max_ctx, system_overhead, output_tokens） |
| 2b.6 | `test_build_system_prompt_unchanged` | system prompt 拼装逻辑不变 |

使用 Mock trimmer（在测试模块中定义）验证调用行为。

#### 🟢 绿 — 实现

- 从 `prompt.rs` 删除所有裁剪相关函数和常量
- `PromptBuilder::build()` 签名增加 `trimmer: &dyn ContextTrimmer` 参数
- `build()` 内部：
  - 拼 system prompt、计算 system_overhead
  - 过滤 skip_context
  - 当 `max_context_tokens` 为 Some 时调用 `trimmer.trim()`
  - 当 `max_context_tokens` 为 None 时不裁剪
  - 拼接 [system_msg] + trimmed

#### 🧪 测试 → 🔍 类型检查

```bash
cargo test -p visp-core -- prompt
cargo clippy -p visp-core -- -D warnings
cargo fmt -- --check
```

#### 📦 提交

```
refactor(core): simplify PromptBuilder, delegate trimming to ContextTrimmer
```

---

### 步骤 2c：AgentLoopContext 添加 trimmer 字段

**文件**：`crates/visp-core/src/agent.rs`（修改）

#### 🔴 红 — 测试

| # | 测试用例 | 描述 |
|---|---------|------|
| 2c.1 | `test_agent_loop_context_has_trimmer` | AgentLoopContext 包含 context_trimmer 字段，类型为 Arc<dyn ContextTrimmer + Send + Sync> |
| 2c.2 | `test_agent_loop_passes_trimmer_to_build` | Agent 循环将 ctx.context_trimmer.as_ref() 传给 PromptBuilder::build |

#### 🟢 绿 — 实现

- `AgentLoopContext` 增加 `context_trimmer: Arc<dyn ContextTrimmer + Send + Sync>` 字段
- 在 `run_agent_loop` 中调用 `PromptBuilder::build()` 时，传入 `ctx.context_trimmer.as_ref()`

#### 🧪 测试 → 🔍 类型检查

```bash
cargo test -p visp-core -- agent
cargo clippy -p visp-core -- -D warnings
```

#### 📦 提交

```
feat(core): add context_trimmer to AgentLoopContext
```

---

### 步骤 2d：SessionManager.start_loop 增加 trimmer 参数

**文件**：`crates/visp-core/src/session.rs`（修改）

#### 🔴 红 — 测试

| # | 测试用例 | 描述 |
|---|---------|------|
| 2d.1 | `test_start_loop_injects_trimmer` | start_loop 将 trimmer 注入 AgentLoopContext |
| 2d.2 | `test_start_loop_existing_behavior_unchanged` | 除 trimmer 外 start_loop 行为不变（状态检查、token 注册） |

#### 🟢 绿 — 实现

- `SessionManager::start_loop()` 签名增加 `context_trimmer: &Arc<dyn ContextTrimmer + Send + Sync>` 参数
- 构造 `AgentLoopContext` 时填充 `context_trimmer: Arc::clone(context_trimmer)`

#### 🧪 测试 → 🔍 类型检查

```bash
cargo test -p visp-core -- session
cargo clippy -p visp-core -- -D warnings
```

#### 📦 提交

```
feat(core): add context_trimmer param to SessionManager::start_loop
```

---

## Wave 3：组装（串行，1 个任务）

### 步骤 3：Daemon 组装注入

**文件**：`crates/visp-daemon/Cargo.toml`（修改）、`crates/visp-daemon/src/main.rs`（修改）、`crates/visp-daemon/src/service.rs`（修改）

#### 🔴 红 — 测试

| # | 测试用例 | 描述 |
|---|---------|------|
| 3.1 | `test_default_context_trimmer_created` | 验证 daemon 启动代码创建了 DefaultContextTrimmer::default() |
| 3.2 | 全量回归 | `cargo test` 全部通过，确保编译和功能完整 |

#### 🟢 绿 — 实现

- `daemon/Cargo.toml`：增加 `visp-context` 依赖
- `daemon/src/main.rs`：
  - 在 step 6（创建 session manager）之前创建 `let context_trimmer = Arc::new(visp_context::DefaultContextTrimmer::default())`
  - 传给 `CoderDaemonService::new()` 新参数
- `daemon/src/service.rs`：
  - `CoderDaemonService` 增加 `context_trimmer: Arc<DefaultContextTrimmer>` 字段
  - `new()` 接受并存储 trimmer
  - 调用 `session_mgr.start_loop(&session_id, &self.context_trimmer)` 时传入 trimmer 引用

#### 🧪 测试 → 🔍 类型检查

```bash
cargo test
cargo clippy -- -D warnings
cargo fmt -- --check
```

#### 📦 提交

```
feat(daemon): wire DefaultContextTrimmer into agent loop via dependency injection
```

---

## Wave 并行策略

### Wave 1（1 个任务，串行）

```
步骤 1: 定义 ContextTrimmer trait（core/context.rs）
```
其他所有任务依赖此步骤。

### Wave 2（4 个任务，并行）

```
任务 A: 2a — 新建 visp-context crate + 迁移裁剪函数和测试
任务 B: 2b — 简化 PromptBuilder（prompt.rs）
任务 C: 2c — AgentLoopContext 添加 trimmer 字段（agent.rs）
任务 D: 2d — SessionManager.start_loop 增加参数（session.rs）
```

四个任务操作不同文件，无互相依赖，可并行执行。

### Wave 3（1 个任务，串行）

```
步骤 3: Daemon 组装（main.rs + service.rs + Cargo.toml）
```
依赖 Wave 2 全部完成。

### Wave 4：验证

全量质量门禁：`cargo test && cargo clippy -- -D warnings && cargo fmt -- --check`

---

## 依赖关系总览

```
Wave 1
  步骤 1: ContextTrimmer trait
    │
    ├─────────────────────────────┐
    │                             │
Wave 2 (并行)                   │
  ├── 2a: visp-context crate     │
  ├── 2b: prompt.rs 简化 ←───────┤  (都依赖 trait)
  ├── 2c: agent.rs 字段 ←────────┤
  └── 2d: session.rs 参数 ←──────┘
    │
    ├── 全部完成
    │
Wave 3
  步骤 3: Daemon 组装 (依赖 2a + 2b + 2c + 2d)
```

---

## 测试覆盖汇总

| Wave | 并行数 | 模块 | 步骤 | 测试用例数 |
|------|--------|------|------|-----------|
| 1 | 1 | visp-core | ContextTrimmer trait | 2 |
| 2 | 4 | visp-context | DefaultContextTrimmer | 20（迁移+新增） |
| 2 | 4 | visp-core | PromptBuilder 简化 | 6 |
| 2 | 4 | visp-core | AgentLoopContext | 2 |
| 2 | 4 | visp-core | SessionManager | 2 |
| 3 | 1 | visp-daemon | 组装注入 | 2 + 全量回归 |
| **合计** | | | | **34+** |

---

## 备注

- Wave 2 中 2b 的 prompt.rs 测试需要 Mock trimmer，在测试模块中定义，不依赖 visp-context
- prompt.rs 现有测试中，被迁移走的部分将在 2a 中覆盖，留在 prompt.rs 的 5 个 build 相关测试在 2b 中更新
- SessionManager 的 `start_loop` 调用方除 daemon（Wave 3）外，在 core 内部测试中也需要更新调用签名
- `message.rs`、`provider.rs`、`agent/config.rs` 等文件不改动
