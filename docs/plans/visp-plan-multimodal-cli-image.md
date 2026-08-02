# visp 多模态 CLI 图片展示：工作计划

## 概述

基于设计文档 `docs/design/visp-design-multimodal-cli-image.md`，实现 CLI TUI 中的图片展示功能。改动涉及 `visp-cli`（主要）、`visp-tools`、`visp-mcp` 三个 crate。

核心链路：用户输入 `@path` -> 替换为 `<image: path>` 标记 -> gRPC 纯文本传输 -> CLI `render_pending`/`flush_streaming` 拆分为 `LineType::Image` ChatLine -> `render_chat_area` 两阶段渲染（文本走 `render_block`，图片走 `StatefulImage`）。

---

## 步骤 1：依赖与基础类型

### 1a：添加依赖 + `LineType::Image` + `split_image_markers`

#### 🔴 红 - 测试

| # | 测试用例 | 简明描述 |
|---|---------|---------|
| 1 | `split_image_markers` 无标记的纯文本 | 返回单条 ChatLine，line_type 不变 |
| 2 | `split_image_markers` 单个标记在文本中间 | 返回 3 条：前文本 + Image + 后文本 |
| 3 | `split_image_markers` 标记在文本开头 | 跳过空文本段，返回 Image + 后文本 |
| 4 | `split_image_markers` 标记在文本结尾 | 跳过空文本段，返回前文本 + Image |
| 5 | `split_image_markers` 多个标记 | 交替拆分，空文本段跳过 |
| 6 | `split_image_markers` 仅一个标记无其他文本 | 返回单条 Image |
| 7 | `split_image_markers` URL 标记 | path 字段存储 URL |
| 8 | `LineType::Image` 的 path 和 alt_text 正确提取 | alt_text 从路径/URL 末段提取 |

#### 🟢 绿 - 实现

- `crates/visp-cli/Cargo.toml`：新增 `ratatui-image`、`image`、`reqwest`（workspace）依赖
- `crates/visp-cli/src/app.rs`：`LineType` 新增 `Image { path, alt_text }` 变体
- `crates/visp-cli/src/image.rs`（新文件）：`split_image_markers(content, base_line_type) -> Vec<ChatLine>` 函数，解析 `<image: path>` 标记，空文本段跳过
- `crates/visp-cli/src/main.rs`：`mod image;`

#### 🧪 测试 -> 🔍 类型检查

```bash
cargo test -p visp-cli -- image::tests
cargo check -p visp-cli
```

#### 📦 提交

`feat(cli): add LineType::Image and split_image_markers parser`

---

### 1b：`@path` 输入解析

#### 🔴 红 - 测试

| # | 测试用例 | 简明描述 |
|---|---------|---------|
| 1 | `@/abs/path.png` 绝对路径 | 替换为 `<image: /abs/path.png>` |
| 2 | `@./relative.png` 相对路径 | 基于 project_path 解析为绝对路径后替换 |
| 3 | `@screenshots/error.png` 相对路径（文件存在+图片格式） | 替换为 `<image: abs_path>` |
| 4 | `@nonexistent.png` 文件不存在 | `@nonexistent.png` 原样保留，不替换 |
| 5 | `@mention` 非图片文件 | `@mention` 原样保留 |
| 6 | `@https://example.com/img.png` URL | 替换为 `<image: https://example.com/img.png>` |
| 7 | `user@email.com` @ 不在词首 | 原样保留 |
| 8 | 多个 `@` 引用混合文本 | 全部正确替换 |
| 9 | `@` 后跟空格 | `@` 原样保留 |

#### 🟢 绿 - 实现

- `crates/visp-cli/src/image.rs`：`parse_image_refs(text, project_path) -> String` 函数
  - 识别词首 `@` 后跟非空白字符序列
  - `http://`/`https://` 前缀 -> 直接匹配为 URL
  - 其他 -> 基于 `project_path` 解析为绝对路径，检查文件存在 + 图片扩展名（`.png`/`.jpg`/`.jpeg`/`.gif`/`.webp`/`.bmp`/`.ico`）
  - 不满足 -> 原样保留 `@` 及后续文本

#### 🧪 测试 -> 🔍 类型检查

```bash
cargo test -p visp-cli -- image::tests
cargo check -p visp-cli
```

#### 📦 提交

`feat(cli): add @path image reference parsing in user input`

---

## 步骤 2：ImageCache 与图片加载

### 2a：`ImageCache` 结构 + `Picker` 初始化 + 本地图片加载

#### 🔴 红 - 测试

| # | 测试用例 | 简明描述 |
|---|---------|---------|
| 1 | `ImageCache::new` 创建成功，picker 已初始化 | 不 panic |
| 2 | `get_or_load` 本地图片首次加载 | 返回 `ImageEntry::Ready`，pixel_size 正确 |
| 3 | `get_or_load` 本地图片缓存命中 | 第二次调用不重新读取文件 |
| 4 | `get_or_load` 文件不存在 | 返回 `ImageEntry::Error` |
| 5 | `get_or_load` 非图片文件 | 返回 `ImageEntry::Error` |
| 6 | `query_height` Ready 状态 | 返回 `ImageHeightInfo::Ready(n)` |
| 7 | `query_height` Error 状态 | 返回 `ImageHeightInfo::Placeholder` |
| 8 | `calc_image_height` 宽图缩放 | 高度按宽度比例缩放 |
| 9 | `calc_image_height` 窄图不放大 | 用原始尺寸计算行数 |

#### 🟢 绿 - 实现

- `crates/visp-cli/src/image.rs`：
  - `ImageCache` 结构（`picker` + `cache: HashMap`）
  - `ImageEntry` 枚举（`Ready { protocol, pixel_size }` / `Loading` / `Error(String)`）
  - `ImageHeightInfo` 枚举（`Ready(u16)` / `Placeholder`）
  - `ImageCache::new() -> Self`（Picker 初始化，失败降级 Halfblocks）
  - `ImageCache::get_or_load(&mut self, path) -> &mut ImageEntry`（本地文件同步加载）
  - `ImageCache::query_height(&self, path, max_cols) -> ImageHeightInfo`
  - `calc_image_height(pixel_w, pixel_h, max_cols, font_size) -> u16`

#### 🧪 测试 -> 🔍 类型检查

```bash
cargo test -p visp-cli -- image::tests
cargo check -p visp-cli
```

#### 📦 提交

`feat(cli): add ImageCache with local image loading and height calculation`

---

### 2b：网络图片异步下载 + `image_ready` channel

#### 🔴 红 - 测试

| # | 测试用例 | 简明描述 |
|---|---------|---------|
| 1 | `get_or_load` URL 首次调用 | 返回 `Loading`，spawn 下载 task |
| 2 | URL 下载成功后缓存更新为 `Ready` | 下载 task 完成后 `cache` 为 `Ready` |
| 3 | URL 下载失败后缓存更新为 `Error` | 下载 task 失败后 `cache` 为 `Error` |
| 4 | URL 下载中缓存命中 | 第二次调用返回 `Loading`，不重复 spawn |
| 5 | `image_ready_tx` 在下载完成后发信号 | tx.send(()) 被调用 |
| 6 | `query_height` Loading 状态 | 返回 `Placeholder` |

#### 🟢 绿 - 实现

- `crates/visp-cli/src/image.rs`：
  - `ImageCache` 新增 `image_ready_tx: Option<mpsc::UnboundedSender<()>>` 字段
  - `get_or_load` 对 URL 前缀：插入 `Loading`，`tokio::spawn` 异步下载（`reqwest` + 10s 超时），完成后更新 cache 为 `Ready` 或 `Error`，通过 `image_ready_tx` 发信号
  - URL 下载中再次调用返回已有 `Loading`，不重复 spawn

#### 🧪 测试 -> 🔍 类型检查

```bash
cargo test -p visp-cli -- image::tests
cargo check -p visp-cli
```

#### 📦 提交

`feat(cli): add async URL image download with image_ready notification`

---

## 步骤 3：ChatLine 拆分集成

### 3a：`render_pending` 和 `flush_streaming` 集成 `split_image_markers`

#### 🔴 红 - 测试

| # | 测试用例 | 简明描述 |
|---|---------|---------|
| 1 | `render_pending` 处理含标记的 UserMessage | 拆分为 User + Image ChatLine，id 唯一递增 |
| 2 | `render_pending` 处理含标记的 ToolResult | 拆分为 ToolResult + Image ChatLine |
| 3 | `flush_streaming` 处理含标记的 streaming_text | 拆分为 Assistant + Image ChatLine |
| 4 | `flush_streaming` 无标记的 streaming_text | 行为不变，单条 Assistant ChatLine |
| 5 | 拆分后多条 ChatLine 的 id 连续递增 | next_message_id 正确推进 |
| 6 | 标记跨消息边界的回放 | 多帧 UserMessage 各自独立拆分 |

#### 🟢 绿 - 实现

- `crates/visp-cli/src/app.rs`：
  - `flush_streaming`：调用 `split_image_markers` 替代直接 `push_chat_line`，逐条分配 id
  - `render_pending` 中 `UserMessage` 和 `ToolResult` 分支：调用 `split_image_markers` 替代直接 `push_chat_line`
  - 新增 `push_chat_lines(lines: Vec<ChatLine>)` 方法，逐条分配 id 并 push

#### 🧪 测试 -> 🔍 类型检查

```bash
cargo test -p visp-cli -- app::tests
cargo check -p visp-cli
```

#### 📦 提交

`feat(cli): integrate split_image_markers into render_pending and flush_streaming`

---

### 3b：流式渲染截断未完成标记

#### 🔴 红 - 测试

| # | 测试用例 | 简明描述 |
|---|---------|---------|
| 1 | `streaming_text` 含完整标记 | 渲染时标记部分不显示为文本（标记完整则正常截断到标记前） |
| 2 | `streaming_text` 含未完成标记 `<image: /tmp/sc` | 渲染时截断到 `<image:` 之前，不显示半截标记 |
| 3 | `streaming_text` 无标记 | 行为不变 |
| 4 | `streaming_text` 标记后有后续文本 | 标记前的文本正常显示 |

#### 🟢 绿 - 实现

- `crates/visp-cli/src/app.rs`：新增 `streaming_display_text() -> String` 方法，从尾部扫描未完成 `<image:` 标记并截断
- `crates/visp-cli/src/ui.rs`：`render_chat_area` 中流式文本渲染改用 `streaming_display_text()`

#### 🧪 测试 -> 🔍 类型检查

```bash
cargo test -p visp-cli -- app::tests
cargo check -p visp-cli
```

#### 📦 提交

`feat(cli): truncate incomplete image markers in streaming text display`

---

## 步骤 4：渲染层改造

### 4a：`MessageCache` 支持 `LineType::Image`

#### 🔴 红 - 测试

| # | 测试用例 | 简明描述 |
|---|---------|---------|
| 1 | `from_message` 处理 `LineType::Image` + Ready 状态 | line_count 为计算的实际行高 |
| 2 | `from_message` 处理 `LineType::Image` + Loading 状态 | line_count = 1 |
| 3 | `from_message` 处理 `LineType::Image` + Error 状态 | line_count = 1 |
| 4 | `from_message` 非图片消息 + image_metrics=None | 行为不变 |
| 5 | `MessageCache.image_state` 字段记录创建时状态 | Ready/Loading/Error 正确记录 |
| 6 | `ensure_all_caches` 检测图片状态变化触发重建 | Loading -> Ready 时缓存重建 |

#### 🟢 绿 - 实现

- `crates/visp-cli/src/app.rs`：
  - `MessageCache` 新增 `image_state: Option<ImageState>` 字段
  - `ImageState` 枚举（`Loading` / `Ready` / `Error`）
  - `from_message` 签名新增 `image_metrics: Option<&ImageMetrics>` 参数
  - `from_message` 处理 `LineType::Image`：通过 `ImageMetrics` 查询高度，设置 `line_count` 和 `image_state`
  - `ensure_all_caches`：图片消息检查 `image_state` 是否与当前 `ImageEntry` 状态一致，不一致则重建
  - `ensure_all_caches`：构造 `ImageMetrics` 传入 `from_message`

#### 🧪 测试 -> 🔍 类型检查

```bash
cargo test -p visp-cli -- app::tests
cargo check -p visp-cli
```

#### 📦 提交

`feat(cli): extend MessageCache to support LineType::Image with height calculation`

---

### 4b：`render_chat_area` 两阶段渲染 + `render_image_block`

#### 🔴 红 - 测试

| # | 测试用例 | 简明描述 |
|---|---------|---------|
| 1 | `render_chat_area` 含 Image 消息时不 panic | 正常渲染 |
| 2 | 图片消息高度正确计入总高度 | total 包含图片行高 |
| 3 | 图片消息在视口外不渲染 | 滚动后图片不可见时不调用 render_image_block |
| 4 | `render_image_block` Ready 状态 | 调用 `StatefulImage` widget 渲染 |
| 5 | `render_image_block` Loading 状态 | 渲染 `[加载中: url]` 占位文本 |
| 6 | `render_image_block` Error 状态 | 渲染错误提示文本 |
| 7 | 文本消息渲染不受影响 | 原有 render_block 路径不变 |

#### 🟢 绿 - 实现

- `crates/visp-cli/src/ui.rs`：
  - 新增 `LayoutEntry { msg_idx, y_offset, height, is_image }` 结构
  - `render_chat_area` 重构为两阶段：阶段1 `compute_layout`（只读 AppState），阶段2 遍历布局表分流渲染
  - 新增 `render_image_block(f, area, image_cache, msg, y_offset)` 函数：
    - `Ready` -> `StatefulImage::default().resize(Resize::Fit)` 渲染
    - `Loading` -> 占位文本 `[加载中: path]`
    - `Error` -> 错误文本 `[图片加载失败: path (reason)]`

#### 🧪 测试 -> 🔍 类型检查

```bash
cargo test -p visp-cli -- ui::tests
cargo check -p visp-cli
```

#### 📦 提交

`feat(cli): two-phase render_chat_area with image block rendering`

---

## 步骤 5：主循环集成 + 输入处理

### 5a：`AppState` 集成 `ImageCache` + `image_ready` channel + 主循环监听

#### 🔴 红 - 测试

| # | 测试用例 | 简明描述 |
|---|---------|---------|
| 1 | `AppState::new` 后 `image_cache` 已初始化 | 不 panic，picker 可用 |
| 2 | `image_ready_rx` 收到信号后 `needs_render = true` | 主循环 select 分支生效 |
| 3 | 非图片消息的 `ensure_all_caches` 传 `image_metrics` 正确 | image_metrics 构造自 app.image_cache |

#### 🟢 绿 - 实现

- `crates/visp-cli/src/app.rs`：
  - `AppState` 新增 `image_cache: ImageCache` 字段
  - `AppState` 新增 `image_ready_rx: mpsc::UnboundedReceiver<()>` 字段
  - `AppState::new` 中创建 `ImageCache` 和 `image_ready` channel，tx 传入 ImageCache
- `crates/visp-cli/src/event.rs`：
  - `run` 函数主循环 `tokio::select!` 新增 `image_ready_rx.recv()` 分支，设置 `needs_render = true`
  - `ensure_all_caches` 调用时构造 `ImageMetrics`（从 `app.image_cache` 获取 `font_size` 和缓存引用）

#### 🧪 测试 -> 🔍 类型检查

```bash
cargo check -p visp-cli
cargo test -p visp-cli
```

#### 📦 提交

`feat(cli): integrate ImageCache into AppState and main event loop`

---

### 5b：用户 Enter 输入处理集成 `parse_image_refs`

#### 🔴 红 - 测试

| # | 测试用例 | 简明描述 |
|---|---------|---------|
| 1 | 输入 `@path.png` 按 Enter | 发送的文本含 `<image: abs_path>` 标记 |
| 2 | 输入 `@nonexistent.png` 按 Enter | 发送的文本原样含 `@nonexistent.png` |
| 3 | 输入纯文本按 Enter | 行为不变 |
| 4 | 输入 `@https://...` 按 Enter | 发送的文本含 `<image: url>` 标记 |

#### 🟢 绿 - 实现

- `crates/visp-cli/src/event.rs`：
  - `handle_key_event` 中 Enter 处理分支（非 `/` 命令路径）：在 `chat_handle.send_input(&text)` 之前调用 `parse_image_refs(&text, &app.project_path)`，用处理后的文本替换原文本
  - `app.add_message` 也使用处理后的文本（这样对话区显示拆分后的 Image 块）

#### 🧪 测试 -> 🔍 类型检查

```bash
cargo check -p visp-cli
```

#### 📦 提交

`feat(cli): integrate @path image parsing into Enter key handler`

---

## 步骤 6：工具层改造

### 6a：`read_file` 图片格式检测

#### 🔴 红 - 测试

| # | 测试用例 | 简明描述 |
|---|---------|---------|
| 1 | `read_file` 读取 `.png` 文件 | 返回 `<image: /abs/path>` 标记文本 |
| 2 | `read_file` 读取 `.jpg` 文件 | 返回 `<image: /abs/path>` 标记文本 |
| 3 | `read_file` 读取 `.webp` 文件 | 返回 `<image: /abs/path>` 标记文本 |
| 4 | `read_file` 读取 `.txt` 文件 | 行为不变，返回文件内容 |
| 5 | `read_file` 读取不存在的 `.png` 文件 | 返回原有 "Failed to read file metadata" 错误 |
| 6 | `read_file` 读取 `.png` 时不读取文件内容 | 不触发 binary 检测 |

#### 🟢 绿 - 实现

- `crates/visp-tools/src/file.rs`：
  - `read_single_file` 中，在 binary 检测之前增加图片扩展名检测分支
  - 支持的扩展名：`.png`、`.jpg`、`.jpeg`、`.gif`、`.webp`、`.bmp`、`.ico`
  - 匹配则直接返回 `Ok(format!("<image: {}>", path.display()))`
  - `validate_path` 仍正常调用（路径安全检查不变）

#### 🧪 测试 -> 🔍 类型检查

```bash
cargo test -p visp-tools -- file
cargo check -p visp-tools
```

#### 📦 提交

`feat(tools): read_file returns image marker for image format files`

---

### 6b：MCP Image content 改造

#### 🔴 红 - 测试

| # | 测试用例 | 简明描述 |
|---|---------|---------|
| 1 | MCP `RawContent::Image` 含本地路径 | 转为 `<image: path>` 标记 |
| 2 | MCP `RawContent::Image` 纯二进制无路径 | 保留 `[Image: (N bytes)]` 占位文本 |
| 3 | 回归：MCP `RawContent::Text` | 行为不变 |

#### 🟢 绿 - 实现

- `crates/visp-mcp/src/client.rs`（约 `:242` 处）：
  - `RawContent::Image` 处理：若 data 中含可解析的本地文件路径，转为 `<image: path>` 标记
  - 纯二进制数据保留原有 `[Image: … (N bytes)]` 占位文本

#### 🧪 测试 -> 🔍 类型检查

```bash
cargo test -p visp-mcp
cargo check -p visp-mcp
```

#### 📦 提交

`feat(mcp): convert image content with local path to image marker`

---

## 步骤 7：端到端集成验证

### 7a：全量编译 + 集成测试

#### 🟢 绿 - 实现

- 确认所有模块编译通过
- 运行全量测试确认无回归
- 手动验证场景（如有测试图片资源）：
  - 用户输入 `@path/to/image.png` -> 对话区显示图片
  - Agent `read_file` 读取图片 -> 工具结果区显示图片
  - 终端不支持图形协议 -> 降级为 Halfblocks

#### 🧪 测试 -> 🔍 类型检查

```bash
cargo test --workspace
cargo check --workspace
```

#### 📦 提交

`test: end-to-end integration verification for CLI image display`

---

## Wave 并行策略

### Wave 1：基础类型与解析（1 个任务，串行）

任务 A: 1a -> 1b

`LineType::Image` 和 `split_image_markers` 是后续所有步骤的基础，必须先行。

### Wave 2：ImageCache（1 个任务，串行）

任务 A: 2a -> 2b

依赖 Wave 1 的 `ImageEntry` 等类型定义。2a（本地图片）和 2b（网络图片）有内部依赖，串行执行。

### Wave 3：ChatLine 拆分 + 渲染层（2 个并行任务）

任务 A: 3a -> 3b -> 4a

任务 B: 6a -> 6b

- 任务 A 依赖 Wave 1 + Wave 2（`split_image_markers` + `ImageCache`）
- 任务 B（工具层改造）仅依赖 Wave 1 的标记格式定义，与 CLI 渲染改动无关，可并行

### Wave 4：主循环集成（1 个任务，串行）

任务 A: 4b -> 5a -> 5b

依赖 Wave 3 的 `MessageCache` 扩展和 `render_chat_area` 改造。

### Wave 5：集成验证（1 个任务，串行）

任务 A: 7a

依赖所有前置 Wave 完成。

---

## 依赖关系总览

```
Wave 1: [1a: LineType::Image + split_image_markers]
          |
          +-> [1b: @path 输入解析]
                |
Wave 2:     +-> [2a: ImageCache 本地加载]
                  |
                  +-> [2b: ImageCache 网络下载]
                        |
Wave 3:     +-----------+--------------------------+
            |                                      |
            v                                      v
  [3a: render_pending 集成]               [6a: read_file 图片检测]
            |                                      |
            v                                      v
  [3b: 流式截断]                         [6b: MCP Image 改造]
            |
            v
  [4a: MessageCache 扩展]
            |
Wave 4:     v
  [4b: render_chat_area 两阶段]
            |
            v
  [5a: AppState + 主循环集成]
            |
            v
  [5b: Enter 输入处理]
            |
Wave 5:     v
  [7a: 集成验证]
```

---

## 测试覆盖汇总

| Wave | 并行数 | 模块/包 | 步骤 | 测试用例数 |
|------|--------|---------|------|-----------|
| 1 | 1 | visp-cli | 1a, 1b | 8 + 9 = 17 |
| 2 | 1 | visp-cli | 2a, 2b | 9 + 6 = 15 |
| 3 | 2 | visp-cli + visp-tools/mcp | 3a, 3b, 4a, 6a, 6b | 6 + 4 + 6 + 6 + 3 = 25 |
| 4 | 1 | visp-cli | 4b, 5a, 5b | 7 + 3 + 4 = 14 |
| 5 | 1 | 全 workspace | 7a | - |
| **合计** | | | | **71** |

---

## 备注

1. **`ratatui-image` 版本兼容**：需确认 `ratatui-image` 与现有 `ratatui = "0.30"` 兼容，可能需要锁定特定版本
2. **`Picker::from_query_stdio()` 在非 TTY 环境中的行为**：CI 环境可能无 TTY，测试中需 mock 或使用 Halfblocks 降级
3. **`StatefulProtocol` 的 `Clone`/`Send` 约束**：需确认 `ImageCache` 在 `AppState` 中的线程安全性
4. **图片测试资源**：测试需要小型 PNG/JPEG 文件，可在 `tests/fixtures/` 下放置或运行时生成
5. **`reqwest` 在 `visp-cli` 中的异步运行时**：CLI 已有 `tokio` multi-thread runtime，`reqwest` 异步下载可直接使用
6. **已知限制**：图片缓存无 LRU 淘汰；网络图片不持久化；MCP 纯二进制图片不处理（见设计文档 §7）
