# 验收核对清单：/reload、自动文件监听热重载与 TUI 自动重连

- 日期：2026-09-25
- 依据设计：`docs/design/2026-09-24-reload-design.md`（v3.1）§10 的 23 条验收
- 依据计划：`docs/plans/2026-09-24-reload-plan.md`
- 全量质量门：`cargo test --workspace` ✅ · `cargo clippy --workspace --all-targets -- -D warnings` ✅ · `cargo fmt -- --check` ✅

## 覆盖总览

| 分类 | 条目 | 自动化覆盖 | 需手工验收 |
|---|---|---|---|
| 10.1 显式 `/reload` | 1–6 | 6/6（其中 1、4 的最终生效时机宜辅以日志/手工确认） | 1、4 |
| 10.2 自动热重载 | 7–15 | 9/9（其中 7–9 的真实编辑器体感属手工） | 7–9 |
| 10.3 重连与看门狗 | 16–19 | 4/4（真实 daemon 重启 / SIGSTOP 属手工） | 16、18a |
| 10.4 质量门 | 20–23 | 4/4 | — |

## 10.1 显式 `/reload`

| # | 验收项 | 覆盖证据 | 判定 |
|---|---|---|---|
| 1 | rules 即时生效（L3） | 3a `rules_item_reports_success_and_stats`；L3「下一轮迭代」由既有 `agent_loop` 每轮读取保证（3b 消费 `RuleEngine::reload().changed`） | ✅ 自动化（生效时机建议日志确认） |
| 2 | skills 列表生效（L3） | 3a `skills_item_rebuilds_tool_description`（重建 SkillTool + ToolRegistry::update） | ✅ 自动化 |
| 3 | agents 生效与对账一致（L2） | 3c 六例（含 `tool_set_matches_subagent_set_after_reconcile`）；L2 生效由 2b `test_agent_registry_swap_takes_effect_on_next_spawn` | ✅ 自动化 |
| 4 | base 模板只对新 session 生效（L1） | 4a `reload_config_passes_through_system_prompt_receipt`；reload 流程无 session 写路径 | ✅ 自动化（模板逐字节不变建议单测/日志确认） |
| 5 | 部分失败隔离 | 3a `partial_failure_is_isolated_to_agents_skip`；4a `reload_config_partial_failure_returns_ok_response` | ✅ 自动化 |
| 6 | 结果反馈 | 5a `test_reload_results_render_per_item_styles` 等 4 例 + `test_reload_listed_in_hint_and_help_with_daemon_note` | ✅ 自动化 |

## 10.2 自动热重载

| # | 验收项 | 覆盖证据 | 判定 |
|---|---|---|---|
| 7 | 保存即生效（rules） | 6b `writing_agents_md_triggers_rules_reload`（集成） | ✅ 自动化（真实编辑器体感手工） |
| 8 | 保存即生效（skills） | 6b 领域过滤/跨领域聚合用例；3a skills 重建用例 | ✅ 自动化（技能目录新建的真机体感手工） |
| 9 | 保存即生效（agents） | 6b 领域映射用例；3c 对账用例 | ✅ 自动化（真机体感手工） |
| 10 | 变化守卫 | 3b 六例（含无变化不替换副作用）；6b `excluded_files_produce_no_reload` | ✅ 自动化 |
| 11 | 自动失败不伤状态 | 3a 部分失败；3b 无变化保护；6c `failure_pushes_error_status` | ✅ 自动化 |
| 12 | 开关 | 6d `filewatcher_defaults_enabled` / `filewatcher_explicit_false` / `filewatcher_factory_disabled_returns_none` + 启动冒烟 | ✅ 自动化 + 冒烟 |
| 13 | 原子写收敛 | 6b `atomic_write_converges_to_rules_reload` | ✅ 自动化 |
| 14 | 缺目录场景 | 6a `fresh_project_without_visp_uses_root_fallback` / `missing_subdirs_fall_back_to_nearest_ancestor`；6b `missing_dir_is_remounted_and_rescanned` | ✅ 自动化 |
| 15 | 9 字段守卫 | 3b `agents_permission_only_change_is_detected` / `agents_system_prompt_only_change_is_detected`（基于 1b 的全字段 `PartialEq`） | ✅ 自动化 |

## 10.3 重连与 idle 看门狗

| # | 验收项 | 覆盖证据 | 判定 |
|---|---|---|---|
| 16 | 自动重连 | 7b 七例（不再退出 / 回放 / 刷模型 / 清残留）；7d 四例（丢 subagent tab / 复位 per-tab / 保留主 tab / 提示） | ✅ 自动化（真实 daemon 重启端到端手工） |
| 17 | 重连期间交互 | 7b `test_input_blocked_while_reconnecting` / `test_input_works_after_reconnected`；exit 通道全程保留 | ✅ 自动化 |
| 18 | idle 看门狗 | 7a/7c：探测失败→重连、探测成功→重置稳态、陈旧结果忽略、常量集中（负向断言「连续多次探测成功不重建」） | ✅ 自动化（SIGSTOP/半开真机手工，验收 18a） |
| 19 | 共用状态机 | 7a `transition` 纯函数测试 + 7c event 级接线测试 | ✅ 自动化 |

## 10.4 质量门

| # | 验收项 | 证据 | 判定 |
|---|---|---|---|
| 20 | 新增逻辑均有单测 | 各子步骤报告；本清单上表逐条可溯 | ✅ |
| 21 | TUI 重连状态机与看门狗可测 | `connection_tests.rs` 20 例（注入时钟/伪连接）+ event 级 10 例 | ✅ |
| 22 | 文件监听集成测试 | `crates/visp-daemon/tests/filewatch_integration.rs` 8 例（「最终收敛」断言） | ✅ |
| 23 | `cargo test` / clippy / fmt 全绿 | workspace 全量：`cargo test --workspace` ✅、`cargo clippy --workspace --all-targets -- -D warnings` ✅、`fmt -- --check` ✅ | ✅（收尾修复了 visp-proto 测试在 `--all-targets` 下的 2 处告警） |

## 需人工验收的部分（自动化无法覆盖）

1. **验收 7–9（保存即生效的真机体感）**：用真实编辑器修改项目 `AGENTS.md` / skills 目录 / agent 定义并保存，观察 TUI 主 tab 出现「已热重载」汇总提示、约 1s 内生效。
2. **验收 12 + §9 回滚**：把 `daemon.toml` 的 `[daemon].filewatcher` 置 `false` 并重启 daemon，确认文件修改不再触发自动重载、`/reload` 仍工作。
3. **验收 16（daemon 重启恢复）**：真实重启 daemon，确认 TUI 不退出、状态栏显示重连尝试、恢复后自动回放主 session 历史、`/model` 为最新列表。
4. **验收 18a（看门狗检出）**：`kill -STOP <daemon_pid>` 或切断连接，确认 TUI 在 idle 45s + 探测 5s 量级内检出并自动重连恢复（正常空闲时不应产生任何重连）。

## 已知限制（有意不做 / 记录在案）

- **`append_system_prompt_template` 无去重**（`session.rs:417-426`）：既有缺陷，设计 §5.4 明确本次不修；reload 会放大但不在本范围。**建议单独立项**。
- **变更计数粒度**：proto `Item` 有 4 个细分计数，核心仅提供聚合 `changes`（映射到 `modified`，其余为 0；摘要由 `message` 文本承载）。对用户可见行为无影响（见计划备注 17）。
- **daemon.toml 不在热重载范围**：providers/MCP/tool 参数仍需重启 daemon（设计 §1.3 非目标）。
- **system-prompt.md 不在自动监听范围**：仅显式 `/reload`，且只对新 session 生效（L1）。

## 回滚锚点（§9 核验）

| 回滚项 | 锚点 | 核验 |
|---|---|---|
| 自动热重载整体回退 | `daemon.toml [daemon].filewatcher = false`（`config.rs:156-157`，默认 `true`，重启生效）；门控在 `main.rs:628` `start_file_watcher(...)` | ✅ 已存在 |
| 看门狗参数回退 | `connection.rs:25-31` 集中定义 `IDLE_TIMEOUT=45s` / `PROBE_TIMEOUT=5s` / `BACKOFF_INITIAL=1s` / `BACKOFF_MAX=30s` | ✅ 已存在，`event.rs` 无内联魔数 |
