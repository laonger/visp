# DEFAULT_SYSTEM_PROMPT 内容质量优化设计

> 本文档基于对 `crates/visp-core/src/prompt.rs` 中 `DEFAULT_SYSTEM_PROMPT`（第 89-261 行）的 prompt engineering 审核，给出内容质量优化方案。
>
> 与 `visp-design-prompt-optimization.md` 的区别：前者是**首次创建** `DEFAULT_SYSTEM_PROMPT` 的架构性设计（已完成实施）；本文档是针对**已实施提示词**的内容质量二次优化。

## 1. 背景

当前 `DEFAULT_SYSTEM_PROMPT` 已从最初的一句话扩展为 173 行、约 751 词（~1000 tokens）的完整提示词。经 prompt engineering 视角审核，发现以下问题：

- **6 个严重问题**：指令矛盾、不可执行指令、模糊术语、无优先级目标、重复段落
- **5 个建议改进**：空洞指令、结构混乱、边界模糊、传递失效、内容缺失
- **token 效率低**：约 200 tokens 用于重复表述

## 2. 优化目标

| 指标 | 当前 | 目标 |
|------|------|------|
| 主 prompt token 数 | ~1000 | ~560（降 44%） |
| 一级段落数 | 7 | 5 |
| 严重问题解决 | 0/6 | 6/6 |
| 建议改进解决 | 0/5 | 5/5 |
| 指令矛盾数 | 2 | 0 |

## 3. 优化原则

| 原则 | 说明 |
|------|------|
| **去冗余** | 同一条规则只出现一次。Delegation Philosophy 和 Workflow §3 高度重叠，必须合并 |
| **去矛盾** | 所有互斥指令必须消解为单一权威陈述 |
| **去模糊** | 模糊术语（"critical details"、"symbol references"）改为具体示例 |
| **可执行** | 每条指令必须是模型可以照做的行为指令，不可执行的抽象指令直接删除 |
| **保特色** | 委托优先是 visp 的架构差异点，保留并强化，但软化绝对化表述 |
| **降 token** | 目标降 40-45% |

**不做的**：
- 不改变 `USER_QUERY_INSTRUCTION` 附加块（独立常量，不在本次优化范围）
- 不改变 rules 加载机制
- 不在 prompt 中加入语言特定的代码风格（通过 rules 系统注入）

## 4. 结构重组方案

### 4.1 当前结构 → 优化后结构

当前 7 段（~170 行）重组为 5 段（~75 行）：

| 当前段落 | 处理 | 优化后段落 |
|---------|------|-----------|
| Interaction Rules | 合并 | I. Core Principle |
| Task Delegation Philosophy | 合并 | I. Core Principle |
| Workflow §1 Understand | 删除（空洞） | — |
| Workflow §2 Path Selection | 合并 | I. Core Principle |
| Workflow §3 Delegation Check | 合并（与 Delegation Philosophy 重复） | I. Core Principle |
| Exploration Stop Rule | 前置 | II. Execution Workflow → A |
| Workflow §4-6 Plan/Execute/Verify | 合并 | II. Execution Workflow → B-D |
| Failure Handling | 合并 | II. Execution Workflow → E |
| Communication | 压缩 | IV. Communication |
| Context Budget | 合并 | V. Constraints |
| Result Contract | 移出（子 Agent 收不到） | 需代码改动注入子 Agent |
| — | 新增 | III. Code Quality |

### 4.2 调整理由

- **合并 Delegation Philosophy + Path Selection + Delegation Check**：三段内容重合度极高，都在定义"什么情况下委托"。合并为单一决策条件表，消除 ~200 tokens 冗余
- **删除 Understand 步骤**："Parse request: explicit requirements + implicit needs" 是任何模型不需要指令也会做的事，零边际收益
- **Exploration Stop Rule 前置**："不要过度探索"是前置约束，应在决策阶段起作用，而非放在 Path Selection 之后
- **Plan → Execute → Verify → Failure 合并为 Execution Workflow**：当前拆成 4-5 个子段落，每段 3-5 行，碎片化严重。合并后形成完整行为流
- **Communication 压缩**：部分表述可合并（如 "Don't explain code unless asked" + "Don't summarize what you did unless asked" → "Don't explain, summarize, or preamble unless asked"）
- **Result Contract 移出主 prompt**：架构问题——子 Agent 收不到主 Agent 的 system prompt，此约定放在主 prompt 中既不可执行又误导。需代码改动配合注入到子 Agent task description

## 5. 逐问题改写方案

### 5.1 🔴 "Sub-agents are the default mechanism for code work" 过于绝对

**改写方向**：保留委托优先理念，去掉绝对化表述，改为具体触发条件。

**说明性示例**：

    For any code task that involves discovery or spans multiple locations, delegate to a specialist.
    Execute directly only when the change is trivial and the target is already known:
    - editing one line in a known function
    - fixing a typo / obvious syntax error
    - adding a print / log statement in a known location

**理由**：从绝对化的"default mechanism"变为具体触发条件。模型能明确判断"需要搜索/理解/多文件 → 委托" vs "已知位置的小修改 → 自己来"。

### 5.2 🔴 指令矛盾 A："Always wait for tool results" vs "Do not immediately wait after spawning background tasks"

**改写方向**：拆到不同章节并加明确修饰语。工具调用（tool call）与子 Agent 派发（sub-agent dispatch）是不同并发模型，不应混在同一指令集合。

**说明性示例**：

Interaction Rules 部分：

    ## Tool Calls
    - Wait for each tool call to complete before using its result; do not assume outcomes
    - Multiple independent tool calls may run in parallel within a single reply

Execution Workflow → Dispatch 部分：

    - When spawning multiple sub-agents with independent tasks, dispatch them in parallel
      and collect results together — don't serialize unnecessarily
    - When a sub-agent's result is required by a subsequent step, wait before proceeding

**理由**：工具调用必须等待结果才能继续；子 Agent 派发可以并行。用"independent tasks"作为并行化条件，消除表面矛盾。

### 5.3 🔴 指令矛盾 B："Brief user on delegation goal" vs "Don't summarize what you did"

**改写方向**：区分"派发前通知"和"完成后总结"。

**说明性示例**：

    When delegating: a brief one-liner is enough ("Checking auth module via explorer...").
    After completion: don't summarize — present the result or next question directly.

**理由**：明确两个指令的边界——一个是"派发时"（during），一个是"完成后"（after），不再矛盾。

### 5.4 🔴 "Record task IDs and state" 不可执行

**改写方向**：删除。替换为可执行的替代指令。

**说明性示例**：

    Track ownership: know which specialist is handling which file or topic to avoid conflicting edits.

**理由**：模型没有本地"任务 ID 存储系统"，"Record task IDs"是无法落地的抽象指令。改为具体的"track ownership by file/topic"。

### 5.5 🔴 "symbol references" 术语含义不明

**改写方向**：用具体示例替代抽象术语。

**说明性示例**：

    Prefer compact references over inline content:
    - File paths with line numbers: `src/auth.rs:42-58`
    - Function signatures: `fn validate(token: &str) -> Result<User>`
    - Brief summaries of findings, not full file contents

**理由**：三个具体示例覆盖了"symbol references"原本想表达的所有含义，且每种都是模型可照做的行为指令。

### 5.6 🔴 "Optimize for quality, speed, cost, and reliability" 无优先级

**改写方向**：明确定义优先级。对于代码生成任务，质量是硬底线。

**说明性示例**：

    Priorities for code work: 1) correctness, 2) cost-effectiveness, 3) speed.
    Don't trade correctness for speed. Be economical with context and delegation overhead.

**理由**：三段式优先级。Correctness 不可妥协；Cost-effectiveness 通过合理委托控制上下文消耗；Speed 是优化项而非硬目标。

### 5.7 🔴 Delegation Philosophy 与 Workflow §3 高度重复

**改写方向**：合并为单一权威段落，放在 Core Principle 中。

**说明性示例**（替换原三段的"when to delegate / when not to"）：

    Delegate when:
    - locating relevant code (codebase search, file discovery)
    - understanding unfamiliar code (reading, tracing logic)
    - implementing changes across multiple files or modules
    - debugging or investigating behavior
    - writing or updating tests alongside implementation

    Skip delegation (execute directly) when:
    - the change is trivial: single-line edit, typo fix, obvious syntax error
    - the target location is already known with certainty
    - answering a non-coding question or explaining a concept
    - the user explicitly asks for direct reasoning

**理由**：从三个分散列表（约 20 条）压缩为两组明确触发条件（10 条），内容覆盖原三段所有要点，无重复。

### 5.8 🟡 "1. Understand: Parse request..." 空洞无用

**改写方向**：直接删除（节省 15 tokens）。任何 LLM 都会自动解析请求。

**替代方案**（如需保留"理解"步骤）：改为具体指令：

    Start by identifying: the user's goal, which code areas are involved, and which specialists are needed.

### 5.9 🟡 Path Selection 步骤结构混乱

**改写方向**：用扁平化决策流程替代嵌套树。决策逻辑已在 Core Principle 完整定义，Path Selection 压缩为一行提示。

**说明性示例**（合并到 Execution Workflow → Plan）：

    For code tasks: identify which specialists are needed, then dispatch the independent ones in parallel.
    For non-code questions: answer directly.

### 5.10 🟡 "critical details" vs "minor details" 边界模糊

**改写方向**：列出"必须确认"的硬类别，其余可以假设。

**说明性示例**：

    Ask the user before guessing about:
    - which file or module to modify (when ambiguous)
    - library/API choices (when alternatives exist)
    - architectural patterns (when the codebase allows multiple approaches)

    For everything else (variable names, formatting, default values), make a reasonable choice and proceed.

**理由**：三个硬类别 + "everything else"排除法，比"critical vs minor"模糊二分法可操作性高得多。

### 5.11 🟡 Result Contract 可能无法传递到子 Agent

**改写方向**：从主 prompt 移除，改为工程 notes。需在子 Agent task description 构建处注入输出格式约定（涉及 `orchestrator.rs` 代码改动，不在本次纯 prompt 优化范围）。

**主 prompt 中的替代文本**（软性提醒）：

    When integrating sub-agent results: check for completeness. If a result is ambiguous or partial,
    re-delegate or narrow the task scope.

### 5.12 🟡 缺少代码风格/质量指令

**改写方向**：新增 Code Quality 段落。加入语言无关的工程原则，语言特定规则通过 rules 系统注入。

**说明性示例**：

    ## Code Quality
    - Write minimal, working code — no speculative features, no premature abstraction
    - Prefer simple solutions. If it feels too clever, simplify
    - Include tests that cover the change
    - After making changes, run the relevant validation command before considering work done
      (tests, linter, build — whatever the project uses)

## 6. 新增内容建议

### 6.1 委托任务描述格式指南（Dispatch 子节）

当前提示了 "Provide clear, bounded task specifications" 但未说明什么是好的任务描述。

**说明性示例**：

    Task descriptions for sub-agents:
    - State the goal, not the method: "Find where authentication logic lives" not "Read src/auth.rs"
    - Include relevant file paths or search keywords
    - Set a clear scope boundary: what to include and what to exclude
    - Keep under ~300 tokens

### 6.2 多轮委托的结果整合规则（Integrate 子节）

当前有 "Reconcile results, resolve conflicts, and gate dependent lanes" 但未说明如何整合。

**说明性示例**：

    After collecting sub-agent results:
    - Cross-check: do findings from different specialists agree? If not, investigate the discrepancy
    - Synthesize: produce a unified answer, not a collection of raw specialist outputs
    - Gap check: is anything still unknown that blocks the next step?

## 7. Token 效率预估

| 度量项 | 优化前 | 优化后 | 节省 |
|--------|--------|--------|------|
| 词数 | ~751 | ~420 | -44% |
| Token 估算 | ~1000 | ~560 | -440 tokens |
| 一级 + 二级段落数 | 7 + 12 | 5 + 8 | 更扁平 |
| 冗余消除 | — | 合并 3 段重复 → 1 段 | ~200 tokens |
| 空洞指令消除 | — | 删除 Understand、Result Contract | ~50 tokens |
| 模糊术语替换 | — | 具体化后更紧凑 | ~30 tokens |
| 表述精炼 | — | 全篇 tighten 语言 | ~160 tokens |

**附加收益**：减少 token 的同时，每条指令的明确性和可执行性均提升。矛盾消除意味着模型不会在"wait"和"don't wait"之间困惑。

## 8. 优化后完整结构大纲

    ## I. Core Principle (~8 lines)
       ├── You are visp, a lightweight AI coding assistant.
       ├── Priorities: correctness > cost-effectiveness > speed
       ├── Delegation-first for code work: [Delegate when...] / [Skip delegation when...]
       └── Main agent role: decompose, coordinate, integrate, communicate

    ## II. Execution Workflow (~30 lines)
       ├── A. Explorer Stop Rule (前置约束)
       ├── B. Plan & Route
       │    ├── Code tasks → identify specialists → work graph
       │    └── Non-code → direct answer
       ├── C. Dispatch
       │    ├── Task description format
       │    ├── Parallel vs serial
       │    └── Reference style (file:line, not full paste)
       ├── D. Integrate & Verify
       │    ├── Cross-check, synthesize, gap check
       │    ├── Validation: smallest check first
       │    └── Examples: single fn → unit test, module → crate test
       └── E. Failure Recovery
            └── Retry → alternative → narrow scope → ask user

    ## III. Code Quality (~5 lines) [新增]
       ├── Minimal, working code
       ├── Simple over clever
       ├── Include tests
       └── Run validation before done

    ## IV. Communication (~12 lines)
       ├── Concision rules
       │    ├── No preamble, no unsolicited explanations
       │    ├── Delegation notices: brief one-liners only
       │    └── After completion: present result directly
       ├── Pushback format (state concern + alternative + ask)
       ├── No flattery
       └── When to ask the user
            ├── Must ask: file/module ambiguous, API choice, architecture pattern
            └── Can assume: everything else

    ## V. Constraints (~5 lines)
       ├── Use file:line references and summaries, not full file contents
       ├── Don't repeat previous findings
       └── Keep sub-agent task descriptions under ~300 tokens

> **注**：`USER_QUERY_INSTRUCTION`（~250 tokens）和 rules 内容按现有机制在 build 阶段追加，不在本大纲中重复。

## 9. 需代码配合的后续行动项

### 9.1 Result Contract 注入子 Agent

**问题**：当前 Result Contract 放在主 prompt 中，但子 Agent 收不到主 Agent 的 system prompt，此约定无效。

**方案**：在子 Agent 的 task description 构建处（`orchestrator.rs` 中）注入输出格式约定。

**涉及文件**：`crates/visp-agent/src/orchestrator.rs`，可能的 `crates/visp-agent/src/agent_loader.rs`。

**优先级**：中。不阻塞本次 prompt 优化，但应在本次优化后跟进。

## 10. 实施优先级

1. **先做**：合并 Delegation Philosophy + Path Selection + Delegation Check（问题 5.7）——最大冗余源，改一处影响三段
2. **再做**：消解矛盾指令（问题 5.2、5.3）——直接影响模型行为正确性
3. **然后**：逐条改写模糊术语和绝对化表述（问题 5.1、5.5、5.6、5.10）
4. **最后**：新增 Code Quality 段落（问题 5.12）、删除空洞段落（问题 5.4、5.8）、标记 Result Contract 行动项（问题 5.11）

## 11. 验收标准

1. **token 数下降**：主 prompt 从 ~1000 tokens 降至 ~560 tokens
2. **指令矛盾消除**：不存在互斥指令
3. **可执行性提升**：每条指令都是模型可照做的行为指令
4. **测试通过**：`cargo test -p visp-core` 全量通过（含 prompt.rs 现有 10 个测试）
5. **Clippy 零警告**：`cargo clippy -- -D warnings`
6. **格式检查通过**：`cargo fmt -- --check`
7. **语义完整性**：优化后 prompt 覆盖原始 prompt 的所有有效指令（人工验证）
