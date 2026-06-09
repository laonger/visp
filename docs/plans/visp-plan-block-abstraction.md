# visp 工作计划：BlockStyle 统一渲染重构

## 概述

基于 `visp-design-block-abstraction.md`，将 `render_chat_area` 中的分散 if-else 渲染逻辑替换为统一的 `BlockStyle` + `render_block()` 抽象，删除 `build_text_stack`。

改动范围：ui.rs（核心）、app.rs（清理）。event.rs 不动。

## 步骤 1：BlockStyle 结构体 + viewport_intersect + render_block

### 1a：定义 BlockStyle 和四种实例常量

#### 🔴 红 — 测试
无新测试（纯结构体定义，编译器验证）。

#### 🟢 绿 — 实现
- 定义 `BlockStyle` Copy struct（inset, bg_fill, shadow, bottom_pad）
- 实现 `total_height(line_count) -> u16`
- 定义四个 `const` 实例：USER_STYLE, ASSISTANT_STYLE, TOOL_STYLE, STREAMING_STYLE（同 ASSISTANT）

#### 🧪 验证
```bash
cargo test -p visp-cli && cargo clippy -p visp-cli -- -D warnings
```

#### 📦 提交
```
refactor(visp-cli): add BlockStyle struct with four type presets
```

### 1b：实现 viewport_intersect 和 render_block

#### 🔴 红 — 测试
无新测试（渲染函数，手动验证）。

#### 🟢 绿 — 实现
- `viewport_intersect(y, h, scroll, visible, area_bottom) -> Option<(rel_y, clipped_h)>`
- `render_block(f, area, style, lines, line_count, rel_y)` — 按 style 数据执行：bg_fill → content Paragraph → bottom_pad 分隔 → shadow

#### 🧪 验证
```bash
cargo test -p visp-cli && cargo clippy -p visp-cli -- -D warnings
```

#### 📦 提交
```
refactor(visp-cli): add viewport_intersect and unified render_block function
```

---

## 步骤 2：重写 render_chat_area

### 2a：替换为统一循环

#### 🔴 红 — 测试
无新测试。现有测试用于回归。

#### 🟢 绿 — 实现
- 实现 `ensure_all_caches(app, width)` — 从旧 build_text_stack 提取缓存更新逻辑
- 重写 `render_chat_area`：
  - 总高度计算使用 `style.total_height(cache.line_count)`
  - 统一 for 循环：match line_type → 获取 style → render_block
  - 流式文本作为最后一个"虚拟 block"处理

#### 🧪 验证
```bash
cargo test -p visp-cli && cargo clippy -p visp-cli -- -D warnings && cargo fmt -p visp-cli -- --check
```

#### 📦 提交
```
refactor(visp-cli): rewrite render_chat_area with unified BlockStyle loop
```

### 2b：删除 build_text_stack 旧代码

#### 🟢 绿 — 实现
- 删除 `build_text_stack` 函数
- 删除 `build_stream_lines` 如果已内联到 render_chat_area
- 清理未使用的 import

#### 📦 提交
```
refactor(visp-cli): remove build_text_stack, replaced by ensure_all_caches
```

---

## 步骤 3：清理 app.rs

### 3a：删除 streaming_rendered_len / streaming_rendered_lines

#### 🟢 绿 — 实现
- 从 `AppState` 删除 `streaming_rendered_len` 和 `streaming_rendered_lines` 字段
- 从 `new()` 删除初始化
- 从 `flush_streaming()` 和 `clear_messages()` 删除重置代码
- 更新相关测试

#### 🧪 验证
```bash
cargo test -p visp-cli && cargo clippy -p visp-cli -- -D warnings && cargo fmt -p visp-cli -- --check
```

#### 📦 提交
```
refactor(visp-cli): remove streaming_rendered_len/lines from AppState
```

---

## 步骤 4：全量回归验证

```bash
cargo test -p visp-cli && cargo clippy -p visp-cli -- -D warnings && cargo fmt -p visp-cli -- --check
```

#### 📦 提交
```
test(visp-cli): verify BlockStyle refactor passes all tests
```

---

## Wave 并行策略

全部串行（每步依赖上一步输出）：

```
1a → 1b → 2a → 2b → 3a → 4
```

## 测试覆盖

| 步骤 | 新增 | 修改 | 删除 |
|------|------|------|------|
| 1a | 0 | 0 | 0 |
| 1b | 0 | 0 | 0 |
| 2a | 0 | 0 | 0 |
| 2b | 0 | 0 | 0 |
| 3a | 0 | 2 | 0 |
| 4 | 0 | 0 | 0 |

主力验证靠现有 18 个回归测试。BlockStyle 是纯数据 + 函数重组，不影响运行时语义。

## 备注

- 阴影绘制代码从当前实现**直接搬运**，不改变算法
- `ensure_all_caches` 从当前 `build_text_stack` 的步骤 1-3 提取（HashMap 构建 + 惰性渲染 + 清理残留）
- 流式文本实时构建 lines 时，颜色硬编码为 `Color::White`（只有 Assistant 会流式输出）
- `render_chat_area` 删除后，ui.rs 的 `render` 函数中调用不变
