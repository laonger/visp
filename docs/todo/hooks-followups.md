# visp Hooks — 遗留与后续（follow-ups）

> 来源：hook 系统实施（`docs/design/2026-09-27-visp-hooks-design.md` v3.2 §20.2）
> 日期：2026-09-28
> 状态：一期（Phase 0 / 1a / 1b）已实施；以下为**已知限制**与**二期候选**。

## A. 已知限制（一期不修，需知悉）

| # | 项 | 说明 |
|---|---|---|
| A1 | ~~`[herdr]` 配置节未实现~~ | **已消解**：herdr 已回退为**外置脚本**（`assets/hooks/herdr.hook.sh` + 一条 `[hooks]` 规则，用户自装），无需 `[herdr]` 配置节。 |
| A2 | **`dropped` 计数口径不全** | 仅统计「drain 后到达」与「总线 `Lagged` 跳过」；**不含** executor 内部 `on_full` 队列溢出丢弃（`visp-hooks` 无可观测出口）。 |
| A3 | **关停 `SessionEnd` 为 daemon 级** | `session_id=""`；未按活跃会话逐一细化（`ShutdownRequest` 不带 session id，且历史会话会误发）。 |
| A4 | **`PermissionResult.outcome` 为推断** | 协议 `UserResponse` 无显式 cancel 标志；由 `selected_index`/`text` 推断 `selected`/`cancelled`。 |
| A5 | **seq 跨重启非严格递增** | `base = epoch_ms`；同毫秒重启 / 系统时钟回拨下可能回退（herdr 会丢弃旧序号）。 |
| A6 | **Windows 未验证** | 进程树终止依赖 unix `process_group`/`libc::kill`；非 unix 退化为 `start_kill`。 |
| A7 | **`PermissionResult` 的 `source`/`cwd` 为缺省值** | 未回查会话 `project_path`。 |
| A8 | **`HookRule→DispatchRule` 适配重复** | daemon 与 launcher 各一份（约 15 行）；建议下沉到 `visp-hooks` 单一实现。 |

## B. 二期候选（Phase 2 / 3）

| # | 项 | 说明 |
|---|---|---|
| B1 | **`2b` 受控 Pre 介入** | 在 `PreToolUse` 处增加可选同步 gate：仅**信任规则**、显式开启；**fail-open/closed 语义待定**（超时=放行还是拒绝）。设计 §5 D1。 |
| B2 | **`[herdr]` 配置节** | `enabled = auto|true|false`、`report_state`、`heartbeat_ms`、`source` 等（对应 herdr 设计 §3 决策 9）。 |
| B3 | **`--format claude` 适配层** | 让既有 Claude 脚本可迁移（一期仅「命名同族」，不可直接复用）。 |
| B4 | **`PreCompact`/`PostCompact`** | 语义尚未定义（当前仅「每请求窗口裁剪」，无「压缩发生」判定与历史改写）。 |
| B5 | **`SubagentStart`** | 与已落地的 `SubagentStop` 配对（锚点在 orchestrator spawn）。 |
| B6 | **MCP-as-hook-transport** | 把 hook 投递经 MCP 通道（可选）。 |
| B7 | **hook 配置热重载策略** | 当前显式不热重载（D11）；二期评估「全局可热重载、项目级不」。 |
| B8 | **无头/CI 非交互信任途径** | 环境变量预设 / 预设信任清单（否则项目 hook 在 CI 无法启用）。 |
| B9 | **seq 持久化高水位** | 若 herdr 依赖严格单调，可用小文件持久化 `last_seq`。 |
| B10 | **`Executor` 内部 `on_full` 丢弃计数出口** | 补齐 A2。 |

## C. 手工验收项（无法自动化）

- herdr 真实上报（`HERDR_ENV=1` 环境下 `herdr agent list`/`explain` 与 visp 状态一致）。
- daemon 优雅退出与 launcher 等待的体感（`Shutdown` → 退出，不悬挂）。
- `visp hooks doctor` 的「需重启」提示。
- 异常退出（`kill -9`）无 `SessionEnd`。
