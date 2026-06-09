# TODO & 已知限制

> 最后更新：2026-06-08
> 测试：201 passed

## Phase 完成状态

| Phase | 内容 | 状态 |
|-------|------|------|
| 1 | 项目骨架 + 核心抽象 | ✅ |
| 2 | LLM Provider + 内置工具 | ✅ |
| 3 | Agent 核心 + Daemon | ✅ |
| 4 | CLI 前端 (visp-cli) | ✅ |
| 5 | CodeGraph 代码智能 | ✅ |

**测试分布**：visp-codegraph 18+64 · visp-core 53 · visp-daemon 17 · visp-llm 22 · visp-tools 27

---

## ~~待合并分支：`mouse`~~ ✅ 已合并

~~所有 CLI UI 改进，尚未合并回 master：~~

| 提交 | 内容 |
|------|------|
| `fbe96cb` | feat: input area text color white |
| `c112cbb` | feat: Alt+Enter inserts newline in input area |
| `17cc17e` | feat: slash command hints and Tab autocomplete |
| `776c839` | feat: add /mouse command toggle mouse capture |
| `039f398` | fix: use KeyCode::F(2) instead of Ctrl+M |
| `b501806` | feat: add Ctrl+M toggle mouse capture |

---

## Phase 4+ 计划（待实现）

### Session 持久化到 SQLite

当前使用 `InMemorySessionStore`，重启 daemon 后所有会话丢失。

**后续方案**：实现 `SqliteSessionStore: SessionStore`，使用 rusqlite 持久化 session 状态和历史。

### 对话历史 token 计数 + 裁剪

`Session.history` 不设上限，超长对话可能导致 LLM context window 溢出。

**后续方案**：实现 token 计数（tiktoken-rs）和消息裁剪（保留最近 N 轮或最近 N tokens）。

### ConfigUpdate 支持 model 切换

当前 `/model` 命令只更新内存配置，daemon 侧的 LlmProvider 不会重新初始化。

**后续方案**：Chat 流中收到 ConfigUpdate 时重建 LlmProvider。

### gRPC TLS 支持

目前 daemon 监听明文连接，生产环境需加密。

**后续方案**：config 添加 `[daemon.tls]` section + tonic TLS 配置。

### Agent 委派（多 specialist）

当前 Agent 是单循环模式，不支持多 specialist 协作。

**后续方案**：设计 sub-agent 编排机制，支持 specialist 委派。

---

## Phase 3 已知限制（可优化）

### 1. Agent 循环 panic 时状态无法自动恢复

**场景**：Agent 循环在等待 UserQuery 确认时 panic，mpsc sender 被 drop，但 daemon service 正阻塞等待客户端 UserResponse，无法及时检测到 mpsc 关闭，导致 Session 永久卡在 Running 状态。

**临时缓解**：客户端超时断开后，需通过 DeleteSession 手动清理。概率极低。

**后续方案**：用 `tokio::select!` 同时监听 mpsc receiver 和 UserResponse，加 heartbeat 检测。

### 2. Session 锁粒度

当前 `InMemorySessionStore` 使用 `Arc<RwLock<HashMap>>`，整个 session map 一把锁。高并发下可能成为瓶颈。

**后续方案**：按 session 粒度锁（`Arc<RwLock<Session>>` per session），或使用 `dashmap`。

### 3. 对话历史无长度限制

见 Phase 4+ 计划「对话历史 token 计数 + 裁剪」。

### 4. 规则热重载不打断运行中的 Agent

规则文件变更后，正在运行的 Agent 循环继续使用旧规则（快照机制）。只有下一轮对话才加载新规则。这是设计行为，但用户可能期望立即生效。

### 5. Chat 流中 UserQuery 等待时无法处理其他消息

当 daemon service 阻塞等待客户端回复 UserQuery 时，无法处理同一 Chat 流上的其他消息（如新的 UserInput）。当前设计下，同一会话同一时刻只有一个 UserQuery，此限制可接受。

---

## Phase 5 已知限制（可优化）

### 1. 全量构建无进度反馈

全量构建是 daemon 启动时的后台初始化操作，对用户透明。构建期间查询返回"索引构建中"。MVP 不做进度回调。

**后续方案**：添加 `build_status() -> Progress` 状态查询接口。

### 2. 跨文件关系解析：A↔B 循环导入

两个文件互相导入对方符号时，不产生死循环（全局符号表已完成），但调用关系可能产生间接循环边。查询调用者/被调用者时 SQL JOIN 只需一步，不受影响。

**后续方案**：如需实现路径追踪/影响分析，需在查询层加环检测。

### 3. Source 字段截取规则

`SymbolDetails.source` 基于 `line` 字段读文件取源码片段，MVP 截取前 500 字符。

**后续方案**：使用 tree-sitter 的 node range 精确定位完整函数源码。
