# AI Agent Context Engine 设计方案（Rust / 多模型 / Code Agent）

## 目标

支持：

- GPT-5
- Claude Sonnet
- Qwen3.x
- DeepSeek-V3
- Llama3

能力：

- 长会话
- Tool Calling
- Code Agent
- Memory
- RAG
- Context Pruning
- Workspace Awareness

设计目标：

- O(1)~O(logN) Context 管理
- 数百轮会话稳定运行
- Token 利用率最大化
- 支持不同模型定制策略

---

# 一、总体架构

```text
                   User Input
                        │
                        ▼
                Context Builder
                        │
        ┌───────────────┼───────────────┐
        ▼               ▼               ▼
 Short Memory     Long Memory      Workspace
        │               │               │
        └───────────────┴───────────────┘
                        │
                        ▼
                Budget Planner
                        │
                        ▼
                 Pruning Engine
                        │
                        ▼
                 Prompt Builder
                        │
                        ▼
                     Model
```

---

# 二、Context 分层

不要：

```rust
Vec<Message>
```

应该：

```rust
pub struct AgentContext {
    pub system: SystemLayer,
    pub memory: MemoryLayer,
    pub workspace: WorkspaceLayer,
    pub conversation: ConversationLayer,
    pub tool_results: ToolLayer,
}
```

---

# 三、五层 Context

## Layer 1：System Layer

### 内容

- System Prompt
- Agent Rules
- Tool Description
- 安全策略
- 项目规范

### 技术

- Arc<String> 缓存
- Prompt Cache
- Tool Discovery

### 优化

不要一次塞入全部 Tool Schema。

采用：

```text
Tool Discovery
↓
按需加载 Tool Schema
```

这样可以大幅降低工具上下文开销。OpenAI 和 Atlassian 都强调上下文应尽量聚焦，而不是一次加载所有信息。  [oai_citation:0‡OpenAI](https://openai.com/index/harness-engineering/?utm_source=chatgpt.com)

### 预算

```text
5%
```

---

## Layer 2：Memory Layer

### 分类

```rust
enum Memory {
    Fact,
    Preference,
    ProjectKnowledge,
    Summary,
}
```

### Fact Memory

例如：

```text
用户主要开发 Rust
```

技术：

```text
KV Store
```

### Preference Memory

例如：

```text
喜欢 Ratatui
```

技术：

```text
KV Store
```

### Project Knowledge

例如：

```text
Kafka Result Topic
```

技术：

```text
Embedding
Vector Search
```

推荐：

- Qdrant
- LanceDB

### Summary Memory

例如：

```text
第1~50轮总结
```

技术：

```text
Embedding + Metadata
```

### Memory Promotion

当系统发现长期稳定事实：

```text
Conversation
↓
LLM Extraction
↓
Fact Memory
```

长期记忆与短期记忆分离是主流 Agent Memory 架构。  [oai_citation:1‡AWS Documentation](https://docs.aws.amazon.com/prescriptive-guidance/latest/agentic-ai-patterns/memory-augmented-agents.html?utm_source=chatgpt.com)

### 预算

```text
5~10%
```

---

## Layer 3：Workspace Layer

Code Agent 最重要的层。

### 内容

```rust
pub struct WorkspaceState {
    pub current_file: String,
    pub git_diff: String,
    pub diagnostics: Vec<Diagnostic>,
    pub opened_files: Vec<String>,
}
```

### 技术

#### AST

推荐：

```text
tree-sitter
```

不要发送整个文件。

应该：

```text
当前函数
+
上下函数
+
相关调用链
```

#### Git Diff

来源：

```bash
git diff
```

然后进行：

```text
Patch Compression
```

#### Diagnostics

来源：

```text
rust-analyzer
tsserver
```

只保留：

```text
Error
Warning
```

### 不要放入

```text
Cargo.lock
target/
node_modules/
```

### 预算

```text
20~30%
```

---

## Layer 4：Conversation Layer

### 存储结构

```rust
VecDeque<Message>
```

### Token Prefix Sum

维护：

```rust
[32]
[58]
[91]
[130]
...
```

支持：

```text
最近 64K Token
```

二分查找：

```text
O(logN)
```

而不是：

```text
O(N)
```

### Protect Head/Tail

永远保留：

```text
Conversation Start
Recent N Turns
```

例如：

```rust
const PROTECTED_HEAD = 5;
const PROTECTED_TAIL = 10;
```

结构：

```text
HEAD
MIDDLE
TAIL
```

只有：

```text
MIDDLE
```

允许压缩。

### 预算

```text
30~50%
```

---

## Layer 5：Tool Result Layer

最大的 Token 消耗来源。Agent 长时间运行时，工具输出通常比聊天历史更容易膨胀。  [oai_citation:2‡OpenAI](https://openai.com/index/unrolling-the-codex-agent-loop/?utm_source=chatgpt.com)

### Cargo Build

原始：

```text
5000 lines
```

处理：

```text
Regex Extractor
```

提取：

```text
error
warning
```

保留：

```text
±30行上下文
```

### Search Result

使用：

```text
MMR
(Maximal Marginal Relevance)
```

去重。

### File Read

使用：

```text
AST Slice
```

保留：

```text
相关函数
相关模块
```

### 预算

```text
10~30%
```

---

# 四、Token Engine

核心：

```toml
rs-bpe
```

用途：

- Token Count
- Range Count
- Incremental Count
- Exact Boundary Split

rs-bpe 的主要价值不是训练 tokenizer，而是高性能 Token Counting 和 Chunking。  [oai_citation:3‡AWS Documentation](https://docs.aws.amazon.com/prescriptive-guidance/latest/agentic-ai-patterns/memory-augmented-agents.html?utm_source=chatgpt.com)

接口：

```rust
pub trait TokenEngine {
    fn count(&self, text: &str) -> usize;

    fn split_after(
        &self,
        text: &str,
        limit: usize,
    ) -> usize;
}
```

---

# 五、Model Profile

```rust
pub struct ModelProfile {
    pub model: String,

    pub context_window: usize,

    pub reserve_tokens: usize,

    pub strategy: Arc<
        dyn PruningStrategy
    >,
}
```

支持：

```text
GPT5
ClaudeSonnet
Qwen3
DeepSeekV3
Llama3
```

---

# 六、Budget Planner

不要用满上下文。

例如：

```text
128K
```

保留：

```text
20%
```

给：

```text
Completion
Tool Call
Reasoning
```

可用：

```text
102K
```

---

# 七、Pruning Cascade

原则：

```text
先压缩
再删除
最后总结
```

与 Rovo Dev 和 OpenAI Context Management 思路一致。  [oai_citation:4‡OpenAI Cookbook](https://cookbook.openai.com/examples/agents_sdk/session_memory?utm_source=chatgpt.com)

---

## Stage 1：Tool Compression

技术：

```text
Regex
AST
Structured Extractor
```

目标：

```text
5000 token
↓
300 token
```

---

## Stage 2：Drop Old Tool Results

删除：

```text
历史 Tool Output
```

保留：

```text
最近 Tool Output
```

---

## Stage 3：Semantic Deduplication

技术：

```text
SimHash
MinHash
Embedding Similarity
```

删除：

```text
重复读取
重复搜索
重复日志
```

---

## Stage 4：Middle Collapse

保留：

```text
HEAD
TAIL
```

压缩：

```text
MIDDLE
```

生成：

```text
Task Summary
```

---

## Stage 5：Hierarchical Summary

不要：

```text
100轮
→
1个Summary
```

应该：

```text
10轮
→
Summary A

10轮
→
Summary B
```

形成：

```text
Summary Tree
```

结构：

```text
Level0 Message

Level1 Mini Summary

Level2 Task Summary

Level3 Project Summary
```

---

## Stage 6：Memory Promotion

流程：

```text
Conversation
↓
LLM Extraction
↓
Memory Store
```

---

# 八、Task-Aware Budget

比 Model-Aware 更重要。

```rust
pub enum TaskType {
    Coding,
    Debugging,
    Planning,
    Research,
    Chat,
}
```

---

## Coding

优先：

```text
Source Code
Git Diff
```

---

## Debugging

优先：

```text
Diagnostics
Logs
Stacktrace
```

---

## Planning

优先：

```text
Requirement
Architecture
History
```

---

## Research

优先：

```text
Search Result
Reference Material
```

---

# 九、Repository Knowledge

不要：

```text
2000行 AGENTS.md
```

OpenAI Codex 的实践是：

```text
AGENTS.md
↓
目录
↓
docs/
↓
真实知识
```

Repository 应成为知识源，而不是把所有知识直接塞进 Prompt。  [oai_citation:5‡OpenAI](https://openai.com/index/harness-engineering/?utm_source=chatgpt.com)

结构：

```text
AGENTS.md
ARCHITECTURE.md

docs/
├── design/
├── workflows/
├── api/
└── decisions/
```

按需检索：

```text
RAG
```

---

# 十、推荐 Rust 技术栈

```toml
rs-bpe            # Token Engine
tree-sitter       # AST
qdrant-client     # Memory
tantivy           # Repository Search
simhash-rs        # Dedup
serde
tokio
dashmap
```

---

# 十一、Crate 拆分

```text
agent-token
    rs-bpe封装

agent-memory
    Memory管理

agent-workspace
    AST + Diff + Diagnostics

agent-context
    Budget + Pruning

agent-rag
    Repository Search

agent-runtime
    Agent Loop
```

---

# 十二、核心思想

不要把 Context 理解为：

```text
聊天记录
```

而应该理解为：

```text
Memory
+
Workspace
+
Conversation
+
Tool Result
+
Repository Knowledge
```

最终形成：

```text
Context
=
Working Memory
+
Long Term Memory
+
Current Workspace
+
Task State
```

剪枝只是其中一个优化环节。

真正的目标是：

在有限 Token Budget 下，始终保留下一步决策最有价值的信息。
