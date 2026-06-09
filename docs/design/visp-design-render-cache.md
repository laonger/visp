# 对话区渲染缓存设计

## 问题

当前 `build_text_stack` 的缓存键是 `(消息数, streaming_len, 宽度)`，AI 流式输出时每帧 streaming_len 变化，缓存全部失效，每帧重建全部 500+ 行。后续加入 Markdown 解析和代码高亮后，单行渲染成本将从微秒级跃升到毫秒级，全量重建不可接受。

## 方案：三层渲染 + 三项性能优化

```
Layer 1 — 全量数据（不渲染）
  messages: Vec<ChatLine>   → 原始文本，永久保留，零渲染成本
  streaming_text: String    → 流式接收中的文本

Layer 2 — 按消息粒度渲染缓存
  message_caches: HashMap 查找 → 每条消息按 id+version 匹配，永久缓存
  streaming_rendered_lines  → 流式文本增量 wrap，只渲染新增部分

Layer 3 — 视窗
  从组装的完整行数组中按 scroll 位置切片  → O(1)，纯索引
```

核心思想：渲染成本最高的部分（文本wrap、配色、未来Markdown/语法高亮）按消息粒度缓存，**一条消息只渲染一次**。流式文本采用增量 wrap——只计算新增字符的换行，不重算已输出的部分。配合 gRPC 渲染节流（30ms），流式阶段从 ~50fps 降到 ~33fps。

## 消息身份与版本机制

### 问题

多 agent 和工具调用场景下，中间某条消息的内容可能被修改或替换。简单的按索引用 `Vec` 长度对齐无法检测到"同索引下内容变了"，也无法处理插入/删除引起的索引起伏。

### ChatLine 字段

| 字段 | 类型 | 说明 |
|------|------|------|
| `id` | u64 | 全局单调递增，唯一标识一条消息 |
| `version` | u64 | 该消息的修改计数器，每次内容变化 +1 |

全局计数器 `next_message_id: u64` 维护在 AppState 中，每次创建新消息时分配。

### MessageCache 字段

| 字段 | 类型 | 说明 |
|------|------|------|
| `msg_id` | u64 | 对应 ChatLine 的 id |
| `msg_version` | u64 | 渲染时 ChatLine 的 version |
| `width` | u16 | 渲染时的终端宽度 |
| `lines` | `Vec<Line<'static>>` | 该消息渲染后的所有行（含 wrap、颜色、填充） |
| `line_count` | u16 | 行数，用于滚动索引计算 |

### 缓存匹配逻辑

```
组装时构建 msg_id → cache_index 的 HashMap (O(N))
  对每条消息:
    1. HashMap 查找 msg_id (O(1))
    2. 如果找到 且 cache.version == msg.version 且 cache.width == 当前宽度:
       → 命中，直接拼接
    3. 否则 → 重新渲染该条消息，更新 cache，拼接
    4. 未找到 → 新消息，渲染并追加 cache
  最后清理 messages 中不存在的残留 cache 条目
```

此方案保证：
- **新增消息**：新 id，自动触发渲染
- **修改消息**：同 id，version 不匹配，触发**仅该条**重渲
- **删除消息**：id 不在 messages 中，不参与组装，残留 cache 惰性清理
- **插入消息**：新 id，触发渲染，不影响前后消息的 cache
- **消息不变**：id + version + width 三重匹配，直接复用

## 流式文本增量渲染

### 设计原理

`streaming_text` 只会追加（不会修改已有内容），因此可以利用这一特性做增量 wrap：只对新增的字符做换行计算，已输出的部分不变。

### AppState 状态

| 字段 | 说明 |
|------|------|
| `streaming_rendered_len` | 已渲染的 streaming_text 字节数 |
| `streaming_rendered_lines` | 已渲染的行缓存 |

### 增量流程

```
streaming_text 变化时:
  1. 找到重新渲染起始点:
     → 上一个已渲染部分的最后一个 \n 之后
     (需要从上一个完整行开始，因为追加的文本可能改变当前行的换行)
  2. 截断缓存中属于重新渲染起点的旧行
  3. 只对重新渲染起始点到末尾做 wrap_text
  4. 追加新行到 streaming_rendered_lines
  5. 更新 streaming_rendered_len
```

### 重置时机

| 操作 | 重置 |
|------|------|
| `flush_streaming` | 清空 streaming_rendered_len 和 streaming_rendered_lines |
| `clear_messages` | 同上 |

## 三项性能优化

### 优化 1：gRPC 流式渲染节流

| 字段/方法 | 说明 |
|-----------|------|
| `last_stream_render: Option<Instant>` | 上次流式渲染时间 |
| `try_begin_stream_render() -> bool` | 30ms 冷却，未到期返回 false |

在事件循环中：`generating == true` 时通过 `try_begin_stream_render` 控制渲染频率，冷却期内跳过 `terminal.draw`。

### 优化 2：流式文本增量 wrap（如上）

### 优化 3：cache 查找 O(N²) → O(N)

每帧开始时构建 `HashMap<msg_id, cache_index>`，后续每条消息 O(1) 查找，替代原先的 `iter().any()` + `iter().find()` 双重线性扫描。

## 数据结构

### AppState 全部消息操作封装

**禁止外部直接操作 `messages` 或 `message_caches`**：

| 方法 | 行为 | cache 维护 |
|------|------|------------|
| `add_message(line_type, content)` | 分配新 id，version=0，追加到 messages | 下一帧渲染时惰性创建 cache |
| `update_message(id, content)` | 找到对应消息，更新 content，version+1 | 版本号不匹配触发下帧重渲该条 |
| `append_streaming(delta)` | 追加到 streaming_text | 无影响 |
| `flush_streaming()` | streaming_text → add_message | 重置增量渲染状态 |
| `clear_messages()` | 清空 messages 和 message_caches | 重置增量渲染状态 |

### AppState 字段总览

| 字段 | 说明 |
|------|------|
| `messages` | (已有) 消息列表 |
| `message_caches` | (新增) 消息渲染缓存列表 |
| `streaming_text` | (已有) 流式接收文本 |
| `streaming_rendered_len` | (新增) 已渲染字节数，用于增量 wrap |
| `streaming_rendered_lines` | (新增) 已渲染的流式文本行缓存 |
| `next_message_id` | (新增) 全局消息 id 分配器 |
| `cache_width` | (新增) 上次渲染宽度 |
| `last_stream_render` | (新增) 流式渲染节流时间戳 |
| `scroll_state` | (保留) 滚动位置 |
| `scroll_following` | (保留) 自动跟底标记 |
| `needs_render` | (保留) 脏标记 |
| `last_scroll_time` | (保留) 滚动冷却时间戳 |

## 渲染流程

### 每帧流程

```
事件循环:
  1. 事件到达 → handle_key_event / handle_grpc_message
     ├─ 滚动事件: try_begin_scroll(30ms冷却) → 修改 scroll_state
     ├─ 键盘事件: 修改状态
     └─ gRPC消息: append_streaming → needs_render = true

  2. 渲染门控:
     ├─ !needs_render → 跳过
     ├─ generating && !try_begin_stream_render(30ms) → 跳过
     └─ 否则 → terminal.draw(render)

render → render_chat_area → build_text_stack:
  1. HashMap 构建 msg_id → cache_index (O(N))
  2. 对每条消息: 命中则拼接，未命中则渲染+缓存
  3. 清理残留 cache
  4. 流式文本增量 wrap:
     - 只 wrap 新增部分
     - 拼接已缓存行
  5. 返回完整行数组
  6. 按 scroll_state 切片 → Paragraph → 渲染
```

### 滚动索引

继续用 `scroll_state.offset().y` 作为从顶部计的行索引。组装后的行数组中，每行按 messages 顺序排列，索引语义不变。

## 缓存维护

### 消息增加时（add_message）

- 分配新 id（next_message_id++），创建 ChatLine（id, version=0, line_type, content）
- MessageCache 在下帧 build_text_stack 中惰性创建

### 消息修改时（update_message）

- 找到 id 匹配的 ChatLine，更新 content，version++
- 对应的 MessageCache 在下帧检测到 version 不匹配，自动重新渲染
- **无需主动删除旧 cache**

### 消息删除时（clear_messages）

- 清空 messages 和 message_caches
- 重置 `streaming_rendered_len` 和 `streaming_rendered_lines`

### 流式输出时（append_streaming）

- `streaming_text` 追加 delta
- 下帧 build_text_stack 中增量 wrap（只处理新增字符）

### 流式结束（flush_streaming）

- streaming_text → add_message（走消息增加流程）
- 重置 `streaming_rendered_len` 和 `streaming_rendered_lines`

### 终端 resize

- 新宽度 != cache_width → width 不匹配导致所有 cache 失效，下帧全量重建

## 单条消息渲染（未来扩展点）

当前单条消息渲染逻辑与 `build_text_stack` 中对每条消息的处理相同：
- 按 LineType 确定配色
- wrap_text 按宽度换行
- pad_to_width 填充
- ToolResult 截断

接口预留：
- Markdown 解析在此层介入（输入：原始文本，输出：styled Line 序列）
- 代码高亮在此层介入（输入：代码块内容 + 语言，输出：语法高亮的 Line 序列）

## 边界情况

| 场景 | 处理 |
|------|------|
| 消息追加过快（如批量输出） | 逐条渲染，拼装 O(N) 拷贝可接受 |
| 消息中间插入 | 新 id，仅渲染该条，不影响前后 |
| 消息中间修改（version++） | 仅该条重渲，其他 cache 不受影响 |
| 消息删除 | id 不在 messages 中，跳过，残留 cache 惰性清理 |
| 消息数极大（>1000） | 1000 行 Line 拷贝 < 1ms，可接受 |
| 宽度频繁变化 | 每次 resize 全量重建，但 resize 事件罕见 |
| 消息被截断显示（ToolResult >5 行） | 截断逻辑在消息渲染层处理，cache 存截断后的结果 |
| 残留 cache 积累（消息频繁增删） | 组装时清理不在 messages 中的 cache 条目 |
| 流式文本增量 wrap 换行边界 | 从上一个已渲染部分的最后 \n 开始重新 wrap，保证换行正确 |
| gRPC 消息洪水 | `try_begin_stream_render` 30ms 节流，冷却期内跳过渲染 |

## 影响范围

| 文件 | 改动 |
|------|------|
| app.rs | ChatLine 新增 id/version 字段；新增 MessageCache 结构体；AppState 新增 message_caches/streaming_rendered_len/streaming_rendered_lines/next_message_id/cache_width/last_stream_render 等字段；新增 add_message/update_message/clear_messages/try_begin_stream_render 方法 |
| ui.rs | 重写 build_text_stack：HashMap 查找 + 增量 wrap + 组装流程；render_chat_area 适配 |
| event.rs | 事件循环增加 gRPC 渲染节流；`/clear` 改用 clear_messages() |

## 与当前实现的兼容性

- 冷却机制（30ms try_begin_scroll）不变
- 脏标记（needs_render）不变
- ScrollViewState 滚动管理不变
- 视窗切片逻辑不变
- 所有现有测试已适配

## 不变的部分

- 输入区渲染不动
- 状态栏渲染不动
- 确认栏渲染不动
- tui-scrollview crate 依赖保留（仅用 ScrollViewState）
