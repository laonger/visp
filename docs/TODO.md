# TODO & 已知限制

## Phase 3 已知限制

### 1. Agent 循环 panic 时状态无法自动恢复

**场景**：Agent 循环在等待 UserQuery 确认时 panic，mpsc sender 被 drop，但 daemon service 正阻塞等待客户端 UserResponse，无法及时检测到 mpsc 关闭，导致 Session 永久卡在 Running 状态。

**临时缓解**：客户端超时断开后，需通过 DeleteSession 手动清理。概率极低。

**后续方案**：用 `tokio::select!` 同时监听 mpsc receiver 和 UserResponse，加 heartbeat 检测。

### 2. Session 锁粒度

当前 `InMemorySessionStore` 使用 `Arc<RwLock<HashMap>>`，整个 session map 一把锁。高并发下可能成为瓶颈。

**后续方案**：按 session 粒度锁（`Arc<RwLock<Session>>` per session），或使用 `dashmap`。

### 3. 对话历史无长度限制

Session.history 不设上限，超长对话可能导致 LLM context window 溢出。

**后续方案**：实现 token 计数和消息裁剪（保留最近 N 轮或最近 N tokens）。

### 4. 规则热重载不打断运行中的 Agent

规则文件变更后，正在运行的 Agent 循环继续使用旧规则（快照机制）。只有下一轮对话才加载新规则。这是设计行为，但用户可能期望立即生效。

### 5. Chat 流中 UserQuery 等待时无法处理其他消息

当 daemon service 阻塞等待客户端回复 UserQuery 时，无法处理同一 Chat 流上的其他消息（如新的 UserInput）。当前设计下，同一会话同一时刻只有一个 UserQuery，此限制可接受。

---

## Phase 4+ 计划

- [ ] Session 持久化到 SQLite
- [ ] 对话历史 token 计数 + 裁剪
- [ ] ConfigUpdate 支持 model 切换（需重新初始化 LlmProvider）
- [ ] gRPC TLS 支持
- [ ] Agent 委派（多 specialist）
- [ ] CodeGraph 集成

## Phase 5 已知限制

### 1. 全量构建无进度反馈

全量构建是 daemon 启动时的后台初始化操作，对用户透明。构建期间查询返回"索引构建中"。MVP 不做进度回调。

**后续方案**：添加 `build_status() -> Progress` 状态查询接口。

### 2. 跨文件关系解析：A↔B 循环导入

两个文件互相导入对方符号时，不产生死循环（全局符号表已完成），但调用关系可能产生间接循环边。查询调用者/被调用者时 SQL JOIN 只需一步，不受影响。

**后续方案**：如需实现路径追踪/影响分析，需在查询层加环检测。

### 3. Source 字段截取规则

`SymbolDetails.source` 基于 `line` 字段读文件取源码片段，MVP 截取前 500 字符。

**后续方案**：使用 tree-sitter 的 node range 精确定位完整函数源码。
