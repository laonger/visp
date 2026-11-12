# visp 工作计划：软上限 + Doom Loop 检测

## 概述

将 `run_agent_loop` 的 `for` 循环改为 `loop`，实现三层防护：软上限提示（默认 50 轮注入收尾提示）、硬上限兜底（200 轮强制终止）、doom_loop 检测（连续 5 轮相同工具调用时警告后退出）。

涉及 3 个文件，2 个 Wave。

## Wave 并行策略

```
Wave 1（类型层·可并行）
├─ 1a: crates/visp-core/src/error.rs        ← 新增 StuckInLoop 枚举
└─ 1b: crates/visp-core/src/agent.rs        ← AgentConfig 字段变更

Wave 2（逻辑层·可并行）
├─ 2a: crates/visp-core/src/agent.rs        ← run_agent_loop 重写 + 测试
├─ 2b: crates/visp-daemon/src/config.rs     ← config 字段 + 映射 + 测试
└─ 2c: docs/ + README.md                     ← 配置示例改名 + 字段更新
```

## 步骤 1：类型层（Wave 1，可并行）

### 1a：error.rs — 新增 StuckInLoop 错误码

#### 🔴 红 — 测试

| # | 测试用例 | 描述 |
|---|---------|------|
| 1.1 | stuck_in_loop_display | `AgentErrorCode::StuckInLoop` 的 Display 输出包含 "stuck" 或 "loop" |
| 1.2 | stuck_in_loop_match | 模式匹配 `AgentErrorCode::StuckInLoop` 能正确识别 |

#### 🟢 绿 — 实现

在 `AgentErrorCode` 枚举中新增 `StuckInLoop` 变体，在 Display impl 中添加对应的格式化分支。

#### 🧪 测试 → 🔍 类型检查

```bash
cargo test -p visp-core
cargo clippy -p visp-core -- -D warnings
```

#### 📦 提交

```
feat(core): add StuckInLoop error code for doom loop detection
```

---

### 1b：agent.rs — AgentConfig 字段变更

#### 🔴 红 — 测试

| # | 测试用例 | 描述 |
|---|---------|------|
| 2.1 | soft_limit_default | `AgentConfig::default().soft_limit` 为 50 |
| 2.2 | hard_limit_default | `AgentConfig::default().hard_limit` 为 200 |
| 2.3 | doom_loop_threshold_default | `AgentConfig::default().doom_loop_threshold` 为 5 |
| 2.4 | soft_limit_zero | 设为 0 可以关闭软上限 |
| 2.5 | debug_struct_has_new_fields | Debug 输出包含 soft_limit/hard_limit/doom_loop_threshold |

#### 🟢 绿 — 实现

在 `AgentConfig` 中：

- `max_iterations` 重命名为 `soft_limit`（字段名变更，字段序位置不变）
- 新增 `hard_limit: u32` 字段，默认 200
- 新增 `doom_loop_threshold: u32` 字段，默认 5
- `Default` impl 同步更新

#### 🧪 测试 → 🔍 类型检查

```bash
cargo test -p visp-core
cargo clippy -p visp-core -- -D warnings
```

#### 📦 提交

```
feat(core): add soft_limit, hard_limit, doom_loop_threshold to AgentConfig
```

---

## 步骤 2：逻辑层（Wave 2，可并行）

### 2a：agent.rs — run_agent_loop 重写

#### 🔴 红 — 测试

##### 正常完成

| # | 测试用例 | 描述 |
|---|---------|------|
| 3.1 | normal_completion | 模型一轮内无工具调用 → 正常 `Done` 事件退出，无错误 |
| 3.2 | multi_round_completion | 模型先调工具再一轮无工具调用 → 正常 `Done` 退出 |
| 3.3 | completion_within_soft_limit | 48 轮工具调用后正常完成 → `Done`，无 `MaxIterations` |

##### 软上限

| # | 测试用例 | 描述 |
|---|---------|------|
| 3.4 | soft_limit_injects_prompt | `soft_limit=2`，模型一直调工具：第 2 轮 LLM 调用前注入收尾提示 → agent 继续运行 |
| 3.5 | soft_limit_then_complete | `soft_limit=2`，模型一直调工具：第 2 轮提示后，第 3 轮返回无工具调用 → 正常 `Done` |
| 3.6 | soft_limit_disabled | `soft_limit=0`：即使超过 50 轮也不会注入收尾提示（硬上限兜底） |

##### 硬上限

| # | 测试用例 | 描述 |
|---|---------|------|
| 3.7 | hard_limit_triggers_error | `hard_limit=1`，模型一直调工具 → 第 1 轮后 `MaxIterations` 错误 |
| 3.8 | hard_limit_after_soft_limit | `soft_limit=1, hard_limit=2`：第 1 轮注入软提示，第 2 轮 `MaxIterations` 错误 |

##### Doom Loop 检测

| # | 测试用例 | 描述 |
|---|---------|------|
| 3.9 | doom_loop_warning | `doom_loop_threshold=3`，模型连续 3 轮调同名工具 → 触发警告（`StatusUpdate` 含 "stuck"） |
| 3.10 | doom_loop_recovery | 警告后模型改变工具调用模式 → 继续正常运行，最终 `Done` |
| 3.11 | doom_loop_triggers_stuck | 警告后模型仍连续 3 轮相同工具 → `StuckInLoop` 错误退出 |
| 3.12 | doom_loop_disabled | `doom_loop_threshold=0`：模型永远重复调同名工具 → 永不触发 doom_loop |
| 3.13 | doom_loop_different_args | 每轮工具名相同但参数不同 → 签名不同 → 不触发 doom_loop |

##### 取消和错误

| # | 测试用例 | 描述 |
|---|---------|------|
| 3.14 | cancellation_still_works | 循环中 `CancellationToken` 取消 → `Cancelled` 错误 |
| 3.15 | user_query_still_works | 模型发 `[USER_QUERY]` 标记 → 收到 `UserQuery` 事件，用户回复后继续 |

#### 🟢 绿 — 实现

`run_agent_loop` 的核心改动：

1. **循环结构**：`for _ in 0..max_iterations` → `loop`，新增 `iteration: u32` 局部计数（从 1 开始）
2. **硬上限检查**：在 LLM 调用之前，`iteration >= hard_limit` → 返回 `MaxIterations` 错误
3. **软上限检查**：在 LLM 调用之前，`soft_limit > 0 && iteration >= soft_limit` → 在发送给 LLM 的消息列表中追加收尾提示消息
4. **tool_calls 为空的分支**：原有逻辑不变（加 `[USER_QUERY]` 标记检查 → 用户交互 → continue；无标记 → 正常 Done 返回）
5. **tool_calls 非空的分支**：
   - 计算本轮签名 `(name, args_value)` 列表
   - 推入 `doom_loop_window`（滑动窗口，大小 = `doom_loop_threshold`）
   - 窗口满后检查签名是否全相同：
     - 全相同且 `doom_loop_warned == true` → `StuckInLoop` 错误
     - 全相同且 `doom_loop_warned == false` → 注入警告文本 + 重置窗口 + 标记 `doom_loop_warned = true`
     - 不完全相同 → 正常流程
   - 执行工具（原有逻辑不变）
   - `iteration += 1`

**状态变量**（`run_agent_loop` 内部，不需改 `AgentLoopContext`）：
- `iteration: u32` — 轮次计数（从 1 开始）
- `doom_loop_window: Vec<Vec<(String, serde_json::Value)>>` — 滑动窗口
- `doom_loop_warned: bool` — 是否已发出警告

#### 🧪 测试 → 🔍 类型检查

```bash
cargo test -p visp-core
cargo clippy -p visp-core -- -D warnings
cargo fmt -p visp-core -- --check
```

#### ♻️ 重构

- `test_max_iterations` 测试需调整：用 `hard_limit = 1` 替代 `max_iterations = 1`
- 验证无工具调用时的 done 分支行为是否与改前一致

#### 📦 提交

```
feat(core): rewrite agent loop with soft limit and doom loop detection
```

---

### 2b：config.rs — AgentSection 字段变更

#### 🔴 红 — 测试

| # | 测试用例 | 描述 |
|---|---------|------|
| 4.1 | soft_limit_config | TOML 中 `agent.soft_limit = 30` → 正确解析 |
| 4.2 | doom_loop_threshold_config | TOML 中 `agent.doom_loop_threshold = 3` → 正确解析 |
| 4.3 | backward_compat_max_iterations | TOML 中 `agent.max_iterations = 50` → 仍然可解析（serde alias） |
| 4.4 | default_doom_loop_threshold | 未配置时 `doom_loop_threshold` 默认为 5 |
| 4.5 | agent_config_mapping | `AgentSection` → `AgentConfig` 映射正确传递所有新字段 |

#### 🟢 绿 — 实现

- `AgentSection` 中 `max_iterations` 重命名为 `soft_limit`，加 `#[serde(alias = "max_iterations")]` 支持旧配置
- `AgentSection` 新增 `doom_loop_threshold` 字段，默认值函数 `default_doom_loop_threshold() -> u32 { 5 }`
- `default_config()` 中初始化新字段
- `main.rs` 中 `AgentConfig` 构造新增 `soft_limit` 和 `doom_loop_threshold` 映射
- `hard_limit` 不暴露为配置项（内部常量，仅在 `AgentConfig::default()` 中定义）
- 移除 `default_max_iterations` 函数（不再需要，替换为 `default_soft_limit`）

#### 🧪 测试 → 🔍 类型检查

```bash
cargo test -p visp-daemon
cargo clippy -p visp-daemon -- -D warnings
cargo fmt -p visp-daemon -- --check
```

#### 📦 提交

```
feat(daemon): add soft_limit and doom_loop_threshold config fields
```

### 2c：配置文件改名 + 字段更新

操作：
1. **删除** `docs/daemon.toml`（精简版，与模板冗余）
2. **重命名** `docs/visp-daemon-config.example.toml` → `docs/daemon.example.toml`（更简洁的命名）
3. **更新内容**：文件中 `agent.max_iterations` 换为 `agent.soft_limit`，注释同步为新语义
4. **更新引用**：`README.md` 中指向旧路径的链接改为 `docs/daemon.example.toml`

无测试要求。

#### 🟢 绿 — 实现

**`docs/daemon.example.toml`**（原 `visp-daemon-config.example.toml`）第 60-62 行：

```
[agent]
# 软上限：达到此轮次后在 LLM 调用前注入"请收尾"提示，由模型自主决定何时结束
# 设为 0 表示关闭软上限，依赖硬上限兜底
# soft_limit = 50

# 连续相同工具调用的 doom_loop 检测窗口（0 = 关闭）
# doom_loop_threshold = 5
```

文件名变更：`visp-daemon-config.example.toml` → `daemon.example.toml`。`visp-` 前缀移除，与 `daemon.toml`（已删除）统一命名风格。

#### 📦 提交

```
chore(docs): rename config example to daemon.example.toml, update fields
```

---

## 依赖关系总览

```
error.rs ─────────────────────────────────────────────────────┐
                                                               │
AgentConfig (agent.rs) ─┬─ run_agent_loop (agent.rs)          │
                         │              ↑                      │
                         └─ config.rs ──┘                      │
                                     (AgentSection → AgentConfig)
                                                               │
                                         docs/*.toml ──────────┘
```

## 测试覆盖汇总

| Wave | 并行数 | 模块 | 新增测试数 |
|------|--------|------|-----------|
| 1a | 1 | visp-core/error.rs | 2 |
| 1b | 1 | visp-core/agent.rs (AgentConfig) | 5 |
| 2a | 1 | visp-core/agent.rs (loop) | 15 |
| 2b | 1 | visp-daemon/config.rs | 5 |
| 2c | 1 | docs/ + README.md | 0（纯文档） |
| **合计** | **2 Wave** | **3 代码文件 + 文档** | **27** |

## 备注

- `hard_limit` 不暴露给最终用户配置，仅作为内部安全兜底
- 旧配置中的 `agent.max_iterations` 通过 `#[serde(alias)]` 继续生效，用户无感知
- `soft_limit = 0` 表示关闭软上限（完全依赖硬上限兜底），`doom_loop_threshold = 0` 表示关闭 doom_loop 检测
- 本轮不改 gRPC proto、AgentEvent 枚举、AgentLoopContext、TUI
