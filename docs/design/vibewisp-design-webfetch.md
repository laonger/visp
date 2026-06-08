# WebFetch 工具设计

## 概述

新增 `fetch_web` 工具，允许 Agent 通过 HTTP/HTTPS 获取网页内容并自动转换为 Markdown。

---

## 模块职责

| 模块 | 职责 |
|------|------|
| `vbw-tools/src/fetch.rs` | WebFetch 工具实现（URL 校验、HTTP 请求、HTML→Markdown、白名单检查、配置解析） |
| `vbw-daemon/src/config.rs` | `DaemonConfig` 新增通用 `tool: HashMap<String, toml::Value>` 字段，仅做 raw 传递 |
| `vbw-daemon/src/main.rs` | 从 `config.tool` 取出 raw toml 传给 `WebFetch::from_toml()`，注册到 ToolRegistry |

---

## 数据流

```
LLM 调用 fetch_web(url)
  → ToolRegistry.execute("fetch_web", { url })
    → WebFetch::execute()       [async]
      1. URL 协议校验（仅 http/https） ← sync
      2. 白名单检查
         ├─ 项目级白名单: tokio::fs::read 异步加载 .vibewisp/webfetch.toml
         ├─ 合并 daemon 白名单
         ├─ 域名命中 → 放行
         └─ 未命中 → requires_approval() = true → 弹 UserQuery
      3. HTTP GET 请求 ← reqwest (原生 async)
         ├─ Content-Length > 5MB → 拒绝
         ├─ 超时 30s → 拒绝
         └─ 非文本 MIME → 拒绝
      4. 流式读取 body ← reqwest 流 (async)，累计 > 5MB 截断
      5. HTML→Markdown ← spawn_blocking 卸到线程池（html-to-markdown-rs 是同步 CPU 操作）
      6. 返回 ToolResult::success(markdown)
```

## 白名单机制

### 配置传递

daemon 不感知 webfetch 内部配置结构，通过通用 raw 容器传递：

```toml
# daemon.toml — 工具配置统一放在 [tool.<name>] 下
[tool.webfetch]
allow_domains = ["docs.rs", "crates.io", "github.com"]
```

```toml
# .vibewisp/webfetch.toml — 项目级（可选）
allow_domains = ["example.com"]
```

`DaemonConfig` 新增 `tool: HashMap<String, toml::Value>` 字段，serde 自动捕获 `[tool.webfetch]` 段，不定义具体结构。

WebFetch 通过 `WebFetch::from_toml(value: Option<&toml::Value>)` 自行解析内部配置字段。

### 数据源 & 匹配规则

- **Daemon 级**：从 `config.tool` 取出 `webfetch` 段 → tool 自己解析 `allow_domains`
- **项目级**：`execute` 时从 `.vibewisp/webfetch.toml` 加载（文件不存在则跳过）
- **合并**：两层白名单取并集
- **匹配**：URL host exact match 白名单 → 自动放行；否则 `requires_approval() = true`
- **边界**：白名单为空 / 不配置 → 所有 URL 都需确认

---

## 安全措施

| 措施 | 说明 |
|------|------|
| 协议白名单 | 仅允许 `http://`、`https://`，拒绝 `file://`、`ftp://`、`data:` 等 |
| 内容类型过滤 | 仅接受 `text/*`、`application/json`、`application/xml` 等文本 MIME |
| 大小限制 | Content-Length > 5MB 直接拒绝；流式读取超 5MB 截断 |
| 超时控制 | 默认 30s，可通过 daemon.toml 配置（`timeout_secs`，最大 120s） |
| 权限确认 | 非白名单 URL 需用户确认，拒绝执行 |
| 错误封装 | 内部错误统一映射，不暴露网络细节 |

---

## 输出格式

始终返回 Markdown 格式（使用 `html-to-markdown-rs` 转换）。

返回值结构：
- 成功：Markdown 文本
- 失败：错误描述（"Unable to fetch {url}" 或具体错误）

---

## 工具定义（给 LLM）

| 字段 | 内容 |
|------|------|
| name | `fetch_web` |
| description | 获取网页内容并转为 Markdown |
| parameters | `{ url: string }` |

---

## 影响范围

| 文件 | 改动类型 |
|------|----------|
| `crates/vbw-tools/Cargo.toml` | 添加 `reqwest` + `html-to-markdown-rs` |
| `crates/vbw-tools/src/lib.rs` | 添加 `pub mod fetch;` |
| `crates/vbw-tools/src/fetch.rs` | 新文件，WebFetch 工具 |
| `crates/vbw-daemon/src/config.rs` | `DaemonConfig` 加 `tool: HashMap<String, toml::Value>`（通用 raw 容器，不定义 webfetch 字段） |
| `crates/vbw-daemon/src/main.rs` | 从 `config.tool` 取出 raw toml 传给 `WebFetch::from_toml()`，注册到 ToolRegistry |

---

## 不涉及

- `ToolContext` 不变 — WebFetch 直接持有白名单配置
- `vbw-core` 不变 — 无需修改核心 trait
- proto 不变 — 不需要新的 RPC
