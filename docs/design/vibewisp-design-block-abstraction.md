# 对话区渲染统一 Block 抽象设计

## 动机

当前 `render_chat_area` 的渲染逻辑散落在多个 if-else 分支中。每种消息类型（User/Assistant/Tool/流式）有独立的 inset、底色、阴影、分隔符逻辑，代码重复且难以扩展。需要通过统一的 Block 抽象消除分支。

## BlockStyle（布局配置）

纯数据，不可变，定义 block 的视觉参数：

| 字段 | 类型 | 说明 |
|------|------|------|
| `inset` | u16 | 内容四周缩进字符数 |
| `bg_fill` | Option<Color> | 缩进区域底色，None 表示不填充 |
| `shadow` | bool | 是否绘制右侧+底部 drop shadow |
| `separator` | bool | 是否在 block 下方绘制分隔条（chat 底色） |
| `bottom_pad` | u16 | 内容下方留白行数（分隔符之上） |

`BlockStyle` 提供 `total_height(line_count: u16) -> u16`：

```
total_h = 1 (顶部空白) + inset + line_count + bottom_pad
          + if separator { 1 } else { 0 }
```

## BlockRenderer trait（渲染协议）

```rust
trait BlockRenderer {
    fn style() -> &BlockStyle;
    fn render_content(&self, f: &mut Frame, area: Rect, width: u16);
    fn line_count(&self) -> u16;
    fn ensure_cache(&self, app: &mut AppState, width: u16);
}
```

各方法职责：

| 方法 | 职责 |
|------|------|
| `style()` | 返回该类型 block 的布局配置 |
| `render_content()` | 在给定 area 内渲染消息文本（各类型自行决定样式和定位） |
| `line_count()` | 返回该消息的内容行数（用于高度计算和滚动） |
| `ensure_cache()` | 确保 MessageCache 存在且有效（惰性渲染），由默认实现调用 |

## 具体 Block 类型及配置

| | UserBlock | AssistantBlock | ToolBlock | StreamingBlock |
|------|------|------|------|------|
| `inset` | 0 | 2 | 0 | 2 |
| `bg_fill` | None | 0x00222A3E | None | 0x00222A3E |
| `shadow` | ✅ | ✅ | ✅ | ✅ |
| `separator` | ✅ | ❌ | ✅ | ❌ |
| `bottom_pad` | 2 | 2 | 2 | 2 |
| `total_h` | line+3 | line+5 | line+3 | line+5 |

### 各类型 content 渲染差异

- **UserBlock**: Paragraph 渲染，from_message 内置样式（青字蓝底）
- **AssistantBlock**: Paragraph 渲染，from_message 内置样式（白字蓝灰底）
- **ToolBlock**: Paragraph 渲染，from_message 内置样式（黄/灰字）
- **StreamingBlock**: 不从 MessageCache 取，实时构建 `Vec<Line>`，样式同 Assistant

### 统一渲染流程（应用于所有 block）

```
每个 block:
  1. ensure_cache(app, width)        // 惰性构建/更新 MessageCache
  2. total_h = style().total_height(line_count)
  3. visible? → viewport_intersect(y, total_h, scroll, viewport)
     → 不可见: y += total_h, continue
     → 可见:   rel_y = 计算
  4. style().bg_fill 填充底色区域
  5. render_content(f, content_rect, width)
  6. style().separator 画分隔线
  7. style().shadow 画右侧+底部阴影
  8. return total_h (调用侧: y += total_h)
```

### viewport_intersect 工具函数

```rust
/// 检查 block 是否与视窗相交，返回相对 y 和裁剪高度
fn viewport_intersect(
    y: u16, h: u16, scroll: u16, visible: u16, area_bottom: u16
) -> Option<(u16, u16)> {
    if y + h <= scroll || y >= scroll + visible { return None; }
    let rel_y = y.saturating_sub(scroll);
    let max_h = h.min(area_bottom.saturating_sub(rel_y));
    if max_h == 0 { None } else { Some((rel_y, max_h)) }
}
```

## 流式文本统一

`StreamingBlock` 实现 `BlockRenderer`，作为虚拟的"最后一条消息"加入渲染循环。`render_content` 内按 AssistantBlock 相同样式实时构建 lines。`line_count` 返回 `app.streaming_text.lines().count()`。

在 `render_chat_area` 中：

```rust
let blocks: Vec<Box<dyn BlockRenderer>> = app.messages.iter()
    .map(/* 创建对应 Block 类型 */)
    .collect();
if !app.streaming_text.is_empty() {
    blocks.push(Box::new(StreamingBlock));
}

for block in &blocks {
    block.ensure_cache(app, width);
    let h = block.total_height();
    if let Some((rel_y, actual_h)) = viewport_intersect(y, h, scroll, visible, area.bottom()) {
        let block_area = Rect::new(area.x, area.y + rel_y, area.width, actual_h);
        // bg_fill → render_content → separator → shadow
    }
    y += h;
}
```

## 删除 build_text_stack

当前 `build_text_stack` 仅作缓存更新副作用（返回值被丢弃）。其职责由 `BlockRenderer::ensure_cache` 替代：

- 遍历 messages，对每条消息查找/构建 HashMap 索引
- 未命中则调用 `MessageCache::from_message` 创建缓存
- 清理残留缓存

该逻辑移入 `render_chat_area` 的前置步骤，不依赖特定 widget。

## AppState 变更

| 操作 | 说明 |
|------|------|
| 移除 `streaming_rendered_len` | 流式文本改为走 StreamingBlock 统一渲染 |
| 移除 `streaming_rendered_lines` | 同上 |
| 保留 `message_caches` | 消息缓存不变 |
| 保留 `cache_width` | 宽度变化检测不变 |
| 保留所有滚动/冷却/脏标记字段 | 事件处理完全不变 |

## 影响范围

| 文件 | 改动 |
|------|------|
| ui.rs | 新增 BlockStyle、BlockRenderer trait、viewport_intersect；重写 render_chat_area 为统一循环；删除 build_text_stack |
| app.rs | 删除 streaming_rendered_len/streaming_rendered_lines 字段；清理相关初始化和重置代码 |
| event.rs | 无改动 |

## 不变的部分

- `MessageCache` 结构体和渲染逻辑不变
- `from_message` 不变
- 消息 id/version/call_id 机制不变
- 滚动冷却、脏标记、gRPC 节流不变
- 阴影绘制逻辑（右列+底行）移入统一流程，算法不变
