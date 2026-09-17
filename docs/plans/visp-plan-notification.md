# visp 工作计划：终端通知功能（notification）

## 概述

依据定稿设计 `docs/design/notification.md` 实现终端桌面通知：visp-cli 在「回合完成」与「需要用户输入」两个事件点，按标准终端通知协议（OSC 99 / OSC 777 / OSC 9）向终端写入转义序列。

> **实现偏离（2026-09-18）**：探测兜底策略已由「未知终端盲发 OSC 9」修订为「未识别终端（含 rmux / tmux 等复用器）回退 BEL」，记录于设计 §1「实现偏离说明」。本计划文内残留的 `Osc9` 兜底描述（探测映射表步骤等）以此修订为准，不再回改，保留原始计划痕迹。

范围：

- visp-config 新增 `[notification]` 配置段（5 字段，serde 默认兜底）
- visp-cli 新增 `notify` 模块（探测器 / 编码器 / 节流器 / 会话过滤 / 引擎编排）
- visp-cli `main.rs` 新增配置加载 + 引擎构造；`event.rs` 两处挂接；`app.rs` 新增引擎字段

设计已明确的硬约束（实现必须遵守）：

1. **写入线程**：通知写入只在主事件循环线程内联同步完成，禁止 spawn；仅 `write_all + flush`，禁止 `println!`
2. **写入内容**：序列以 ST（`\x1b\\`）结尾、无尾随字节
3. **挂接位置**：`Done` 挂接点必须在 `stale_done_expected` 提前 return 分支之后
4. **会话过滤**：Done / UserQuery 统一使用 `is_main` 判别（`session_id.is_empty() || == main_session_id`）
5. **守卫**：`stdout().is_tty()` 不满足时整模块禁用
6. **节流**：Done / UserQuery 两类独立计时，默认 3s

### 实现级决策（设计未定死，本计划固化）

| # | 决策 | 理由 |
|---|---|---|
| D1 | 三种协议统一以 ST 结尾（`\x1b\\`） | kitty 规范示例用 ST；iTerm2 / WezTerm / ghostty 均接受 ST；单一形态保证测试确定性 |
| D2 | 控制字符处理：`\n`/`\r`/`\t` → 空格；其余 C0/C1（含 ESC/BEL）→ 删除 | 满足 safe-UTF-8 要求，同时避免多行正文被拼成一行后不可读 |
| D3 | body 为空（空串或清洗后为空）→ 不发送 | 无意义通知；也避免 UserQuery 空 message 产生空弹窗 |
| D4 | 截断分层：engine 层把 UserQuery body 截到 200 字符；encoder 层强制 OSC 99 payload ≤ 2048 字节（按 UTF-8 字符边界） | 分别对应设计中的文案规则与 kitty 协议硬限制 |
| D5 | **引擎不持有 writer**：`on_event(...) -> Option<Vec<u8>>`，由挂接点内联写 stdout | 满足「写入在事件循环线程内联」约束；编码与节流逻辑成为纯逻辑，单测无需捕获 stdout |
| D6 | 引擎时间参数化：`on_event(..., now: Instant)` | 节流测试无需 sleep，时间线可完全控制 |
| D7 | `AppState` 新增 `pub notify: NotifyEngine`，`AppState::new` 内初始化为 `NotifyEngine::disabled()`，由 `event::run` 构造后注入 | 与既有 `available_models` 注入范式同构（event.rs:105-106）；`AppState::new` 签名不变 → 既有测试构造点零改动 |
| D8 | 引擎暴露只读访问器 `last_sent_at(kind)` / `selected_protocol()` | 供挂接点单测断言「是否发送」，无需为测试引入 stdout 抽象 |

## 步骤 1：visp-config 新增 `[notification]` 配置段

文件：`crates/visp-config/src/config.rs`（内联 `#[cfg(test)] mod tests`，与 `[llm]` 段同风格）

### 1a：NotificationSection 定义与默认值

#### 🔴 红 — 测试

| # | 测试用例 |
|---|---|
| 1 | TOML 无 `[notification]` 段 → 全字段默认（enabled=true, on_complete=true, on_user_input=true, protocol=Auto, min_interval_secs=3） |
| 2 | `enabled = false` 显式解析生效 |
| 3 | `protocol` 四个取值 `"auto"` / `"osc9"` / `"osc777"` / `"kitty99"` 均可解析为对应枚举变体 |
| 4 | 非法 `protocol` 值 → 反序列化返回 Err（沿用现有 serde 错误策略） |
| 5 | `on_complete = false` 且 `on_user_input = true` 两开关独立生效 |
| 6 | `min_interval_secs = 0` 合法且生效（表示不节流） |
| 7 | 回归：`DaemonConfig::default()` 含 notification 段默认值 |
| 8 | 回归：Serialize → Deserialize 往返一致（save_config 路径不丢字段） |

#### 🟢 绿 — 实现

- `pub struct NotificationSection { enabled, on_complete, on_user_input, protocol: NotificationProtocol, min_interval_secs }`，`#[derive(Debug, Clone, Serialize, Deserialize)]`，每字段 `#[serde(default = "...")]`
- `pub enum NotificationProtocol { Auto, Osc9, Osc777, Kitty99 }`，`#[serde(rename_all = "lowercase")]`
- `default_notification_section()` 函数 + `impl Default for NotificationSection`
- `DaemonConfig` 增加字段 `pub notification: NotificationSection`，并在手写 `impl Default`（config.rs:130-142）中补上
- 经 `crates/visp-config/src/lib.rs` 重导出 `NotificationSection` / `NotificationProtocol`
- 同步更新 `docs/daemon.example.toml`（新增注释掉的 `[notification]` 段示例）

#### 🧪 测试 → 🔍 类型检查

```bash
cargo test -p visp-config
cargo clippy -p visp-config -- -D warnings
```

#### 📦 提交

`feat(visp-config): 新增 [notification] 通知配置段`

## 步骤 2：notify 模块 — 类型骨架与探测器

文件：`crates/visp-cli/src/notify.rs` + `crates/visp-cli/src/notify_tests.rs`；`main.rs:1-9` 增加 `mod notify;`

### 2a：类型定义、引擎骨架、协议探测器

#### 🔴 红 — 测试

| # | 测试用例 |
|---|---|
| 1 | `KITTY_WINDOW_ID` 存在 → `Protocol::Kitty99` |
| 2 | `TERM=xterm-kitty` → `Kitty99` |
| 3 | `TERM_PROGRAM=WezTerm` → `Osc777` |
| 4 | `TERM_PROGRAM=ghostty` → `Osc9` |
| 5 | `TERM_PROGRAM=iTerm.app` → `Osc9` |
| 6 | `TERM=xterm-ghostty`（SSH 透传场景）→ `Osc9` |
| 7 | 全空环境 → 兜底 `Osc9`（盲发策略） |
| 8 | `protocol_override = Some(Kitty99)` 且环境为 ghostty → `Kitty99`（配置优先于探测） |
| 9 | `is_tty = false` → `None`（整模块禁用；override 也不能救活） |
| 10 | 冲突：`KITTY_WINDOW_ID` 与 `TERM_PROGRAM=ghostty` 同时存在 → `Kitty99`（优先级 kitty > 777 系 > 9 系） |
| 11 | `NotifyEngine::disabled()` → 任意 `on_event` 均返回 `None` |

#### 🟢 绿 — 实现

- `pub enum Protocol { Osc9, Osc777, Kitty99 }`、`pub enum NotifyKind { Done, UserQuery }`
- `struct Env { kitty_window_id: Option<String>, term: Option<String>, term_program: Option<String> }`，`Env::from_process_env()` + `Env::from_pairs(...)`（测试注入用）
- `pub fn detect(env: &Env, protocol_override: Option<Protocol>, is_tty: bool) -> Option<Protocol>`
  - `is_tty == false` → `None`
  - override 优先；否则按下表映射；无命中 → `Some(Osc9)`（盲发）
- `pub struct NotifyEngine { protocol: Option<Protocol>, enabled/on_complete/on_user_input: bool, min_interval: Duration, last_sent: [Option<Instant>; 2] }`
  - `NotifyEngine::new(section: &NotificationSection, is_tty: bool) -> Self`
  - `NotifyEngine::disabled() -> Self`（protocol=None）
  - 只读访问器 `selected_protocol()` / `last_sent_at(kind)`

#### 🧪 测试 → 🔍 类型检查

```bash
cargo test -p visp-cli
cargo clippy -p visp-cli -- -D warnings
```

#### ♻️ 重构

映射表以 `const` 键值对（或匹配函数）集中表达，避免 if-else 阶梯。

#### 📦 提交

`feat(visp-cli): notify 模块骨架与终端协议探测器`

## 步骤 3：notify 模块 — 编码器、节流器、引擎编排

文件：`crates/visp-cli/src/notify.rs`（同文件，续 2a）

### 3a：三协议编码器（纯函数）

#### 🔴 红 — 测试

| # | 测试用例 |
|---|---|
| 1 | OSC 9 基本：body=`hi` → `\x1b]9;hi\x1b\\` |
| 2 | OSC 777 基本：title=`visp`, body=`hi` → `\x1b]777;notify;visp;hi\x1b\\` |
| 3 | OSC 99 无标题 → 单块 `\x1b]99;;hi\x1b\\` |
| 4 | OSC 99 带标题 → 两段：`i=1:d=0:p=title`（含 `f=<base64("visp")>`）后 `i=1:d=1:p=body` |
| 5 | 控制字符：body 含 `\x1b` / `\x07` / `\x01` → 被删除，输出中除序列定界符外无 ESC |
| 6 | 换行：body 含 `\n` / `\r` / `\t` → 替换为空格（决策 D2） |
| 7 | 多字节 UTF-8：中文 / emoji body 原样保留，字节序列正确 |
| 8 | OSC 99 超长：4000 字节 body → payload 截断至 ≤2048 字节且不在 UTF-8 字符中间切断（决策 D4） |
| 9 | 空 body → 返回 `None`，不生成序列（决策 D3） |
| 10 | C1 控制字符（U+0080..U+009F）→ 删除 |

#### 🟢 绿 — 实现

- `pub fn encode(protocol: Protocol, title: &str, body: &str) -> Option<Vec<u8>>`
- 内部 `sanitize(s: &str) -> String`（D2）与 `truncate_utf8_safe(s: &str, max_bytes: usize) -> &str`（D4）
- `f` 值用 base64 编码应用名 `visp`（`dmlzcA==`）；不引入新依赖（手写 8 行 base64 或复用 workspace 既有 base64 依赖——实现时先查 workspace 是否已有）

#### 🧪 测试 → 🔍 类型检查

```bash
cargo test -p visp-cli notify
```

#### 📦 提交

`feat(visp-cli): 通知编码器支持 OSC 9/777/99 三协议`

### 3b：节流器与引擎编排

#### 🔴 红 — 测试

| # | 测试用例 |
|---|---|
| 1 | 首次 `Done` → 返回 `Some(bytes)` |
| 2 | 同 kind 3s 内第二次 → `None` |
| 3 | 距上次 ≥3s → 再次 `Some` |
| 4 | `Done` 刚发送后立刻 `UserQuery` → `Some`（两类独立计时） |
| 5 | `min_interval_secs = 0` → 连续调用均 `Some` |
| 6 | 时间由参数注入（固定 `Instant` 构造时间线，无 sleep） |
| 7 | 子会话（`is_main=false`）→ `None`（任何 kind） |
| 8 | `enabled=false` → `None` |
| 9 | `on_complete=false`：`Done`→`None`，`UserQuery`→`Some` |
| 10 | `on_user_input=false`：`UserQuery`→`None`，`Done`→`Some` |
| 11 | UserQuery body 取 `message`，超 200 字符被截断（D4） |
| 12 | `message` 为空或纯控制字符 → `None` |
| 13 | 协议未选定（`disabled()`）→ 永远 `None` |

#### 🟢 绿 — 实现

```rust
pub fn on_event(&mut self, kind: NotifyKind, is_main: bool, body: &str, now: Instant) -> Option<Vec<u8>>
```

闸门顺序：`is_main` → `enabled` → 对应 kind 开关 → 节流 → 编码 → 更新 `last_sent`。
配套 `pub fn write_to_stdout(bytes: &[u8])`：`stdout().write_all + flush`，失败 `tracing::debug!` 静默降级（决策 D5，唯一 IO 点）。

#### 🧪 测试 → 🔍 类型检查

```bash
cargo test -p visp-cli
cargo clippy -p visp-cli -- -D warnings
```

#### 📦 提交

`feat(visp-cli): 通知引擎节流、会话过滤与事件编排`

## 步骤 4：装配（配置加载与引擎注入）

文件：`crates/visp-cli/src/main.rs`、`event.rs`、`app.rs`

### 4a：main 早期加载配置、构造引擎、注入 AppState

#### 🔴 红 — 测试

| # | 测试用例 |
|---|---|
| 1 | `AppState::new` 后 `notify` 为 disabled 状态（协议为 None） |
| 2 | 回归：`app_tests.rs` / `event_tests.rs` 既有用例全绿（`AppState::new` 签名不变，构造点零改动） |
| 3 | 注入函数（`app.notify = engine` 的封装或直接赋值）后，`app.notify.selected_protocol()` 与传入配置一致 |

#### 🟢 绿 — 实现

- `app.rs`：`AppState` 增加 `pub notify: NotifyEngine`（`app.rs:1384` 结构体内），`AppState::new`（`app.rs:1480`）内初始化为 `NotifyEngine::disabled()`（决策 D7）
- `main.rs`：`Cli::parse()`（`main.rs:42`）之后、`event::run`（`main.rs:192`）之前调用 `visp_config::load_config(None)`；失败时按现有策略处理（启动告警 + 用默认值继续，**不阻断启动**——通知是可选功能）
- `event.rs`：`run()`（`event.rs:79-88`）增加 `notification: NotifyEngine` 值参数；`ratatui::init()` 与 `AppState::new` 之后赋值 `app.notify = notification`（紧随 `available_models` 注入范式，`event.rs:105-106`）
- 探测器调用点：`main.rs` 构造 engine 时传 `std::io::stdout().is_tty()`；日志记录选定协议（`tracing::debug!`）

#### 🧪 测试 → 🔍 类型检查

```bash
cargo build -p visp-cli && cargo test -p visp-cli
cargo clippy -p visp-cli -- -D warnings
```

#### 📦 提交

`feat(visp-cli): 启动加载 [notification] 配置并注入通知引擎`

## 步骤 5：event.rs 挂接点

文件：`crates/visp-cli/src/event.rs` + `event_tests.rs`

### 5a：Done 挂接（避开 stale 路径）

#### 🔴 红 — 测试

| # | 测试用例 |
|---|---|
| 1 | 主 session `Done`（非 stale）→ `notify.last_sent_at(Done)` 为 `Some` |
| 2 | 主 session `Done` 且 `stale_done_expected = true` → 不发送，且 stale 标记被清除（回归 Cancel 语义） |
| 3 | 子 session `Done`（session_id 命中 tab_bar 子 tab）→ 不发送 |
| 4 | `enabled = false` 的主 session `Done` → 不发送 |

#### 🟢 绿 — 实现

- 在 `Payload::Done` 分支（`event.rs:1046`）的 `stale_done_expected` 提前 return **之后**、其余 tab 逻辑之前插入：
  ```rust
  let is_main = d.session_id.is_empty() || d.session_id == app.main_session_id;
  if let Some(bytes) = app.notify.on_event(NotifyKind::Done, is_main, DONE_BODY, Instant::now()) {
      notify::write_to_stdout(&bytes);
  }
  ```
- `DONE_BODY` 为固定文案常量（「任务完成，等待输入」）

#### 🧪 测试 → 🔍 类型检查

```bash
cargo test -p visp-cli event
```

#### 📦 提交

`feat(visp-cli): 回合完成触发终端通知（避让 Cancel 路径）`

### 5b：UserQuery 挂接（补读 session_id）

#### 🔴 红 — 测试

| # | 测试用例 |
|---|---|
| 1 | `UserQuery.session_id == main_session_id` → 已发送记录为 `Some` |
| 2 | `UserQuery.session_id` 为空 → 按主会话处理，发送 |
| 3 | 子 session 的 `UserQuery` → 不发送 |
| 4 | 回归：`UserQuery` 仍正常构造 `ConfirmState`（原有断言不变） |
| 5 | 正文取自 `uq.message`（含超长截断行为在引擎层已覆盖，此处只断言非空 message 会发出） |

#### 🟢 绿 — 实现

- 在 `Payload::UserQuery` 分支（`event.rs:1000`）中补读 `uq.session_id`，按同一 `is_main` 判别调用 `on_event(NotifyKind::UserQuery, is_main, &uq.message, Instant::now())`

#### 🧪 测试 → 🔍 类型检查

```bash
cargo test -p visp-cli
cargo clippy -p visp-cli -- -D warnings
```

#### 📦 提交

`feat(visp-cli): 需要用户输入时触发终端通知（仅根会话）`

## 步骤 6：整体验证与收尾

### 6a：全量校验

```bash
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo fmt -- --check
```

### 6b：手动验收（需真实终端，对应设计 §6）

| # | 场景 | 预期 |
|---|---|---|
| 1 | ghostty 直连，长任务完成 | 收到桌面通知（OSC 9 路径） |
| 2 | agent 请求工具审批 | 收到通知，正文含工具说明 |
| 3 | kitty 环境（`TERM=xterm-kitty`） | 走 OSC 99，标题 `visp` + 正文正确 |
| 4 | `[notification] protocol = "osc777"` | 按 777 编码（可用 `script`/`cat -v` 旁路观察字节） |
| 5 | `enabled = false` | 完全静默；`on_complete` / `on_user_input` 可独立关闭 |
| 6 | 子 agent tab 完成任务 / 子 agent 审批 | 不通知 |
| 7 | 连续多次审批请求 | 只发一条（3s 节流） |
| 8 | `visp ... | cat`（stdout 非 tty） | 输出中无任何 `\x1b]` 序列 |
| 9 | 主 session 按 Ctrl-C 取消 | 不弹「任务完成」通知 |
| 10 | alacritty / Terminal.app | 无通知但不报错、无副作用（debug 日志可见降级） |

### 6c：记录已知限制

在 `docs/todo/TODO.md` 追加小节，登记设计 §5（不做项）与 §7（未来扩展）：kitty 能力探测（`a=q`）、系统通知 fallback、tmux DCS 包裹、聚焦检测、文案模板。

## Wave 并行策略

### Wave 1：基础层（2 个并行任务）

- 任务 A：步骤 1a（`crates/visp-config/src/config.rs`）
- 任务 B：步骤 2a（`crates/visp-cli/src/notify.rs` + `notify_tests.rs` + `main.rs` 的 `mod` 声明）

无文件重叠，可并行。

### Wave 2：实现与装配（2 个并行任务，依赖 Wave 1）

- 任务 A：步骤 3a → 3b（`notify.rs`，串行）
- 任务 B：步骤 4a（`main.rs` / `event.rs` / `app.rs`）

文件不重叠（A 独占 `notify.rs`，B 独占其余三文件）——但二者**都依赖 2a 定下的类型与构造签名**，故 Wave 1 必须先完成。

### Wave 3：挂接与验收（串行，依赖 Wave 2）

- 步骤 5a → 5b（`event.rs`，同文件串行）→ 步骤 6a → 6b → 6c

## 依赖关系总览

```
1a (visp-config) ─────────┐
                          ├─→ 4a (装配: main/event/app) ──┐
2a (notify: 类型+探测器) ──┤                              ├─→ 5a → 5b → 6 (验收)
                          └─→ 3a → 3b (notify: 编码+引擎) ─┘

Wave 1: 1a ‖ 2a        Wave 2: 3a→3b ‖ 4a        Wave 3: 5a→5b→6
```

## 测试覆盖汇总

| Wave | 并行数 | 模块/包 | 步骤 | 测试用例数 |
|---|---|---|---|---|
| 1 | 2 | visp-config | 1a | 8 |
| 1 | 2 | visp-cli notify | 2a | 11 |
| 2 | 2 | visp-cli notify | 3a | 10 |
| 2 | 2 | visp-cli notify | 3b | 13 |
| 2 | 2 | visp-cli 装配 | 4a | 3 |
| 3 | 1 | visp-cli event | 5a | 4 |
| 3 | 1 | visp-cli event | 5b | 5 |
| 3 | 1 | 手动验收 | 6b | 10（手测） |

合计新增自动化单测 54 条 + 手工验收 10 项。

## 备注

1. **测试文件形态**：visp-cli 用外置测试文件（`notify.rs` + `notify_tests.rs`，末尾 `#[cfg(test)] #[path = "notify_tests.rs"] mod tests;`）；visp-config 用内联 `mod tests`——两 crate 约定不同，勿混用。
2. **base64**：encoder 需对 `f` 值做 base64。实现前先查 workspace 是否已有 base64 依赖，避免新增；否则手写编码（输入仅 ASCII `visp`）。
3. **不新增依赖**：crossterm 已是 visp-cli 直接依赖（`crates/visp-cli/Cargo.toml:19`），通知写入用 `std::io::Stdout` 即可。
4. **不新增 crate**：notify 只被 visp-cli 消费，按设计以模块形态落地。
5. **配置加载失败不阻断启动**：通知是可选功能，`load_config` 出错时用默认值继续并告警（区别于 daemon 的严格策略）。
6. **`stale_done_expected` 是回归高危点**：5a 的测试 2 必须覆盖，避免破坏既有 Cancel 语义。
7. **protoc 前置**：workspace 构建需要 protoc（CI `rust.yml:22-23`），本地 `cargo test --workspace` 前确认可用。
