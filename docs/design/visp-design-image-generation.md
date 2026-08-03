# 设计：文生图模型支持

## 背景

用户配置了豆包 `doubao-seedream-5.0-lite` 文生图模型，使用火山引擎 Ark 平台的 Images API (`/images/generations`)，而非 Chat API (`/chat/completions`)。当前 visp 的 provider 架构只支持 chat completion 流式对话，无法调用文生图 API。

## 目标

在 visp 中支持文生图模型，使用户配置 `image_generation = true` 的模型后，agent loop 能识别并调用 Images API，将返回的图片 URL 通过 `ChatEvent::ImageBlock` 传播到 CLI 渲染。

## 架构设计

### 配置层

`LlmModelConfig`（daemon.toml）新增字段：
- `image_generation: Option<bool>` — 标记该模型为文生图模型，缺省 `None`/`false`

`LlmConfig`（visp-core）新增字段：
- `image_generation: bool` — 运行时标记，缺省 `false`

### Provider 层

在 `visp-llm/src/openai.rs` 中新增 `image_generation_request` 函数和 `OpenAiProvider::image_generate` 方法：

- **请求构建**：从 `messages` 中提取最后一条 user message 的文本作为 `prompt`，构建 `{ model, prompt, response_format: "url" }` 请求体
- **API 调用**：POST 到 `{base_url}/images/generations`（复用 `is_versioned_base_url` 逻辑处理路径前缀）
- **响应解析**：解析 JSON 响应中的 `data[0].url`，发出 `ChatEvent::ImageBlock { path: "", mime_type: "", remote_url: Some(url) }` + `ChatEvent::Done`
- **非流式**：文生图 API 不支持 SSE 流式，使用一次性 JSON 响应，但通过 `stream::iter` 适配为 `Stream<Item = Result<ChatEvent, LlmError>>`

### Agent Loop 层

`chat_stream` trait 方法不变。`OpenAiProvider::chat_stream` 内部检查 `config.image_generation`：
- `true` → 调用 `image_generate` 路径
- `false` → 走现有 chat completion 路径

这样 `agent_loop.rs` 和 `collect_stream_events` **无需修改** — 文生图 provider 产生的 `ChatEvent::ImageBlock` 会被现有的 image 事件处理逻辑正确处理。

### 数据流

```
用户输入 "画一只猫"
  -> agent_loop 构建 messages
  -> chat_stream (config.image_generation = true)
  -> OpenAiProvider 检测到 image_generation
  -> POST {base_url}/images/generations { model, prompt: "画一只猫" }
  -> 解析 JSON response.data[0].url
  -> ChatEvent::ImageBlock { remote_url: Some(url) }
  -> collect_stream_events -> AgentEvent::ImageBlock
  -> daemon 转换 -> proto::ImageBlock
  -> CLI render_pending -> LineType::Image { remote_url }
  -> ImageCache 异步下载 URL -> 渲染图片 + 🔗 地址
```

### Prompt 提取策略

从 `messages` 数组中提取最后一条 `Role::User` 消息的 `content` 作为 `prompt`。如果最后一条 user message 包含图片标记等非文本内容，取纯文本部分。

### 请求参数

文生图请求体支持以下字段（从 `config.extra` 读取，可选）：
- `size` — 图片尺寸（如 "2K"、"1024x1024"）
- `output_format` — 输出格式（如 "png"、"jpeg"）
- `watermark` — 是否添加水印
- `response_format` — 固定为 "url"

这些参数在 `config.extra` 中以 key-value 形式配置，构建请求时透传。

### 错误处理

- API 返回非 2xx → `LlmError::ApiError { status, message }`
- 响应 JSON 缺少 `data` 或 `data[0].url` → `LlmError::ApiError`
- 网络超时 → `LlmError::Network`
- 取消 → `LlmError::Cancelled`

### 配置示例

```toml
[[llm.models]]
name = "doubao-seedream"
protocol = "openai"
model = "doubao-seedream-5.0-lite"
api_key = "ark-xxx"
base_url = "https://ark.cn-beijing.volces.com/api/plan/v3"
image_generation = true
use_tool = false

[llm.models.extra]
size = "2K"
output_format = "png"
watermark = "false"
```

## 不做什么

- 不支持流式 SSE 文生图（API 本身不支持）
- 不支持批量图片生成（只取 `data[0].url`）
- 不支持 image edit / variation 等其他 Images API 端点
- 不修改 agent_loop.rs 的事件处理逻辑
- 不修改 CLI 渲染逻辑（已有的 `ImageBlock` 渲染路径复用）

## 影响范围

| 文件 | 改动 |
|------|------|
| `crates/visp-daemon/src/config.rs` | `LlmModelConfig` 新增 `image_generation` 字段 |
| `crates/visp-core/src/provider.rs` | `LlmConfig` 新增 `image_generation` 字段 + Default |
| `crates/visp-daemon/src/service.rs` | 两个 `LlmConfig` 构建点传递 `image_generation` |
| `crates/visp-llm/src/openai.rs` | 新增 `image_generate` 方法 + `chat_stream` 内部分支 |
| `crates/visp-llm/src/openai_tests.rs` | 新增文生图请求构建和响应解析测试 |
