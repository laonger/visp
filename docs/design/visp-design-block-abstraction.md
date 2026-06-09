# 对话区渲染统一 Block 抽象设计（v3 终版）

## 动机

当前 `render_chat_area` 的渲染逻辑散落在多个 if-else 分支中。每种消息类型有独立的 inset、底色、阴影、分隔逻辑，代码重复且难以扩展。需要统一抽象消除分支，**但不加运行时开销**。

## 设计原则

- **零堆分配**：不引入 trait object / `Box<dyn>`
- **编译期确定**：根据消息类型直接匹配到 `BlockStyle` 常量，无虚函数
- **Copy 传递**：`BlockStyle` 纯数据，栈上传递，开销为 0

## BlockStyle（布局配置）

```rust
#[derive(Copy, Clone)]
struct BlockStyle {
    inset: u16,              // 内容四周缩进
    bg_fill: Option<Color>,  // 缩进区域底色；为 None 时 bottom_pad 用分隔线样式
    shadow: bool,            // 是否绘制右侧 + 底部 drop shadow
    bottom_pad: u16,         // 内容下方行数；bg_fill 非空时填底色，否则画分隔线
}

impl BlockStyle {
    fn total_height(self, line_count: u16) -> u16 {
        1 + self.inset + line_count + self.bottom_pad
    }
}
```

## 四种消息类型的实例

| | USER | ASSISTANT | TOOL | STREAMING |
|------|------|------|------|------|
| `inset` | 0 | 2 | 0 | 2 |
| `bg_fill` | None | Some(0x00222A3E) | None | Some(0x00222A3E) |
| `shadow` | true | true | true | true |
| `bottom_pad` | 2 | 2 | 2 | 2 |
| `total_h` | line+3 | line+5 | line+3 | line+5 |

STREAMING 共享 ASSISTANT 同款配置。

## 统一渲染流程

一个函数 `render_block()`，内部按固定顺序执行。**所有消息类型走同一套渲染逻辑**，差异仅由 `BlockStyle` 数据驱动：

```
render_block(f, area, style, lines, line_count, rel_y):
  1. 计算实际可见高度（viewport 裁剪）
  2. style.bg_fill 不为 None → 填充底色区域
     (top_pad: 1+inset 行, content: line_count 行, bottom: bottom_pad 行)
  3. 渲染内容 Paragraph
     (位置按 inset 缩进，宽度 = area.width - 1 - inset*2)
  4. style.bg_fill 为 None → 用分隔线样式填充 bottom_pad 行
  5. style.shadow → 右侧阴影列（内容行）+ 底部阴影行（第一行 bottom_pad）
```

## viewport_intersect 工具

```rust
fn viewport_intersect(
    y: u16, h: u16, scroll: u16, visible: u16, area_bottom: u16
) -> Option<(u16, u16)> {
    if y + h <= scroll || y >= scroll + visible { return None; }
    let rel_y = y.saturating_sub(scroll);
    let max_h = h.min(area_bottom.saturating_sub(rel_y));
    if max_h == 0 { None } else { Some((rel_y, max_h)) }
}
```

## render_chat_area 最终形态

```
let width = area.width.saturating_sub(1);

// 1. 更新所有消息缓存（HashMap 查找，惰性渲染）
ensure_all_caches(app, width);

// 2. 计算总高度 + 滚动
let total = calc_total_height(app);
let scroll = ...;

// 3. 统一渲染循环
let mut y = 0;
for msg in &app.messages {
    let style = match msg.line_type {
        User => USER_STYLE,
        Assistant => ASSISTANT_STYLE,
        _     => TOOL_STYLE,
    };
    let cache = find_cache(app, msg.id).unwrap();
    let h = style.total_height(cache.line_count);
    if let Some((rel_y, _)) = viewport_intersect(y, h, scroll, visible, area.bottom()) {
        render_block(f, area, style, &cache.lines, cache.line_count, rel_y);
    }
    y += h;
}

// 4. 流式文本（复用 ASSISTANT_STYLE，实时构建 lines）
if !app.streaming_text.is_empty() {
    let stream_lines = build_stream_lines(app, width);
    let h = ASSISTANT_STYLE.total_height(stream_lines.len() as u16);
    if let Some((rel_y, _)) = viewport_intersect(y, h, scroll, visible, area.bottom()) {
        render_block(f, area, ASSISTANT_STYLE, &stream_lines, stream_lines.len() as u16, rel_y);
    }
}
```

## AppState 清理

| 操作 | 说明 |
|------|------|
| 移除 `streaming_rendered_len` | 流式文本直接实时构建，不做增量缓存 |
| 移除 `streaming_rendered_lines` | 同上 |
| 保留 `message_caches` | 不变 |
| 保留 `cache_width` | 不变 |
| 保留所有滚动/冷却/脏标记字段 | 事件处理完全不变 |

## 影响范围

| 文件 | 改动 |
|------|------|
| ui.rs | 新增 BlockStyle、viewport_intersect、render_block、ensure_all_caches；重写 render_chat_area；删除 build_text_stack |
| app.rs | 删除 streaming_rendered_len/lines 字段及初始化/重置代码 |
| event.rs | 无改动 |

## 不变的部分

- `MessageCache` 和 `from_message` 不变
- 消息 id/version/call_id 机制不变
- 滚动冷却、脏标记、gRPC 节流不变
- 阴影绘制算法不变（右列 + 底行）
