# visp 多模态支持：CLI 图片展示

## 1. 目标

让 visp 支持多模态的第一步：在 CLI TUI 中展示图片。

### 本阶段范围（做）

1. **用户发送图片**：用户在 CLI 输入框通过特定语法引用本地图片文件，图片随文本一起发送给 agent
2. **CLI 展示用户图片**：用户发送的图片在对话区以内联图片形式渲染（而非文件路径文本）
3. **CLI 展示 Agent 返回的图片**：Agent 通过工具（如 `read_file` 读取图片、截图工具等）获取的图片，在对话区以内联图片展示
4. **终端协议自适应**：自动检测终端图片渲染能力（Kitty / iTerm2 / Sixel / Halfblocks 降级），不支持时退化为占位文本

### 不做（后续阶段）

- Agent 通过 LLM API 发送图片给多模态模型（需要改造 LLM provider 层、proto、message 模型）
- 图片编辑 / 标注
- 剪贴板粘贴图片
- 网络图片本地持久化缓存（重复访问同一 URL 每次从网络下载，仅内存缓存）

## 2. 技术选型

### 2.1 终端图片渲染：`ratatui-image`

- **库**：`ratatui-image`（与现有 `ratatui = "0.30"` 兼容）
- **能力**：提供 `Image`（静态）和 `StatefulImage`（可缩放）两个 widget，自动适配终端协议
- **协议检测**：`Picker::from_query_stdio()` 在启动时探测终端能力，返回 `ProtocolType`（Kitty / Sixel / Iterm2 / Halfblocks）
- **降级**：不支持任何图形协议时，`Halfblocks` 协议用 Unicode 半块字符渲染低分辨率图片，保证所有终端都有输出

### 2.2 图片解码：`image`

- **库**：`image`（标准 Rust 图片解码库）
- **用途**：将 PNG / JPEG / GIF / WebP 等格式解码为 `DynamicImage`，供 `ratatui-image` 编码为终端协议

## 3. 架构概览

### 3.1 当前数据流（纯文本）

```
CLI 输入框
  -> ChatHandle::send_input(text)
  -> ClientMessage::UserInput { text }
  -> gRPC stream -> daemon
  -> agent loop（Message { content: String }）
  -> ServerMessage::TextDelta { delta }
  -> gRPC stream -> CLI
  -> TabEntry::frames -> render_pending() -> ChatLine { content: String }
  -> ratatui Paragraph 渲染
```

### 3.2 目标数据流（多模态）

本阶段只涉及 **CLI 展示** 层面的改造，不改动 proto / core / daemon 的文本传输链路。图片的传输方式为：

- **用户图片**：用户输入 `@path/to/image.png` 语法引用图片。CLI 解析后：
  - 将 `@path` 替换为 `<image: path>` 标记，随 `UserInput.text` 发送给 daemon
  - 图片文件的读取和解码延迟到渲染时（`ImageCache::get_or_load`），不在输入阶段执行
  - 对话区渲染时，识别 `ChatLine` 中的图片标记，用 `ratatui-image` widget 内联渲染
- **Agent 返回图片**：Agent 通过 `read_file` 等工具读取图片文件时，工具结果中包含图片标记。CLI 渲染 `ToolResult` 行时，识别标记并内联渲染。

### 3.3 核心设计决策

**图片数据不通过 gRPC 传输，在 CLI 端本地解码渲染。**

理由：
1. 图片通常较大，通过 gRPC 传输二进制数据会显著增加带宽和序列化开销
2. CLI 和 daemon 运行在同一台机器上（daemon 的 project_path 就是本地路径），CLI 可以直接访问本地文件
3. 保持 proto 和 core 消息模型的纯文本特性，本阶段改动面最小
4. 后续若需要支持远程 CLI 连接远程 daemon，再考虑二进制传输

## 4. 模块设计

### 4.1 CLI 图片渲染器（`crates/visp-cli/src/image.rs`）

新模块，负责：

- **终端协议检测**：启动时调用 `Picker::from_query_stdio()`，创建全局 `Picker` 实例
- **图片缓存**：`ImageCache` 结构，存储已解码图片的 `StatefulProtocol`（ratatui-image 的可缩放状态），按图片路径缓存
- **图片标记解析**：将文本中的 `<image: path>` 标记解析为 `ImageRef { path, alt_text }` 结构
- **图片渲染接口**：提供 `render_image_block(f, area, image_cache, image_ref)` 函数，在指定区域渲染图片

#### 终端协议检测策略

```
启动时：
  1. Picker::from_query_stdio() 探测终端能力
  2. 若成功 -> 使用检测到的协议
  3. 若失败（如非 TTY 环境或探测超时）-> 降级为 Halfblocks
  4. 将 Picker 存入 AppState，全局共享
```

#### 图片缓存结构

```
ImageCache {
    picker: Picker,
    cache: HashMap<String, ImageEntry>,  // key = 文件绝对路径或 URL
}

enum ImageEntry {
    Ready {
        protocol: StatefulProtocol,   // ratatui-image 可缩放状态
        pixel_size: (u32, u32),       // 图片原始像素尺寸 (width, height)
    },
    Loading,        // 网络图片下载中
    Error(String),  // 加载/下载/解码失败
}
```

`pixel_size` 在首次加载时从 `DynamicImage` 读取，供 `MessageCache` 计算图片在终端中占用的行数。

```
fn get_or_load(&mut self, path: &str) -> &mut ImageEntry {
    // 1. 检查缓存
    // 2. 若未缓存：
    //    a. 本地文件：fs::read -> image::load -> DynamicImage
    //    b. 网络图片：插入 Loading，spawn 异步下载，完成后更新为 Ready 或 Error
    //    c. 记录 pixel_size = (image.width(), image.height())
    //    d. picker.new_resize_protocol(image) -> StatefulProtocol
    //    e. 存入缓存为 Ready
    // 3. 返回 &mut ImageEntry
}
```

缓存不主动淘汰。典型使用场景下图片数量有限，且 TUI 会话生命周期内持续访问。若未来需要，可加 LRU 淘汰策略。

#### 网络图片异步下载的渲染触发

网络图片下载在后台 `tokio::spawn` task 中完成，下载完成后需要通知主循环重新渲染。机制：

- `AppState` 中新增 `image_ready_tx: mpsc::UnboundedSender<()>`，主事件循环持有对应的 `image_ready_rx`
- `ImageCache` 持有 `image_ready_tx` 的引用
- 下载 task 完成后（无论成功或失败），通过 `tx.send(())` 发信号
- 主事件循环在 `tokio::select!` 中监听 `image_ready_rx`，收到信号后设置 `needs_render = true`

```
// 主事件循环中
tokio::select! {
    Some(()) = app.image_ready_rx.recv() => {
        app.needs_render = true;
    }
    // ... 其他事件（终端事件、gRPC 消息等）
}
```

与现有 gRPC 消息、终端事件并列处理，无需轮询。

### 4.2 图片标记协议

在消息文本中嵌入图片标记：

```
<image: /abs/path/to/image.png>       <!-- 本地文件 -->
<image: https://example.com/cat.png>   <!-- 网络图片 URL -->
```

规则：
- 标记格式：`<image: ` + 路径或 URL + `>`
- 路径/URL 的来源类型由前缀自动区分：`http://` / `https://` 为网络图片，其余为本地文件
- 一条消息中可包含多个图片标记
- 路径中的空格用 `%20` 编码（避免与标记语法冲突）

解析时，将包含标记的消息拆分为多个独立 `ChatLine`，通过统一的标记拆分函数 `split_image_markers(content, base_line_type) -> Vec<ChatLine>` 完成：

```
split_image_markers("看这张图 <image: /a.png> 和 <image: /b.png>", LineType::User)
  -> [
       ChatLine { line_type: User, content: "看这张图" },
       ChatLine { line_type: Image { path: "/a.png", alt_text: "a.png" } },
       ChatLine { line_type: Image { path: "/b.png", alt_text: "b.png" } },
     ]
```

所有创建 `ChatLine` 的地方（`render_pending` 处理 UserMessage/ToolResult、`flush_streaming` 处理 Assistant 文本）统一调用此函数，产出 0~N 条 ChatLine。标记解析逻辑只有一处实现，不按 `LineType` 区分是否解析。

**多条 ChatLine 的 id 分配**：`split_image_markers` 返回的 ChatLine `id` 字段为占位值（0），由调用方在 push 前逐条分配唯一 id（`tab.next_message_id` 自增）。函数只负责内容拆分，不关心 id。

**空文本段处理**：标记在文本开头/结尾时会产生空文本段。空文本段（长度为 0）不生成 ChatLine，直接跳过。

#### 本地图片 vs 网络图片

`LineType::Image` 的 `path` 字段统一存储路径或 URL，`ImageCache` 内部按前缀区分处理：

- **本地图片**（`/abs/path` 或 `./relative`）：直接 `fs::read` + `image::load` 解码
- **网络图片**（`http://` / `https://`）：用 `reqwest` 下载到内存，再 `image::load` 解码。下载是异步操作，首次加载时图片位置显示 `[加载中: url]` 占位，下载完成后下次渲染时替换为图片

网络图片下载策略：
- 下载超时：10 秒，超时后显示 `[图片下载超时: url]`
- 下载失败：显示 `[图片下载失败: url (reason)]`
- 下载的图片数据不持久化，仅在内存缓存中保留解码后的 `StatefulProtocol`
- 重复引用同一 URL 时从缓存命中，不重复下载

`ImageCache` 结构见 §4.1。内部按 `path` 前缀区分处理：

#### 流式文本中的标记处理

`TextDelta` 逐片到达，追加到 `streaming_text`。如果标记跨多个 delta，流式渲染会显示半截标记。处理方式：**流式渲染截断 + flush 时拆分**，不引入额外状态字段。

**1. 流式渲染时**：渲染 `streaming_text` 前，从尾部扫描是否有未完成的 `<image:` 标记（有 `<image:` 但无对应 `>`）。如果有，渲染时截断到 `<image:` 之前，未完成部分不显示。

**2. flush 时**：`streaming_text` 完整，按标记拆分为多个 ChatLine（文本段 -> `LineType::Assistant`，图片段 -> `LineType::Image`）。与 User/ToolResult 的拆分逻辑复用同一套标记解析函数。

### 4.3 `ChatLine` 扩展

新增 `LineType::Image` 变体，图片作为独立消息块，不混入文本行：

```
LineType::Image {
    path: String,       // 绝对路径或 URL
    alt_text: String,   // 替代文本（不支持图片时显示）
}
```

不需要在 `ChatLine` 中新增 `images` 字段。图片信息完全由 `LineType::Image` 携带，`content` 字段存储路径文本（用于调试/日志）。

### 4.4 渲染层改造（`ui.rs`）

#### 4.4.1 独立图片块渲染（两阶段布局）

图片作为独立的 `LineType::Image` 消息块，与文本消息块平行。`render_chat_area` 采用两阶段渲染避免借用冲突（`message_caches` 与 `image_cache` 同在 `AppState` 中，不可同时 `&mut`）：

**阶段 1 - 布局**：遍历 `messages` + `message_caches`，计算每条消息的 y 偏移和高度，产出布局表。此阶段只读 `AppState`，不触碰 `image_cache`。

```
struct LayoutEntry {
    msg_idx: usize,
    y_offset: u16,
    height: u16,
    is_image: bool,
}
```

**阶段 2 - 渲染**：遍历布局表，按 `is_image` 分流：
- 文本消息 -> `render_block(Paragraph)`（借用 `message_caches`）
- 图片消息 -> `render_image_block(StatefulImage)`（借用 `image_cache`）

```
render_chat_area:
  // 阶段 1：布局
  let layout = compute_layout(app);  // &AppState -> Vec<LayoutEntry>

  // 阶段 2：渲染
  for entry in &layout {
      if entry.is_image {
          render_image_block(f, area, &mut app.image_cache, msg, entry.y_offset)
      } else {
          render_block(f, area, style, &cache.lines, entry.height, entry.y_offset)
      }
  }
```

每个 `&mut` 借用在单次迭代中获取并释放，不跨迭代持有，避免同时借用 `message_caches` 和 `image_cache`。

#### 4.4.2 图片尺寸计算

图片在终端中的显示尺寸需要根据可用宽度和终端字体像素尺寸自适应：

```
fn calc_image_height(
    pixel_w: u32, pixel_h: u32,  // 图片像素尺寸
    max_cols: u16,                // 可用终端列数
    font_size: (u16, u16),        // Picker.font_size (width_px, height_px)
) -> u16 {
    // 1. 按最大宽度缩放：scaled_w_px = max_cols * font_size.0
    // 2. 保持宽高比：scaled_h_px = pixel_h * (scaled_w_px / pixel_w)
    // 3. 转为行数：rows = ceil(scaled_h_px / font_size.1)
    // 4. 若图片本身窄于 max_cols，不放大，用原始尺寸
    // 返回缩放后的行数
}
```

图片像素尺寸在首次加载时读取并缓存在 `ImageCache` 中。

**`MessageCache` 签名扩展**：`from_message` 新增参数以获取图片高度信息：

```
/// 图片度量信息，供 MessageCache 计算图片行高
struct ImageMetrics<'a> {
    font_size: (u16, u16),                    // Picker.font_size
    image_cache: &'a ImageCache,              // 查询图片像素尺寸
}

/// 图片高度查询结果
enum ImageHeightInfo {
    Ready(u16),    // 已计算的实际行高
    Placeholder,   // Loading/Error 状态，返回 1 行占位高度
}

impl ImageCache {
    fn query_height(&self, path: &str, max_cols: u16) -> ImageHeightInfo {
        // Ready: 用 pixel_size + font_size 计算实际行高
        // Loading/Error: 返回 Placeholder (1 行)
    }
}

impl MessageCache {
    pub fn from_message(
        msg: &ChatLine,
        width: u16,
        expanded: bool,
        image_metrics: Option<&ImageMetrics>,  // 非图片消息传 None
    ) -> Self
}
```

`ensure_all_caches` 在调用 `from_message` 时构造 `ImageMetrics` 传入。终端 resize 时，`width` 变化触发缓存重建，图片行数自动重算。

**图片状态变化时的缓存失效**：`MessageCache` 新增 `image_state` 字段记录创建时的图片状态（`Loading` / `Ready` / `Error`）。`ensure_all_caches` 检查图片消息的当前 `ImageEntry` 状态与缓存记录是否一致，不一致则重建该条缓存。这样网络图片下载完成后（`Loading` -> `Ready`），`needs_render` 触发的 `ensure_all_caches` 会自动重建对应 `MessageCache`，更新 `line_count` 为实际图片高度。

### 4.5 用户输入处理

#### 4.5.1 图片路径语法

用户在输入框中输入 `@/path/to/image.png`（本地文件）或 `@https://example.com/cat.png`（网络图片）。

`@` 后跟路径或 URL，按以下规则匹配：

- `@` 后以 `http://` 或 `https://` 开头 -> URL 图片，直接匹配
- `@` 后为其他文本 -> 尝试作为文件路径解析（相对于 `project_path`），若文件存在且扩展名为支持的图片格式 -> 匹配为本地图片
- 以上都不满足 -> `@` 及后续文本按普通文本处理

示例：
- `@/abs/path.png` ✅ 绝对路径
- `@./relative.png` ✅ 相对路径
- `@../parent.png` ✅ 上级路径
- `@screenshots/error.png` ✅ 相对路径（文件存在且为图片格式）
- `@https://example.com/img.png` ✅ URL
- `@mention` ❌ 文件不存在，普通文本
- `user@email.com` ❌ `@` 不在词首（前面有文本），普通文本

匹配在用户按 Enter 时执行（输入处理阶段），非逐字符检查，无性能开销。

1. CLI 解析输入文本，识别 `@` 开头的词（`@` 在词首，后跟非空白字符序列）
2. URL（`http://` / `https://` 开头）：直接匹配为 URL 图片
3. 本地文件：基于 `AppState.project_path` 解析为绝对路径，检查文件存在且扩展名为支持的图片格式；不满足则 `@` 及后续文本按普通文本处理，原样发送，不报错
4. 替换为 `<image: path-or-url>` 标记，随文本一起发送

> **注意**：输入阶段和渲染阶段的"文件不存在"处理不同：
> - **输入阶段**（用户输入 `@path`）：文件不存在 -> `@path` 按普通文本处理，原样发送
> - **渲染阶段**（消息文本中已有 `<image: path>` 标记，CLI 读取文件失败）：显示 `[图片未找到: path]` 错误提示

#### 4.5.2 输入解析流程

解析在 `event.rs` 输入处理阶段完成（用户按 Enter 后、`send_input` 调用前），基于 `AppState.project_path`（`app.rs:1110`，由 `main.rs` 创建 session 时传入）解析相对路径：

```
用户输入: "请分析这张截图 @screenshots/error.png"
         ↓
event.rs 解析:
  - 识别 @screenshots/error.png
  - 验证：文件存在，扩展名 .png
  - 基于 AppState.project_path 解析为绝对路径：
    /Users/xxx/project/screenshots/error.png
  - 替换为标记：<image: /Users/xxx/project/screenshots/error.png>
         ↓
发送给 daemon: "请分析这张截图 <image: /Users/xxx/project/screenshots/error.png>"
         ↓
CLI 对话区渲染:
  - ChatLine(User): "请分析这张截图"
  - ChatLine(Image): [内联渲染 screenshots/error.png 的内容]
```

URL 引用（`@https://...`）不经过路径解析，直接替换为 `<image: url>` 标记。

### 4.6 工具结果中的图片

当 Agent 调用 `read_file` 等工具读取图片文件时，工具返回的结果文本中嵌入 `<image: path>` 标记。CLI 渲染 `ToolResult` 类型的 `ChatLine` 时，同样解析标记并拆分为文本 ChatLine + Image ChatLine。

#### `read_file` 工具改造（`visp-tools/src/file.rs`）

在现有 binary 检测**之前**增加图片格式检测分支：

1. 检查文件扩展名是否为支持的图片格式（`.png`、`.jpg`、`.jpeg`、`.gif`、`.webp`、`.bmp`、`.ico`）
2. 若是图片格式，直接返回 `<image: /abs/path>` 标记文本，**不读取文件内容**，不经过 binary 检测
3. 若不是图片格式，走原有的 binary 检测 + UTF-8 读取逻辑

路径使用 `read_file` 已有的绝对路径（工具执行时基于 `project_path` 解析）。daemon 端不读取图片内容，只返回路径标记，CLI 端负责实际读取和渲染。

#### MCP 图片处理改造（`visp-mcp/src/client.rs`）

现有代码（`client.rs:242`）将 MCP `RawContent::Image` 转为 `[Image: … (N bytes)]` 占位文本。改为：若图片数据含本地文件路径，转为 `<image: path>` 标记；若为纯二进制数据，保留占位文本（MCP 图片通常为内联二进制，无本地路径，本阶段不处理内联二进制图片）。

## 5. 数据流图

```
┌──────────────────────────────────────────────────────────────┐
│                         CLI (visp-cli)                        │
│                                                               │
│  ┌──────────┐    ┌──────────────┐    ┌───────────────────┐   │
│  │ 输入框    │───->│ @path 解析   │───->│ <image: path> 标记│   │
│  └──────────┘    └──────────────┘    └───────┬───────────┘   │
│                                              │                │
│                                              ▼                │
│  ┌──────────────────────────────────────────────────────┐    │
│  │         ChatHandle::send_input(text_with_marker)      │    │
│  └──────────────────────────┬───────────────────────────┘    │
│                             │ gRPC (纯文本)                    │
│  ┌──────────────────────────▼───────────────────────────┐    │
│  │              ServerMessage (TextDelta / ToolResult)    │    │
│  └──────────────────────────┬───────────────────────────┘    │
│                             │                                │
│  ┌──────────────────────────▼───────────────────────────┐    │
│  │  render_pending() -> ChatLine 拆分                     │    │
│  │  (解析 <image: path> 标记 -> LineType::Image 块)        │    │
│  └──────────────────────────┬───────────────────────────┘    │
│                             │                                │
│  ┌──────────────────────────▼───────────────────────────┐    │
│  │  render_chat_area()                                    │    │
│  │  ├─ 文本块 -> render_block(Paragraph)  (不变)           │    │
│  │  └─ 图片块 -> render_image_block(StatefulImage)        │    │
│  │                   ┌─────────────────────┐              │    │
│  │                   │  ImageCache          │              │    │
│  │                   │  (路径->Protocol)     │              │    │
│  │                   │  本地文件读取+解码    │              │    │
│  │                   └─────────────────────┘              │    │
│  └──────────────────────────────────────────────────────┘    │
└──────────────────────────────────────────────────────────────┘
         │ gRPC (纯文本)
         ▼
┌──────────────────────────────────────────────────────────────┐
│                    Daemon (visp-daemon)                       │
│  ┌──────────────────────────────────────────────────────┐    │
│  │  agent loop                                           │    │
│  │  - 接收 UserInput.text (含 <image: path> 标记)         │    │
│  │  - 工具执行 (read_file 读取图片 -> 返回标记文本)         │    │
│  │  - LLM 对话 (文本中包含图片标记，LLM 看到的是路径文本)  │    │
│  └──────────────────────────────────────────────────────┘    │
└──────────────────────────────────────────────────────────────┘
```

## 6. 影响范围分析

| 模块 | 文件 | 改动类型 | 说明 |
|------|------|----------|------|
| visp-cli | `src/image.rs` | **新增** | 图片渲染器、缓存、协议检测 |
| visp-cli | `src/app.rs` | 修改 | `ChatLine` 新增 `LineType::Image` 变体；`render_pending()` 解析图片标记并拆分为独立 ChatLine；`AppState` 新增 `image_ready_tx` 及 `ImageCache` 字段 |
| visp-cli | `src/ui.rs` | 修改 | `render_chat_area` 按 `LineType` 分流：文本走 `render_block`，图片走 `render_image_block` |
| visp-cli | `src/event.rs` | 修改 | 输入文本中的 `@path` 解析为 `<image: path>` 标记；基于 `AppState.project_path` 解析相对路径 |
| visp-cli | `src/main.rs` | 修改 | 启动时初始化 `Picker` / `ImageCache`，创建 `image_ready` channel，注入 `AppState`；主循环 `select!` 监听 `image_ready_rx` |
| visp-cli | `Cargo.toml` | 修改 | 新增 `ratatui-image`、`image`、`reqwest`（workspace）依赖 |
| visp-tools | `src/file.rs` | 修改 | `read_file` 读取图片文件时返回 `<image: path>` 标记 |
| visp-mcp | `src/client.rs` | 修改 | MCP Image content 转为 `<image: path>` 标记（若涉及本地文件） |

**不改动的模块**：`visp-proto`（proto 定义不变）、`visp-core`（Message 模型不变）、`visp-daemon`（service 逻辑不变，图片标记作为普通文本透传）、`visp-llm`（provider 层不变）。

## 7. 边界情况

1. **文件不存在**：图片路径指向的文件不存在 -> 渲染 `[图片未找到: path]` 红色提示文本
2. **格式不支持**：图片解码失败 -> 渲染 `[图片解码失败: path (reason)]` 红色提示文本
3. **网络图片下载失败**：渲染 `[图片下载失败: url (reason)]` 红色提示文本
4. **网络图片下载超时**：10 秒超时 -> 渲染 `[图片下载超时: url]` 红色提示文本
5. **网络图片下载中**：首次渲染显示 `[加载中: url]` 灰色提示，下载完成后下次渲染替换为图片
6. **终端不支持图形协议**：降级为 Halfblocks（半块字符渲染），或纯文本 `[图片: filename.png]`
7. **图片过大**：按终端可用宽度缩放，`StatefulImage` 的 `Resize::Fit` 模式自动处理
8. **终端窗口缩放**：`StatefulImage` 会根据新的 area 自动重新编码渲染
9. **非 TTY 环境**：`Picker::from_query_stdio()` 失败时，降级为 Halfblocks 或纯文本占位
10. **相对路径**：`@./screenshot.png` 基于 `AppState.project_path`（`app.rs:1110`）解析为绝对路径

## 8. 验收标准

1. 用户输入 `@path/to/image.png 请描述这张图` -> 对话区显示内联图片 + 文本
2. 用户输入 `@https://example.com/diagram.png 请分析这张图` -> 对话区显示加载占位，下载完成后显示内联图片
3. Agent 调用 `read_file` 读取 PNG -> 工具结果区显示内联图片
4. 在 Kitty / iTerm2 终端中使用图形协议渲染；在不支持的终端中降级为 Halfblocks 或占位文本
5. 终端窗口缩放时，图片自适应缩放
6. 文件不存在或格式错误时，显示明确的错误提示而非崩溃
7. 网络图片下载失败/超时时，显示明确的错误提示而非崩溃
8. 流式文本中出现图片标记时，未完成标记不在渲染中显示为半截文本

## 9. 后续阶段（不在本次范围）

- **Phase 2**：proto 扩展 - `UserInput` 支持携带图片二进制 / base64 数据
- **Phase 3**：core Message 模型扩展 - 支持 `ImageContent` content block
- **Phase 4**：LLM provider 扩展 - 将图片作为多模态 content 发送给 LLM API
- **Phase 5**：剪贴板粘贴图片 / 截图工具 / 网络图片本地持久化缓存
