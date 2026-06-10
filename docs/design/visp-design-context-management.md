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
Phase 1 (即刻)   → 轻量：chars/4 估算 + 基础裁剪 + 配置化
Phase 2 (后续)   → 精度：rs-bpe + Prefix Sum + LLM 摘要
Phase 3 (远期)   → 完整：Memory / Workspace / RAG / 多级摘要
```

## 总体架构

### 处理流程

```
Agent Loop 迭代开始
    │
    ├─ ① Token 估算
    │   消息构造时：chars/4 估算每条消息，存入 estimated_tokens 字段
    │
    ├─ ② 预算计算
    │   available = max_context_tokens − max(output_tokens, 4K)
    │   再减去 system 消息的 token → 得到对话历史可用预算
    │
    ├─ ③ 轮次边界识别
    │   扫描 User 消息位置，划分轮次（User → 下一 User 之前为完整一轮）
    │
    ├─ ④ HEAD/TAIL 保护
    │   HEAD = 前5轮  |  MIDDLE = 可裁剪区域  |  TAIL = 后10轮
    │
    ├─ ⑤ 预算检查 ──→ 够用？── 是 ──→ 跳至 ⑦
    │       │
    │      否
    │       │
    │       ├─ ⑥ 轮次裁剪
    │       │   从 MIDDLE 最旧轮次开始，完整删除整轮（User → Assistant → Tools）
    │       │   每删一轮重新估算，回到 ⑤
    │       │
    │       └─ HEAD+TAIL 本身已超预算？
    │             │
    │             └─ 极端保底
    │                  保留首条 User（任务锚点）+ 尾部最近消息
    │                  过滤掉对应 tool_use 不在保留集中的孤立 ToolResult
    │
    ├─ ⑦ 工具输出截断
    │   Tool 消息副本截断到 2000 字符（仅 prompt 副本，存储中原件不动）
    │
    └─ ⑧ Prompt 组装
         System + 裁剪后历史 → 发给 LLM
```

所有裁剪和截断只在 prompt 组装时发生，不修改 `Session.history`。

**处理顺序：先剪枝，后截断。** 因为剪枝决策依赖准确的 token 预算，而预算估算（`estimate_messages_tokens_for_prompt`）已经按截断后长度计算 Tool 消息。两者分离保证：剪枝决定"留哪些"，截断决定"留的怎么发"。

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

| | Phase 1（即刻） | Phase 2（后续） | Phase 3（远期） |
|---|---|---|---|
| **Token 计数** | chars/4 估算 | rs-bpe 精确计数 | — |
| **裁剪粒度** | 整轮删除 | 整轮删除 + 摘要替换 | 级联裁剪 |
| **查找** | O(N) 扫描 | O(logN) 前缀和 | O(logN) |
| **工具输出** | 截断 2000 字符 | 截断 | 结构化提取 |
| **配置** | LlmConfig + daemon.toml | 同 Phase 1 | 同 Phase 1 |
| **总结** | 无 | MIDDLE 区域 LLM 摘要 | 多级摘要树 |
| **Memory** | 无 | 无 | KV + Embedding |
| **Workspace** | 无 | 无 | AST + Diff + Diagnostics |
| **RAG** | 无 | 无 | 知识库检索 |

## 核心组件

| 组件 | 职责 |
|------|------|
| Token 估算器 | 估算文本/消息/消息列表的 token 数 |
| Budget Planner | 根据 `max_context_tokens` 和 `output_tokens` 计算可用预算 |
| Pruning Engine | 从历史中选择符合预算的子集 |
| Prompt Builder | 组装 system + 裁剪后历史，输出最终消息列表 |
| 配置层 | daemon.toml → proto → LlmConfig → AgentLoopContext 的传递与合并 |

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
