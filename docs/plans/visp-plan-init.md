# visp 工作计划：/init 斜杠命令

## 概述

实现 `/init` 斜杠命令，一键完成项目初始化：创建 `.visp/` 目录、初始化 CodeGraph、构造 prompt 并启动 agent loop 生成 AGENTS.md。

涉及文件：`cli/event.rs`、`daemon/src/command/init.rs`（新增）、`daemon/src/command/mod.rs`（新增）、`daemon/src/service.rs`、`visp-core/src/message.rs`、`visp-core/src/prompt.rs`。

## 步骤 0：Core — Message.skip_context + prompt 过滤

### 0a：Message 新增 skip_context 字段

#### 🔴 红 — 测试

| # | 测试用例 | 描述 |
|---|---------|------|
| 1 | 默认 Message.skip_context = false | 验证现有 message 创建不受影响 |
| 2 | 显式设置 skip_context = true | 验证字段可正常读写 |

#### 🟢 绿 — 实现

`visp-core/src/message.rs`：
- `Message` 结构体新增 `skip_context: bool` 字段（默认 `false`）
- 所有构造函数（`system()`、`user()`、`assistant()`、`tool()`）默认 `skip_context: false`

#### 🧪 测试 → 🔍 类型检查

```bash
cargo test -p visp-core && cargo clippy -- -D warnings
```

#### 📦 提交

```
feat(core): add skip_context field to Message
```

### 0b：prompt 构建过滤 skip_context 消息

#### 🔴 红 — 测试

| # | 测试用例 | 描述 |
|---|---------|------|
| 1 | 历史中有 skip_context=true 的消息被跳过 | 验证该消息不出现在构建的 prompt 中 |
| 2 | skip_context=false 的消息正常出现 | 验证不受影响 |

#### 🟢 绿 — 实现

`visp-core/src/prompt.rs`：
- 遍历 history 构建 prompt 时，过滤 `m.skip_context == true` 的消息

#### 🧪 测试 → 🔍 类型检查

```bash
cargo test -p visp-core && cargo clippy -- -D warnings
```

#### 📦 提交

```
feat(core): filter skip_context messages from prompt history
```

## 步骤 1：CLI 端 — 新增 /init 斜杠命令

### 1a：CLI 识别并转发 /init

#### 🔴 红 — 测试

| # | 测试用例 | 描述 |
|---|---------|------|
| 1 | 输入 `/init` 发送到 daemon | `handle_command` 将 `/init` 作为普通消息发送 |
| 2 | 输入 `/init --force` 发送到 daemon | `--force` 参数原样保留在消息文本中 |
| 3 | `/init` 添加用户消息到对话区 | 对话区显示 "User: /init" |

#### 🟢 绿 — 实现

`cli/event.rs`：
- `handle_command` 新增 `"/init"` 分支
- 提取 `parts[0]` 匹配 `/init`（含 `--force` 变体）
- `app.add_message(LineType::User, text)` 在对话区显示
- `chat_handle.send_input(&text)` 发送给 daemon

#### 🧪 测试 → 🔍 类型检查

```bash
cargo test -p visp-cli && cargo clippy -- -D warnings
```

#### 📦 提交

```
feat(cli): add /init slash command in handle_command
```

## 步骤 2：Daemon — 实现 init 核心逻辑

### 2a：创建 command 模块骨架

#### 🔴 红 — 测试

| # | 测试用例 | 描述 |
|---|---------|------|
| 1 | `prepare()` 返回 `Message` | 验证返回的 Message 角色为 User，内容包含 project_path |
| 2 | `prepare()` 创建 .visp/ 目录 | 验证 `.visp/rules/`、`skills/`、`plans/` 目录被创建 |
| 3 | `.visp/` 已存在时幂等 | 目录已存在时不报错，正常继续 |
| 4 | `prepare()` 创建 CodeGraph 数据库 | 验证 `.visp/codegraph.db` 被创建 |
| 5 | CodeGraph db 已存在时幂等 | `CodeGraph::open()` 幂等，不损坏已有数据 |

#### 🟢 绿 — 实现

`daemon/src/command/mod.rs`（新增）：
- `pub mod init;`

`daemon/src/command/init.rs`（新增）：
- `pub fn prepare(project_path: &Path, text: &str) -> Result<(Message, Vec<String>), String>`
- 解析 `--force`：`text.contains("--force")`
- 创建目录：`std::fs::create_dir_all` 三条路径，每步 push 状态消息
- 初始化 CodeGraph：`CodeGraph::open(project_path)`，失败记 `tracing::warn`，push 状态消息
- **同步等待 build_full**：`cg.build_full(project_path, &config).await`，完成后 push 状态消息
- 构造 prompt：选择默认或 --force 模板，`format!` 注入 project_path
- Message 设 `skip_context = true`
- 返回 `(Message::user(prompt), status_messages)`

#### 🧪 测试 → 🔍 类型检查

```bash
cargo test -p visp-daemon && cargo clippy -- -D warnings
```

#### 📦 提交

```
feat(daemon): add command/init module with prepare() function
```

### 2b：prompt 模板和 --force 逻辑

#### 🔴 红 — 测试

| # | 测试用例 | 描述 |
|---|---------|------|
| 1 | 默认模式 prompt 包含 "读取已有 AGENTS.md" 指令 | 验证 prompt 文本有关键词 |
| 2 | --force 模式 prompt 包含 "重写" 指令 | 验证 prompt 文本有关键词 |
| 3 | prompt 包含正确的 project_path | 验证 project_path 被注入到 prompt 中 |

#### 🟢 绿 — 实现

`daemon/src/command/init.rs`：
- 定义两个 const 字符串：`PROMPT_DEFAULT` 和 `PROMPT_FORCE`
- `prepare()` 中根据 `--force` 选择模板
- `format!` 注入 `project_path`

#### 🧪 测试 → 🔍 类型检查

```bash
cargo test -p visp-daemon && cargo clippy -- -D warnings
```

#### 📦 提交

```
feat(daemon): add init prompt templates with --force logic
```

## 步骤 3：Daemon — chat handler 集成

### 3a：chat handler 调用 init::prepare

#### 🔴 红 — 测试

| # | 测试用例 | 描述 |
|---|---------|------|
| 1 | 收到 "/init" 文本时调用 prepare | 验证 chat handler 正确拦截 /init 消息 |
| 2 | prepare 返回错误时返回错误给客户端 | 验证错误消息正确传递 |
| 3 | prepare 成功后正常启动 agent loop | 验证 session 状态正确，agent loop 启动 |
| 4 | session 非 Idle 时拒绝 /init | 验证 SessionBusy 错误 |

#### 🟢 绿 — 实现

`daemon/src/service.rs`：
- chat handler 的 `UserInput` 分支中，在 session 状态检查之后、agent loop 启动之前
- 添加 `if text.trim().starts_with("/init")` 判断
- 调用 `command::init::prepare(&session.project_path, &text)`
- 失败时发送错误消息给客户端
- 成功时：
  - 先逐个发送 status messages 作为 StatusUpdate（通过 tx 发回 CLI）
  - 用返回的 prompt 替换 `text`，继续正常 agent loop 流程

#### 🧪 测试 → 🔍 类型检查

```bash
cargo test -p visp-daemon && cargo clippy -- -D warnings
```

#### 📦 提交

```
feat(daemon): integrate /init command into chat handler
```

## Wave 并行策略

```
Wave 0 (串行 — 前置依赖):
  └─ 任务: 0a + 0b (Message.skip_context + prompt 过滤)  → commit 1, 2

Wave 1 (并行 — 2 个任务):
  ├─ 任务 A: 1a (CLI /init 命令)        → commit 3
  └─ 任务 B: 2a + 2b (init.rs 核心逻辑)  → commit 4, 5

Wave 2 (串行 — 依赖 Wave 1):
  └─ 任务: 3a (service.rs 集成)          → commit 6
```

```
0a ──→ 0b ─────────────────────────────────────────┐
                                                      │
1a ───────────────────────────────────┐                │
                                        ├──→ 3a
2a ──→ 2b ────────────────────────────┘                │
                                                      │
（0a→0b 是其他步骤的前置条件）───────────────────────────┘
```

## 依赖关系总览

```
visp-core/message.rs ──→ visp-core/prompt.rs ──→ (其他步骤的基础)
                                                    │
cli/event.rs ────────────────────────────────────────┤
                                                     ├──→ daemon/service.rs
daemon/command/init.rs ──→ init::prepare() ──────────┘
```

## 依赖关系总览

```
cli/event.rs ───────────────────────────────┐
                                              ├──→ chat handler 集成
daemon/command/init.rs ──→ init::prepare() ──┘
```

## 测试覆盖汇总

| Wave | 并行 | 模块 | 步骤 | 测试用例 |
|------|------|------|------|---------|
| 0 | 串行 | visp-core/message.rs | 0a | 2 |
| 0 | 串行 | visp-core/prompt.rs | 0b | 2 |
| 1 | 并行 | cli/event.rs | 1a | 3 |
| 1 | 并行 | daemon/command/init.rs | 2a | 5 |
| 1 | 并行 | daemon/command/init.rs | 2b | 3 |
| 2 | 串行 | daemon/service.rs | 3a | 4 |
| **合计** | | | | **19** |

## 备注

- 不需要改 proto，不需要新增 gRPC RPC
- `daemon/src/command/` 目录为新增
- `prepare()` 返回状态消息列表，由 chat handler 发送 StatusUpdate 给 CLI
