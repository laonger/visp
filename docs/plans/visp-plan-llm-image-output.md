# visp 工作计划：大模型输出图片

## 概述

基于设计文档 `docs/design/visp-design-llm-image-output.md`，实现 LLM 响应中图片内容的端到端处理：Provider 解析 -> 事件传播 -> CLI 渲染 + 地址展示 + URL 懒加载。

## 依赖关系总览

```
Wave 1 (基础层)
  1a: visp-proto (proto 消息)     ──────────────┐
  1b: visp-llm/image_util.rs     ─────────┐    │
                                           │    │
Wave 2 (核心逻辑)                         │    │
  2a: visp-core (类型 + match) ◄──────────┤    │
  2b: visp-daemon (转换) ◄── 2a           │    │
  2c: visp-cli (LineType + markers) ◄─────┼────┤
                                           │    │
Wave 3 (实现层)                           │    │
  3a: visp-llm/openai.rs ◄── 2a, 1b ◄────┘    │
  3b: visp-llm/anthropic.rs ◄── 2a, 1b        │
  3c: visp-cli/render_pending ◄── 2c, 1a ◄────┘
```

## Wave 1：基础层（2 个并行任务）

### 1a：visp-proto 新增 ImageBlock + ImageError 消息

#### 🟢 绿 - 实现

在 `crates/visp-proto/proto/visp.proto` 中：

- 新增 `ImageBlock` message：path, mime_type, remote_url, session_id, agent_name
- 新增 `ImageError` message：reason, session_id, agent_name
- 在 `ServerMessage.oneof payload` 中新增 `ImageBlock image_block = 11` 和 `ImageError image_error = 12`

#### 🧪 测试 -> 🔍 类型检查

```bash
cargo build -p visp-proto
```

#### 📦 提交

`feat(proto): add ImageBlock and ImageError messages to ServerMessage`

---

### 1b：visp-llm 新增 image_util.rs

#### 🔴 红 - 测试

在 `crates/visp-llm/src/image_util.rs` 中编写单元测试（同文件 `#[cfg(test)] mod tests`）：

| # | 测试用例 | 简明描述 |
|---|---------|---------|
| 1 | parse_data_uri 正确解析 | `data:image/png;base64,iVBOR=` -> `("image/png", "iVBOR=")` |
| 2 | parse_data_uri 无效输入 | `"not-a-data-uri"` -> `None` |
| 3 | parse_data_uri 无 MIME | `data:;base64,aGVsbG8=` -> MIME 默认或 None |
| 4 | mime_to_extension png | `"image/png"` -> `"png"` |
| 5 | mime_to_extension jpeg | `"image/jpeg"` -> `"jpg"` |
| 6 | mime_to_extension webp | `"image/webp"` -> `"webp"` |
| 7 | mime_to_extension 未知 | `"image/avif"` -> `"png"` (fallback) |
| 8 | save_base64_image 成功 | 传入小 PNG base64 + tempdir，验证文件存在且可读 |
| 9 | save_base64_image 超限 | 传入估算 > 20MB 的 data，返回 Err |
| 10 | save_base64_image 解码失败 | 传入无效 base64，返回 Err |

#### 🟢 绿 - 实现

新建 `crates/visp-llm/src/image_util.rs`：

- `parse_data_uri(uri: &str) -> Option<(String, String)>` - 解析 data URI，返回 (mime_type, base64_data)
- `mime_to_extension(mime_type: &str) -> &str` - MIME -> 扩展名映射，未知回退 `png`
- `save_base64_image(data: &str, mime_type: &str, project_path: &str) -> Result<String, LlmError>` - 解码前估算大小（`data.len() * 3 / 4`），超 20MB 返回 Err；解码写入 `{project_path}/.visp/images/{timestamp}_{index}.{ext}`；创建目录如不存在

在 `crates/visp-llm/src/lib.rs` 中 `pub mod image_util;`（或 `mod image_util;`）

#### 🧪 测试 -> 🔍 类型检查

```bash
cargo test -p visp-llm image_util
cargo clippy -p visp-llm
```

#### 📦 提交

`feat(llm): add image_util module for base64 image saving and data URI parsing`

## Wave 2：核心逻辑（3 个任务，2c 与 2a/2b 并行）

### 2a：visp-core 新增 ChatEvent + AgentEvent 变体并更新 match

#### 🔴 红 - 测试

在 `crates/visp-core/src/agent_loop_tests.rs`（或对应测试文件）中新增测试：

| # | 测试用例 | 简明描述 |
|---|---------|---------|
| 1 | collect_stream_events 处理 ImageBlock (base64) | 发出 TextDelta flush + AgentEvent::ImageBlock + text_buffer 含 `<image: path>` 标记 |
| 2 | collect_stream_events 处理 ImageBlock (URL) | path 为空 + remote_url 有值，text_buffer 含 `<image: \| url>` 标记 |
| 3 | collect_stream_events 处理 ImageError | 发出 AgentEvent::ImageError，text_buffer 不追加标记 |
| 4 | event_to_msg 对 ImageBlock 返回 None | 验证 `event_to_msg(&AgentEvent::ImageBlock{..})` == `None` |
| 5 | event_to_msg 对 ImageError 返回 None | 验证 `event_to_msg(&AgentEvent::ImageError{..})` == `None` |

#### 🟢 绿 - 实现

1. `crates/visp-core/src/provider.rs`：ChatEvent 新增 `ImageBlock { path: String, mime_type: String, remote_url: Option<String> }` 和 `ImageError { reason: String }`
2. `crates/visp-core/src/agent.rs`：AgentEvent 新增 `ImageBlock` 和 `ImageError`，字段同 ChatEvent
3. `crates/visp-core/src/agent_loop.rs`：
   - `event_to_msg`：新增 `ImageBlock { .. } \| ImageError { .. } => None`
   - `collect_stream_events`：新增 `ChatEvent::ImageBlock` 分支（flush text_buffer -> send AgentEvent::ImageBlock -> 追加标记到 text_buffer）和 `ChatEvent::ImageError` 分支（send AgentEvent::ImageError，不追加到 text_buffer）

#### 🧪 测试 -> 🔍 类型检查

```bash
cargo test -p visp-core agent_loop
cargo clippy -p visp-core
```

#### 📦 提交

`feat(core): add ImageBlock and ImageError to ChatEvent and AgentEvent with agent_loop handling`

---

### 2b：visp-daemon 新增 agent_event_to_server_message 分支

#### 🔴 红 - 测试

在 `crates/visp-daemon/src/service_tests.rs`（或对应测试文件）中新增测试：

| # | 测试用例 | 简明描述 |
|---|---------|---------|
| 1 | ImageBlock 转换 (base64) | AgentEvent::ImageBlock { path, remote_url: None } -> proto::ImageBlock { path, remote_url: "" } |
| 2 | ImageBlock 转换 (URL) | AgentEvent::ImageBlock { path: "", remote_url: Some } -> proto::ImageBlock { path: "", remote_url: url } |
| 3 | ImageError 转换 | AgentEvent::ImageError { reason } -> proto::ImageError { reason } |

#### 🟢 绿 - 实现

`crates/visp-daemon/src/service.rs` 的 `agent_event_to_server_message` 函数新增两个 match 分支：
- `AgentEvent::ImageBlock { path, mime_type, remote_url }` -> `proto::ServerMessage::ImageBlock`
- `AgentEvent::ImageError { reason }` -> `proto::ServerMessage::ImageError`

#### 🧪 测试 -> 🔍 类型检查

```bash
cargo test -p visp-daemon service
cargo clippy -p visp-daemon
```

#### 📦 提交

`feat(daemon): add ImageBlock and ImageError conversion in agent_event_to_server_message`

---

### 2c：visp-cli LineType 扩展 + split_image_markers + download_and_decode 扩展

> 与 2a/2b 并行，仅依赖 1a（proto 类型）

#### 🔴 红 - 测试

在 `crates/visp-cli/src/image_tests.rs`（或对应测试文件）中新增测试：

| # | 测试用例 | 简明描述 |
|---|---------|---------|
| 1 | split_image_markers 带 URL 标记 | `<image: \| https://example.com/img.png>` -> LineType::Image { path: "", remote_url: Some } |
| 2 | split_image_markers 向后兼容 | `<image: /local/path.png>` -> LineType::Image { path: "/local/path.png", remote_url: None } |
| 3 | split_image_markers 混合内容 | 文本 + base64 标记 + 文本 + URL 标记 -> 正确拆分为多个 ChatLine |
| 4 | split_image_markers 空路径 URL | `<image: \| https://...>` 中 path 为空字符串，remote_url 有值 |

#### 🟢 绿 - 实现

1. `crates/visp-cli/src/app.rs`：`LineType::Image` 新增 `remote_url: Option<String>` 字段
2. `crates/visp-cli/src/image.rs`：
   - `split_image_markers`：解析 ` | ` 分隔符，前为 path（可为空），后为 remote_url
   - `make_image_line`：支持 remote_url 参数
   - 所有构造 `LineType::Image` 的位置添加 `remote_url: None`（保持向后兼容）

#### 🧪 测试 -> 🔍 类型检查

```bash
cargo test -p visp-cli image
cargo clippy -p visp-cli
```

#### 📦 提交

`feat(cli): extend LineType::Image with remote_url and update split_image_markers for URL markers`

## Wave 3：实现层（3 个并行任务）

### 3a：visp-llm OpenAI Provider SSE 图片解析

#### 🔴 红 - 测试

在 `crates/visp-llm/src/openai_tests.rs`（或对应测试文件）中新增测试：

| # | 测试用例 | 简明描述 |
|---|---------|---------|
| 1 | parse_openai_sse_data base64 图片 | delta.content 为数组含 image_url (data URI) -> OpenAiStreamEvent::ImageBlock { data: Some } |
| 2 | parse_openai_sse_data URL 图片 | delta.content 为数组含 image_url (https URL) -> OpenAiStreamEvent::ImageBlock { data: None, remote_url: Some } |
| 3 | parse_openai_sse_data 混合内容 | delta.content 为数组含 text + image_url -> 分别返回 TextDelta 和 ImageBlock |
| 4 | parse_openai_sse_data 字符串 content | delta.content 为字符串 -> 仍返回 TextDelta（向后兼容） |
| 5 | byte_stream base64 图片端到端 | SSE 流含 image_url (base64) -> ChatEvent::ImageBlock { path 非空, remote_url: None } |
| 6 | byte_stream URL 图片端到端 | SSE 流含 image_url (URL) -> ChatEvent::ImageBlock { path: "", remote_url: Some } |
| 7 | byte_stream base64 解码失败 | 无效 base64 data -> ChatEvent::ImageError { reason } |

#### 🟢 绿 - 实现

1. `crates/visp-llm/src/openai.rs`：
   - `OpenAiStreamEvent` 新增 `ImageBlock { data: Option<String>, mime_type: String, remote_url: Option<String> }`
   - `parse_openai_sse_data`：当 `delta.content` 为数组时，遍历元素识别 `image_url` 类型，解析 data URI 或 URL
   - `byte_stream_to_chat_events`：处理 `ImageBlock` 事件 - base64 调用 `image_util::save_base64_image` 落盘，URL 直接传递；失败时发出 `ChatEvent::ImageError`
   - project_path 从 `config.extra.get("project_path")` 获取

#### 🧪 测试 -> 🔍 类型检查

```bash
cargo test -p visp-llm openai
cargo clippy -p visp-llm
```

#### 📦 提交

`feat(llm): add image content block parsing to OpenAI provider with base64 saving and URL passthrough`

---

### 3b：visp-llm Anthropic Provider SSE 图片解析

#### 🔴 红 - 测试

在 `crates/visp-llm/src/anthropic_tests.rs`（或对应测试文件）中新增测试：

| # | 测试用例 | 简明描述 |
|---|---------|---------|
| 1 | parse_anthropic_event image base64 | content_block_start type=image, source.type=base64 -> ParsedEvent::ImageBlock { data: Some } |
| 2 | parse_anthropic_event image url | content_block_start type=image, source.type=url -> ParsedEvent::ImageBlock { data: None, remote_url: Some } |
| 3 | stream base64 图片端到端 | SSE 流含 image content block (base64) -> ChatEvent::ImageBlock { path 非空 } |
| 4 | stream URL 图片端到端 | SSE 流含 image content block (URL) -> ChatEvent::ImageBlock { path: "", remote_url: Some } |

#### 🟢 绿 - 实现

1. `crates/visp-llm/src/anthropic.rs`：
   - `ParsedEvent` 新增 `ImageBlock { data: Option<String>, mime_type: String, remote_url: Option<String> }`
   - `parse_anthropic_event` 的 `content_block_start` 分支新增 `"image"` 类型处理
   - 流式累积循环中处理 `ImageBlock` - base64 落盘，URL 直接传递

#### 🧪 测试 -> 🔍 类型检查

```bash
cargo test -p visp-llm anthropic
cargo clippy -p visp-llm
```

#### 📦 提交

`feat(llm): add image content block parsing to Anthropic provider`

---

### 3c：visp-cli render_pending + 地址渲染 + download_and_decode 扩展

#### 🔴 红 - 测试

在 `crates/visp-cli/src/app_tests.rs` 和 `image_tests.rs` 中新增测试：

| # | 测试用例 | 简明描述 |
|---|---------|---------|
| 1 | render_pending ImageBlock (base64) | ServerMessage::ImageBlock { path, remote_url: "" } -> flush + LineType::Image { path, remote_url: None } |
| 2 | render_pending ImageBlock (URL) | ServerMessage::ImageBlock { path: "", remote_url: url } -> flush + LineType::Image { path: "", remote_url: Some } |
| 3 | render_pending ImageError | ServerMessage::ImageError { reason } -> flush + LineType::Error "[图片加载失败: reason]" |
| 4 | render_pending 文本+图片交替 | TextDelta + ImageBlock + TextDelta -> 正确交替渲染，无重复 |

#### 🟢 绿 - 实现

1. `crates/visp-cli/src/app.rs`：
   - `render_pending` 新增 `ServerMessage::ImageBlock` 分支：flush_streaming -> push_chat_line(LineType::Image)
   - `render_pending` 新增 `ServerMessage::ImageError` 分支：flush_streaming -> push_chat_line(LineType::Error)
2. `crates/visp-cli/src/image.rs`：
   - `download_and_decode` 扩展：解码成功后将原始 bytes 写入 `{project_path}/.visp/images/{url_hash}.{ext}` 缓存文件
   - `ImageEntry::Ready` 新增 `local_path: Option<String>` 字段
   - 图片地址渲染：`LineType::Image` 渲染改为复合渲染（image widget + Paragraph 垂直排列），显示 🔗 remote_url + 📁 path
   - URL 图片（path 为空）：将 remote_url 作为 ImageCache::get_or_load 的 key，触发现有异步下载

#### 🧪 测试 -> 🔍 类型检查

```bash
cargo test -p visp-cli
cargo clippy -p visp-cli
```

#### 📦 提交

`feat(cli): add ImageBlock/ImageError rendering with address display and URL lazy download`

## Wave 并行策略

### Wave 1：基础层（2 个并行任务）

```
任务 A: 1a (visp-proto)
任务 B: 1b (visp-llm/image_util.rs)
```

### Wave 2：核心逻辑（2c 与 2a->2b 并行）

```
任务 A: 2a -> 2b (visp-core -> visp-daemon，串行)
任务 B: 2c (visp-cli，与 A 并行)
```

### Wave 3：实现层（3 个并行任务）

```
任务 A: 3a (visp-llm/openai.rs)
任务 B: 3b (visp-llm/anthropic.rs)
任务 C: 3c (visp-cli/render_pending + image.rs)
```

## 测试覆盖汇总

| Wave | 并行数 | 模块/包 | 步骤 | 测试用例数 |
|------|--------|---------|------|-----------|
| 1 | 2 | visp-proto | 1a | 0 (编译验证) |
| 1 | 2 | visp-llm | 1b | 10 |
| 2 | 2 | visp-core | 2a | 5 |
| 2 | 2 | visp-daemon | 2b | 3 |
| 2 | 2 | visp-cli | 2c | 4 |
| 3 | 3 | visp-llm | 3a | 7 |
| 3 | 3 | visp-llm | 3b | 4 |
| 3 | 3 | visp-cli | 3c | 4 |
| **合计** | | | | **37** |

## 备注

- 每步完成后运行 `cargo build` 确保全 workspace 编译通过（特别是跨 crate 依赖）
- Wave 2a 新增 AgentEvent 变体会导致 visp-daemon 编译失败，2b 必须紧跟 2a
- Wave 2c 中 `LineType::Image` 新增字段会导致所有构造处编译失败，需同 commit 内全部更新
- Wave 3c 的地址渲染改动涉及 ratatui 布局逻辑，需手动验证 TUI 渲染效果
- proto 代码生成：修改 `.proto` 后运行 `cargo build -p visp-proto` 触发 tonic-build 重新生成
