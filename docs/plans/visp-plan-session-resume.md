# Session Resume — 工作计划

## 概述

实现 `-s <session_id>` 参数恢复历史会话功能。Session ID 保持 UUID v4 格式，CLI 和 daemon 支持前缀匹配。同时补充会话浏览（`--list`）、agent 异常终止恢复、项目路径校验等边界处理。

**设计文档**: `docs/design/visp-design-session-resume.md`

**涉及 crate**: visp-proto, visp-daemon, visp-cli, visp (launcher), visp-core

## 步骤

### 步骤 1：Proto — 新增 GetSession RPC

**文件**: `crates/visp-proto/proto/visp.proto`

新增消息和 RPC，不修改已有定义。

#### 🔴 红 — 测试

Proto 定义不需要测试，依赖 tonic-build 编译检查（`cargo build -p visp-proto` 会验证 proto 完整性）。

验证方式：`cargo build -p visp-proto` 编译通过。

#### 🟢 绿 — 实现

在 `service CoderDaemon` 中新增：
```protobuf
message GetSessionRequest {
    string session_id = 1;
}

// 在 CoderDaemon service 中
rpc GetSession(GetSessionRequest) returns (Session);
```

#### 🧪 构建验证 → 🔍 类型检查

```
cargo build -p visp-proto
cargo clippy -p visp-proto -- -D warnings
```

#### ♻️ 重构

无

#### 📦 提交

```
feat(proto): add GetSession RPC for session resume
```

### 步骤 2：Daemon — get_session handler（含前缀匹配）

**文件**: `crates/visp-daemon/src/service.rs`

实现 `get_session` gRPC handler。包含精确匹配 + 前缀匹配搜索逻辑。

#### 🔴 红 — 测试

| # | 测试用例 | 描述 |
|---|---------|------|
| 1 | 精确匹配 | 用完整 session_id 调用 get_session → 返回 Session |
| 2 | 前缀匹配（唯一） | 用 UUID 前 6 字符调用 → 返回匹配的 Session |
| 3 | 前缀匹配 0 结果 | 用不存在的前缀调用 → 返回 NotFound |
| 4 | 前缀匹配多结果 | 用短前缀（如 `0`）匹配到 2 个 session → 返回 NotFound |
| 5 | ListSessions 二次筛选 | 多匹配时 CLI 调用 list_sessions 过滤前缀 → 返回匹配列表 |
| 6 | session_mgr.get 失败 | 内部错误 → 返回 gRPC Status 错误 |

测试位置：`crates/visp-daemon/src/service.rs`（已有测试模块）
或 `crates/visp-daemon/tests/service_test.rs`（集成测试）

#### 🟢 绿 — 实现

1. 新增 `get_session` handler 方法
2. 先调 `session_mgr.get(id)` 精确匹配
3. 精确匹配不到时遍历 `session_mgr.list()` 按前缀过滤
4. 0 个/多个匹配 → 返回 `Status::not_found`
5. 1 个匹配 → 返回 `session_to_proto(&session)`

#### 🧪 测试 → 🔍 类型检查

```
cargo test -p visp-daemon
cargo clippy -p visp-daemon -- -D warnings
cargo fmt -p visp-daemon -- --check
```

#### ♻️ 重构

- 如有重复的 session→proto 转换逻辑，提取公共函数

#### 📦 提交

```
feat(daemon): add GetSession handler with prefix matching
```

### 步骤 3：CLI client — get_session 方法

**文件**: `crates/visp-cli/src/client.rs`

CLI gRPC 客户端新增 `get_session` 方法。

#### 🔴 红 — 测试

| # | 测试用例 | 描述 |
|---|---------|------|
| 1 | get_session 成功 | 调用 gRPC GetSession → 返回 Session |
| 2 | get_session 未找到 | session 不存在 → 返回 Err/None |

测试位置：`crates/visp-cli/src/client.rs`（已有测试模块）

#### 🟢 绿 — 实现

新增：
```rust
pub async fn get_session(&mut self, session_id: &str) -> Result<proto::Session, Box<dyn std::error::Error>> {
    let req = tonic::Request::new(proto::GetSessionRequest {
        session_id: session_id.to_string(),
    });
    let resp = self.client.get_session(req).await?;
    Ok(resp.into_inner())
}
```

#### 🧪 测试 → 🔍 类型检查

```
cargo test -p visp-cli
cargo clippy -p visp-cli -- -D warnings
```

#### ♻️ 重构

无

#### 📦 提交

```
feat(cli): add get_session client method
```

### 步骤 4：CLI — `-s` 参数 + `--list` + resume 流程

**文件**: `crates/visp-cli/src/main.rs`（CLI 入口 + 参数解析）

核心实现：`-s` 参数解析、session 查找、路径校验、未找到引导、`--list` 会话浏览。

#### 🔴 红 — 测试

| # | 测试用例 | 描述 |
|---|---------|------|
| 1 | 无 `-s` → create_session | 新建会话（回归测试，行为不变） |
| 2 | `-s` 有效 session_id → 启动 chat | resume 成功 |
| 3 | `-s` 无效 session_id → 打印最近会话 → 退出 | 未找到引导 |
| 4 | `-p` 与 session path 不匹配 → 拒绝并提示 | 路径校验 |
| 5 | `--list` 列出最近会话 | 会话浏览 |
| 6 | `--list -p /project` 按项目过滤 | 项目限定浏览 |
| 7 | 不传任何参数 → 正常启动 | 全回归 |

测试位置：`crates/visp-cli/src/main.rs` 或 `crates/visp-cli/tests/`（新建测试目录需确认）

#### 🟢 绿 — 实现

1. Cli 结构体新增：
   - `session: Option<String>`
   - `list: bool`
2. main 逻辑：
   - `--list` 模式：调 `list_sessions` → 格式化输出 → 退出
   - `-s <id>` 模式：调 `get_session` → 路径校验 → 启动 chat
   - 无参数：create_session（现有逻辑，不变）
3. 未找到引导：
   - 调用 `list_sessions` 过滤 project_path
   - 输出短 ID + 创建时间 + 状态
   - 提示 "use `visp -s <short-id>` to resume"
4. 路径校验：`-p` 与 session.project_path 不一致时拒绝

#### 🧪 测试 → 🔍 类型检查

```
cargo test -p visp-cli
cargo clippy -p visp-cli -- -D warnings
cargo fmt -p visp-cli -- --check
```

#### ♻️ 重构

- CLI 参数解析和主流程逻辑拆分为可测试的辅助函数（如 `resolve_session`）

#### 📦 提交

```
feat(cli): add -s/--session and --list flags for session resume
```

### 步骤 5：Launcher — `-s` 参数透传

**文件**: `crates/visp/src/main.rs`

Launcher 新增 `-s` 参数，透传给 CLI 子进程。

#### 🔴 红 — 测试

| # | 测试用例 | 描述 |
|---|---------|------|
| 1 | 传 `-s abc` → CLI 收到 `--session abc` | 正确透传 |
| 2 | 不传 `-s` → 无 `--session` 参数 | 回归正常 |
| 3 | `-s` + `-p` 同时透传 | 参数组合正确 |
| 4 | 只传 `-s` 不传 `-p` → `-p` 使用默认值 | `-s` 不依赖 `-p` |

测试位置：`crates/visp/src/main.rs`（已有测试模块）

#### 🟢 绿 — 实现

1. Cli 结构体新增 `session: Option<String>`
2. args 构建时透传：`if let Some(sid) = &cli.session { args.extend(["--session", sid]); }`

#### 🧪 测试 → 🔍 类型检查

```
cargo test -p visp
cargo clippy -p visp -- -D warnings
cargo fmt -p visp -- --check
```

#### ♻️ 重构

无

#### 📦 提交

```
feat(launcher): add -s/--session passthrough to CLI
```

### 步骤 6：Agent 异常终止恢复（健壮性加固）

**文件**: `crates/visp-core/src/agent.rs`（`run_agent_loop`）

在 agent loop 终止时确保 session 状态重置为 Idle。

#### 🔴 红 — 测试

| # | 测试用例 | 描述 |
|---|---------|------|
| 1 | agent loop 正常结束 → session 状态为 Idle | 正常路径 |
| 2 | agent loop panic → session 状态仍为 Idle | panic 恢复 |
| 3 | catch_unwind 后重新抛出 panic | 不吞 panic |

测试位置：`crates/visp-core/src/agent.rs`（已有测试）

#### 🟢 绿 — 实现

在 `run_agent_loop` 中：
```rust
let session_id = ctx.session_id.clone();
let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
    // 现有 loop 逻辑
}));

// 无论正常还是 panic，确保 session 状态重置
if let Err(e) = session_mgr.update_status(&session_id, SessionStatus::Idle) {
    log::error!("failed to reset session status: {e}");
}

if let Err(panic) = result {
    std::panic::resume_unwind(panic);
}
```

#### 🧪 测试 → 🔍 类型检查

```
cargo test -p visp-core
cargo clippy -p visp-core -- -D warnings
cargo fmt -p visp-core -- --check
```

#### ♻️ 重构

- 如 `update_status` 调用分散多处，统一收敛

#### 📦 提交

```
fix(core): reset session status to Idle on agent loop exit
```

## Wave 并行策略

```
Wave 1（3 个并行任务）
├── 步骤 1: Proto — GetSession RPC       [visp-proto]
├── 步骤 6: Agent 异常终止恢复            [visp-core]
└── (步骤 2 的前置条件满足后进入 Wave 2)

Wave 2（3 个并行任务，依赖 Wave 1 proto 完成）
├── 步骤 2: Daemon — get_session handler  [visp-daemon]
├── 步骤 3: CLI client — get_session 方法 [visp-cli]
└── 步骤 5: Launcher — -s 透传            [visp]

Wave 3（1 个任务，依赖 Wave 2 所有任务）
└── 步骤 4: CLI — -s + --list + resume    [visp-cli]
     （步骤 3 的 client 和步骤 2 的 daemon 都完成后再集成测试）

Wave 4（最终验证）
└── 全量质量门禁
```

## 依赖关系总览

```
Proto ──→ Daemon handler ──┐
                            ├──→ CLI -s/--list (集成)
CLI client ────────────────┘
Launcher ──→ (无依赖)

Agent recovery ──→ (独立，任意 Wave 完成)
```

## 测试覆盖汇总

| Wave | 模块 | 测试用例数 | 关键场景 |
|------|------|-----------|---------|
| W1 | visp-proto | 0 (编译验证) | proto 编译通过 |
| W1 | visp-core | 3 | 正常退出、panic 重置、不吞 panic |
| W2 | visp-daemon | 6 | 精确/前缀/0/多匹配、error 传播 |
| W2 | visp-cli client | 2 | 成功/失败 |
| W2 | visp launcher | 4 | 透传正误、组合、默认值 |
| W3 | visp-cli main | 7 | resume/未找到/路径校验/--list/回归 |
| W4 | 全量 | 全量 | `cargo test && cargo clippy && cargo fmt` |

## 备注

1. **测试位置**：现有测试在同文件 `#[cfg(test)] mod tests` 中。如果文件变得过长再提取到 `tests/` 目录。
2. **ListSessions 过滤**：proto 层不支持按 project 过滤 — 当前设计让 CLI 端做二次筛选，proto 保持简洁。
3. **跨 Wave 依赖**：Wave 2 的三个任务各自独立，但步骤 4（Wave 3）需要 daemon handler 和 CLI client 都完成才能完整测试。
4. **Session 状态重置优先级**：步骤 6 是健壮性加固，不是关键路径，可放在任意 Wave 执行。
