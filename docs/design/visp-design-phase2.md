# visp Phase 2 阶段设计：LLM Provider + 内置工具

## 1. 阶段目标

实现 LLM 调用能力和基础工具集，让 Agent 可以对话和执行简单操作。

**一句话总结**：LlmProvider trait 能连接真实 LLM 并流式返回结果，Tool trait 能在文件系统和 shell 中执行操作。

## 2. 模块划分

Phase 2 涉及两个新 crate 和一个已有 crate 的扩展：

| Crate | 职责 | 类型 |
|---|---|---|
| **visp-core** | 扩展：LlmError 枚举 + LlmConfig 扩展字段 + trait 错误类型修正 | 修改 |
| **visp-llm** | Anthropic provider 实现 + SSE 流解析 + mock provider | 新建 |
| **visp-tools** | 文件读写、bash 执行、grep/glob 搜索 | 新建 |

### 2.1 visp-core 扩展

Phase 1 定义的 `LlmProvider` trait 使用了 `String` 作为错误类型，过于粗糙。Phase 2 升级为结构化错误类型。

#### 2.1.1 LlmError 枚举

新增到 `visp-core::error`：

- **Network**：网络连接超时、DNS 解析失败等（可重试）
- **RateLimit { retry_after_secs }**：速率限制，携带建议等待时间（可重试）
- **Auth**：API key 无效（不可重试）
- **Api { status, message }**：服务端返回错误（如 400/500，视状态码决定）
- **Stream**：流解析失败（不可重试）

同时 `CoreError::Llm` 变体从 `String` 改为包装 `LlmError`。

#### 2.1.2 LlmConfig 扩展

新增 `extra: HashMap<String, String>` 字段，用于传递 provider 特定参数（如 Anthropic 的 `max_tokens` 上限因模型而异）。api_key 不作为配置参数——它属于 provider 构造参数，不随每次调用传递。

#### 2.1.3 LlmProvider trait 错误类型修正

`chat_stream` 返回的 `Result` 中，`String` 错误统一替换为 `LlmError`。

#### 2.1.4 ToolContext 扩展 (`tool.rs`)

`ToolContext` 新增 `session_id: Option<String>` 字段，用于日志追踪和 bash 确认模式判断。Phase 3 Agent 循环会填充此字段。

### 2.2 visp-llm crate

#### 2.2.1 模块结构

```
visp-llm/
├── Cargo.toml
└── src/
    ├── lib.rs          # 模块声明 + re-export
    ├── anthropic.rs    # Anthropic Messages API 客户端
    ├── streaming.rs    # SSE 事件流解析
    └── mock.rs         # Mock provider（用于测试）
```

#### 2.2.2 Anthropic Provider

实现 `LlmProvider` trait，对接 Anthropic Messages API（`/v1/messages`）。

**构造参数**：
- `api_key: String`
- `base_url: String`（默认 `https://api.anthropic.com`）

**请求转换**（provider 内部处理，trait 不感知）：
- 将 `Vec<Message>` 转为 Anthropic 的 `messages` 数组格式
- 将 `Vec<ToolDefinition>` 转为 Anthropic 的 `tools` 数组（`name` + `description` + `input_schema`）
- 所有请求携带 HTTP 头：`anthropic-version: 2023-06-01`（必填）和 `x-api-key: {api_key}`

消息格式转换规则（Anthropic API 硬性要求）：
- **system 消息分离**：从 messages 数组中提取所有 `role: system` 的消息，拼接后放入请求顶层的 `system` 字段，不在 messages 数组中
- **tool 消息合并**：`role: tool` 的消息不是 Anthropic 的独立角色，必须合并到上一条 `user` 消息中作为 `content` 数组里的 `tool_result` block
- **同角色合并**：如果出现连续两条同角色消息（如两个 `assistant`），必须合并为一条，内容放在 `content` 数组的多个 block 中

**流式响应解析**：
- Anthropic 使用自定义 SSE 事件流：`message_start` → `content_block_start` → `content_block_delta` → `content_block_stop` → `message_delta` → `message_stop`
- 文本增量（text_delta）转为 `ChatEvent::TextDelta`
- 工具调用（tool_use）转为 `ChatEvent::ToolCall { id, name, arguments }`
- `message_stop` 转为 `ChatEvent::Done`

**错误处理**：
- HTTP 4xx → `LlmError::Auth` 或 `LlmError::Api`
- HTTP 429 → `LlmError::RateLimit`（解析响应头 `Retry-After` 秒数，填充到 `retry_after_secs`；若响应头缺失，默认值 30 秒）
- 网络错误 → `LlmError::Network`
- JSON 解析失败 → `LlmError::Stream`

#### 2.2.3 OpenAI Provider

MVP 阶段作为第二优先级。基础框架预留，功能可比 Anthropic 简化。如果 Phase 2 时间充裕则实现，否则标记为 TODO 延后到 Phase 3。

#### 2.2.4 SSE 流解析器

通用 SSE 解析逻辑（`streaming.rs`）：
- 按行读取 HTTP 响应体
- 解析 `event:` 和 `data:` 字段
- 处理 `data: [DONE]` 结束信号（OpenAI 格式）
- 返回解析后的事件流

注意：Anthropic 的事件不严格遵循 SSE 规范，需要专属解析逻辑。

#### 2.2.5 Mock Provider

用于测试的 `MockProvider`：
- 可预设响应序列（列表 of ChatEvent）
- 用于测试 Agent 循环、工具调用流程等上层逻辑
- 不依赖真实 API

### 2.3 visp-tools crate

#### 2.3.1 模块结构

```
visp-tools/
├── Cargo.toml
└── src/
    ├── lib.rs          # 模块声明 + re-export
    ├── file.rs         # 文件读写工具
    ├── bash.rs         # Shell 命令执行
    └── search.rs       # Grep/Glob 搜索
```

#### 2.3.2 工具结果截断

所有工具的统一边界规则：
- 结果内容硬截断：默认 100KB，超出的部分截断并追加 `... [output truncated at N bytes]`
- 截断阈值可配置
- 此规则适用于所有工具（文件读取、bash 输出、搜索结果等）

#### 2.3.3 文件工具

**ReadFile**（实现 `Tool` trait）：
- name: `read_file`
- description: 读取文件内容
- parameters: `{ path: string }`
- 执行：读取文件，返回内容
- 边界：检查路径是否在项目目录内（防路径穿越）
- 文件大小限制：默认 1MB，可配置。超过限制拒绝读取并提示文件过大。
- 二进制检测：读取时检测文件内容，若前 8000 字节中 null 字节占比超过 10%，判定为二进制文件，拒绝读取并返回错误提示 "Binary file detected, use another tool to view it"

**WriteFile**（实现 `Tool` trait）：
- name: `write_file`
- description: 写入文件（覆盖）
- parameters: `{ path: string, content: string }`
- 执行：创建/覆盖文件，写入内容
- 边界：同 read 的路径安全检查
- 写入前处理：若目标路径的父目录不存在，自动递归创建（`create_dir_all`），避免 Agent 先调一次 bash mkdir 再写——节省一次 LLM 往返

**EditFile**（实现 `Tool` trait）：
- name: `edit_file`
- description: 精确字符串替换编辑
- parameters: `{ path: string, old_string: string, new_string: string }`
- 执行：读取文件 → 匹配 `old_string` → 替换为 `new_string` → 写回
- 匹配规则：`old_string` 在文件中必须恰好出现一次
  - 0 次匹配：返回错误 "未找到匹配内容"
  - 多次匹配：返回错误并列出所有匹配位置，要求用户缩小范围
- 边界：路径安全检查

写入策略（防进程崩溃导致文件损坏）：
1. 将新内容写入目标文件同目录下的临时文件（`{filename}.visp-tmp`）
2. 写入完成后，`rename` 临时文件到目标文件（POSIX 保证 rename 是原子操作）
3. 若中途失败，清理临时文件

这个规则与 OpenCode 的 `edit` 工具一致，防止 Agent 在错误位置进行编辑。

#### 2.3.4 Bash 工具

**Bash**（实现 `Tool` trait）：
- name: `bash`
- description: 执行 shell 命令
- parameters: `{ command: string, description?: string }` — 增加可选 description 参数
- 执行：通过 `tokio::process::Command` 执行，捕获 stdout + stderr
- 执行环境：必须设置 `Command::current_dir(ctx.working_dir)`，确保命令在项目目录执行；不设置会导致所有 bash 命令在 daemon 启动目录执行，而非项目目录
- 超时：默认 120 秒
- 子进程安全设置：
  - STDIN 传 `/dev/null`（防交互命令挂死）
  - 设置进程组隔离（防 Ctrl+C 误杀 daemon）

输出编码：
- 进程输出的 `Vec<u8>` 必须使用 `String::from_utf8_lossy()` 转换，不能用 `String::from_utf8()`
- 非 UTF-8 字节会被替换为 `�` 而非 panic
- 避免进程输出包含非法 UTF-8 时 daemon 崩溃

安全模式（通过配置开关控制）：

**确认模式**（默认）：
- 执行前向用户展示即将执行的命令和描述
- 用户确认后执行
- 提供 "总是允许当前会话" 的跳过选项

**信任模式**：
- 直接执行，不询问
- 适合高级用户或可信场景

危险命令黑名单（两种模式均生效）：
- `sudo`、`chmod 777`、`rm -rf /` 等明确破坏性命令
- 被拦截时返回错误，不允许执行
- 黑名单可配置

#### 2.3.5 搜索工具

**Grep**（实现 `Tool` trait）：
- name: `grep`
- description: 内容搜索（正则）
- parameters: `{ pattern: string, path?: string }`

执行策略（优先级从高到低）：

1. 检测 `rg`（ripgrep）是否可用，优先使用
2. 若 `rg` 不可用，提示用户安装 ripgrep（`brew install ripgrep` / `apt install ripgrep`），同时回退到系统 grep
3. 系统 grep 适配：
   - Linux（GNU grep）：使用 `grep -rnPI`（Perl 正则，跳过二进制）
   - macOS（BSD grep）：使用 `grep -rnI`（基础正则，跳过二进制）
   - 在工具初始化时检测平台并记录可用特性
4. 所有命令调用均追加 `-I`（`--binary-files=without-match`）标志，跳过二进制文件
5. 若两种工具均不可用，返回明确错误

grep 子进程安全设置：
- **超时**：30 秒（防止在大目录上搜索耗光时间）
- **目录排除**：自动排除 `.git`、`node_modules`、`target`、`.venv`、`__pycache__`、`dist`、`build` 等常见非源码目录。排除列表可配置
- 这些目录占用大量 IO 且几乎不包含用户关心内容

**Glob**（实现 `Tool` trait）：
- name: `glob`
- description: 文件名搜索（通配符）
- parameters: `{ pattern: string, path?: string }`
- 执行：优先使用 `rg --files -g`（如果 rg 可用），否则使用 `find` 命令
- 跨平台适配：macOS 的 `find` 语法与 Linux 一致，无需特殊处理

#### 2.3.6 路径安全

所有文件操作工具共享一个路径安全校验函数：
- 接受目标路径 + ToolContext.working_dir
- 解析为绝对路径
- **使用 `fs::canonicalize` resolve 所有符号链接**（防 `ln -s /etc/passwd project/link` 绕过）
- 检查解析后的真实路径是否在 working_dir 子树内
- 不在则拒绝操作，返回错误

## 3. 依赖关系

```
visp-core (扩展 Error + trait)
    ↑
    ├── visp-llm (实现 LlmProvider trait)
    │      依赖: reqwest, serde_json, tokio
    │
    └── visp-tools (实现 Tool trait)
           依赖: tokio (bash), 无额外 crate（搜索用系统命令）
```

visp-llm 和 visp-tools 互不依赖，可并行开发。

## 4. 数据流

### 4.1 LLM 调用流

```
调用方 (Agent)
  │
  │ chat_stream(messages, tools, config)
  ▼
LlmProvider impl (Anthropic)
  │
  ├─ 1. 转换 messages → Anthropic 格式
  ├─ 2. 转换 tools → Anthropic function schema
  ├─ 3. 构造 HTTP 请求 (POST /v1/messages, stream: true)
  ├─ 4. 发送请求
  └─ 5. 读取 SSE 响应流
       │
       ├─ content_block_delta(text) → ChatEvent::TextDelta
       ├─ content_block_start(tool_use) → ChatEvent::ToolCall
       └─ message_stop → ChatEvent::Done
```

### 4.2 工具执行流

```
Agent
  │
  │ tool.execute(arguments, context)
  ▼
Tool impl (ReadFile)
  │
  ├─ 1. 路径安全校验
  ├─ 2. 读取文件
  └─ 3. 返回 ToolResult { content, is_error }
```

## 5. Phase 2 不做什么

- ❌ 不实现 OpenAI provider（框架预留，功能后续补充）
- ❌ 不实现流式工具输出（bash 等待完整结果）
- ❌ 不实现工具权限白名单（所有工具对所有会话可用）
- ❌ 不实现 MCP 工具
- ❌ 不实现网络搜索工具

## 6. 验收标准

- `cargo test --workspace` 所有测试通过
- `cargo clippy --workspace -- -D warnings` 通过
- `cargo fmt --check --all` 通过
- Anthropic provider 集成测试通过（需要有效 API key 或 mock）
- 每个工具至少 2 个测试用例（正常 + 边界）
- Mock provider 可用于测试上层逻辑
