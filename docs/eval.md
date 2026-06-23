# Multi-Agent Vibe Agent Observability Platform

Version: v1.0

Status: Draft

---

# 1. Overview

## 我们要建设什么系统

本方案设计的是：

```text
Multi-Agent Vibe Agent Observability Platform
```

即：

```text
面向 Multi-Agent Coding Agent 的
观测、回放、评估与诊断平台
```

该平台并不负责：

- Agent 编排
- Agent Runtime
- Workflow Engine
- DAG 执行

而是作为独立基础设施，为 Agent Runtime 提供：

```text
Tracing

Replay

Evaluation

Metrics

Root Cause Analysis
```

能力。

---

# 2. Why

随着 Agent 系统复杂度提升：

```text
Single Agent
    ↓
Tool Calling
    ↓
Multi-Agent
    ↓
MCP
    ↓
Coding Agent
```

传统日志系统已经无法回答以下问题：

---

## Agent 做了什么？

例如：

```text
为什么修改 login.ts？
```

---

## Agent 为什么这么做？

例如：

```text
为什么调用 MCP？
为什么没有复用已有实现？
```

---

## Agent 如何完成任务？

例如：

```text
Coordinator
 ↓
Coding Agent
 ↓
Review Agent
 ↓
Test Agent
```

具体执行路径是什么？

---

## 哪个 Agent 出问题？

例如：

```text
任务失败
```

到底是：

```text
Coding Agent

Review Agent

Test Agent

MCP
```

中的哪一个导致？

---

## Agent 能力是否提升？

例如：

```text
Success Rate

Cost

Latency

Regression Rate
```

是否持续改善？

---

# 3. Design Goal

建设一个统一平台，实现：

```text
Observe
Explain
Replay
Evaluate
Optimize
```

即：

```text
知道发生了什么

知道为什么发生

知道如何重现

知道质量如何

知道如何优化
```

---

# 4. Platform Positioning

系统定位如下：

```text
                    User

                      │

                      ▼

              Multi-Agent Runtime

                      │

                      ▼

      ┌─────────────────────────┐

      │ Observability Platform  │

      └─────────────────────────┘

                      │

      ┌───────────────┼───────────────┐

      ▼               ▼               ▼

   Replay        Evaluation       Metrics
```

Observability Platform 不参与执行。

只负责：

```text
记录

分析

回放

评估
```

---

# 5. Core Capability

平台由五个核心模块组成。

---

## 5.1 Tracing

回答：

```text
发生了什么？
```

记录：

```text
Agent

Tool

Skill

MCP

LLM
```

调用关系。

---

## 5.2 Replay

回答：

```text
Agent到底干了什么？
```

例如：

```text
Coordinator

 ↓

Coding Agent

 ↓

修改 login.ts

 ↓

运行测试

 ↓

Review Agent
```

Replay 能完整还原执行过程。

---

## 5.3 Decision Trace

回答：

```text
为什么这么做？
```

例如：

```text
为什么修改数据库？

为什么调用 Review Agent？

为什么重新执行测试？
```

记录 Agent 决策过程。

---

## 5.4 Evaluation

回答：

```text
结果是否正确？
```

评估：

```text
Feature Completion

Regression

Build

Test

Review
```

---

## 5.5 Metrics

回答：

```text
系统运行得怎么样？
```

统计：

```text
Success Rate

Latency

Cost

Agent Utilization

Tool Utilization
```

---

# 6. Design Philosophy

本平台遵循三个核心原则。

---

## Principle 1

Replay First

传统系统关注：

```text
Log
```

Agent 系统关注：

```text
Behavior Replay
```

用户真正需要看到：

```text
Agent做了什么
```

而不是：

```text
Agent打印了什么
```

---

## Principle 2

Decision First

记录行为不够。

必须记录：

```text
为什么产生该行为
```

因为：

```text
Action
```

解决：

```text
What
```

而：

```text
Decision
```

解决：

```text
Why
```

---

## Principle 3

Evaluation Driven

最终目标不是 Trace。

最终目标是：

```text
持续提升 Agent 能力
```

因此：

```text
Trace
 ↓
Replay
 ↓
Evaluation
 ↓
Optimization
```

形成闭环。

---

# 7. High-Level Architecture

```text
                 Agent Runtime

                        │

                        ▼

                   Event Bus

                        │

                        ▼

                OpenTelemetry

                        │

                        ▼

                 Event Storage

                        │

      ┌──────────────────────────┐

      │ Observability Platform   │

      └──────────────────────────┘

                        │

    ┌────────────┬────────────┬────────────┐

    ▼            ▼            ▼

 Replay      Evaluation    Metrics

                        │

                        ▼

                 Dashboard
```

---

# 8. Event-Centric Architecture

本平台采用：

```text
Event First
```

设计。

所有行为统一抽象为 Event。

例如：

```text
AgentStarted

AgentCompleted

AgentFailed

AgentHandoff

ToolCalled

ToolReturned

SkillExecuted

MCPCalled

FileEdited

DecisionMade

TestPassed

TestFailed
```

在此基础上构建：

```text
Trace

Replay

Evaluation

Metrics
```

所有上层能力。

因此：

```text
Event
```

是核心资产。

```text
Trace
```

只是 Event 的一种展示形式。

---

# 9. Expected Outcome

平台上线后可以实现：

---

## 对开发者

快速定位：

```text
哪里失败

为什么失败

如何重现
```

---

## 对 Agent 团队

持续评估：

```text
Agent质量

Agent成本

Agent效率
```

---

## 对平台

建立：

```text
Observability

Evaluation

Optimization
```

完整闭环。

---

# 10. Conclusion

本方案设计的是：

```text
Multi-Agent Vibe Agent Observability Platform
```

而不是：

```text
Workflow Engine

Agent Orchestrator

Agent Runtime
```

平台核心目标是：

```text
让 Multi-Agent 系统

可观测

可解释

可回放

可评估

可优化
```

通过统一 Event 模型，构建：

```text
Tracing

Replay

Decision Trace

Evaluation

Metrics
```

五大能力模块，最终形成 Agent 能力持续优化闭环。
