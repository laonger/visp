# 设计：用户图片多模态发送（vision 支持）

## 背景

用户在 CLI 中通过 `@path` 引用图片时，`parse_image_refs` 将其转换为 `<image: path>` 标记。该标记仅用于 CLI 本地渲染（ratatui-image 显示），发送到 daemon 和 LLM 时作为纯文本传输。LLM 看到的是字符串 `<image: /path/to/file>`，无法获取图片内容，导致识图功能不工作。

## 目标

使用户附带的图片以多模态格式发送给 LLM provider，支持 OpenAI vision API 和 Anthropic vision API。

## 架构设计

### 数据流

```
CLI: 用户输入 "@screenshot.png 这是什么？"
  -> parse_image_refs -> "<image: /abs/screenshot.png> 这是什么？"
  -> gRPC UserInput { text: "<image: /abs/screenshot.png> 这是什么？" }
  -> daemon 接收
  -> 构造 Message::user(text)
  -> [新增] 解析 text 中的 <image: path> 标记，读取图片文件，base64 编码
  -> Message.images = vec![ImageData { path, base64, mime_type }]
  -> text 中移除 <image: path> 标记，保留纯文本部分
  -> chat_stream -> build_openai_messages / build_anthropic_messages
  -> [新增] 当 Message.images 非空时，构建多模态 content 数组
  -> OpenAI: content: [{ type: "text", text: "这是什么？" }, { type: "image_url", image_url: { url: "data:image/png;base64,..." } }]
  -> Anthropic: content: [{ type: "text", text: "这是什么？" }, { type: "image", source: { type: "base64", media_type: "image/png", data: "..." } }]
```

### 数据结构

`Message` 新增字段：

```rust
pub images: Vec<ImageData>,
```

`ImageData` 结构：

```rust
pub struct ImageData {
    pub path: String,
    pub base64: String,
    pub mime_type: String,
}
```

### 解析位置

在 daemon 的 `service.rs` 中，用户输入被接收并构造 `Message::user(text)` 之后，立即解析 `<image: path>` 标记：

1. 用正则或简单字符串搜索找到所有 `<image: ...>` 标记
2. 区分本地路径标记（`<image: /path>`）和 URL 标记（`<image: | url>`）
3. 对于本地路径标记：读取文件，base64 编码，推断 MIME 类型
4. 从 text 中移除标记，保留纯文本
5. 构造 `Message::user(clean_text)` 并设置 `images` 字段

**不处理 URL 标记**：URL 图片需要下载，且当前 `@url` 引用场景极少。仅处理本地文件路径。

### Provider 层修改

#### build_openai_messages (openai.rs)

当 `msg.role == Role::User && !msg.images.is_empty()` 时，content 构建为数组：

```json
{
  "role": "user",
  "content": [
    { "type": "text", "text": "这是什么？" },
    { "type": "image_url", "image_url": { "url": "data:image/png;base64,..." } }
  ]
}
```

如果文本为空但有图片，只发送图片 content block。

#### build_anthropic_messages (anthropic.rs)

当 `msg.role == Role::User && !msg.images.is_empty()` 时，content blocks 中添加 image block：

```json
{
  "type": "image",
  "source": {
    "type": "base64",
    "media_type": "image/png",
    "data": "base64data..."
  }
}
```

### 不做什么

- 不处理 URL 图片的下载和编码（仅本地文件）
- 不修改 `<image: path>` 标记的 CLI 渲染逻辑
- 不修改 `parse_image_refs` 的行为
- 不持久化图片 base64 数据到 session history（`Message.images` 在序列化到 history 时清空，避免膨胀）

### 影响范围

| 文件 | 改动 |
|------|------|
| `crates/visp-core/src/message.rs` | 新增 `ImageData` 结构 + `Message.images` 字段 |
| `crates/visp-daemon/src/service.rs` | 用户输入构造 Message 时解析图片标记 |
| `crates/visp-llm/src/openai.rs` | `build_openai_messages` 多模态 content |
| `crates/visp-llm/src/anthropic.rs` | `build_anthropic_messages` 多模态 content |
| `crates/visp-llm/src/openai_tests.rs` | 多模态请求构建测试 |
