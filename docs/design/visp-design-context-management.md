# Context 管理设计总纲

## 问题

对话历史无 token 计数和裁剪。每次 LLM 调用发送全部历史，长对话必然超出 context window。

## 设计原则

### 1. 单条时间线

所有消息（System / User / Assistant / Tool）共享同一条时间线，存储在单个有序列表中。"分层"只是裁剪优先级的概念，不是存储方式。

### 2. 选择而非删除

裁剪只在 prompt 组装时执行——从完整历史中选择一个子集发给 LLM。`Session.history` 始终保留完整消息，不被裁剪操作修改。

### 3. 完整轮次原子性

裁剪的最小不可分单位是完整轮次（`User → Assistant → [ToolCalls] → [ToolResults] → ... → 下一个 User 之前`）。不做部分删除，保证保留的每一段对话都是自洽的。

### 4. 渐进式演进

```
Phase 1 (已完成) → 轻量：chars/4 估算 + 基础裁剪 + 配置化
Phase 2 (进行中) → 架构：visp-context 独立 crate + 依赖倒置
Phase 3 (后续)   → 精度：rs-bpe + Prefix Sum + LLM 摘要
Phase 4 (远期)   → 完整：Memory / Workspace / RAG / 多级摘要
```

## 总体架构

### 处理流程

```
Agent Loop 迭代开始
    │
    ├─ ① Token 估算（visp-core）
    │   消息构造时：chars/4 估算每条消息，存入 estimated_tokens 字段
    │
    ├─ ② 拼接 system prompt（visp-core）
    │   计算 system_overhead = system_msg.estimated_tokens
    │
    ├─ ③ 过滤 skip_context 消息（visp-core）
    │   
    └─→ 【 trimmer.trim(history, max_ctx, system_overhead, output_tokens) 】（visp-context crate）
              │
              ├─ ④ 预算计算
              │   available = max_ctx − max(output_tokens, 4K)
              │   对话历史可用预算 = available − system_overhead
              │
              ├─ ⑤ 轮次边界识别
              │   扫描 User 消息位置，划分轮次
              │
              ├─ ⑥ HEAD/TAIL 保护
              │   HEAD = 前5轮  |  MIDDLE = 可裁剪区域  |  TAIL = 后10轮
              │
              ├─ ⑦ 轮次裁剪
              │   MIDDLE 超预算 → drop_old_turns 删除最旧完整轮次
              │   HEAD+TAIL 超预算 → keep_head_and_tail 极端保底
              │
              ├─ ⑧ 工具输出截断
              │   Tool 消息截断到 2000 字符
              │
              └─→ 返回裁剪后 Vec<Message>
    
    │
    └─ ⑨ Prompt 组装（visp-core）
         [system_msg] + trimmed → 发给 LLM
```

所有裁剪和截断只在 prompt 组装时发生，不修改 `Session.history`。

**crate 边界**：步骤 ④-⑧ 在 `visp-context` crate 内执行，core 通过 `ContextTrimmer` trait 调用。预算是 crate 内部自主计算的，core 不感知分配策略。剪枝决策在前、截断在后，两者统一在 `trim()` 内部完成，对外是一个原子操作。

### 机制定义

| 机制 | 触发条件 | 职责 | 修改存储？ |
|------|---------|------|-----------|
| **Token 估算** | 消息构造时（一次性） | `chars/4` 估算，预填入 `Message.estimated_tokens` | ✅ 构造时填充 |
| **预算计算** | 每次 LLM 调用前 | `max_context_tokens − max(output_tokens, 4K)`，得出本轮输入预算 | ❌ |
| **轮次边界识别** | 预算计算后 | 扫描 User 消息位置，划分轮次边界（User → 下一 User 之前） | ❌ |
| **HEAD/TAIL 保护** | 边界识别后 | 标记前 5 轮为 HEAD、后 10 轮为 TAIL，两者之间为 MIDDLE | ❌ |
| **轮次裁剪** | 预算不足时 | 从 MIDDLE 最旧轮次开始完整删除，直到预算满足 | ❌ |
| **极端保底** | HEAD+TAIL 本身已超预算 | 保留首条 User（任务锚点）+ 尾部最近消息，过滤孤立 ToolResult | ❌ |
| **工具输出截断** | Prompt 组装时 | 对 Tool 消息副本截断到 2000 字符，存储中原件不动 | ❌ |
| **Prompt 组装** | 所有裁剪完成后 | System + 裁剪后历史 → 发送给 LLM | ❌ |

核心原则：**Storage ≠ Prompt Context**——存储保留原始信息，prompt 使用压缩版本。

## 阶段路线

| | Phase 1（已完成） | Phase 2（进行中） | Phase 3（后续） | Phase 4（远期） |
|---|---|---|---|---|
| **架构** | visp-core 内 | visp-context 独立 crate + 依赖倒置 | — | — |
| **Token 计数** | chars/4 估算 | — | rs-bpe 精确计数 | — |
| **裁剪粒度** | 整轮删除 | — | 整轮删除 + 摘要替换 | 级联裁剪 |
| **查找** | O(N) 扫描 | — | O(logN) 前缀和 | O(logN) |
| **工具输出** | 截断 2000 字符 | — | 截断 | 结构化提取 |
| **配置** | LlmConfig + daemon.toml | — | 同 Phase 1 | 同 Phase 1 |
| **总结** | 无 | 无 | MIDDLE 区域 LLM 摘要 | 多级摘要树 |
| **Memory** | 无 | 无 | 无 | KV + Embedding |
| **Workspace** | 无 | 无 | 无 | AST + Diff + Diagnostics |
| **RAG** | 无 | 无 | 无 | 知识库检索 |
| **接口** | 直接函数调用 | ContextTrimmer trait + Arc 注入 | — | — |

## 核心组件

| 组件 | 所在 crate | 职责 |
|------|-----------|------|
| Token 估算器 | visp-core | 估算文本/消息/消息列表的 token 数 |
| ContextTrimmer (trait) | visp-core | 定义裁剪接口，隔离 core 与具体策略 |
| Budget Planner | visp-context | 根据 `max_context_tokens` 和 `output_tokens` 计算可用预算 |
| Pruning Engine | visp-context | 从历史中选择符合预算的子集（三段式、极端保底） |
| Tool 输出截断 | visp-context | 裁剪后对 Tool 消息截断到 2000 字符 |
| Prompt Builder | visp-core | 拼接 system prompt + 过滤 skip_context + 调用 trimmer + 组装最终消息列表 |
| 配置层 | visp-core/visp-daemon | daemon.toml → proto → LlmConfig → AgentLoopContext 的传递与合并 |

**注入关系**：daemon 创建 `DefaultContextTrimmer`（visp-context），通过 `Arc<dyn ContextTrimmer>` 注入 `AgentLoopContext`。core 只依赖 trait，不依赖具体实现。详见 [Phase 2 设计文档](visp-design-context-management-phase2.md)。

## 触发点

| 触发点 | 时机 | 动作 | 是否修改存储 |
|--------|------|------|-------------|
| 对话历史裁剪 | 每次 LLM 调用前 | 从历史中选择子集 + 截断 Tool 消息 | ❌ |

所有的裁剪和截断只在 prompt 组装时发生。`Session.history` 始终保留完整的原始内容。

## 裁剪策略

```
HEAD (5 turns) | MIDDLE (可裁剪) | TAIL (10 turns)

裁剪引擎只操作 MIDDLE:
  从 MIDDLE 中删除最早的完整轮次，直到预算满足。
  极端情况（HEAD+TAIL 仍超预算）→ 保留首条 User（任务锚点）+ 尾部最近消息。
```

## 参考文档

- `docs/design/visp-design-context-management-phase1.md` — Phase 1 实现细节（Token 估算、预算公式、裁剪算法、配置、文件改动清单）
- `docs/design/visp-design-context-management-phase2.md` — Phase 2 架构演进（visp-context crate 抽取、ContextTrimmer trait、依赖倒置）
- `docs/context_management_suggestions.md` — Context Engine 完整架构愿景（远期参考）

## 术语表

| 术语 | 含义 |
|------|------|
| `max_context_tokens` | effective limit，用户配置的上下文上限，默认 128K（已含 20% 预留） |
| 轮次 (Turn) | 从 `User` 开始到下一个 `User` 之前结束的完整交互序列 |
| HEAD | 对话开头的 N 轮（保护不裁剪） |
| TAIL | 对话末尾的 N 轮（保护不裁剪） |
| MIDDLE | HEAD 与 TAIL 之间的轮次（可裁剪） |
| Budget Planner | 计算可用 token 预算的组件 |
| Pruning Engine | 执行裁剪的组件 |
| ContextTrimmer | visp-core 中定义的 trait 接口，隔离 core 与具体裁剪实现 |
| DefaultContextTrimmer | visp-context 中实现的默认裁剪策略 |
