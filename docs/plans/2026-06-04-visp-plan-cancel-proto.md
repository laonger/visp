# visp 前置工作计划：Cancel 消息协议扩展

## 概述

为 Chat 双向流添加 Cancel 消息支持。改动范围：proto 文件 + daemon service Chat handler。

---

## 步骤 1：Proto 添加 Cancel 消息

修改 `crates/visp-proto/proto/visp.proto`。

- `ClientMessage` oneof 新增字段 4：`Cancel cancel = 4;`
- 新增消息：`message Cancel { string session_id = 1; }`

验证：`cargo build -p visp-proto` 通过，生成的 Rust 代码包含 `Cancel` 类型。

#### 📦 提交

```bash
git add crates/visp-proto/ && git commit -m "feat(visp-proto): add Cancel message for client-initiated agent cancellation"
```

---

## 步骤 2：Daemon Service 处理 Cancel

修改 `crates/visp-daemon/src/service.rs` 的 Chat handler。

当前 Chat handler 在 while 循环中串行处理 `ClientMessage`。agent 事件在每轮 spawn 的 task 中通过 mpsc 转发。

需要改动：
- 新增 `Cancel` 分支处理：触发 CancellationToken（session Running 时），否则静默忽略
- 响应流构建：`mpsc::channel` → `ReceiverStream` → tonic Response（每轮转发 task clone `client_tx`）

不需要改动：
- Agent 循环（已有 CancellationToken 支持）
- UserInput / ConfigUpdate / UserResponse 处理
- 转发 task 架构（Phase 3 已实现）

### 🔴 红 — 测试

| # | 测试用例 |
|---|---|
| 1 | `test_cancel_during_agent_loop` — Agent 运行中发送 Cancel，验证 daemon 返回 Error(Cancelled)，Agent 循环终止 |
| 2 | `test_cancel_idle_session` — session 为 Idle 时发送 Cancel，静默忽略，不报错 |

### 🟢 绿 — 实现

- Chat handler 的 `while let` 循环中新增 `Cancel` match 分支
- `Cancel` 分支：`session_mgr.get(sid)` → 检查 Running → 触发 `CancellationToken`
- 不存在的 session 或非 Running 状态 → 静默忽略

### 🧪 测试 → 🔍 类型检查

```bash
cargo test -p visp-daemon && cargo clippy -p visp-daemon -- -D warnings
```

#### 📦 提交

```bash
git add crates/visp-daemon/ && git commit -m "feat(visp-daemon): handle Cancel message in Chat stream"
```

---

## 步骤 3：质量门

```bash
cargo test --workspace && cargo clippy --workspace -- -D warnings && cargo fmt --check --all
```

---

## 测试覆盖汇总

| 步骤 | 测试用例 |
|---|---|
| 1 (proto) | 0（编译验证） |
| 2 (daemon) | 2 |
| 3 (质量门) | — |

总计：**2 个测试用例，2 个提交**。
