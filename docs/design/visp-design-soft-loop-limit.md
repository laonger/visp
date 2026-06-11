# visp-design-soft-loop-limit.md

## 1. 目标

**一句话**：将 visp Agent 循环从"硬上限 50 轮强制终止"改为"软上限 + doom_loop 检测 + 模型自主收尾"，消除因轮次耗尽而中断用户任务的体验问题。

**背景**：当前 visp 在 `run_agent_loop` 中使用 `for _ in 0..max_iterations`（默认 50），一旦达到上限立即以 `MaxIterations` 错误退出，即使模型即将完成任务也会中断。对比 opencode 的分析显示，opencode V1 使用 `while(true)` + 软上限提示机制，任务由模型自主终止而非被循环强行掐断。

## 2. 模块划分

| 模块 | 改动范围 | 职责 |
|------|---------|------|
| `visp-core/src/agent.rs` | 核心改动 | `run_agent_loop` 循环逻辑、doom_loop 检测、软上限提示注入 |
| `visp-core/src/error.rs` | 小改 | `AgentErrorCode` 新增 `StuckInLoop` |
| `visp-daemon/src/config.rs` | 小改 | `AgentSection` 字段重命名，新增 `doom_loop_threshold` |

### 2.1 visp-core Agent 循环逻辑（核心改动）

**职责变更**：

- **旧行为**：`for` 循环 0..`max_iterations`，用完 → `MaxIterations` 错误退出
- **新行为**：`loop` 无限循环，在以下条件退出：
  1. 模型返回无工具调用（正常完成）— 原有逻辑不变
  2. 用户取消（CancellationToken）— 原有逻辑不变
  3. doom_loop 检测二次触发 — 新增
  4. 硬上限（兜底保护）— 保留但大幅提高阈值

**关键设计决策**：

#### 软上限机制

- `AgentConfig.soft_limit` 新增（替代原 `max_iterations`），默认 50
- 软上限检查在**本轮 LLM 调用之前**执行。当前迭代次数（从 1 计数）达到 `soft_limit` 时，将收尾提示注入到发送给 LLM 的消息列表中，告诉模型"已达到最大轮次上限，请立即在当前回复中完成所有未完成的工作"
- `soft_limit = 0` 表示关闭软上限，直接依赖硬上限兜底
- 注入后继续运行（不退出），由模型自主决定收尾
- 注入软提示后模型若仍未停止，将触发硬上限兜底

#### 硬上限（兜底保护）

- `AgentConfig.hard_limit` 新增字段，默认 200
- 硬上限检查在 LLM 调用之前执行，优先于软上限检查
- 达到 `hard_limit` 时以 `MaxIterations` 错误退出
- `hard_limit` 在测试中可覆写（测试中设为 1 即可验证上限行为）

#### Doom Loop 检测

- 跟踪最近连续 N 轮的工具调用模式
- **签名比较**：按 `(name, args_value)` 比较，其中 `args_value` 为 `serde_json::Value`（比较语义，顺序无关）
- 检测条件：连续 `doom_loop_threshold`（默认 5）轮，每轮的工具调用签名完全相同
- **首次检测触发**：注入系统提示，告知模型"你似乎陷入了重复的操作循环，请改变策略或总结当前进度"
- **重置检测窗口**：发出警告后重置窗口，从警告之后的下一轮开始重新统计，给模型改变的机会
- **二次检测触发**：若警告后模型仍然重复（重新达到 `doom_loop_threshold` 轮相同签名），则作为 `StuckInLoop` 错误退出
- **状态变量**：`run_agent_loop` 中新增局部变量 `doom_loop_window: Vec<Vec<(String, serde_json::Value)>>` 和 `doom_loop_warned: bool`，无需改动 `AgentLoopContext`

#### 检测优先级顺序

```
LLM 响应收集完成
    │
    ├─ 无工具调用（正常完成）→ return Done（行为不变）
    │
    └─ 有工具调用
         │
         ├─ doom_loop 签名记录 → 二次检测 → StuckInLoop 错误
         │                                   退出
         ├─ doom_loop 首次检测 → 警告 → 重置窗口
         ├─ [USER_QUERY] 处理（如果模型插入了查询标记）
         └─ 执行工具 → 迭代计数 + 1 → 下一轮
```

注意：v1 的 doom_loop 检测仅作用于有工具调用的分支。[USER_QUERY] 无工具调用，不做 doom_loop 检测。

#### 配置参数

| 参数 | 默认值 | 说明 |
|------|--------|------|
| `soft_limit` | 50 | 软上限阈值，达到后在 LLM 调用前注入收尾提示。0 = 关闭 |
| `hard_limit` | 200 | 硬上限兜底，达到后强制终止。`AgentConfig` 字段，可覆写 |
| `doom_loop_threshold` | 5 | 连续相同工具调用的检测窗口大小 |

### 2.2 visp-core 错误码

- 保留 `MaxIterations` — 语义不变（硬上限触发时使用）
- 新增 `StuckInLoop` — doom_loop 二次触发时使用
- `AgentEvent` 不需要新增变体（doom_loop 警告通过现有 `StatusUpdate` 传达）

### 2.3 visp-daemon 配置

`AgentSection` 变更：

| 字段 | 变更 | 新默认值 | 说明 |
|------|------|---------|------|
| `max_iterations` | **移除** | — | 替换为 `soft_limit` |
| `soft_limit` | **新增** | 50 | 软上限，0 表示关闭 |
| `doom_loop_threshold` | **新增** | 5 | 连续相同工具调用检测窗口。0 表示关闭检测 |

## 3. 依赖关系

```
visp-daemon/config → visp-core/AgentConfig → visp-core/agent::run_agent_loop
                                ↑
                    visp-core/error::AgentErrorCode (+StuckInLoop)
```

## 4. 核心数据流

```
用户消息 → run_agent_loop 启动 (iteration=1)
    │
    ▼
┌─ loop ─────────────────────────────────────────────────────────────┐
│                                                                      │
│  1. 取消检查 (CancellationToken)                                     │
│                                                                      │
│  2. 上限检测（在 LLM 调用之前）                                       │
│     ├─ iteration >= hard_limit(200)?                                 │
│     │   └─ Y → return MaxIterations                                  │
│     ├─ soft_limit > 0 && iteration >= soft_limit(50)?                │
│     │   └─ Y → 在 system prompt 中注入收尾提示                        │
│     └─ 继续                                                          │
│                                                                      │
│  3. 构建 prompt → 调用 LLM → 收集响应                                 │
│                                                                      │
│  4. 决策：无工具调用？                                                │
│     ├─ Y → [USER_QUERY] 标记？                                       │
│     │   ├─ Y → 处理用户询问 → continue                               │
│     │   └─ N → 正常完成 → return Done                                │
│     │                                                                │
│     └─ N → 有工具调用                                                │
│           │                                                          │
│           ├─ 记录本轮签名（工具名 + args_value）到窗口                  │
│           │                                                          │
│           ├─ doom_loop 二次检测：窗口已满且签名相同且已警告？           │
│           │   └─ Y → return StuckInLoop                               │
│           │                                                          │
│           ├─ doom_loop 首次检测：窗口已满且签名相同且未警告？           │
│           │   ├─ Y → 注入警告消息 → 重置窗口 → 标记已警告              │
│           │   └─ N → 正常流程                                        │
│           │                                                          │
│           ├─ [USER_QUERY] 处理（如有查询标记） → 用户输入后 continue   │
│           │                                                          │
│           └─ 执行工具 → 追加结果 → iteration++ → 继续                  │
│                                                                      │
└──────────────────────────────────────────────────────────────────────┘
```

**软上限/硬上限层级**：

```
iteration=1 ──────────► soft_limit(50) ───────► hard_limit(200)
  正常运行          LLM 调用前注入收尾提示         LLM 调用前强制终止
                      (模型应主动收尾)              (兜底保护)
```

## 5. 不做什么

- **不** 实现 opencode 式的 `compaction` 自动压缩
- **不** 实现 opencode 式的 `shouldBreak` 权限拒绝分支
- **不** 改变 `AgentLoopContext` 结构（现有字段足够）
- **不** 改变 `AgentEvent` 枚举（doom_loop/软上限通过现有 `StatusUpdate` + 文本流传达）
- **不** 新增 gRPC 接口

## 6. 验收标准

1. **正常完成不受影响**：任务在 50 轮内正常完成时，行为与改前完全一致
2. **超限不中断**：任务超过 50 轮时，注入收尾提示后模型应能继续并完成，不会被 `MaxIterations` 错误打断
3. **硬上限兜底**：任务达到 `hard_limit`(200) 轮时，以 `MaxIterations` 错误退出
4. **doom_loop 检测**：模型连续 5 轮发出完全相同工具调用时，触发警告；警告后再连续重复 5 轮，以 `StuckInLoop` 退出
5. **测试可覆写**：`hard_limit` 可通过 `AgentConfig` 覆写，使 `test_max_iterations` 能验证上限行为
6. `cargo test` 全量通过
7. `cargo clippy -- -D warnings` 零警告
8. 现有 `test_max_iterations` 测试调整语义，新增软上限和 doom_loop 的测试用例
