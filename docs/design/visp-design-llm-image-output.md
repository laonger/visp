# visp 设计：大模型输出图片

## 1. 目标

让 visp 支持 LLM 响应中包含图片内容的端到端处理：从 API 响应解析、事件传播、到 CLI 渲染。

### 本阶段范围（做）

1. **解析 LLM 图片输出**：OpenAI / Anthropic provider 解析 API 响应中的图片内容块（base64 或 URL）
2. **事件传播**：新增 `ImageBlock` / `ImageError` 事件类型，贯穿 ChatEvent -> AgentEvent -> ServerMessage -> CLI 渲染链路
3. **CLI 展示**：复用现有 `ratatui-image` 渲染管线，将 LLM 生成的图片在对话区内联展示
4. **URL 图片懒加载**：URL 来源图片由 CLI 渲染时按需下载，不阻塞 Provider 流处理
5. **地址信息展示**：图片下方显示远程地址（如有）和本地缓存路径，方便用户取用
6. **会话持久化**：图片引用以扩展的 `<image: path | url>` 标记存储在 `Message.content` 中，支持 session 回放

### 不做（后续阶段）

- LLM 主动调用图片生成工具（如 DALL-E API、Stable Diffusion）-- 这属于工具层，不在本次范围
- 图片编辑 / 标注
- 远程图片 URL 的本地持久化缓存策略（当前每次回放重新下载，内存级缓存）
- OpenAI Responses API 的 `image_generation` tool 流式部分图片（partial_images）
- 将图片作为输入发送给多模态模型（已有独立设计文档 `visp-design-multimodal-cli-image.md` 的后续阶段）

## 2. 背景

### 2.1 现有图片处理架构

visp 已有一套基于 `<image: path>` 文本标记的图片处理机制（见 `visp-design-multimodal-cli-image.md`）：

- **用户输入**：用户输入 `@path/to/image.png`，CLI 替换为 `<image: path>` 标记，随 `UserInput.text` 发送
- **工具结果**：`read_file` 等工具遇到图片文件时，在结果中插入 `<image: path>` 标记
- **CLI 渲染**：`split_image_markers()` 从文本中提取标记，生成 `LineType::Image { path, alt_text }` 行，用 `ratatui-image` 内联渲染

该机制的核心理念：**图片数据不通过 gRPC 传输，在 CLI 端本地解码渲染**（CLI 和 daemon 运行在同一台机器）。

### 2.2 当前数据流（纯文本 + 工具）

```
LLM API (SSE)
  -> Provider: parse -> ChatEvent (TextDelta/ToolCall/ThinkingBlock/...)
  -> agent_loop: collect_stream_events -> AgentEvent
  -> daemon: agent_event_to_server_message -> ServerMessage (proto)
  -> gRPC stream -> CLI
  -> render_pending -> ChatLine -> ratatui 渲染
```

### 2.3 缺失环节

当前 `ChatEvent` / `AgentEvent` / `ServerMessage` 均无图片事件类型。Provider 的 SSE 解析只处理 `text` / `tool_use` / `thinking` 内容块，不识别 `image` 类型。

## 3. 技术选型

### 3.1 图片存储位置

- **base64 来源**（Provider 落盘）：保存到 `{project_path}/.visp/images/{timestamp}_{index}.{ext}`，持久化存储，session 回放时可直接访问
- **URL 来源**（CLI 懒加载下载）：下载到 `{project_path}/.visp/images/{url_hash}.{ext}`，以 URL 哈希为文件名实现去重

**理由**：
- 持久化存储，session 回放时图片仍可访问（临时目录可能被系统清理）
- 与项目关联，用户可自行查看/管理生成的图片
- URL 哈希命名避免重复下载同一图片

### 3.2 图片数据来源与处理分工

LLM API 响应中的图片数据有两种形式，处理分工不同：

| 来源 | 格式 | Provider 处理 | CLI 处理 |
|------|------|--------------|----------|
| base64 data URI | `data:image/png;base64,...` | 解码写入文件，发出 path | 直接渲染本地文件 |
| base64 原始数据 | 纯 base64 + media_type | 解码写入文件，发出 path | 直接渲染本地文件 |
| URL | `https://...` | 不下载，发出 remote_url | 渲染时懒加载下载到缓存 |

### 3.3 图片地址展示

每张 LLM 输出的图片下方显示地址信息，方便用户取用：

- **有远程 URL 时**（图片来源为 URL）：显示远程地址 + 本地缓存路径
- **无远程 URL 时**（图片来源为 base64）：仅显示本地缓存路径

展示格式（终端暗色/灰色文本，与正文区分，便于辨认和复制）：

```
  🔗 https://example.com/generated/image.png
  📁 /home/user/project/.visp/images/20260803_120000_0.png
```

### 3.4 事件类型设计

新增 `ImageBlock` 事件，携带**文件路径**和**远程 URL**（非 base64 数据），贯穿整个管线。

- base64 来源：`path` = 本地文件路径，`remote_url` = None
- URL 来源：`path` = 空字符串，`remote_url` = URL（CLI 渲染时下载）

新增 `ImageError` 事件，携带失败原因，用于 base64 解码失败 / 文件写入失败场景。

**理由**：
- 与现有 `<image: path>` 设计理念一致：不在事件链路中传输二进制数据
- Provider 负责将 base64 转为本地文件；URL 不下载，避免阻塞流处理
- `remote_url` 独立携带，CLI 可据此渲染地址信息并懒加载下载

## 4. 架构概览

### 4.1 目标数据流

**base64 来源**：

```
LLM API (SSE) 响应包含 image 内容块 (base64)
  -> Provider: 解析 image block -> base64 解码 -> 保存到 .visp/images/
  -> ChatEvent::ImageBlock { path: "/path/to/file", mime_type, remote_url: None }
  -> agent_loop -> AgentEvent::ImageBlock -> ServerMessage::ImageBlock -> CLI
  -> CLI: 直接渲染本地文件 + 显示 📁 path
```

**URL 来源**：

```
LLM API (SSE) 响应包含 image 内容块 (URL)
  -> Provider: 解析 image block，不下载
  -> ChatEvent::ImageBlock { path: "", mime_type: "", remote_url: Some("https://...") }
  -> agent_loop -> AgentEvent::ImageBlock -> ServerMessage::ImageBlock -> CLI
  -> CLI: 懒加载下载 URL 到 .visp/images/ -> 渲染缓存文件 + 显示 🔗 url + 📁 cached_path
```

### 4.2 关键设计决策

**决策 1：base64 由 Provider 落盘，URL 由 CLI 懒加载**

Provider 收到 base64 图片数据后，解码写入本地文件，发出携带路径的事件。URL 图片不下载，直接发出 remote_url，由 CLI 渲染时按需下载。

理由：
- base64 数据已在 SSE 事件中，Provider 解码落盘是自然处理点
- URL 下载可能耗时数秒，在 Provider stream loop 内会阻塞后续 SSE 事件
- CLI 懒加载不阻塞 LLM 响应流，文本输出不受影响

**决策 2：新增独立事件类型，不复用 TextDelta + marker**

不将图片以 `TextDelta("<image: path>")` 形式发送，而是使用独立的 `ImageBlock` 事件。

理由：
- 避免流式渲染时短暂显示 `<image: path>` 原始标记文本
- 语义清晰，便于扩展（未来可携带尺寸、alt_text 等元数据）
- CLI 可在收到 ImageBlock 时立即 flush 当前文本并创建图片行，渲染体验更好

**决策 3：扩展标记格式持久化图片 + 远程 URL**

`collect_stream_events` 在处理 `ImageBlock` 事件时，向 `text_buffer` 追加扩展标记：

- base64 来源（有 path，无 remote_url）：`<image: /path/to/file>`
- URL 来源（无 path，有 remote_url）：`<image: | https://remote.url/image.png>`
- 兼容现有格式：无 ` | ` 分隔符时，整个内容为 path

`split_image_markers` 解析时识别可选的 ` | url` 部分，提取远程 URL 传入 `LineType::Image`。

**决策 4：图片保存路径使用项目目录而非临时目录**

确保 session 回放时图片仍可访问。

**决策 5：双缓冲区避免图片重复渲染**

`collect_stream_events` 中维护：
- `text_buffer`：用于构建最终 `Message.content`，包含 `<image: path | url>` 标记
- 发送给 CLI 的 TextDelta 事件直接从 ChatEvent::TextDelta 转发，不包含标记

`Message.content` 包含标记（用于回放），但 CLI 实时收到的是独立的 `ImageBlock` 事件 + 纯文本 TextDelta，不会重复渲染。

### 4.3 标记格式扩展

现有标记格式：`<image: path>`

扩展格式：`<image: path | remote_url>`

- ` | remote_url` 部分可选
- `split_image_markers` 向后兼容：无 ` | ` 分隔符时，整个内容为 path（与现有行为一致）
- 有 ` | ` 分隔符时，` | ` 前为本地 path（可为空），后为 remote_url
- `LineType::Image` 新增 `remote_url: Option<String>` 字段
- path 为空 + remote_url 有值：CLI 懒加载下载 URL 到本地缓存

## 5. 模块职责与改动

### 5.1 visp-core：类型定义

#### ChatEvent（`provider.rs:258`）

新增两个变体：

`ImageBlock`：
- `path: String` - 图片本地文件路径（base64 来源有值，URL 来源为空字符串）
- `mime_type: String` - 图片 MIME 类型（base64 来源有值，URL 来源为空字符串）
- `remote_url: Option<String>` - 远程 URL（URL 来源有值，base64 来源为 None）

`ImageError`：
- `reason: String` - 图片处理失败原因（如 `"base64 decode error"`, `"file write error"`）

#### AgentEvent（`agent.rs:39`）

新增 `ImageBlock` 和 `ImageError` 变体，字段与 ChatEvent 一致。

`event_to_msg`（`agent_loop.rs:39`）对 `ImageBlock` 和 `ImageError` 返回 `None`（与 `UserQuery` 相同），不转发给 Orchestrator。图片事件仅通过 `tx` -> forwarding task -> `grpc_tx` 直达 CLI。

#### agent_loop（`agent_loop.rs:393`）

`collect_stream_events` 函数新增对 `ChatEvent::ImageBlock` 的处理：

1. 先 flush 当前 `text_buffer` 中的文本为 `AgentEvent::TextDelta`（确保图片前的文本先显示）
2. 发送 `AgentEvent::ImageBlock { path, mime_type, remote_url }`
3. 向 `text_buffer` 追加标记：
   - base64 来源（path 非空，remote_url = None）：`<image: {path}>`
   - URL 来源（path 为空，remote_url = Some）：`<image: | {remote_url}>`
4. 将 path 记录到 `StreamOutput`（用于构建最终 assistant Message）

新增对 `ChatEvent::ImageError` 的处理：

1. 发送 `AgentEvent::ImageError { reason }`
2. 不向 `text_buffer` 追加任何内容（错误图片不持久化到 Message.content）

### 5.2 visp-llm：Provider 解析

#### OpenAI Provider（`openai.rs`）

**问题**：当前 `parse_openai_sse_data` 中 `delta.content` 只处理字符串类型（`as_str()`）。部分 OpenAI 兼容模型（如通义千问、豆包等）在图片输出时，`delta.content` 可能是数组，包含 `{"type": "image_url", "image_url": {"url": "data:..."}}` 内容块。

**改动**：

1. `OpenAiStreamEvent` 新增 `ImageBlock { data: Option<String>, mime_type: String, remote_url: Option<String> }` 变体
   - base64 来源：`data` = base64 字符串，`remote_url` = None
   - URL 来源：`data` = None，`remote_url` = Some(url)
2. `parse_openai_sse_data` 中，当 `delta.content` 为数组时，遍历元素，识别 `image_url` 类型：
   - 解析 `image_url.url`：
     - 若为 data URI（`data:image/png;base64,...`）：提取 MIME 类型和 base64 数据，`remote_url` = None
     - 若为普通 URL（`https://...`）：`remote_url` = 该 URL，data = None
3. `byte_stream_to_chat_events` 中，收到 `ImageBlock` 事件时：
   - base64 来源（data = Some）：调用 `save_base64_image` 解码写入文件，发出 `ChatEvent::ImageBlock { path, mime_type, remote_url: None }`
   - URL 来源（data = None）：不下载，直接发出 `ChatEvent::ImageBlock { path: "", mime_type: "", remote_url: Some(url) }`
   - 失败时发出 `ChatEvent::ImageError { reason }`

#### Anthropic Provider（`anthropic.rs`）

**问题**：当前 `parse_anthropic_event` 的 `content_block_start` 只处理 `tool_use` / `thinking` / `redacted_thinking` / `text` 类型。Anthropic API 的 image 内容块（当前 Claude 不生成，但为兼容未来模型和第三方 Anthropic 兼容 API 预留）。

**改动**：

1. `ParsedEvent` 新增 `ImageBlock { data: Option<String>, mime_type: String, remote_url: Option<String> }` 变体
2. `parse_anthropic_event` 的 `content_block_start` 分支中，新增 `"image"` 类型处理：
   - `source.type = "base64"`：提取 `source.media_type` 和 `source.data`，`remote_url` = None
   - `source.type = "url"`：提取 `source.url`，`remote_url` = Some(url)，data = None
3. 流式累积循环中，收到 `ImageBlock` 时：
   - base64 来源：保存文件并发出 `ChatEvent::ImageBlock { path, mime_type, remote_url: None }`
   - URL 来源：不下载，发出 `ChatEvent::ImageBlock { path: "", mime_type: "", remote_url: Some(url) }`

#### Provider 共享图片工具模块

新增 `visp-llm/src/image_util.rs`：

- `save_base64_image(data: &str, mime_type: &str, project_path: &str) -> Result<String, LlmError>` - 返回文件路径。解码前检查 `data.len() * 3 / 4` 估算大小，超 20MB 返回错误
- `parse_data_uri(uri: &str) -> Option<(String, String)>` - 解析 data URI，返回 (mime_type, base64_data)
- `mime_to_extension(mime_type: &str) -> &str` - MIME 类型到文件扩展名映射（`image/png` -> `png`, `image/jpeg` -> `jpg`, `image/webp` -> `webp`, `image/gif` -> `gif`，未知类型回退 `png`）

**图片大小限制**：20MB，在 `save_base64_image` 内部检查。超限时返回 `LlmError`，由 provider 转为 `ChatEvent::ImageError { reason: "image too large (max 20MB)" }`。

**MIME 类型确定**（base64 来源）：从 data URI 的 media type 或 Anthropic `source.media_type` 字段直接获取。

### 5.3 visp-proto：gRPC 消息定义

#### 新增 ImageBlock / ImageError 消息（`visp.proto`）

在 `ServerMessage` 的 `oneof payload` 中新增字段：

```protobuf
message ImageBlock {
    string path = 1;              // 图片本地文件路径（URL 来源为空）
    string mime_type = 2;         // 图片 MIME 类型（URL 来源为空）
    string remote_url = 3;        // 远程 URL（空字符串表示无远程来源）
    string session_id = 4;
    string agent_name = 5;
}

message ImageError {
    string reason = 1;            // 失败原因
    string session_id = 2;
    string agent_name = 3;
}

message ServerMessage {
    oneof payload {
        // ... 现有字段 1-10 ...
        ImageBlock image_block = 11;  // LLM 输出的图片
        ImageError image_error = 12;  // 图片处理失败
    }
}
```

### 5.4 visp-daemon：事件转换

#### agent_event_to_server_message（`service.rs:1375`）

新增 `AgentEvent::ImageBlock { path, mime_type, remote_url }` 和 `AgentEvent::ImageError { reason }` 分支，转换为对应的 `proto::ServerMessage` 变体。`remote_url` 为 None 时序列化为空字符串。

### 5.5 visp-cli：渲染与懒加载

#### LineType 扩展（`app.rs:231`）

`LineType::Image` 新增 `remote_url: Option<String>` 字段：

- 现有：`Image { path: String, alt_text: String }`
- 扩展后：`Image { path: String, alt_text: String, remote_url: Option<String> }`
- path 为空字符串 + remote_url 有值：表示 URL 图片，需懒加载下载

#### render_pending（`app.rs:444`）

新增 `ServerMessage::ImageBlock` 处理分支：

1. 调用 `flush_streaming()`（将当前累积的文本先渲染）
2. 调用 `push_chat_line(LineType::Image { path, alt_text, remote_url }, ...)` 创建图片行
3. alt_text：path 非空时从文件名提取；path 为空时从 remote_url 提取
4. remote_url 从 proto 消息中提取（空字符串转为 None）

新增 `ServerMessage::ImageError` 处理分支：

1. 调用 `flush_streaming()`
2. 调用 `push_chat_line(LineType::Error, format!("[图片加载失败: {}]", reason), None)` 创建错误行
3. 复用现有 `LineType::Error` 渲染样式（红色文本），无需新增 LineType

#### URL 图片懒加载（`image.rs`）

**复用现有 `ImageCache` 机制**：CLI 已有完整的 URL 异步下载 + 占位渲染管线（`image.rs:118-230`）：

- `ImageEntry` 三态：`Ready` / `Loading` / `Error(String)`
- `get_or_load()` 对 URL 首次调用返回 `Loading`，`tokio::spawn` 异步下载
- `download_and_decode()` 用 `reqwest` 下载 + `image` 库解码
- 下载完成后通过 `image_ready_tx` channel 通知触发重绘
- `ImageHeightInfo::Placeholder` 为 Loading/Error 状态提供占位高度

**改动**：

1. `LineType::Image` path 为空 + remote_url 有值时，将 remote_url 作为 `ImageCache::get_or_load()` 的 key（现有 URL 下载逻辑自动触发）
2. 扩展 `download_and_decode()`：解码成功后，将原始 bytes 同步写入 `{project_path}/.visp/images/{url_hash}.{ext}` 缓存文件
   - url_hash：URL 的 SHA-256 前 16 字符 hex
   - ext：从 URL 路径推断，无法推断时默认 `png`
   - 缓存文件已存在时跳过下载，直接从文件加载
3. 扩展 `ImageEntry::Ready` 新增 `local_path: Option<String>` 字段，URL 下载成功后记录缓存路径，渲染层据此显示 `📁 {local_path}`
4. 下载失败时，`ImageEntry::Error` 已有占位渲染（现有逻辑）

**project_path 获取**：CLI 启动时已知（来自 daemon 连接配置），或从环境变量获取。

#### 图片地址渲染（`image.rs` 或 `app.rs` 渲染逻辑）

`LineType::Image` 的渲染改为复合渲染（同一 ChatLine 产出图片 + 地址文本）：

1. 计算图片 widget 所需高度 `img_height`（由 `ratatui-image` protocol 决定）
2. 计算地址文本行数 `caption_height`（有 remote_url = 2 行，无 = 1 行）
3. 在 ChatLine 对应的渲染区域内，用垂直布局分割为 `img_height` + `caption_height`
4. 上半区域：渲染 `ratatui-image` widget（现有逻辑）
5. 下半区域：渲染 Paragraph，内容为地址信息：
   - 有 `remote_url`：两行
     ```
       🔗 {remote_url}
       📁 {path}
     ```
   - 无 `remote_url`：一行
     ```
       📁 {path}
     ```
   - URL 来源图片下载后，path 更新为本地缓存路径
6. 地址文本样式：`Style::default().fg(Color::DarkGray)`，与正文区分

渲染循环改造范围：仅 `LineType::Image` 分支，从"单 widget 渲染"改为"image widget + Paragraph 垂直排列"。其他 LineType 渲染不受影响。

#### split_image_markers 扩展（`image.rs`）

`split_image_markers` 解析扩展标记格式 `<image: path | url>`：

1. 在提取 marker 内容后，检测 ` | ` 分隔符
2. 有分隔符：` | ` 前为 path（可为空字符串），后为 remote_url
3. 无分隔符：整个内容为 path，remote_url = None（向后兼容）
4. 生成的 `LineType::Image` 携带 `remote_url`
5. path 为空 + remote_url 有值时，触发懒加载下载

### 5.6 会话回放

daemon 的 session 回放逻辑（`service.rs:1241-1390`）回放 assistant 消息时，`Message.content` 中的标记通过 `TextDelta` 发送给 CLI：

- base64 图片标记 `<image: /local/path>`：`split_image_markers` 解析为 path，直接渲染（文件已持久化）
- URL 图片标记 `<image: | https://url>`：`split_image_markers` 解析为 path="" + remote_url，CLI 懒加载下载（缓存命中则直接渲染）

**无需额外改动**：回放路径自动复用扩展后的标记解析 + 懒加载机制。

## 6. 数据流详解

### 6.1 流式响应中的图片（base64 来源）

```
SSE event 1: delta.content = "Here's a diagram:\n"
SSE event 2: delta.content = [{"type":"image_url","image_url":{"url":"data:image/png;base64,iVBOR..."}}]
SSE event 3: delta.content = "\nHope this helps!"
SSE event 4: [DONE]
```

处理流程：

```
event 1 -> ChatEvent::TextDelta("Here's a diagram:\n")
          -> AgentEvent::TextDelta("Here's a diagram:\n")

event 2 -> OpenAiStreamEvent::ImageBlock { data: Some("iVBOR..."), remote_url: None }
          -> provider 解码写入: .visp/images/20260803_120000_0.png
          -> ChatEvent::ImageBlock { path: ".../.visp/images/20260803_120000_0.png", mime_type: "image/png", remote_url: None }
          -> agent_loop:
            1. flush text_buffer -> AgentEvent::TextDelta("Here's a diagram:\n")
            2. AgentEvent::ImageBlock { path: "...", remote_url: None }
            3. text_buffer 追加 "<image: .../.visp/images/20260803_120000_0.png>"

event 3 -> ChatEvent::TextDelta("\nHope this helps!")

event 4 -> Done
```

### 6.2 流式响应中的图片（URL 来源）

```
SSE event 1: delta.content = "Here's a diagram:\n"
SSE event 2: delta.content = [{"type":"image_url","image_url":{"url":"https://cdn.example.com/img123.png"}}]
SSE event 3: delta.content = "\nHope this helps!"
SSE event 4: [DONE]
```

处理流程：

```
event 1 -> ChatEvent::TextDelta("Here's a diagram:\n")
          -> AgentEvent::TextDelta("Here's a diagram:\n")

event 2 -> OpenAiStreamEvent::ImageBlock { data: None, remote_url: Some("https://cdn.example.com/img123.png") }
          -> provider 不下载，直接发出:
          -> ChatEvent::ImageBlock { path: "", mime_type: "", remote_url: Some("https://...") }
          -> agent_loop:
            1. flush text_buffer -> AgentEvent::TextDelta("Here's a diagram:\n")
            2. AgentEvent::ImageBlock { path: "", remote_url: Some("https://...") }
            3. text_buffer 追加 "<image: | https://cdn.example.com/img123.png>"

event 3 -> ChatEvent::TextDelta("\nHope this helps!")  (不阻塞，因为 provider 没有下载)

event 4 -> Done
```

CLI 收到 ImageBlock 后：
1. flush 当前文本
2. 创建 `LineType::Image { path: "", remote_url: Some("https://...") }`
3. 渲染时检测 path 为空，异步下载 URL 到 `.visp/images/{hash}.png`
4. 下载完成后渲染图片 + 显示 🔗 url + 📁 cached_path

### 6.3 非流式 / session 回放

session 回放时，`Message.content` 中的标记通过 `TextDelta` 发送：
- `<image: /local/path>` -> `split_image_markers` -> `LineType::Image { path, remote_url: None }` -> 直接渲染
- `<image: | https://url>` -> `split_image_markers` -> `LineType::Image { path: "", remote_url: Some(url) }` -> 懒加载下载

## 7. project_path 传递

- **Provider（base64 落盘）**：`project_path` 通过 `LlmConfig.extra` HashMap 传入。daemon 在构建 `LlmConfig` 时写入 `extra.insert("project_path", path)`。Provider 从 `config.extra.get("project_path")` 读取，缺省时回退到系统临时目录。
- **CLI（URL 懒加载下载）**：CLI 启动时已知 project_path（来自 daemon 连接配置），直接用于构建缓存路径。

## 8. 影响范围分析

| 模块 | 文件 | 改动类型 | 影响程度 |
|------|------|----------|----------|
| visp-core | `provider.rs` | 新增 ChatEvent::ImageBlock + ImageError 变体 | 小 |
| visp-core | `agent.rs` | 新增 AgentEvent::ImageBlock + ImageError 变体 | 小 |
| visp-core | `agent_loop.rs` | collect_stream_events 新增两个分支 + 双缓冲区 + event_to_msg 返回 None | 中 |
| visp-llm | `openai.rs` | SSE 解析 + base64 落盘 + remote_url 提取 | 中 |
| visp-llm | `anthropic.rs` | SSE 解析 + base64 落盘 + remote_url 提取 | 中 |
| visp-llm | `image_util.rs` | 新增文件（base64 解码/落盘/data URI 解析/MIME 映射） | 小 |
| visp-proto | `visp.proto` | 新增 ImageBlock + ImageError 消息 | 小 |
| visp-daemon | `service.rs` | agent_event_to_server_message 新增两个分支 | 小 |
| visp-cli | `app.rs` | render_pending 新增两个分支 + LineType 扩展 | 中 |
| visp-cli | `image.rs` | split_image_markers 扩展 + download_and_decode 扩展（落盘 + local_path）+ 地址渲染 | 中 |

## 9. 验收标准

1. **base64 图片输出**：LLM 响应中包含 base64 图片内容块时，Provider 解码落盘，CLI 内联渲染
2. **URL 图片输出**：LLM 响应中包含 URL 图片内容块时，Provider 不下载，CLI 渲染时懒加载下载并渲染
3. **不阻塞文本流**：URL 图片不阻塞 Provider 的 SSE 事件处理，后续文本正常输出
4. **文本 + 图片混合输出**：文本和图片在对话区正确交替显示，无重复渲染
5. **图片地址展示**：图片下方显示远程地址（如有）和本地缓存路径，格式清晰，便于复制
6. **图片处理失败**：base64 解码失败 / 文件写入失败时，CLI 显示红色错误提示，流继续
7. **URL 下载失败**：CLI 显示占位错误文本，不影响其他内容
8. **会话持久化**：session 回放时，base64 图片直接渲染（文件已持久化），URL 图片重新下载（或缓存命中）
9. **不支持图片输出的模型**：现有纯文本模型的行为不受影响
10. **终端降级**：不支持图形协议的终端，图片退化为占位文本，地址信息仍正常显示

## 10. 风险与缓解

| 风险 | 影响 | 缓解 |
|------|------|------|
| OpenAI 兼容模型图片格式不统一 | Provider 解析失败 | 支持多种格式（data URI、纯 base64、URL），未知格式降级为文本提示 |
| 大图片 base64 解码内存占用 | OOM | 限制图片大小 20MB，`save_base64_image` 解码前估算检查 |
| base64 解码失败 | 图片无法显示 | 发出 `ChatEvent::ImageError { reason }`，流继续 |
| 文件写入失败 | 图片无法显示 | 发出 `ChatEvent::ImageError { reason }`，流继续 |
| URL 下载超时或失败（CLI 侧） | 图片无法显示 | 30s 超时，失败时渲染占位文本 `[图片下载失败: {reason}]` |
| URL 下载大图片（CLI 侧） | OOM | 检查 Content-Length 或流式累计，超 20MB 中断 |
| .visp/images 目录无限增长 | 磁盘空间 | 后续可加清理策略，当前不处理 |
| project_path 未传入 Provider | base64 图片保存到临时目录，回放时可能丢失 | 回退临时目录 + 日志警告 |
| 标记格式扩展破坏兼容性 | 旧 session 回放失败 | `split_image_markers` 向后兼容：无 ` \| ` 时整体为 path |
