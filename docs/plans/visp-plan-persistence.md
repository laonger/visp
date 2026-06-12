# visp 工作计划：数据持久化

## 概述

新增 `visp-db` crate（SqliteSessionStore + schema 迁移 + DAO），扩展 visp-core 的 Message/Session 字段和 SessionStore trait，daemon 集成。

基于设计文档：docs/design/visp-design-persistence.md

---

## Wave 1：visp-core 类型 / trait 扩展

### 步骤 1：`MessageType` 枚举 + `Message` 新增 `kind` + 构造器

**文件**：`crates/visp-core/src/message.rs`（修改）

#### 🔴 红 — 测试

| # | 测试用例 | 描述 |
|---|---------|------|
| 1.1 | `test_message_type_serde` | 各 variant 序列化为对应字符串并反序列还原 |
| 1.2 | `test_user_message_kind` | `Message::user("hi")` → `kind = User` |
| 1.3 | `test_system_message_kind` | `Message::system("prompt")` → `kind = System` |
| 1.4 | `test_tool_message_kind` | `Message::tool("out", "id")` → `kind = ToolResult` |
| 1.5 | `test_assistant_message_kind` | `Message::assistant("text")` → `kind = Text` |
| 1.6 | `test_tool_call_constructor` | `Message::tool_call(calls)` → `kind = ToolCall`，tool_calls 非空 |
| 1.7 | `test_thinking_constructor` | `Message::thinking("...")` → `kind = Thinking` |
| 1.8 | `test_error_constructor` | `Message::error("...")` → `kind = Error` |
| 1.9 | `test_status_constructor` | `Message::status("...")` → `kind = Status` |

#### 🟢 绿 — 实现

- 新增 `MessageType` 枚举（Text/Thinking/ToolCall/ToolResult/Error/Status/System/User）
- `Message` 结构体新增 `kind: MessageType` 字段
- 更新 4 个现有构造器设置 `kind`
- 新增 4 个构造器：`tool_call()` / `thinking()` / `error()` / `status()`

#### 🧪 测试 → 🔍 类型检查

```bash
cargo test -p visp-core -- message
cargo clippy -p visp-core -- -D warnings
```

#### 📦 提交

```
feat(core): add MessageType enum, kind field, and 4 new constructors
```

### 步骤 2：`Message` 新增 `actual_*` 字段

**文件**：`crates/visp-core/src/message.rs`（修改）

#### 🔴 红 — 测试

| # | 测试用例 | 描述 |
|---|---------|------|
| 2.1 | `test_actual_fields_default_none` | 新建 Message 各 actual_* 为 None |
| 2.2 | `test_actual_fields_read_write` | 设值后读写一致 |
| 2.3 | `test_actual_fields_serde` | 序列化/反序列化包含新字段 |
| 2.4 | `test_actual_fields_backward_compat` | 不含新字段的旧 JSON 可反序列化 |

#### 🟢 绿 — 实现

- 新增字段（均 `Option`，`#[serde(default)]`）：
  - `actual_tokens_input: Option<i64>`
  - `actual_tokens_output: Option<i64>`
  - `actual_cache_read: Option<i64>`
  - `actual_cache_write: Option<i64>`
  - `actual_cost: Option<f64>`

#### 🧪 测试 → 🔍 类型检查

```bash
cargo test -p visp-core -- message
cargo clippy -p visp-core -- -D warnings
```

#### 📦 提交

```
feat(core): add actual_* token and cost fields to Message
```

### 步骤 3：`Session` 新增 `created_at_unix` 字段

**文件**：`crates/visp-core/src/session.rs`（修改）

#### 🔴 红 — 测试

| # | 测试用例 | 描述 |
|---|---------|------|
| 3.1 | `test_session_created_at_unix_default_none` | 新建 Session 时 created_at_unix = None |
| 3.2 | `test_session_created_at_unix_read_write` | 设值后读写一致 |
| 3.3 | `test_session_created_at_unix_serde` | 序列化包含新字段 |
| 3.4 | `test_session_created_at_unix_backward_compat` | 不含新字段的旧 JSON 可反序列化 |

#### 🟢 绿 — 实现

- `Session` 新增 `created_at_unix: Option<i64>`，`#[serde(default)]`
- 现有 `created_at: Instant` 保留不动

#### 🧪 测试 → 🔍 类型检查

```bash
cargo test -p visp-core -- session
cargo clippy -p visp-core -- -D warnings
```

#### 📦 提交

```
feat(core): add created_at_unix field to Session for DB persistence
```

### 步骤 4：`SessionStore` 签名变更 + 新增方法 + `InMemorySessionStore` + `home_dir` 公共化

**文件**：`crates/visp-core/src/session.rs`（修改）、`crates/visp-core/src/rules.rs`（修改）、`crates/visp-core/src/lib.rs`（修改）

#### 🔴 红 — 测试

| # | 测试用例 | 描述 |
|---|---------|------|
| 4.1 | `test_store_get_owned` | `get()` 返回 `Session`（owned）而非引用 |
| 4.2 | `test_store_list_owned` | `list()` 返回 `Vec<Session>` |
| 4.3 | `test_in_memory_get_messages` | `get_messages()` 返回 history 副本 |
| 4.4 | `test_in_memory_append_message` | `append_message()` 追加消息到 history |
| 4.5 | `test_in_memory_list_by_project` | `list_by_project("/tmp")` 正确过滤 |
| 4.6 | `test_home_dir_pub` | `visp_core::home_dir()` 可调用 |
| 4.7 | `test_rules_home_dir` | rules.rs 中无重复 home_dir 实现 |
| 4.8 | `test_in_memory_store_crud` | 原有 CRUD 测试仍通过 |
| 4.9 | `test_session_manager_append_message` | SessionManager 委托 store 追加消息 |
| 4.10 | `test_session_manager_start_loop` | 原有 start_loop 测试通过 |

#### 🟢 绿 — 实现

- `SessionStore` trait：改 `get()` 返回 `Session`，`list()` 返回 `Vec<Session>`
- `InMemorySessionStore`：`get()` → `self.sessions.get(id).cloned().ok_or(...)`；`list()` → `.values().cloned().collect()`
- 实现 `get_messages()` / `append_message()` / `list_by_project()`
- `home_dir()` 改为 `pub fn`，rules.rs 导入使用，移除重复实现

#### 🧪 测试 → 🔍 类型检查

```bash
cargo test -p visp-core
cargo clippy -p visp-core -- -D warnings
```

#### 📦 提交

```
refactor(core): change SessionStore get/list to owned type, add methods, pub home_dir
```

---

## Wave 2：visp-db crate

### 步骤 5：创建 `visp-db` crate 骨架

**文件**：`crates/visp-db/Cargo.toml`（新建）、`crates/visp-db/src/lib.rs`（新建）、`crates/visp-db/src/schema.rs`（新建）、`crates/visp-db/src/session_repo.rs`（新建）、`crates/visp-db/src/message_repo.rs`（新建）、`crates/visp-db/src/store.rs`（新建）、`Cargo.toml`（工作区，修改）

#### 🟢 绿 — 实现

- 创建 crate 目录结构
- `Cargo.toml` 依赖：`visp-core`、`rusqlite`（bundled）、`serde`、`serde_json`、`chrono`
- 工作区 `members` 添加 `crates/visp-db`
- `lib.rs` 导出各模块

#### 🧪 测试 → 🔍 类型检查

```bash
cargo build -p visp-db
```

### 步骤 6：Schema 迁移

**文件**：`crates/visp-db/src/schema.rs`

#### 🔴 红 — 测试

| # | 测试用例 | 描述 |
|---|---------|------|
| 6.1 | `test_migrate_creates_tables` | 迁移后 session 表、message 表存在 |
| 6.2 | `test_migrate_creates_indexes` | 迁移后所有索引存在 |
| 6.3 | `test_migrate_idempotent` | 重复运行不报错 |
| 6.4 | `test_migrate_version` | 迁移后 `PRAGMA user_version == 1` |
| 6.5 | `test_migrate_pragma` | WAL、synchronous、foreign_keys 配置正确 |

#### 🟢 绿 — 实现

- 内置 SQL DDL 字符串（CREATE TABLE + INDEX）
- `run_migration(conn)` 函数：检查 `user_version` → 按需执行 → 更新版本
- 所有迁移在事务中执行

#### 🧪 测试 → 🔍 类型检查

```bash
cargo test -p visp-db -- schema
cargo clippy -p visp-db -- -D warnings
```

### 步骤 7：Session DAO

**文件**：`crates/visp-db/src/session_repo.rs`

#### 🔴 红 — 测试

| # | 测试用例 | 描述 |
|---|---------|------|
| 7.1 | `test_insert_session` | 写入后 SELECT |
| 7.2 | `test_get_session_found` | 存在 id 返回 Session（history 空） |
| 7.3 | `test_get_session_not_found` | 不存在返回 None |
| 7.4 | `test_list_sessions` | 返回所有 |
| 7.5 | `test_list_by_project` | 按路径过滤 |
| 7.6 | `test_update_session` | 更新后字段一致 |
| 7.7 | `test_delete_session` | 删除后查不到 |
| 7.8 | `test_delete_session_cascade` | 删除 session 联动删除 message |

#### 🟢 绿 — 实现

- 每个方法接收 `&Connection` 参数
- `history` 永远为 `vec![]`（消息不走 session 表）
- `created_at` / `updated_at` 写 Unix 毫秒

#### 🧪 测试 → 🔍 类型检查

```bash
cargo test -p visp-db -- session_repo
cargo clippy -p visp-db -- -D warnings
```

### 步骤 8：Message DAO

**文件**：`crates/visp-db/src/message_repo.rs`

#### 🔴 红 — 测试

| # | 测试用例 | 描述 |
|---|---------|------|
| 8.1 | `test_insert_message` | 写入一条消息 |
| 8.2 | `test_get_messages_by_session` | 按 session_id 查询，id 升序 |
| 8.3 | `test_insert_message_with_actual` | 写入含 actual_* 字段的消息 |
| 8.4 | `test_insert_message_with_kind` | 写入含 kind 字段的消息 |
| 8.5 | `test_delete_session_cascade_messages` | 删除 session 后 CASCADE 删除 message |

#### 🟢 绿 — 实现

- `insert_message(conn, session_id, msg)` 写入一行
- `get_messages_by_session(conn, session_id)` → `Vec<Message>`，按 id 升序
- `extra_blocks` 和 `provider_metadata` 作为 JSON TEXT 存储
- `created_at` 写入 Unix 毫秒

#### 🧪 测试 → 🔍 类型检查

```bash
cargo test -p visp-db -- message_repo
cargo clippy -p visp-db -- -D warnings
```

### 步骤 9：`SqliteSessionStore` 实现

**文件**：`crates/visp-db/src/store.rs`

#### 🔴 红 — 测试

| # | 测试用例 | 描述 |
|---|---------|------|
| 9.1 | `test_create_session` | create 后 DB 有记录 |
| 9.2 | `test_get_session_empty_history` | get 返回的 Session history 为空 |
| 9.3 | `test_list_sessions` | list 返回所有 |
| 9.4 | `test_delete_session` | delete 删除 session + 级联 message |
| 9.5 | `test_update_session` | update 后读回一致 |
| 9.6 | `test_get_messages` | get_messages 返回所有消息 |
| 9.7 | `test_append_message` | append_message 写入 message 表 |
| 9.8 | `test_list_by_project` | 按项目路径过滤 |
| 9.9 | `test_append_multiple_messages_order` | 多次 append 后顺序正确 |
| 9.10 | `test_store_file_permissions` | DB 文件权限为 0o600 |

#### 🟢 绿 — 实现

```rust
pub struct SqliteSessionStore {
    conn: Mutex<rusqlite::Connection>,
}
```

- 构造时：打开/创建 DB → set_permissions(0o600) → 执行迁移
- `get()`：session_repo::get_session → 构造 Session（history: vec![]）
- `append_message()`：message_repo::insert_message
- `get_messages()`：message_repo::get_messages_by_session

#### 🧪 测试 → 🔍 类型检查

```bash
cargo test -p visp-db
cargo clippy -p visp-db -- -D warnings
```

#### 📦 提交

```
feat(db): implement SqliteSessionStore with schema migration and DAO
```

---

## Wave 3：Daemon 集成

### 步骤 10：daemon 配置扩展

**文件**：`crates/visp-daemon/src/config.rs`（修改）

#### 🔴 红 — 测试

| # | 测试用例 | 描述 |
|---|---------|------|
| 10.1 | `test_storage_default_sqlite` | 默认 driver = "sqlite" |
| 10.2 | `test_storage_default_path` | 默认 path = `$HOME/.visp/data/visp.db` |
| 10.3 | `test_storage_memory_mode` | driver = "memory" 配置生效 |
| 10.4 | `test_storage_custom_path` | 自定义 path 生效 |
| 10.5 | `test_storage_expand_tilde` | `~/` 开头的路径自动展开 |

#### 🟢 绿 — 实现

- `DaemonConfig` 新增 `storage` 配置节（driver + path）
- 配置加载时调用 `expand_tilde` 展开 `~/`

### 步骤 11：main.rs 组装

**文件**：`crates/visp-daemon/src/main.rs`（修改）

#### 🔴 红 — 测试

| # | 测试用例 | 描述 |
|---|---------|------|
| 11.1 | `test_main_creates_sqlite_store` | driver = "sqlite" 时创建 SqliteSessionStore |
| 11.2 | `test_main_creates_in_memory_store` | driver = "memory" 时创建 InMemorySessionStore |
| 11.3 | `test_main_invalid_driver` | 无效 driver 报错 |

#### 🟢 绿 — 实现

- 根据 config 选择 store 实现
- 构造 `SessionManager::new(store)` 注入

### 步骤 12：全量验证

#### 🧪 测试 → 🔍 类型检查

```bash
cargo test && cargo clippy -- -D warnings && cargo fmt -- --check
```

#### 📦 提交

```
feat(daemon): integrate SqliteSessionStore with config-driven backend selection
```

