# visp Phase 4 工作计划：CLI 前端

## 概述

Phase 4 实现终端交互界面。创建新 crate visp-cli，通过 gRPC 连接 daemon，提供 REPL 对话体验。

---

## 步骤 1：visp-cli 项目骨架

### 🔴 红 — 验证

`cargo build -p visp-cli` 失败（crate 不存在）。

### 🟢 绿 — 实现

- 创建 `crates/visp-cli/Cargo.toml`，依赖：`visp-proto`, `tonic`, `tokio` (rt-multi-thread + macros + signal), `clap` (derive), `rustyline` (default-features = false, features = ["with-file-history"])
- 创建 `crates/visp-cli/src/main.rs`（最小入口：`fn main() {}`）
- 修改 workspace `Cargo.toml` 的 `members` 添加 `"crates/visp-cli"`

### 🧪 测试 → 🔍 类型检查

```bash
cargo build -p visp-cli && cargo clippy -p visp-cli -- -D warnings
```

### 📦 提交

```bash
git add crates/visp-cli/ Cargo.toml Cargo.lock && git commit -m "feat(visp-cli): create crate skeleton"
```

---

## 步骤 2：client.rs + display.rs（并行）

### 2a：gRPC 客户端（client.rs）

新建 `crates/visp-cli/src/client.rs`。在 main.rs 中添加 `mod client;`。

#### 🔴 红 — 测试

测试需要 daemon 运行，用 `#[cfg(test)]` 标记跳过（`#[ignore]`），或编写基础的单元测试：

| # | 测试用例 |
|---|---|
| 1 | `test_client_connect` — 用无效地址连接应返回错误 |

#### 🟢 绿 — 实现

- `VbwClient` 结构体：持有 `CoderDaemonClient<Channel>`
- `VbwClient::connect(addr: &str) -> Result<Self>` — 建立 gRPC 连接
- `health_check(&mut self) -> Result<bool>` — 调用 HealthCheck RPC
- `create_session(&mut self, project_path: &str, config: Option<LlmConfig>) -> Result<Session>` — 调用 CreateSession RPC
- `chat(&mut self, session_id: &str) -> Result<ChatHandle>` — 创建 Chat 双向流

`ChatHandle` 结构体：
- `request_tx: mpsc::Sender<ClientMessage>` — 发送端
- `response_stream: Streaming<ServerMessage>` — 接收端
- `session_id: String` — 用于构造 Cancel/ConfigUpdate 消息
- `send_input(&self, text: &str)` — 发送 UserInput
- `send_response(&self, query_id: &str, approved: bool)` — 发送 UserResponse
- `send_cancel(&self)` — 发送 Cancel
- `send_config_update(&self, config: LlmConfig)` — 发送 ConfigUpdate
- `async fn recv(&mut self) -> Option<ServerMessage>` — 接收下一个消息（内部将 tonic 的 `Err(Status)` 转为 `None`）

使用 tonic 双向流的 channel 分离模式：创建 mpsc channel → `Request::new(ReceiverStream::new(rx))` → `client.chat(request)` → 获得 response stream。

**注意**：`CoderDaemonClient` 名称需匹配 proto 生成的实际类型名。

#### 📦 提交

```bash
git add crates/visp-cli/ && git commit -m "feat(visp-cli): gRPC client with ChatHandle bidirectional stream"
```

### 2b：终端显示（display.rs）

新建 `crates/visp-cli/src/display.rs`。在 main.rs 中添加 `mod display;`。

#### 🔴 红 — 测试

| # | 测试用例 |
|---|---|
| 1 | `test_truncate_long` — 超过 2000 字符时截断并附加 truncated 提示 |
| 2 | `test_truncate_short` — 短内容不截断 |

#### 🟢 绿 — 实现

- `print_streaming(delta: &str)` — 维护 `line_has_output` 状态，实时 `print!` 不换行
- `print_tool_call(name: &str, args: &str)` — 先换行再输出 `🔧 name(args)`
- `print_tool_result(content: &str, is_error: bool)` — 换行 + `📄 content`（2000 字符截断）
- `print_query(message: &str)` — 换行 + `❓ message`
- `print_status(message: &str)` — 换行 + message
- `print_daemon_error(code: &str, message: &str)` — `❌ Error [code]: message`
- `print_cli_error(message: &str)` — `❌ message`
- `print_done()` — 换行 + `✓`
- `truncate(content: &str, max_chars: usize) -> String` — 截断辅助函数

#### 📦 提交

```bash
git add crates/visp-cli/ && git commit -m "feat(visp-cli): terminal display formatting with streaming text and truncation"
```

---

## 步骤 3：REPL 循环（repl.rs）

新建 `crates/visp-cli/src/repl.rs`。在 main.rs 中添加 `mod repl;`。

#### 🔴 红 — 测试

REPL 是终端交互组件，单元测试困难。验证编译通过 + 手动测试。

| # | 测试用例 |
|---|---|
| 1 | 编译通过验证即可（无自动化测试，手动验收） |

#### 🟢 绿 — 实现

- `InputMode` 枚举：`Normal`, `ConfirmQuery { query_id: String }`
- `fn prompt(mode: &InputMode) -> &str` — 返回 `"> "` 或 `"[y/N] "`
- `pub async fn run(session_id: String, mut chat_handle: ChatHandle) -> Result<()>`
  - 创建 `Arc<Mutex<rustyline::Editor<()>>>`
  - 加载历史文件（`~/.visp/history`）
  - 三路 `select!` 循环：
    - 分支 1：`spawn_blocking` 读键盘输入，根据 InputMode 处理
    - 分支 2：`chat_handle.recv().await` 处理 gRPC 响应
    - 分支 3：`tokio::signal::ctrl_c()` 处理取消
  - 特殊命令：`/quit`, `/clear`, `/temp`, `/model`, `/help`
  - `/quit` 时 break loop，保存历史

#### 📦 提交

```bash
git add crates/visp-cli/ && git commit -m "feat(visp-cli): REPL loop with select!, InputMode, and special commands"
```

---

## 步骤 4：main.rs 入口

修改 `crates/visp-cli/src/main.rs`。

#### 🔴 红 — 测试

| # | 测试用例 |
|---|---|
| 1 | 编译通过验证即可 |

#### 🟢 绿 — 实现

- 用 `clap` (derive) 解析 CLI 参数
- 建立 gRPC 连接 → 失败则报错退出
- HealthCheck → 失败则报错退出
- CreateSession（传入可选 LlmConfig）→ 获得 session_id
- 调用 `repl::run(session_id, chat_handle)`

#### 📦 提交

```bash
git add crates/visp-cli/ && git commit -m "feat(visp-cli): main entry with CLI args, connection, and REPL startup"
```

---

## 步骤 5：质量门

```bash
cargo build --workspace && cargo test --workspace && cargo clippy --workspace -- -D warnings && cargo fmt --check --all
```

全部通过后 Phase 4 完成。

---

## Wave 并行策略

Phase 4 只有一个 crate，2 个独立模块可并行：

### Wave 1：骨架（1 个 Agent）

```
Agent A: 步骤 1
```

### Wave 2：独立模块（2 个 Agent，并行）

```
Agent A: 2a (client.rs)
Agent B: 2b (display.rs)
```

### Wave 3：依赖模块（2 个 Agent，串行/部分并行）

```
Agent A: 步骤 3 (repl.rs — 依赖 client + display)
Agent B: 步骤 4 (main.rs — 依赖所有模块)
```

Main.rs 依赖所有模块但可与 repl.rs 同时编写（只要模块声明存在）。

### Wave 4：质量门

```
cargo test --workspace && clippy && fmt
```

---

## 测试覆盖汇总

| Wave | 并行数 | Crate | 步骤 | 测试用例 |
|---|---|---|---|---|
| 1 | 1 | visp-cli | 骨架 | 0 |
| 2 | 2 | visp-cli | client + display | 3 |
| 3 | 2 | visp-cli | repl + main | 0 |
| 4 | — | 全 workspace | 质量门 | — |

总计：**5 个步骤，3 个测试用例**（CLI 多为终端交互，自动化测试有限）。
