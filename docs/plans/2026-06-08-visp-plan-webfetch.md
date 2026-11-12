# WebFetch 工具工作计划

## 概述

实现 `fetch_web` 工具：HTTP/HTTPS 获取网页 → HTML→Markdown 转换 → 返回给 LLM。

涉及：visp-core（Tool trait 扩展）、visp-tools（新工具）、visp-daemon（配置+注册）。

---

## 步骤 1：依赖 + Tool trait 扩展

### 1a：添加依赖

**visp-tools/Cargo.toml** 添加：
- `reqwest.workspace = true`（已定义，features=["json","stream"]）
- `html-to-markdown-rs = "3"`

**验证**：`cargo build -p visp-tools` 通过

---

### 1b：扩展 Tool trait——参数级审批

为了让 WebFetch 能根据 URL 动态决定是否需要用户确认，给 Tool trait 加一个带参数的审批方法。

#### 🔴 红 — 测试

| # | 测试用例 | 描述 |
|---|---------|------|
| 1 | `test_requires_approval_for_default` | 未覆盖的 tool，`requires_approval_for` 返回与 `requires_approval()` 一致 |
| 2 | `test_requires_approval_for_override` | 覆盖 `requires_approval_for` 的 tool 返回自定义值 |

**涉及文件**：`crates/visp-core/src/tool.rs`

#### 🟢 绿 — 实现

在 `Tool` trait 中添加方法：

```rust
/// 根据参数判断是否需要用户确认（默认调用 requires_approval()）
fn requires_approval_for(&self, _arguments: &serde_json::Value) -> bool {
    self.requires_approval()
}
```

所有现有 tool 无需修改——默认实现保持向后兼容。

#### 🧪 测试 → 🔍 类型检查

```bash
cargo test -p visp-core && cargo clippy -p visp-core -- -D warnings
```

#### 📦 提交

`feat(visp-core): add requires_approval_for to Tool trait for argument-aware approval`

---

### 1c：Agent loop 使用新的审批方法

#### 🔴 红 — 测试

| # | 测试用例 | 描述 |
|---|---------|------|
| 1 | 现有 agent 测试全部通过，确认参数级审批不影响已有行为 |

#### 🟢 绿 — 实现

`crates/visp-core/src/agent.rs` 中，将：

```rust
let requires_approval = registry
    .get(&tc.name)
    .map(|t| t.requires_approval())
    .unwrap_or(false);
```

改为：

```rust
let requires_approval = registry
    .get(&tc.name)
    .map(|t| t.requires_approval_for(&tc.arguments))
    .unwrap_or(false);
```

同时将 UserQuery message 从 `format!("Allow tool execution: {}?", tc.name)` 改为包含 URL 参数：

```rust
let args_display = serde_json::to_string(&tc.arguments).unwrap_or_default();
message: format!("Allow tool: {}({})?", tc.name, args_display),
```

#### 🧪 测试 → 🔍 类型检查

```bash
cargo test -p visp-core && cargo clippy -p visp-core -- -D warnings
```

#### 📦 提交

`feat(visp-core): use requires_approval_for in agent loop with argument display`

---

## 步骤 2：Daemon 配置扩展

### 2a：DaemonConfig 新增通用 tool 配置容器

#### 🔴 红 — 测试

| # | 测试用例 | 描述 |
|---|---------|------|
| 1 | `test_tool_config_webfetch_allow_domains` | daemon.toml 中 `[tool.webfetch] allow_domains = [...]` 可正确解析 |
| 2 | `test_tool_config_empty` | 不配置 `[tool]` 段时，`tool` 字段为空 HashMap |
| 3 | `test_tool_config_other_tool` | 配置其他 tool 不影响 webfetch |

**涉及文件**：`crates/visp-daemon/src/config.rs`

#### 🟢 绿 — 实现

`DaemonConfig` 新增字段：

```rust
#[derive(Debug, Clone, Deserialize)]
pub struct DaemonConfig {
    pub daemon: DaemonSection,
    pub llm: LlmSection,
    pub tools: ToolsSection,
    pub agent: AgentSection,
    #[serde(default)]
    pub tool: HashMap<String, toml::Value>,
}
```

对应的 daemon.toml 用法：

```toml
[tool.webfetch]
allow_domains = ["docs.rs", "crates.io", "github.com"]
timeout_secs = 60
```

`default_config()` 中 `tool: HashMap::new()`。

#### 🧪 测试 → 🔍 类型检查

```bash
cargo test -p visp-daemon && cargo clippy -p visp-daemon -- -D warnings
```

#### 📦 提交

`feat(visp-daemon): add generic tool config container to DaemonConfig`

---

## 步骤 3：WebFetch 工具实现

### 3a：fetch.rs 核心模块

#### 🔴 红 — 测试

| # | 测试用例 | 描述 |
|---|---------|------|
| 1 | `test_validate_url_https` | `https://example.com` → Ok |
| 2 | `test_validate_url_http` | `http://example.com:8080/path` → Ok |
| 3 | `test_validate_url_file` | `file:///etc/passwd` → Err |
| 4 | `test_validate_url_ftp` | `ftp://files.example.com` → Err |
| 5 | `test_validate_url_empty` | 空字符串 → Err |
| 6 | `test_validate_url_invalid` | `not a url` → Err |
| 7 | `test_is_textual_mime_html` | `text/html` → true |
| 8 | `test_is_textual_mime_plain` | `text/plain` → true |
| 9 | `test_is_textual_mime_json` | `application/json` → true |
| 10 | `test_is_textual_mime_xml` | `application/xml` → true |
| 11 | `test_is_textual_mime_png` | `image/png` → false |
| 12 | `test_is_textual_mime_pdf` | `application/pdf` → false |
| 13 | `test_is_textual_mime_octet` | `application/octet-stream` → false |
| 14 | `test_host_in_allow_list_match` | `example.com` in `["example.com"]` → true |
| 15 | `test_host_in_allow_list_no_match` | `evil.com` in `["example.com"]` → false |
| 16 | `test_host_in_allow_list_empty` | `example.com` in `[]` → false |
| 17 | `test_host_in_allow_list_subdomain` | `sub.example.com` in `["example.com"]` → false（exact match） |
| 18 | `test_html_to_markdown_basic` | `<h1>Hello</h1>` → 包含 `# Hello` |
| 19 | `test_html_to_markdown_empty` | 空字符串 → 空字符串 |
| 20 | `test_html_to_markdown_no_html` | `hello world` → `hello world` |
| 21 | `test_from_toml_empty` | `None` → 空 allow_domains，默认 timeout |
| 22 | `test_from_toml_with_domains` | `{allow_domains: ["a.com"]}` → domains=["a.com"] |
| 23 | `test_from_toml_with_timeout` | `{timeout_secs: 60}` → timeout=60 |
| 24 | `test_requires_approval_for_whitelisted` | URL 命中白名单 → false |
| 25 | `test_requires_approval_for_not_whitelisted` | URL 未命中白名单 → true |
| 26 | `test_load_project_config_exists` | `.visp/webfetch.toml` 存在 → 返回 domains |
| 27 | `test_load_project_config_not_exists` | 文件不存在 → 空列表 |

#### 🟢 绿 — 实现

`crates/visp-tools/src/fetch.rs`，包含：

**WebFetch 结构体**：
- `client: reqwest::Client` — HTTP 客户端
- `daemon_allow_domains: Vec<String>` — daemon 级白名单
- `timeout_secs: u64` — 超时秒数

**关键方法**：
- `WebFetch::from_toml(raw: Option<&toml::Value>) -> Self` — 解析配置
- `validate_url(url: &str) -> Result<url::Url, ToolResult>` — URL 协议校验
- `is_textual_content_type(content_type: &str) -> bool` — MIME 类型过滤
- `host_in_allow_list(host: &str, allow_domains: &[String]) -> bool` — 白名单匹配
- `load_project_config(project_dir: &Path) -> Vec<String>` — 异步加载项目级白名单
- `fetch_url(url: &str, ctx: &ToolContext) -> ToolResult` — HTTP GET + 流式读取

**Tool trait 实现**：
- `name()` → `"fetch_web"`
- `description()` → 描述文本
- `parameters()` → JSON Schema `{ url: string }`
- `requires_approval_for(args)` → 检查 URL 是否在白名单内，在则返回 false
- `execute(args, ctx)` → 完整获取流程

**异步 IO**：
- 项目级白名单：`tokio::fs::read_to_string`
- HTTP 请求：`reqwest::Client` 原生 async
- 流式 body：`response.bytes_stream()` 逐 chunk 累计
- HTML→Markdown：`spawn_blocking` 中执行 CPU 密集型转换

**常量**：
- `MAX_RESPONSE_BYTES = 5 * 1024 * 1024`
- `DEFAULT_TIMEOUT_SECS = 60`
- 允许的协议：`http`, `https`

#### 🧪 测试 → 🔍 类型检查

```bash
cargo test -p visp-tools && cargo clippy -p visp-tools -- -D warnings
```

#### 📦 提交

`feat(visp-tools): implement WebFetch tool with URL validation, whitelist, and HTML-to-Markdown`

---

## 步骤 4：工具注册

### 4a：lib.rs 导出

`crates/visp-tools/src/lib.rs` 添加 `pub mod fetch;`

### 4b：daemon main.rs 注册

`crates/visp-daemon/src/main.rs`：
- 导入 `vbw_tools::fetch::WebFetch`
- 构造：`WebFetch::from_toml(config.tool.get("webfetch"))`
- 注册：`tool_registry.register(Box::new(web_fetch))`

#### 🔴 红 — 测试（编译验证）

| # | 测试用例 |
|---|---------|
| 1 | `cargo build --workspace` 通过 |

#### 🧪 测试 → 🔍 类型检查

```bash
cargo build --workspace && cargo clippy --workspace -- -D warnings
```

#### 📦 提交

`feat(visp-daemon): register WebFetch tool from daemon config`

---

## 步骤 5：质量门

```bash
cargo test --workspace && cargo clippy --workspace -- -D warnings && cargo fmt --check --all
```

---

## Wave 并行策略

```
Wave 1:  1a → 1b → 1c                     [串行，依赖 Tool trait 先就绪]
              │
Wave 2:  2a                                [可并行]
          3a                                [可并行]
              │
Wave 3:  4a → 4b                          [串行，依赖 2a+3a]
              │
Wave 4:  质量门                            [全 workspace]
```

- Wave 1 涉及 visp-core，影响全局，必须串行
- Wave 2 中 config 和 fetch.rs 互不依赖，可并行
- Wave 3 依赖 Wave 2 产出物

## 测试覆盖汇总

| Wave | 并行 | Crate | 步骤 | 测试用例 |
|------|------|-------|------|---------|
| 1 | 1 | visp-core | 1b, 1c | 2 |
| 2 | 2 | visp-daemon, visp-tools | 2a, 3a | 27+3 |
| 3 | 1 | visp-tools, visp-daemon | 4a, 4b | 0（编译验证） |
| 4 | — | 全 workspace | 5 | — |

总计：**5 个主步骤，7 个子步骤，32 个测试用例**

## 备注

- `reqwest` workspace dep 已有 `stream` feature，可用于流式读取 body
- `html-to-markdown-rs` 是同步库，转换时用 `spawn_blocking` 卸到线程池
- 网络相关测试通过工具内部纯函数隔离测试，不对外发起真实 HTTP 请求
- 项目级白名单文件 `.visp/webfetch.toml` 不存在时静默跳过
- config 中 `tool` 字段使用 `HashMap<String, toml::Value>`，不定义具体结构，由各 tool 自行解析
