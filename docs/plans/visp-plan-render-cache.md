# visp 工作计划：对话区渲染缓存

## 概述

基于 `visp-design-render-cache.md` 设计文档，实现按消息粒度的渲染缓存系统，为后续 Markdown 解析和代码高亮预留扩展点。

改动范围：app.rs（核心）、ui.rs（组装流程）。event.rs 仅需小幅适配（1 行改动）。

## 步骤 1：ChatLine 身份系统 + AppState 方法封装

### 1a：ChatLine 增加 id/version 字段

#### 🔴 红 — 测试
| # | 测试用例 | 描述 |
|---|---------|------|
| 1.1 | `test_chatline_id_assign` | 通过 `add_message` 创建的消息，id 按序递增 |
| 1.2 | `test_chatline_version_initial` | 新消息 version = 0 |

#### 🟢 绿 — 实现
- ChatLine 新增 `id: u64` 和 `version: u64` 字段
- AppState 新增 `next_message_id: u64` 字段（new() 中初始化为 0）
- `add_message(line_type, content)` 内部分配 id（next_message_id++），设置 version=0

#### 🧪 测试 → 🔍 类型检查
```bash
cargo test -p visp-cli && cargo clippy -p visp-cli -- -D warnings
```

#### ♻️ 重构
- 无

#### 📦 提交
```
feat(visp-cli): add id/version to ChatLine, auto-assign in add_message
```

### 1b：新增 update_message 和 clear_messages 方法

#### 🔴 红 — 测试
| # | 测试用例 | 描述 |
|---|---------|------|
| 1.3 | `test_update_message_increments_version` | update_message 找到 id 匹配的消息，version+1 |
| 1.4 | `test_update_message_content_changed` | 更新后 content 变为新值 |
| 1.5 | `test_update_message_id_not_found` | 不存在 id 的消息，update 不 panic，不改变 messages |
| 1.6 | `test_clear_messages` | clear_messages 清空 messages |

#### 🟢 绿 — 实现
- `AppState::update_message(id, content)` — 遍历 messages 找 id，更新 content，version+1
- `AppState::clear_messages()` — 清空 messages 和 message_caches

#### 🧪 测试 → 🔍 类型检查
```bash
cargo test -p visp-cli && cargo clippy -p visp-cli -- -D warnings
```

#### ♻️ 重构
- 无

#### 📦 提交
```
feat(visp-cli): add update_message and clear_messages methods
```

---

## 步骤 2：MessageCache 结构体 + 单条消息渲染

### 2a：定义 MessageCache 结构体

#### 🔴 红 — 测试
| # | 测试用例 | 描述 |
|---|---------|------|
| 2.1 | `test_message_cache_creation` | 从 ChatLine 渲染生成 MessageCache，line_count > 0 |
| 2.2 | `test_message_cache_stores_id_version` | cache 中 msg_id 和 msg_version 与 ChatLine 一致 |

#### 🟢 绿 — 实现
- 定义 `MessageCache` 结构体（字段：msg_id, msg_version, width, lines, line_count）
- 实现 `MessageCache::from_message(msg: &ChatLine, width: u16)` 构造方法
- 内部复用当前 `build_text_stack` 中单条消息的渲染逻辑（wrap_text、pad_to_width、配色）

#### 🧪 测试 → 🔍 类型检查
```bash
cargo test -p visp-cli && cargo clippy -p visp-cli -- -D warnings
```

#### ♻️ 重构
- 将单条消息渲染逻辑从 `build_text_stack` 中提取到 `MessageCache::from_message`

#### 📦 提交
```
feat(visp-cli): add MessageCache struct with per-message rendering
```

### 2b：MessageCache 按消息类型差异渲染

#### 🔴 红 — 测试
| # | 测试用例 | 描述 |
|---|---------|------|
| 2.3 | `test_cache_user_message_style` | User 类型消息渲染后行带有正确配色 |
| 2.4 | `test_cache_tool_result_truncation` | ToolResult >5 行消息只存 4+1 行截断 |

#### 🟢 绿 — 实现
- `from_message` 中按 LineType 分支：User 加空行边框，ToolResult 截断，其他正常渲染
- `MessageCache::matches(msg: &ChatLine, width: u16) -> bool` — 三重匹配辅助方法

#### 🧪 测试 → 🔍 类型检查
```bash
cargo test -p visp-cli && cargo clippy -p visp-cli -- -D warnings
```

#### 📦 提交
```
feat(visp-cli): add type-aware rendering and cache matching to MessageCache
```

---

## 步骤 3：AppState 缓存管理

### 3a：add_message 同步维护缓存

#### 🔴 红 — 测试
| # | 测试用例 | 描述 |
|---|---------|------|
| 3.1 | `test_add_message_adds_cache` | add_message 后 message_caches 包含一条对应 cache |

#### 🟢 绿 — 实现
- `add_message` 方法内调用 `MessageCache::from_message`，追加到 `message_caches`
- AppState 新增 `message_caches: Vec<MessageCache>` 字段

#### 🧪 测试 → 🔍 类型检查
```bash
cargo test -p visp-cli && cargo clippy -p visp-cli -- -D warnings
```

#### 📦 提交
```
feat(visp-cli): maintain message_caches in add_message
```

### 3b：流式分段缓存（frozen_cache）

#### 🔴 红 — 测试
| # | 测试用例 | 描述 |
|---|---------|------|
| 3.2 | `test_streaming_below_threshold` | streaming_text ≤ 200行，frozen_cache 为空 |
| 3.3 | `test_streaming_above_threshold_triggers_freeze` | streaming_text > 200行，frozen_cache 非空 |
| 3.4 | `test_frozen_cache_stable` | 冻结后 frozen_cache 行数不变，streaming_cache 只有尾巴 |

#### 🟢 绿 — 实现
- AppState 新增 `frozen_cache: Vec<Line<'static>>` 字段
- `append_streaming` 中检测 streaming_text 行数 > 200 时，冻结前段（总行数-50行），streaming_cache 只渲染后 50 行
- `flush_streaming` 合并 frozen_cache + streaming，走 add_message，清理两者

#### 🧪 测试 → 🔍 类型检查
```bash
cargo test -p visp-cli && cargo clippy -p visp-cli -- -D warnings
```

#### 📦 提交
```
feat(visp-cli): add frozen_cache for long streaming text
```

---

## 步骤 4：重写 build_text_stack 为组装流程

### 4a：核心组装逻辑

#### 🔴 红 — 测试
| # | 测试用例 | 描述 |
|---|---------|------|
| 4.1 | `test_assemble_from_caches` | 从多个 MessageCache 拼装成完整行数组 |
| 4.2 | `test_assemble_with_streaming` | 拼装包含 streaming_cache 的行数组 |
| 4.3 | `test_assemble_with_frozen` | 拼装包含 frozen_cache 的行数组 |

#### 🟢 绿 — 实现
- 重写 `build_text_stack`：
  - 遍历 messages，按 id+version+width 匹配 MessageCache
  - 未命中/version 变化 → 调 `MessageCache::from_message` 重新渲染
  - 命中 → 直接拼接缓存行
  - 拼接 frozen_cache + streaming_cache
  - 清理 messages 中不存在的残留 cache 条目
- 删除旧的 `CACHE_KEY` / `CACHE_TEXT` 线程局部缓存（被 MessageCache 替代）

#### 🧪 测试 → 🔍 类型检查
```bash
cargo test -p visp-cli && cargo clippy -p visp-cli -- -D warnings
```

#### 📦 提交
```
feat(visp-cli): rewrite build_text_stack to assemble from message_caches
```

### 4b：cache_width 变更处理

#### 🔴 红 — 测试
| # | 测试用例 | 描述 |
|---|---------|------|
| 4.4 | `test_width_change_rebuilds_cache` | width 变化后 MessageCache 重新渲染（width 不匹配） |

#### 🟢 绿 — 实现
- AppState 新增 `cache_width: u16` 字段
- `matches()` 方法中加入 width 比较
- render_chat_area 中更新 cache_width

#### 🧪 测试 → 🔍 类型检查
```bash
cargo test -p visp-cli && cargo clippy -p visp-cli -- -D warnings
```

#### 📦 提交
```
feat(visp-cli): invalidate message caches on terminal resize
```

---

## 步骤 5：适配 event.rs 及其他调用点

### 5a：/clear 改用 clear_messages

#### 🔴 红 — 测试
| # | 测试用例 | 描述 |
|---|---------|------|
| 5.1 | `test_clear_messages_via_command` | /clear 命令同时清空 messages 和 caches |

#### 🟢 绿 — 实现
- `event.rs` 中 `handle_command` 的 `/clear` 分支：`app.messages.clear()` → `app.clear_messages()`
- 所有现有 `add_message` 调用签名不变（id 内部自动分配）

#### 🧪 测试 → 🔍 类型检查
```bash
cargo test -p visp-cli && cargo clippy -p visp-cli -- -D warnings
```

#### ♻️ 重构
- 确认所有 `add_message` 调用者无需修改（内部分配 id，对外无感知）

#### 📦 提交
```
refactor(visp-cli): use clear_messages() for /clear command
```

---

## 步骤 6：全量验证

### 6a：回归测试 + 质量门

#### 🔴 红 — 测试
已在上面的步骤中覆盖，此步骤确认全部通过。

#### 🟢 绿 — 实现
- 无需新代码

#### 🧪 测试 → 🔍 类型检查 → 格式检查
```bash
cargo test -p visp-cli && cargo clippy -p visp-cli -- -D warnings && cargo fmt -p visp-cli -- --check
```

#### 📦 提交
```
test(visp-cli): verify all tests pass after render cache refactor
```

---

## Wave 并行策略

所有步骤有强依赖，**全部串行执行**：

```
步骤 1a → 1b → 2a → 2b → 3a → 3b → 4a → 4b → 5a → 6a
```

虽然没有并行机会，但每个步骤粒度小（单一 commit），TDD 循环后立即可验证，出错容易定位。

## 依赖关系总览

```
ChatLine 字段 (1a)
    ↓
AppState 方法 (1b)
    ↓
MessageCache 结构 (2a)
    ↓
样式渲染 (2b)
    ↓
缓存维护 (3a)  ←→  流式分段 (3b)（可调顺序，均依赖 2b）
    ↓
组装流程 (4a)  ←→  width 处理 (4b)（可调顺序，均依赖 3a+3b）
    ↓
event 适配 (5a)
    ↓
全量验证 (6a)
```

## 测试覆盖汇总

| 步骤 | 测试用例数 | 新增 | 修改 | 覆盖内容 |
|------|-----------|------|------|---------|
| 1a | 2 | 2 | 1 | ChatLine id/version |
| 1b | 3 | 3 | 0 | update/clear 方法 |
| 2a | 2 | 2 | 0 | MessageCache 创建和元数据 |
| 2b | 2 | 2 | 0 | 类型差异渲染、截断 |
| 3a | 1 | 1 | 0 | add_message 同步 cache |
| 3b | 3 | 3 | 0 | frozen_cache 阈值和稳定性 |
| 4a | 3 | 3 | 0 | 拼装、流式、冻结组合 |
| 4b | 1 | 1 | 0 | width 变化 |
| 5a | 1 | 1 | 0 | /clear |
| 6a | — | 0 | 0 | 回归 |
| **合计** | **19** | **18** | **1** | |

## 备注

- `ChatLine` 构造现在需要 id/version，所有直接 `ChatLine { ... }` 的地方改为走 `add_message`。当前只有 `app.rs` 内部使用，外部全部走 `add_message`。
- `flush_streaming` 中的 `ChatLine { ... }` 构造改为走 `add_message`。
- 流式分段阈值常量定义为 200（触发冻结）和 50（尾巴行数），hardcode 在代码中。
- build_text_stack 中 `CACHE_KEY`/`CACHE_TEXT` 线程局部缓存可直接删除，MessageCache 替代其职责。
- render_chat_area 逻辑保持不变（切片+渲染），build_text_stack 返回值语义不变（完整行数组）。
- 冷却机制（try_begin_scroll）、脏标记（needs_render）、滚动状态（scroll_state）完全不涉及。
