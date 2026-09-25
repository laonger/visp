# 验收核对清单：`visp-fs` 跨平台监听模块与 AGENTS.md 向上边界

- 日期：2026-09-25
- 依据设计：`docs/design/2026-09-25-fs-watcher-and-agents-bound-design.md`（**v5**）§7 的 18 条验收
- 依据计划：`docs/plans/2026-09-25-visp-fs-and-agents-bound-plan.md`
- 全量质量门：`cargo test --workspace` ✅ · `cargo clippy --workspace --all-targets -- -D warnings` ✅ · `cargo fmt -- --check` ✅

## 覆盖总览

| 分类 | 条目 | 自动化覆盖 | 需人工/环境 |
|---|---|---|---|
| 7.1 `visp-fs` 契约 | 1–9 | 9/9 | — |
| 7.2 迁移与回归 | 10–13 | 4/4（12 的 Linux 侧需 CI 跑） | 12（Linux/inotify） |
| 7.3 AGENTS.md 边界 | 14–18 | 5/5 | — |

## 7.1 `visp-fs` 契约

| # | 验收项 | 覆盖证据（测试名 / 提交） | 判定 |
|---|---|---|---|
| 1 | 内容改写可感知（非递归目标）：**启动前已存在**的文件原地覆写 → `修改` | `existing_file_overwrite_emits_modified`；另经 **daemon 侧端到端冒烟**（原地覆写 AGENTS.md → `热重载结果 domain:Rules`） | ✅ |
| 2 | 原子替换后仍可感知（细则 2a）：tmp+rename → **再原地覆写** → 仍收到 `修改` | 文件级 `atomic_replace_then_in_place_edit_is_still_observed`；**递归级** `recursive_target_atomic_replace_then_in_place_edit_is_still_observed`（修正 `f0fc8462`，修改前实测**红**）；冒烟亦确认原子替换触发 | ✅ |
| 3 | 结构性事件：创建/删除；重命名按「删除(旧)+创建(新)」拆分；元数据变更 → `修改`；inotify `Both` 拆分 | `new_file_emits_created` / `removed_file_emits_removed` / `normalized_*` / `kqueue_any_rename_yields_removed_old` / `inotify_from_to_yields_removed_and_created` / `inotify_both_yields_removed_and_created` / `windows_from_to_yields_removed_and_created` / `metadata_change_is_modified` | ✅ |
| 4 | 目录条目级 Modify 抑制（仅**模块直接挂载**的目录） | `dir_entry_modify_in_mounted_set_is_suppressed`；范围限定见 `dir_entry_modify_outside_mounted_set_is_emitted` | ✅ |
| 5 | 动态补挂：目标目录不存在 → 运行中创建（**含子目录**）并写入 → 事件 + 一次重扫信号 | `missing_target_dir_is_dynamically_attached` / `direct_child_subdir_creation_is_reported` | ✅ |
| 6 | 监听状态收敛：风暴下无句柄泄漏；`watch_not_found` 被忽略；单文件级失败不污染集合且可恢复 | `create_delete_storm_converges_without_leak` / `watch_not_found_is_recognized_and_ignored` / `single_file_mount_failure_does_not_pollute_and_recovers` | ✅ |
| 7 | `stop` 后不再投递、监听释放 | `stop_halts_delivery_and_releases_watches` | ✅ |
| 8 | 根/最近祖先级失败**显式上报降级**（不静默）、有界重试、无祖先时终止上报 | `root_level_mount_failure_reports_degradation_with_bounded_retry` / `unmountable_target_reports_once_and_does_not_mount`；**daemon 侧可观测面**：`degraded_event_pushes_empty_session_status_frame`（`54585405`），日志侧 `tracing::warn` | ✅ |
| 9 | 乱序保护（细则 2b，防御性） | `out_of_order_create_then_remove_keeps_watch`（假体单测，标注防御性） | ✅ |

## 7.2 迁移与回归

| # | 验收项 | 覆盖证据 | 判定 |
|---|---|---|---|
| 10 | daemon 热重载：原地覆写 `AGENTS.md` 触发 rules 重载 | `in_place_overwrite_of_preexisting_file_produces_event`；`tests/filewatch_integration.rs` 9 例全绿；**端到端冒烟**（daemon 日志两条 `热重载结果`） | ✅ |
| 11 | codegraph 索引器回归：内容改写 / 原子替换 / **重命名（旧路径 dangling 行清理 + 新路径索引）** / exclude 零触发 / 无关扩展名零触发 | `test_watcher_in_place_overwrite_updates_index` / `test_watcher_atomic_replace_updates_index` / `test_watcher_rename_cleans_old_rows_and_indexes_new` / `test_watcher_excluded_dir_produces_no_events` / `test_watcher_unsupported_extension_produces_no_events`（共 9 例，连跑 3 轮稳定） | ✅ |
| 12 | 后端无关：同一组集成测试可在 Linux(inotify, CI) 与 macOS(kqueue, 开发机) 通过；无 kqueue 特化断言 | 核对：`visp-fs` 源码**无 `cfg(target_os)`**；kqueue 仅出现在解释性注释与**适配器单测**名称中；集成断言一律有限时间轮询收敛。**macOS/kqueue 本机实测通过**；Linux/inotify 由 CI（`rust.yml` ubuntu-latest）承载 | ✅（Linux 侧待 CI 实跑） |
| 13 | `cargo test --workspace` / `clippy --all-targets` / `fmt --check` 全绿 | ✅ 全绿 | ✅ |

## 7.3 AGENTS.md 边界

| # | 验收项 | 覆盖证据 | 判定 |
|---|---|---|---|
| 14 | 止于 git 根：git 根之上放置 AGENTS.md → 不加载、不监听 | `test_ancestors_stop_at_git_root` / `test_git_file_is_boundary_for_worktree`；daemon 侧 `watch_plan_stops_at_git_root` | ✅ |
| 15 | monorepo：子目录项目仍加载至 git 根的各层 | `test_monorepo_loads_up_to_git_root` | ✅ |
| 16 | 非 git 项目：`$HOME` 之下止于 `$HOME`（含层）；`$HOME` 之外不向上；`$HOME` 不可解析仅项目层 | `test_non_git_under_home_stops_at_home_inclusive` / `test_project_outside_home_does_not_walk_up` / `test_unresolvable_home_loads_only_project_layer` | ✅ |
| 17 | 加载器与监听计划一致（同一祖先链解析共用） | `watch_plan_ancestor_chain_matches_loader`（一致性断言，`42d6f959`）；daemon 已改用 `visp_config::agents_md_ancestors` | ✅ |
| 18 | rename 归一化映射表单测（三类后端）；乱序分支防御性、假体单测 | `kqueue_any_rename_yields_removed_old` / `inotify_from_to_yields_removed_and_created` / `inotify_both_yields_removed_and_created` / `windows_from_to_yields_removed_and_created` / `out_of_order_create_then_remove_keeps_watch` | ✅ |

## 阶段 6a 执行记录

- **workspace 全量回归**：`cargo test --workspace` 全绿（visp-fs 48 / visp-codegraph 129 / visp-config 195 / visp-daemon 88+109+9 / visp-tui 415 / 其余各 crate），零回归。
- **静态检查**：`cargo clippy --workspace --all-targets -- -D warnings` 0 告警；`cargo fmt -- --check` 通过。
- **后端无关核对**：`visp-fs` 无 `cfg(target_os)` 分支；无 kqueue 特化断言。
- **端到端冒烟（关键）**：daemon 以独立配置启动（`filewatcher = true`，`AGENTS.md` **启动前已存在**）→ **原地覆写**触发 `热重载结果 domain:Rules changes:1`（**缺陷场景现已生效**）→ **原子替换**亦触发；日志同时印证细则 1（`watch_not_found` 被忽略）。
- **修正闭环**：Wave 3 暴露的「细则 2a 仅覆盖文件级」缺口已由 `f0fc8462` 中心修复（含递归目标），codegraph 的消费端权宜兜底已由 `00751bba` 回退；白盒验证见 §7.1.2。

## 需人工/环境承接的部分

1. **Linux（inotify）实跑**：本机为 macOS/kqueue；§7.1 集成测试需由 CI（`.github/workflows/rust.yml`，`ubuntu-latest`）实跑确认。
2. **真实编辑器原子保存体感**：用编辑器保存 `AGENTS.md`，确认 TUI 出现「已热重载」汇总提示。

## 已知限制（有意不做 / 记录在案）

- **kqueue 多文件同窗口创建漏报**：纯增量消费者（codegraph）在下次目录条目变化前不监听漏报文件，兜底为手动重建索引；daemon（具全量重扫能力）自然收敛。设计 §4.2 已文档化。
- **递归子树目录的条目级 Modify 不保证抑制**：消费者按幂等处理（daemon 多一次无语义重扫）。
- **不承诺项**（设计 §4.2 表）：mmap 写入、经非监听路径的写入（硬链接）、symlink 跟随、大小写不敏感匹配、监听资源上限（inotify `max_user_watches` 等，资源耗尽时**显式降级**）、跨/网络文件系统。
- **`append_system_prompt_template` 无去重**（关联的 `/reload` 特性遗留，另见其验收文档）：既有缺陷，不在本次范围。
- **daemon 的 Rescan 信号无领域粒度**：`visp_fs::WatchMessage::Rescan` 不带路径/领域，daemon 对 rules/skills/agents 做全量重扫（幂等，符合能力判据，但粒度较粗）。

## 回滚锚点

| 回滚项 | 锚点 |
|---|---|
| 自动热重载整体回退 | `daemon.toml [daemon].filewatcher = false`（重启生效），退回纯显式 `/reload` |
| AGENTS.md 边界语义 | `visp_config::agents_md_ancestors`（单一函数，加载器与监听计划共用） |
| 监听契约 | 集中在 `visp-fs`；两个消费者不含平台分支 |
