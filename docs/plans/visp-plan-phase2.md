# visp Phase 2 工作计划：LLM Provider + 内置工具

## 概述

Phase 2 实现 LLM 调用和基础工具。先扩展 visp-core 的类型和 trait，再分 5 个 Wave 并行构建 visp-llm 和 visp-tools。

每个子步骤都是一个独立的 TDD 循环：**红 → 绿 → 测试 → 类型检查 → 重构 → 提交**。

---

## 步骤 1：扩展 visp-core

---

### 1a：新增 LlmError 枚举

#### 🔴 红 — 测试

在 `crates/visp-core/src/error.rs` 的测试模块中编写：

| # | 测试用例 |
|---|---|
| 1 | `test_llmerror_network_display` — `LlmError::Network("timeout".into()).to_string()` 返回预期格式 |
| 2 | `test_llmerror_ratelimit_display` — `LlmError::RateLimit { retry_after_secs: 30 }.to_string()` 包含 "30" |
| 3 | `test_llmerror_auth_display` — `LlmError::Auth("invalid key".into()).to_string()` |
| 4 | `test_llmerror_api_display` — `LlmError::Api { status: 500, message: "oops".into() }.to_string()` |
| 5 | `test_llmerror_stream_display` — `LlmError::Stream("parse error".into()).to_string()` |

运行 `cargo test -p visp-core` 确认失败（类型尚不存在）。

#### 🟢 绿 — 实现

在 `error.rs` 中：
- 定义 `LlmError` 枚举，5 个变体，使用 `thiserror` derive
- 修改 `CoreError::Llm(String)` → `CoreError::Llm(LlmError)`
- 为 `CoreError` 添加 `impl From<LlmError> for CoreError`

#### 🧪 测试

```bash
cargo test -p visp-core
```

5 个测试全部通过。

#### 🔍 类型检查

```bash
cargo clippy -p visp-core -- -D warnings
```

#### ♻️ 重构

检查 `CoreError` 的 Display/Error 实现是否仍然完整，导入是否整洁。

#### 📦 提交

```bash
git add crates/visp-core/src/error.rs
git commit -m "feat(visp-core): add LlmError enum with five variants"
```

---

### 1b：扩展 LlmConfig

#### 🔴 红 — 测试

在 `crates/visp-core/src/provider.rs` 的测试模块中编写：

| # | 测试用例 |
|---|---|
| 1 | `test_llmconfig_default` — `LlmConfig::default()` 的 model、temperature、max_tokens 为预期值，extra 为空 |
| 2 | `test_llmconfig_extra` — 设置 extra 字段后能读取回 |

运行 `cargo test -p visp-core` 确认失败。

#### 🟢 绿 — 实现

- `LlmConfig` 新增 `extra: HashMap<String, String>` 字段
- 保持 `Default` impl（extra 默认为空 HashMap）

#### 🧪 测试

```bash
cargo test -p visp-core
```

2 个测试全部通过。

#### 🔍 类型检查

```bash
cargo clippy -p visp-core -- -D warnings
```

#### ♻️ 重构

无。

#### 📦 提交

```bash
git add crates/visp-core/src/provider.rs
git commit -m "feat(visp-core): extend LlmConfig with extra HashMap field"
```

---

### 1c：修正 LlmProvider trait 错误类型

#### 🔴 红 — 测试

此项为类型级修改，编译即可验证——不需要额外测试用例。确认当前编译会因类型不匹配失败。

#### 🟢 绿 — 实现

- `chat_stream` 返回的 `Result<..., String>` → `Result<..., LlmError>`
- `Stream<Item = Result<ChatEvent, String>>` → `Stream<Item = Result<ChatEvent, LlmError>>`

#### 🧪 测试

```bash
cargo test -p visp-core
```

现有测试全部通过。

#### 🔍 类型检查

```bash
cargo clippy -p visp-core -- -D warnings
```

#### ♻️ 重构

无。

#### 📦 提交

```bash
git add crates/visp-core/src/provider.rs
git commit -m "refactor(visp-core): replace String errors with LlmError in LlmProvider trait"
```

---

### 1d：扩展 ToolContext

#### 🔴 红 — 测试

在 `crates/visp-core/src/tool.rs` 的测试模块中编写：

| # | 测试用例 |
|---|---|
| 1 | `test_toolcontext_default_session_id` — 新建 ToolContext 时 session_id 为 None |

运行 `cargo test -p visp-core` 确认失败。

#### 🟢 绿 — 实现

- `ToolContext` 新增 `session_id: Option<String>` 字段

#### 🧪 测试

```bash
cargo test -p visp-core
```

测试通过。

#### 🔍 类型检查

```bash
cargo clippy -p visp-core -- -D warnings
```

#### ♻️ 重构

无。

#### 📦 提交

```bash
git add crates/visp-core/src/tool.rs
git commit -m "feat(visp-core): add session_id field to ToolContext"
```

---

## 步骤 2：创建 visp-llm crate（委托 @fixer）

**前置**：步骤 1 完成。

@fixer 按以下子步骤顺序执行，每个子步骤独立 TDD。

---

### 2a：项目骨架

#### 🔴 红 — 验证

创建 crate 结构，无需测试。验证：`cargo build -p visp-llm` 失败（crate 尚不存在）。

#### 🟢 绿 — 实现

- 创建 `crates/visp-llm/Cargo.toml`
  - 依赖：visp-core, reqwest, serde, serde_json, tokio, futures, async-trait
- 创建 `crates/visp-llm/src/lib.rs`（空模块声明）

#### 🧪 测试

```bash
cargo build -p visp-llm
```

编译通过。

#### 🔍 类型检查

```bash
cargo clippy -p visp-llm -- -D warnings
```

#### ♻️ 重构

无。

#### 📦 提交

```bash
git add crates/visp-llm/ Cargo.toml Cargo.lock
git commit -m "feat(visp-llm): create crate skeleton"
```

---

### 2b：SSE 流解析

#### 🔴 红 — 测试

在 `crates/visp-llm/src/streaming.rs` 的测试模块中编写：

| # | 测试用例 |
|---|---|
| 1 | `test_parse_sse_valid_event_data` — 有效的 `event: xxx\ndata: {"key":"val"}\n\n` |
| 2 | `test_parse_sse_missing_event_field` — 只有 `data:` 无 `event:` |
| 3 | `test_parse_sse_multiline_data` — `data:` 跨多行 |
| 4 | `test_parse_sse_empty_input` — 空字符串/空行 |
| 5 | `test_parse_sse_invalid_json_data` — `data:` 后不是合法 JSON |

#### 🟢 绿 — 实现

- 按行解析 SSE 格式：读取行 → 解析 `event:` 字段 → 解析 `data:` 字段 → 遇到空行触发事件回调
- 处理 Anthropic 自定义事件类型
- 返回解析后的事件

#### 🧪 测试

```bash
cargo test -p visp-llm
```

#### 🔍 类型检查

```bash
cargo clippy -p visp-llm -- -D warnings
```

#### ♻️ 重构

检查事件解析逻辑是否有重复，提取公共解析函数如有必要。

#### 📦 提交

```bash
git add crates/visp-llm/src/streaming.rs
git commit -m "feat(visp-llm): SSE stream parser for Anthropic events"
```

---

### 2c：Anthropic Provider — 请求构建 + 消息合并

#### 🔴 红 — 测试

创建 `crates/visp-llm/src/anthropic.rs` 测试模块，编写：

| # | 测试用例 |
|---|---|
| 1 | `test_build_request_basic` — 基本 user/assistant 消息转换 |
| 2 | `test_build_request_system_separation` — system 消息提取到顶层 system 字段 |
| 3 | `test_build_request_tool_messages_merged` — tool role 消息合并到 user 消息 tool_result block |
| 4 | `test_build_request_consecutive_same_role_merged` — 连续两条 assistant 消息合并 |
| 5 | `test_build_request_tool_to_anthropic_schema` — ToolDefinition 转为 Anthropic tools 数组格式 |
| 6 | `test_build_request_anthropic_version_header` — 请求包含 anthropic-version 头 |

#### 🟢 绿 — 实现

- `AnthropicProvider` 结构体（字段：api_key, base_url, http_client）
- `AnthropicProvider::build_request` 方法：
  - Message 数组 → Anthropic messages 格式（交替规则、同角色合并、system 分离）
  - ToolDefinition → Anthropic tools 数组（name/description/input_schema）
  - 拼接完整 HTTP 请求（URL + headers + body）
- 所有请求携带 `anthropic-version: 2023-06-01` + `x-api-key` 请求头

#### 🧪 测试

```bash
cargo test -p visp-llm
```

#### 🔍 类型检查

```bash
cargo clippy -p visp-llm -- -D warnings
```

#### ♻️ 重构

检查消息合并逻辑是否清晰，提取通用合并函数。

#### 📦 提交

```bash
git add crates/visp-llm/src/anthropic.rs
git commit -m "feat(visp-llm): Anthropic request builder with message merging"
```

---

### 2d：Anthropic Provider — 事件解析 + 流式响应

#### 🔴 红 — 测试

| # | 测试用例 |
|---|---|
| 1 | `test_parse_content_block_delta` — text_delta → ChatEvent::TextDelta |
| 2 | `test_parse_tool_use` — tool_use block → ChatEvent::ToolCall |
| 3 | `test_parse_message_stop` — message_stop → ChatEvent::Done |
| 4 | `test_parse_429_rate_limit` — HTTP 429 → LlmError::RateLimit，解析 Retry-After 头 |
| 5 | `test_parse_401_auth_error` — HTTP 401 → LlmError::Auth |
| 6 | `test_parse_500_api_error` — HTTP 500 → LlmError::Api |

#### 🟢 绿 — 实现

- `AnthropicProvider::parse_event` 方法：Anthropic 事件 → ChatEvent
  - `content_block_delta(text)` → `ChatEvent::TextDelta`
  - `content_block_start(tool_use)` → `ChatEvent::ToolCall { id, name, arguments }`
  - `message_stop` → `ChatEvent::Done`
- `AnthropicProvider::chat_stream` 方法（完整流式流程）：
  - 构造请求 → 发送 → 读取 SSE 流 → 解析事件 → 返回 `Pin<Box<dyn Stream>>`
- HTTP 错误码 → LlmError 映射（429 解析 Retry-After，401→Auth，4xx/5xx→Api）

#### 🧪 测试

```bash
cargo test -p visp-llm
```

#### 🔍 类型检查

```bash
cargo clippy -p visp-llm -- -D warnings
```

#### ♻️ 重构

检查 parse_event 是否有重复的 match 分支可合并。

#### 📦 提交

```bash
git add crates/visp-llm/src/anthropic.rs
git commit -m "feat(visp-llm): Anthropic event parser and streaming chat implementation"
```

---

### 2e：Mock Provider

#### 🔴 红 — 测试

创建 `crates/visp-llm/src/mock.rs` 测试模块，编写：

| # | 测试用例 |
|---|---|
| 1 | `test_mock_returns_preset_events` — 预设 3 个 ChatEvent，stream 按序返回 |
| 2 | `test_mock_empty_queue` — 空队列时 stream 立即结束 |

#### 🟢 绿 — 实现

- `MockProvider` 结构体（字段：events: Vec<ChatEvent>）
- 构造函数 `new(events: Vec<ChatEvent>) -> Self`
- `impl LlmProvider for MockProvider`
  - chat_stream 将预设 events 逐条返回为文本或工具调用

#### 🧪 测试

```bash
cargo test -p visp-llm
```

#### 🔍 类型检查

```bash
cargo clippy -p visp-llm -- -D warnings
```

#### ♻️ 重构

无。

#### 📦 提交

```bash
git add crates/visp-llm/src/mock.rs crates/visp-llm/src/lib.rs
git commit -m "feat(visp-llm): add MockProvider for testing"
```

---

## 步骤 3：创建 visp-tools crate（委托 @fixer）

**前置**：步骤 1 完成（与步骤 2 可并行执行）。

@fixer 按以下子步骤顺序执行，每个子步骤独立 TDD。

---

### 3a：项目骨架

#### 🔴 红 — 验证

验证 `cargo build -p visp-tools` 失败（crate 尚不存在）。

#### 🟢 绿 — 实现

- 创建 `crates/visp-tools/Cargo.toml`
  - 依赖：visp-core, tokio, async-trait
- 创建 `crates/visp-tools/src/lib.rs`（空模块声明）

#### 🧪 测试

```bash
cargo build -p visp-tools
```

#### 🔍 类型检查

```bash
cargo clippy -p visp-tools -- -D warnings
```

#### ♻️ 重构

无。

#### 📦 提交

```bash
git add crates/visp-tools/ Cargo.toml Cargo.lock
git commit -m "feat(visp-tools): create crate skeleton"
```

---

### 3b：路径安全校验

#### 🔴 红 — 测试

创建 `crates/visp-tools/src/path.rs` 测试模块，编写：

| # | 测试用例 |
|---|---|
| 1 | `test_validate_path_valid` — 合法相对路径通过 |
| 2 | `test_validate_path_parent_traversal` — `../outside` 被拒绝 |
| 3 | `test_validate_path_symlink_bypass` — 创建临时 symlink 指向 /tmp 外部路径，验证被拒绝 |
| 4 | `test_validate_path_absolute_outside` — 绝对路径指向项目外部被拒绝 |

运行时需要创建临时目录和 symlink（#[cfg(unix)]）。

#### 🟢 绿 — 实现

- `validate_path(target: &Path, working_dir: &Path) -> Result<PathBuf, String>`
- `fs::canonicalize` 解析 `working_dir.join(target)` 为真实路径
- 检查真实路径前缀是否匹配 `working_dir` 的真实路径前缀

#### 🧪 测试

```bash
cargo test -p visp-tools
```

#### 🔍 类型检查

```bash
cargo clippy -p visp-tools -- -D warnings
```

#### ♻️ 重构

无。

#### 📦 提交

```bash
git add crates/visp-tools/src/path.rs crates/visp-tools/src/lib.rs
git commit -m "feat(visp-tools): path safety validation with symlink resolution"
```

---

### 3c：结果截断

#### 🔴 红 — 测试

创建 `crates/visp-tools/src/truncate.rs` 测试模块，编写：

| # | 测试用例 |
|---|---|
| 1 | `test_truncate_short_content` — 短于 100KB 的内容不被截断 |
| 2 | `test_truncate_long_content` — 超过 100KB 的内容被截断，末尾含 truncation message |
| 3 | `test_truncate_exact_boundary` — 恰好 100KB 不被截断 |

#### 🟢 绿 — 实现

- `truncate_output(content: &str, max_bytes: usize) -> String`
- 若 content.len() > max_bytes，截断并追加 `\n... [output truncated at N bytes]`
- 默认 max_bytes = 102400（100KB）

#### 🧪 测试

```bash
cargo test -p visp-tools
```

#### 🔍 类型检查

```bash
cargo clippy -p visp-tools -- -D warnings
```

#### ♻️ 重构

无。

#### 📦 提交

```bash
git add crates/visp-tools/src/truncate.rs crates/visp-tools/src/lib.rs
git commit -m "feat(visp-tools): output truncation with configurable limit"
```

---

### 3d：ReadFile 工具

#### 🔴 红 — 测试

创建 `crates/visp-tools/src/file.rs` 测试模块，编写：

| # | 测试用例 |
|---|---|
| 1 | `test_read_file_success` — 临时文件内容正确读取 |
| 2 | `test_read_file_not_found` — 不存在的文件返回错误 |
| 3 | `test_read_file_path_traversal` — 路径穿越被拒绝 |
| 4 | `test_read_file_too_large` — 超过 1MB 拒绝 |
| 5 | `test_read_file_binary` — 含大量 null 字节的文件拒绝（模拟二进制文件） |
| 6 | `test_read_file_truncated` — 接近但不超过 1MB 的文件正常读取 |

#### 🟢 绿 — 实现

- `ReadFile` 结构体（实现 Tool trait）
- name: `read_file`, description: 读取文件
- parameters: `{ "path": "string" }`
- execute 流程：
  1. `validate_path` 安全检查
  2. `fs::metadata` 获取文件大小
  3. 大小检查（> 1MB 拒绝）
  4. 读取前 8000 字节检测 null 占比（> 10% 拒绝）
  5. 读取全部内容
  6. `truncate_output` 截断

#### 🧪 测试

```bash
cargo test -p visp-tools
```

#### 🔍 类型检查

```bash
cargo clippy -p visp-tools -- -D warnings
```

#### ♻️ 重构

检查 `validate_path` + `metadata` 调用是否有提取为公共函数的空间。

#### 📦 提交

```bash
git add crates/visp-tools/src/file.rs crates/visp-tools/src/lib.rs
git commit -m "feat(visp-tools): ReadFile tool with path safety, size limit, and binary detection"
```

---

### 3e：WriteFile 工具

#### 🔴 红 — 测试

| # | 测试用例 |
|---|---|
| 1 | `test_write_file_success` — 写入临时文件，内容正确 |
| 2 | `test_write_file_path_traversal` — 路径穿越被拒绝 |
| 3 | `test_write_file_auto_create_parent` — 父目录不存在时自动创建 |

#### 🟢 绿 — 实现

- `WriteFile` 结构体（实现 Tool trait）
- name: `write_file`, description: 写入文件（覆盖）
- parameters: `{ "path": "string", "content": "string" }`
- execute 流程：
  1. `validate_path` 安全检查
  2. 若父目录不存在，`create_dir_all`
  3. 写入内容

#### 🧪 测试

```bash
cargo test -p visp-tools
```

#### 🔍 类型检查

```bash
cargo clippy -p visp-tools -- -D warnings
```

#### ♻️ 重构

无。

#### 📦 提交

```bash
git add crates/visp-tools/src/file.rs
git commit -m "feat(visp-tools): WriteFile tool with auto parent dir creation"
```

---

### 3f：EditFile 工具

#### 🔴 红 — 测试

| # | 测试用例 |
|---|---|
| 1 | `test_edit_file_success` — 精确替换成功，文件内容正确 |
| 2 | `test_edit_file_no_match` — 0 次匹配返回错误 |
| 3 | `test_edit_file_multiple_matches` — 多次匹配返回错误并列出位置 |
| 4 | `test_edit_file_atomic_write` — 验证写入到临时文件后再 rename，无残留临时文件 |

#### 🟢 绿 — 实现

- `EditFile` 结构体（实现 Tool trait）
- name: `edit_file`, description: 精确字符串替换编辑
- parameters: `{ "path": "string", "old_string": "string", "new_string": "string" }`
- execute 流程：
  1. `validate_path` 安全检查
  2. 读取文件内容
  3. 查找 old_string 出现次数
  4. 0 次 → 返回错误
  5. >1 次 → 返回错误 + 所有行号
  6. 1 次 → 先写临时文件 `.visp-tmp`，再 `rename` 到目标文件（原子写入）

#### 🧪 测试

```bash
cargo test -p visp-tools
```

#### 🔍 类型检查

```bash
cargo clippy -p visp-tools -- -D warnings
```

#### ♻️ 重构

检查 ReadFile/WriteFile/EditFile 是否共享了路径校验逻辑。

#### 📦 提交

```bash
git add crates/visp-tools/src/file.rs
git commit -m "feat(visp-tools): EditFile tool with unique match constraint and atomic write"
```

---

### 3g：Bash 工具

#### 🔴 红 — 测试

创建 `crates/visp-tools/src/bash.rs` 测试模块，编写：

| # | 测试用例 |
|---|---|
| 1 | `test_bash_echo` — `echo hello` 返回 stdout |
| 2 | `test_bash_timeout` — `sleep 10` 在 2 秒内超时（用短超时值测试） |
| 3 | `test_bash_blocked_command` — `sudo echo` 被黑名单拦截 |
| 4 | `test_bash_stdin_closed` — 需要 STDIN 的命令（如 `cat`）不挂死 |
| 5 | `test_bash_current_dir` — 命令在当前工作目录执行（`pwd` 输出匹配） |
| 6 | `test_bash_non_utf8_output` — 输出包含非法 UTF-8 字节不 panic |

#### 🟢 绿 — 实现

- `Bash` 结构体（实现 Tool trait）
- name: `bash`, description: 执行 shell 命令
- parameters: `{ "command": "string" }`
- execute 流程：
  1. 黑名单检查：含 `sudo`、`chmod 777`、`rm -rf /` 则拒绝
  2. `Command::new("sh").arg("-c").arg(command)`
  3. `current_dir(ctx.working_dir)`
  4. `stdin(Stdio::null())`
  5. `tokio::time::timeout(120s, output)`
  6. stdout + stderr 合并，`String::from_utf8_lossy`
  7. `truncate_output` 截断

#### 🧪 测试

```bash
cargo test -p visp-tools
```

#### 🔍 类型检查

```bash
cargo clippy -p visp-tools -- -D warnings
```

#### ♻️ 重构

检查超时处理和输出截断是否有与其他工具共享的逻辑。

#### 📦 提交

```bash
git add crates/visp-tools/src/bash.rs crates/visp-tools/src/lib.rs
git commit -m "feat(visp-tools): Bash tool with safety blacklist, timeout, and UTF-8 safety"
```

---

### 3h：Grep 工具

#### 🔴 红 — 测试

创建 `crates/visp-tools/src/search.rs` 测试模块，编写：

| # | 测试用例 |
|---|---|
| 1 | `test_grep_with_rg` — rg 可用时搜索结果正确 |
| 2 | `test_grep_fallback_to_system` — 模拟 rg 不可用，回退 grep 正常工作 |
| 3 | `test_grep_skips_binary` — 含二进制文件的目录不返回乱码 |
| 4 | `test_grep_timeout` — 模拟大目录搜索在 30 秒内超时 |
| 5 | `test_grep_excludes_dirs` — 自动排除 .git/node_modules 等目录 |

#### 🟢 绿 — 实现

- `Grep` 结构体（实现 Tool trait）
- name: `grep`, description: 内容搜索（正则）
- parameters: `{ "pattern": "string", "path?": "string" }`
- execute 流程：
  1. 检测 `rg --version` 是否可用
  2. rg 可用：`rg -n <pattern> <path>`（rg 自动跳过二进制和排除目录）
  3. rg 不可用：提示安装 + 回退系统 grep
     - Linux: `grep -rnIP <pattern> <path>`
     - macOS: `grep -rnI <pattern> <path>`
     - 排除 dirs: `--exclude-dir={.git,node_modules,target,...}`
  4. 子进程超时：30 秒
  5. `truncate_output` 截断

#### 🧪 测试

```bash
cargo test -p visp-tools
```

#### 🔍 类型检查

```bash
cargo clippy -p visp-tools -- -D warnings
```

#### ♻️ 重构

检查 `detect_rg()` 和 `build_grep_command()` 是否有重复逻辑可提取。

#### 📦 提交

```bash
git add crates/visp-tools/src/search.rs crates/visp-tools/src/lib.rs
git commit -m "feat(visp-tools): Grep tool with rg priority, platform grep fallback, timeout and binary skip"
```

---

### 3i：Glob 工具

#### 🔴 红 — 测试

| # | 测试用例 |
|---|---|
| 1 | `test_glob_with_rg` — rg 可用时 `rg --files -g` 结果正确 |
| 2 | `test_glob_fallback_to_find` — rg 不可用时 find 回退正常 |

#### 🟢 绿 — 实现

- `Glob` 结构体（实现 Tool trait）
- name: `glob`, description: 文件名搜索（通配符）
- parameters: `{ "pattern": "string", "path?": "string" }`
- execute 流程：
  1. rg 可用：`rg --files -g '<pattern>' <path>`
  2. rg 不可用：`find <path> -name '<pattern>'`
  3. `truncate_output` 截断

#### 🧪 测试

```bash
cargo test -p visp-tools
```

#### 🔍 类型检查

```bash
cargo clippy -p visp-tools -- -D warnings
```

#### ♻️ 重构

检查 Grep 和 Glob 是否共享了 `detect_rg()` 逻辑，如有则提取。

#### 📦 提交

```bash
git add crates/visp-tools/src/search.rs
git commit -m "feat(visp-tools): Glob tool with rg/files priority and find fallback"
```

---

## 步骤 4：全 Workspace 质量门

```bash
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo fmt --check --all
```

全部通过后 Phase 2 完成。

---

## Wave 1：visp-core 扩展（1 个 Agent，串行）

```
Agent A: 1a → 1b → 1c → 1d
```

4 个子步骤串行执行。visp-core 是基础设施，所有后续 crate 依赖它。

---

## Wave 2：项目骨架（2 个 Agent，并行）

```
Agent A: 2a (visp-llm 骨架)
Agent B: 3a (visp-tools 骨架)
```

两个 crate 互不依赖，同时创建目录和 Cargo.toml。

---

## Wave 3：独立模块（5 个 Agent，并行）

```
Agent A: 2b (SSE 流解析)
Agent B: 2c (Anthropic 请求构建)
Agent C: 2e (Mock Provider)
Agent D: 3b (路径安全校验)
Agent E: 3c (结果截断)
```

5 个子步骤互不依赖——各自独立的 `.rs` 文件，无代码冲突。

**依赖约束**：
- 2d（事件解析+流式）依赖 2b + 2c — 需要 SSE 解析和请求构建都完成
- 3d~3i（所有工具）依赖 3b + 3c — 需要路径安全和结果截断都完成

---

## Wave 4：集成模块（4 个 Agent，并行）

```
Agent A: 2d (Anthropic 事件解析 + 流式)
Agent B: 3d 3e 3f (文件工具 ReadFile → WriteFile → EditFile, 同文件串行)
Agent C: 3g (Bash 工具)
Agent D: 3h 3i (搜索工具 Grep → Glob, 同文件串行)
```

Agent B 和 Agent D 各自内部串行（共享同一个 `.rs` 文件），4 个 Agent 之间并行。

---

## Wave 5：质量门

```
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo fmt --check --all
```

---

## 依赖关系总览

```
Wave 1:  1a → 1b → 1c → 1d                        [1 Agent, 串行]
               │
      ┌────────┴────────┐
Wave 2:  2a              3a                          [2 Agent, 并行]
      ┌──┼──┐        ┌───┴───┐
Wave 3: 2b 2c 2e     3b      3c                     [5 Agent, 并行]
      └──┬──┘         └───┬───┘
         ▼                ▼
Wave 4: 2d        3d→3e→3f  3g  3h→3i              [4 Agent, 并行]
         │               └───┬───┘
         └───────────────────┘
                  ▼
Wave 5:   质量门                                     [全 workspace]
```

## 测试覆盖汇总

| Wave | Agent 数 | Crate | 子步骤 | 测试用例 |
|---|---|---|---|---|
| 1 | 1 | visp-core | 1a~1d (4) | 8 |
| 2 | 2 | visp-llm + visp-tools | 2a, 3a | 0 (骨架) |
| 3 | 5 | visp-llm + visp-tools | 2b, 2c, 2e, 3b, 3c | 13 |
| 4 | 4 | visp-llm + visp-tools | 2d, 3d~3i | 28 |
| 5 | — | 全 workspace | 质量门 | — |

总计：**18 子步骤，49 测试用例，最多 5 Agent 并行**。

## 备注

- Anthropic 集成测试需要 `ANTHROPIC_API_KEY` 环境变量，CI 环境跳过
- visp-tools 不依赖任何额外 crate（搜索用系统命令，其他用标准库）
- Bash 的确认/信任模式切换属于 Phase 3 Agent 层逻辑，不在本阶段工具层实现
- 每个子步骤的提交信息使用 conventional commits 格式，见各步骤 📦 节
