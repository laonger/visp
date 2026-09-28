//! `visp hooks` 子命令：`list` / `doctor` / `test` / `logs`（设计 §15 / D10 / D11）。
//!
//! - `list`：列出规则（`id` / 来源 `global|project` / `enabled` / 信任状态）。
//!   静态读配置 + 信任存储；daemon 可达时附运行时计数。
//! - `doctor`：静态检查（配置可解析性、`command` 可解析性、`timeout_ms` 合理性、
//!   项目信任、env 白名单）+ 经只读 `GetHookStats` 的运行时计数（不可达则提示）
//!   + 「`[hooks]`/信任文件改动需重启」提示。
//! - `test <id|event>`：以**样例载荷**做 dry-run 匹配/打印；**默认且当前始终不 spawn**。
//! - `logs`：打印 hook 运行日志路径（daemon tracing 日志）与提示。
//!
//! 解析与匹配复用 `visp-config`（规则加载/merged/信任）与 `visp-hooks`（决策内核
//! [`select_matches`]），本模块只做适配与渲染。

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use tonic::transport::Endpoint;
use visp_proto::visp::GetHookStatsRequest;
use visp_proto::visp::coder_daemon_client::CoderDaemonClient;

use visp_config::path::{
    daemon_toml_global, daemon_toml_project, hook_trust_file, hooks_dir_project, log_dir,
};
use visp_config::{
    DaemonConfig, HookRule, HookScope, HookTrustStore, HooksConfig, OnFull, TrustStatus,
    load_config, merge_hooks, verify,
};
use visp_hooks::{DispatchInput, DispatchRule, HookEventName, QueuePolicy, select_matches};

/// `timeout_ms` 的「合理上界」：超过视为可疑（24h）。执行器另有 0 → 默认 60000 的语义。
const MAX_REASONABLE_TIMEOUT_MS: u64 = 24 * 60 * 60 * 1000;

// ============================ 加载 ============================

/// 严格解析 `[hooks]`（仅该节），用于 `doctor` 的「配置可解析性」检查。
///
/// 与加载路径的宽松降级不同：这里**不吞错**——未知 `event`、缺 `command`、重复 `id`
/// 等都会作为错误暴露，正是 `doctor` 的职责（设计 §1.2 把问题暴露交给 doctor）。
#[derive(Debug, Deserialize)]
struct StrictDoc {
    #[serde(default)]
    hooks: HooksConfig,
}

fn strict_hooks(path: &Path) -> Result<HooksConfig, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    let doc: StrictDoc = toml::from_str(&text).map_err(|e| format!("invalid TOML: {e}"))?;
    doc.hooks
        .validate()
        .map_err(|e| format!("invalid [hooks]: {e}"))?;
    Ok(doc.hooks)
}

/// 合并后的规则集（全局 + 项目，`scope` 已标注），损坏项已在加载层剔除。
fn load_merged_rules(project: &Path) -> Result<Vec<HookRule>, String> {
    let mut base = match daemon_toml_global() {
        Some(path) if path.exists() => load_config(Some(&path))?,
        _ => DaemonConfig::default(),
    };
    let project_file = daemon_toml_project(project);
    if project_file.exists() {
        let project_config = load_config(Some(&project_file))?;
        merge_hooks(&mut base.hooks, &project_config.hooks);
    }
    Ok(base.hooks.rules)
}

/// 加载结果：生效规则 + 严格解析错误 + 项目信任判定。
pub struct LoadedHooks {
    /// 合并后的规则（全局在前，项目覆盖同名全局）。
    pub rules: Vec<HookRule>,
    /// 严格解析/校验错误（仅含存在且非法的配置文件）。
    pub parse_errors: Vec<String>,
    /// 项目级规则信任状态。
    pub trust: TrustStatus,
}

/// 从配置与信任存储加载 hook 视图（不依赖 daemon）。
pub fn load_hooks(project: &Path) -> LoadedHooks {
    let mut parse_errors = Vec::new();
    if let Some(global) = daemon_toml_global()
        && global.exists()
        && let Err(e) = strict_hooks(&global)
    {
        parse_errors.push(format!("global: {e}"));
    }
    let project_file = daemon_toml_project(project);
    if project_file.exists()
        && let Err(e) = strict_hooks(&project_file)
    {
        parse_errors.push(format!("project: {e}"));
    }

    let rules = load_merged_rules(project).unwrap_or_else(|e| {
        parse_errors.push(format!("load: {e}"));
        Vec::new()
    });
    let store = HookTrustStore::load();
    let trust = verify(project, &rules, &store);
    LoadedHooks {
        rules,
        parse_errors,
        trust,
    }
}

// ============================ 规则适配 ============================

/// `HookRule` → [`DispatchRule`]（仅匹配/排序/队列字段；执行字段不进决策内核）。
pub fn dispatch_rule(rule: &HookRule) -> DispatchRule {
    DispatchRule {
        id: rule.id.clone(),
        order: rule.order,
        event: rule.event.clone(),
        matcher: rule.matcher.clone(),
        enabled: rule.enabled,
        on_full: match rule.on_full {
            OnFull::DropNew => QueuePolicy::DropNew,
            OnFull::DropOld => QueuePolicy::DropOld,
            OnFull::CoalesceLatest => QueuePolicy::CoalesceLatest,
        },
        parallel: rule.parallel,
        cooldown_ms: rule.cooldown_ms,
    }
}

// ============================ list ============================

/// 渲染 `hooks list`（读配置 + 信任存储）。
pub fn render_list(project: &Path, stats: Option<&HookStatsView>) -> String {
    let loaded = load_hooks(project);
    render_list_from(&loaded.rules, &loaded.trust, stats)
}

/// 渲染 `hooks list`（注入规则集，便于测试）。
pub fn render_list_from(
    rules: &[HookRule],
    trust: &TrustStatus,
    stats: Option<&HookStatsView>,
) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "{:<28} {:<9} {:<8} trust", "id", "source", "enabled");
    let _ = writeln!(out, "{}", "-".repeat(64));
    for rule in rules {
        let (source, trust_col) = match rule.scope {
            HookScope::Global => ("global", "n/a".to_string()),
            HookScope::Project => ("project", trust_label(trust)),
        };
        let _ = writeln!(
            out,
            "{:<28} {:<9} {:<8} {}",
            rule.id, source, rule.enabled, trust_col
        );
    }
    match stats {
        Some(s) => {
            let _ = writeln!(
                out,
                "\nruntime: emitted={} dropped={} executed={} failed={} timed_out={}",
                s.emitted, s.dropped, s.executed, s.failed, s.timed_out
            );
        }
        None => {
            let _ = writeln!(
                out,
                "\nruntime: daemon unreachable — static view only (start visp, or pass --addr)"
            );
        }
    }
    out
}

fn trust_label(trust: &TrustStatus) -> String {
    match trust {
        TrustStatus::NotRequired => "n/a".to_string(),
        TrustStatus::Trusted => "trusted".to_string(),
        TrustStatus::Untrusted(_) => "untrusted".to_string(),
    }
}

// ============================ doctor ============================

/// `doctor` 报告：文本 + 是否存在失败项（失败时退出码非零）。
pub struct DoctorReport {
    /// 人类可读报告。
    pub text: String,
    /// 是否存在 `FAIL` 项。
    pub has_failures: bool,
}

/// 渲染 `hooks doctor`（读配置、信任存储；可附运行时计数）。
pub fn render_doctor(project: &Path, stats: Option<&HookStatsView>) -> DoctorReport {
    let loaded = load_hooks(project);
    doctor_from(
        project,
        &loaded.rules,
        &loaded.trust,
        &loaded.parse_errors,
        stats,
    )
}

/// 渲染 `hooks doctor`（注入各输入，便于测试）。
pub fn doctor_from(
    project: &Path,
    rules: &[HookRule],
    trust: &TrustStatus,
    parse_errors: &[String],
    stats: Option<&HookStatsView>,
) -> DoctorReport {
    let mut out = String::new();
    let mut has_failures = false;
    let _ = writeln!(out, "visp hooks doctor — project {}", project.display());

    // 1. 配置可解析性（严格解析）。
    if parse_errors.is_empty() {
        let _ = writeln!(out, "[PASS] config: [hooks] parsed");
    } else {
        for error in parse_errors {
            check(&mut out, &mut has_failures, "FAIL", "config", error);
        }
    }

    // 2. 逐规则：command 可解析性 + timeout_ms 合理性。
    if rules.is_empty() {
        let _ = writeln!(out, "[PASS] rules: no hook rules configured");
    }
    for rule in rules {
        match check_command(project, rule) {
            Ok(()) => {
                let _ = writeln!(
                    out,
                    "[PASS] command `{}`: `{}` resolvable",
                    rule.id, rule.command
                );
            }
            Err(e) => check(&mut out, &mut has_failures, "FAIL", "command", &e),
        }
        if rule.timeout_ms == 0 {
            let _ = writeln!(
                out,
                "[WARN] timeout `{}`: 0 means executor default 60000ms",
                rule.id
            );
        } else if rule.timeout_ms > MAX_REASONABLE_TIMEOUT_MS {
            let _ = writeln!(
                out,
                "[WARN] timeout `{}`: {}ms exceeds 24h, check it is intended",
                rule.id, rule.timeout_ms
            );
        } else {
            let _ = writeln!(out, "[PASS] timeout `{}`: {}ms", rule.id, rule.timeout_ms);
        }
    }

    // 3. 项目信任状态。
    match trust {
        TrustStatus::NotRequired => {
            let _ = writeln!(out, "[PASS] trust: no project rules, trust not required");
        }
        TrustStatus::Trusted => {
            let _ = writeln!(out, "[PASS] trust: project rules trusted");
        }
        TrustStatus::Untrusted(reason) => {
            check(
                &mut out,
                &mut has_failures,
                "FAIL",
                "trust",
                &format!("project rules untrusted: {reason}"),
            );
        }
    }

    // 4. env 白名单（执行器强制；此处为信息项）。
    let _ = writeln!(
        out,
        "[INFO] env: executor passes only PATH HOME TERM LANG VISP_* HERDR_* plus per-rule env"
    );

    // 5. 运行时计数（只读 RPC）。
    match stats {
        Some(s) => {
            let _ = writeln!(
                out,
                "[INFO] runtime: emitted={} dropped={} executed={} failed={} timed_out={}",
                s.emitted, s.dropped, s.executed, s.failed, s.timed_out
            );
        }
        None => {
            let _ = writeln!(
                out,
                "[WARN] runtime: daemon unreachable; counters unavailable (start visp, or pass --addr)"
            );
        }
    }

    // 6. 热重载提示（设计 D11：一期不热重载）。
    let _ = writeln!(
        out,
        "[INFO] reload: [hooks] and hook-trust.toml changes require a daemon restart (no hot reload)"
    );

    DoctorReport {
        text: out,
        has_failures,
    }
}

fn check(out: &mut String, has_failures: &mut bool, status: &str, name: &str, detail: &str) {
    if status == "FAIL" {
        *has_failures = true;
    }
    let _ = writeln!(out, "[{status}] {name}: {detail}");
}

/// 校验规则的 `command` 是否可解析。
///
/// - 项目级：必须 canonicalize 到 `.visp/hooks/` 目录内（设计 §7.2 / §8.2）。
///   非法 shell 调用由信任判定（[`verify`]）单独暴露，不在此重复。
/// - 全局：绝对/含路径分隔符 → 检查文件存在；否则在 `PATH` 中查找。
fn check_command(project: &Path, rule: &HookRule) -> Result<(), String> {
    if rule.command.trim().is_empty() {
        return Err(format!("rule `{}` has an empty command", rule.id));
    }
    if rule.scope == HookScope::Project {
        let candidate = if Path::new(&rule.command).is_absolute() {
            PathBuf::from(&rule.command)
        } else {
            project.join(&rule.command)
        };
        let resolved = candidate
            .canonicalize()
            .map_err(|e| format!("`{}` cannot be resolved: {e}", rule.command))?;
        let hooks_dir = hooks_dir_project(project)
            .canonicalize()
            .map_err(|_| format!("`{}` does not resolve inside .visp/hooks/", rule.command))?;
        if !resolved.starts_with(&hooks_dir) {
            return Err(format!(
                "`{}` does not resolve inside .visp/hooks/",
                rule.command
            ));
        }
        return Ok(());
    }

    if rule.command.contains('/') {
        if Path::new(&rule.command).is_file() {
            Ok(())
        } else {
            Err(format!("`{}` not found", rule.command))
        }
    } else if command_in_path(&rule.command) {
        Ok(())
    } else {
        Err(format!("`{}` not found in PATH", rule.command))
    }
}

fn command_in_path(command: &str) -> bool {
    std::env::var_os("PATH")
        .map(|paths| std::env::split_paths(&paths).any(|dir| dir.join(command).is_file()))
        .unwrap_or(false)
}

// ============================ test ============================

/// 渲染 `hooks test`（读配置）。
pub fn render_test(project: &Path, target: &str, dry_run: bool) -> Result<String, String> {
    let loaded = load_hooks(project);
    render_test_from(project, &loaded.rules, target, dry_run)
}

/// 渲染 `hooks test`（注入规则集，便于测试）。
///
/// **dry-run 语义**：本实现永不 spawn 子进程——仅匹配并打印将执行什么。
pub fn render_test_from(
    project: &Path,
    rules: &[HookRule],
    target: &str,
    dry_run: bool,
) -> Result<String, String> {
    let mut out = String::new();
    let mode = if dry_run {
        "dry-run (--dry-run: no process is spawned)"
    } else {
        "dry-run by default (no process is spawned)"
    };
    let _ = writeln!(out, "visp hooks test `{target}` — {mode}");

    if let Some(event) = parse_event(target) {
        let input = sample_input(event);
        let dispatch: Vec<DispatchRule> = rules.iter().map(dispatch_rule).collect();
        let matched = select_matches(&dispatch, &input);
        let _ = writeln!(
            out,
            "event `{}` -> {} matching rule(s)",
            event.as_str(),
            matched.len()
        );
        if matched.is_empty() {
            let _ = writeln!(out, "  (no rules matched)");
        }
        for selected in &matched {
            if let Some(rule) = rules.iter().find(|r| r.id == selected.id) {
                let _ = writeln!(
                    out,
                    "  would run: {} {}  [id={}, source={:?}]",
                    rule.command,
                    rule.args.join(" "),
                    rule.id,
                    rule.scope
                );
            }
        }
        let disabled: Vec<&str> = rules
            .iter()
            .filter(|r| !r.enabled && r.event.contains(&event))
            .map(|r| r.id.as_str())
            .collect();
        if !disabled.is_empty() {
            let _ = writeln!(out, "  (disabled rules skipped: {})", disabled.join(", "));
        }
    } else if let Some(rule) = rules.iter().find(|r| r.id == target) {
        let state = if rule.enabled { "enabled" } else { "disabled" };
        let _ = writeln!(out, "rule `{}` ({:?}, {state})", rule.id, rule.scope);
        let _ = writeln!(out, "  command: {} {}", rule.command, rule.args.join(" "));
        match rule.event.first() {
            Some(event) => {
                let input = sample_input(*event);
                let dispatch: Vec<DispatchRule> = rules
                    .iter()
                    .filter(|r| r.id == rule.id)
                    .map(dispatch_rule)
                    .collect();
                let matched = select_matches(&dispatch, &input);
                if matched.is_empty() {
                    let _ = writeln!(
                        out,
                        "  would not run for sample event `{}` (disabled or matcher miss)",
                        event.as_str()
                    );
                } else {
                    let _ = writeln!(
                        out,
                        "  would run for sample event `{}`: {} {}",
                        event.as_str(),
                        rule.command,
                        rule.args.join(" ")
                    );
                }
            }
            None => {
                let _ = writeln!(out, "  (rule has no event)");
            }
        }
    } else {
        return Err(format!(
            "unknown hook event or rule id `{target}` (project {})",
            project.display()
        ));
    }

    let _ = writeln!(
        out,
        "note: sample payload is synthetic (tool_name=Bash / source=startup / kind=approval)"
    );
    Ok(out)
}

fn parse_event(target: &str) -> Option<HookEventName> {
    HookEventName::ALL
        .iter()
        .copied()
        .find(|event| event.as_str() == target)
}

/// 构造样例事件视图；匹配维度为合成值（缺失维度的规则仍可能因 `matcher` 不命中）。
fn sample_input(event: HookEventName) -> DispatchInput {
    let input = DispatchInput::new(event);
    match event {
        HookEventName::PreToolUse
        | HookEventName::PostToolUse
        | HookEventName::PostToolUseFailure
        | HookEventName::ToolCallRequested => input.with_tool_name("Bash"),
        HookEventName::SessionStart => input.with_source("startup"),
        HookEventName::PermissionRequest => input.with_kind("approval"),
        _ => input,
    }
}

// ============================ logs ============================

/// 渲染 `hooks logs`：hook 运行日志走 daemon tracing，无独立文件；打印路径与提示。
pub fn render_logs() -> String {
    let mut out = String::new();
    let _ = writeln!(out, "visp hook runtime logs (daemon tracing)");
    match log_dir() {
        Some(dir) => {
            let mut logs: Vec<PathBuf> = Vec::new();
            if let Ok(read) = std::fs::read_dir(&dir) {
                for entry in read.flatten() {
                    let path = entry.path();
                    let is_daemon_log = path
                        .file_name()
                        .and_then(|name| name.to_str())
                        .is_some_and(|name| name.starts_with("daemon-") && name.ends_with(".log"));
                    if is_daemon_log {
                        logs.push(path);
                    }
                }
            }
            logs.sort();
            logs.reverse(); // 文件名含时间戳，倒序即最新在前。
            let _ = writeln!(out, "  log dir: {}", dir.display());
            if logs.is_empty() {
                let _ = writeln!(out, "  no daemon log in {} yet", dir.display());
            } else {
                for path in &logs {
                    let _ = writeln!(out, "  {}", path.display());
                }
            }
            let target = logs
                .first()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| dir.display().to_string());
            let _ = writeln!(
                out,
                "  hint: hook events are logged via tracing; search e.g. `grep -i hook {target}`"
            );
        }
        None => {
            let _ = writeln!(out, "  log dir unavailable (HOME not set)");
        }
    }
    if let Some(trust_file) = hook_trust_file() {
        let _ = writeln!(out, "  trust file: {}", trust_file.display());
    }
    let _ = writeln!(
        out,
        "  no dedicated hook log file is written; run `visp hooks doctor` for counters"
    );
    out
}

// ============================ 只读 RPC ============================

/// 生效规则摘要视图（`GetHookStats`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HookRuleSummaryView {
    /// 规则 id。
    pub id: String,
    /// 来源作用域（`global` / `project`）。
    pub scope: String,
    /// 是否启用。
    pub enabled: bool,
}

/// `GetHookStats` 只读统计视图（设计 §15：五类计数 + 生效规则）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HookStatsView {
    /// 进入运行时的事件数。
    pub emitted: u64,
    /// 丢弃的事件数。
    pub dropped: u64,
    /// 正常退出次数。
    pub executed: u64,
    /// 失败次数。
    pub failed: u64,
    /// 超时被杀次数。
    pub timed_out: u64,
    /// 生效规则摘要。
    pub rules: Vec<HookRuleSummaryView>,
}

impl From<visp_proto::visp::HookStatsResponse> for HookStatsView {
    fn from(response: visp_proto::visp::HookStatsResponse) -> Self {
        Self {
            emitted: response.emitted,
            dropped: response.dropped,
            executed: response.executed,
            failed: response.failed,
            timed_out: response.timed_out,
            rules: response
                .rules
                .into_iter()
                .map(|summary| HookRuleSummaryView {
                    id: summary.id,
                    scope: summary.scope,
                    enabled: summary.enabled,
                })
                .collect(),
        }
    }
}

/// 经只读 `GetHookStats` 读取运行时计数；daemon 不可达或超时返回 `None`（优雅降级）。
pub async fn fetch_stats(addr: &str) -> Option<HookStatsView> {
    let endpoint = format!("http://{addr}");
    let request = async {
        let channel = Endpoint::from_shared(endpoint)
            .ok()?
            .connect_timeout(std::time::Duration::from_millis(800))
            .timeout(std::time::Duration::from_millis(1500))
            .connect()
            .await
            .ok()?;
        let mut client = CoderDaemonClient::new(channel);
        let response = client
            .get_hook_stats(GetHookStatsRequest::default())
            .await
            .ok()?
            .into_inner();
        Some(HookStatsView::from(response))
    };
    tokio::time::timeout(std::time::Duration::from_secs(2), request)
        .await
        .ok()
        .flatten()
}

// ============================ 入口 ============================

/// 执行 `visp hooks <action>`；返回进程退出码。
pub async fn run(action: &crate::HooksAction, project: &Path, addr: &str, dry_run: bool) -> i32 {
    match action {
        crate::HooksAction::List => {
            let stats = fetch_stats(addr).await;
            print!("{}", render_list(project, stats.as_ref()));
            0
        }
        crate::HooksAction::Doctor => {
            let stats = fetch_stats(addr).await;
            let report = render_doctor(project, stats.as_ref());
            print!("{}", report.text);
            if report.has_failures { 1 } else { 0 }
        }
        crate::HooksAction::Test { target } => match render_test(project, target, dry_run) {
            Ok(report) => {
                print!("{report}");
                0
            }
            Err(e) => {
                eprintln!("[visp hooks test] {e}");
                2
            }
        },
        crate::HooksAction::Logs => {
            print!("{}", render_logs());
            0
        }
    }
}

// ============================ tests ============================

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn rule(id: &str, scope: HookScope, command: &str) -> HookRule {
        HookRule {
            id: id.to_string(),
            order: None,
            event: vec![HookEventName::Stop],
            matcher: None,
            command: command.to_string(),
            args: Vec::new(),
            env: Default::default(),
            cwd: None,
            timeout_ms: 60_000,
            enabled: true,
            on_full: OnFull::DropNew,
            parallel: false,
            cooldown_ms: 0,
            include: Vec::new(),
            scope,
        }
    }

    /// 写一个可执行脚本（内容为 `body`）。
    fn write_exec(path: &Path, body: &str) {
        std::fs::write(path, body).unwrap();
        let mut perms = std::fs::metadata(path).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(path, perms).unwrap();
    }

    /// 构造带合法项目钩子目录的临时项目。
    fn project_with_hook_script() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let hooks_dir = dir.path().join(".visp/hooks");
        std::fs::create_dir_all(&hooks_dir).unwrap();
        let script = hooks_dir.join("hook.sh");
        write_exec(&script, "#!/bin/sh\ntrue\n");
        (dir, script)
    }

    #[test]
    fn list_shows_source_and_trust_columns() {
        let (dir, _) = project_with_hook_script();
        let rules = vec![
            rule("g1", HookScope::Global, "/bin/echo"),
            rule("p1", HookScope::Project, "hook.sh"),
        ];
        let trust = verify(dir.path(), &rules, &HookTrustStore::default());
        assert!(matches!(trust, TrustStatus::Untrusted(_)));
        let out = render_list_from(&rules, &trust, None);
        assert!(out.contains("id"), "{out}");
        assert!(out.contains("source"), "{out}");
        assert!(out.contains("enabled"), "{out}");
        assert!(out.contains("trust"), "{out}");
        assert!(out.contains("global"), "{out}");
        assert!(out.contains("project"), "{out}");
        assert!(out.contains("untrusted"), "{out}");
    }

    #[test]
    fn list_degrades_gracefully_without_daemon() {
        let rules = vec![rule("g1", HookScope::Global, "/bin/echo")];
        let out = render_list_from(&rules, &TrustStatus::NotRequired, None);
        assert!(out.contains("daemon unreachable"), "{out}");
    }

    #[test]
    fn list_prints_runtime_counters_when_available() {
        let rules = vec![rule("g1", HookScope::Global, "/bin/echo")];
        let stats = HookStatsView {
            emitted: 7,
            dropped: 1,
            executed: 4,
            failed: 2,
            timed_out: 3,
            rules: Vec::new(),
        };
        let out = render_list_from(&rules, &TrustStatus::NotRequired, Some(&stats));
        assert!(out.contains("emitted=7"), "{out}");
        assert!(out.contains("timed_out=3"), "{out}");
    }

    #[test]
    fn test_default_is_dry_run_and_does_not_spawn() {
        let (dir, _) = project_with_hook_script();
        let sentinel = dir.path().join("sentinel");
        let script = dir.path().join(".visp/hooks/run.sh");
        write_exec(
            &script,
            &format!("#!/bin/sh\ntouch {}\n", sentinel.display()),
        );

        let mut r = rule("p-run", HookScope::Project, "run.sh");
        r.event = vec![HookEventName::Stop];
        let out = render_test_from(dir.path(), &[r], "Stop", false).unwrap();
        assert!(out.contains("would run"), "{out}");
        assert!(out.contains("dry-run"), "{out}");
        assert!(
            !sentinel.exists(),
            "dry-run must not spawn the hook process"
        );
    }

    #[test]
    fn test_matches_by_event_with_sample_tool_name() {
        let (dir, _) = project_with_hook_script();
        let mut r = rule("pre-bash", HookScope::Global, "/bin/echo");
        r.event = vec![HookEventName::PreToolUse];
        r.matcher = Some("^Bash$".to_string());
        let out = render_test_from(dir.path(), &[r], "PreToolUse", true).unwrap();
        assert!(out.contains("--dry-run"), "{out}");
        assert!(out.contains("1 matching rule(s)"), "{out}");
        assert!(out.contains("pre-bash"), "{out}");
    }

    #[test]
    fn test_matches_by_rule_id() {
        let (dir, _) = project_with_hook_script();
        let r = rule("g1", HookScope::Global, "/bin/echo");
        let out = render_test_from(dir.path(), &[r], "g1", true).unwrap();
        assert!(out.contains("rule `g1`"), "{out}");
        assert!(out.contains("would run"), "{out}");
    }

    #[test]
    fn test_unknown_target_errors() {
        let (dir, _) = project_with_hook_script();
        let err = render_test_from(dir.path(), &[], "NoSuchThing", true).unwrap_err();
        assert!(err.contains("unknown hook event or rule id"), "{err}");
    }

    #[test]
    fn doctor_detects_invalid_config() {
        let (dir, _) = project_with_hook_script();
        let report = doctor_from(
            dir.path(),
            &[],
            &TrustStatus::NotRequired,
            &["global: invalid [hooks]: unknown variant `NotAnEvent`".to_string()],
            None,
        );
        assert!(report.has_failures, "{}", report.text);
        assert!(report.text.contains("FAIL"), "{}", report.text);
        assert!(report.text.contains("NotAnEvent"), "{}", report.text);
    }

    #[test]
    fn doctor_detects_untrusted_project_rules() {
        let (dir, _) = project_with_hook_script();
        let rules = vec![rule("p1", HookScope::Project, "hook.sh")];
        let trust = verify(dir.path(), &rules, &HookTrustStore::default());
        let report = doctor_from(dir.path(), &rules, &trust, &[], None);
        assert!(report.has_failures, "{}", report.text);
        assert!(report.text.contains("untrusted"), "{}", report.text);
    }

    #[test]
    fn doctor_detects_project_command_outside_hooks_dir() {
        let (dir, _) = project_with_hook_script();
        let rules = vec![rule("p1", HookScope::Project, "/bin/echo")];
        let trust = verify(dir.path(), &rules, &HookTrustStore::default());
        let report = doctor_from(dir.path(), &rules, &trust, &[], None);
        assert!(report.has_failures, "{}", report.text);
        assert!(report.text.contains(".visp/hooks/"), "{}", report.text);
    }

    #[test]
    fn doctor_degrades_gracefully_without_daemon() {
        let (dir, _) = project_with_hook_script();
        let report = doctor_from(dir.path(), &[], &TrustStatus::NotRequired, &[], None);
        assert!(
            report.text.contains("daemon unreachable"),
            "{}",
            report.text
        );
        assert!(report.text.contains("restart"), "{}", report.text);
        assert!(!report.has_failures, "{}", report.text);
    }

    #[test]
    fn doctor_warns_on_zero_timeout() {
        let (dir, _) = project_with_hook_script();
        let mut r = rule("g1", HookScope::Global, "/bin/echo");
        r.timeout_ms = 0;
        let report = doctor_from(dir.path(), &[r], &TrustStatus::NotRequired, &[], None);
        assert!(report.text.contains("WARN"), "{}", report.text);
        assert!(!report.has_failures, "{}", report.text);
    }

    #[test]
    fn logs_report_includes_path_and_hint() {
        let out = render_logs();
        assert!(out.contains("hook runtime logs"), "{out}");
        assert!(
            out.contains("log dir") || out.contains("log dir unavailable"),
            "{out}"
        );
        assert!(out.contains("hint"), "{out}");
    }

    #[tokio::test]
    async fn fetch_stats_returns_none_when_daemon_unreachable() {
        // 取一个空闲端口后立即释放，确保连接被拒绝。
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        drop(listener);
        assert!(fetch_stats(&addr).await.is_none());
    }

    #[test]
    fn dispatch_rule_adapter_maps_event_and_queue_policy() {
        let mut r = rule("g1", HookScope::Global, "/bin/echo");
        r.on_full = OnFull::CoalesceLatest;
        r.order = Some(5);
        r.cooldown_ms = 250;
        let d = dispatch_rule(&r);
        assert_eq!(d.id, "g1");
        assert_eq!(d.order, Some(5));
        assert_eq!(d.event, vec![HookEventName::Stop]);
        assert_eq!(d.on_full, QueuePolicy::CoalesceLatest);
        assert_eq!(d.cooldown_ms, 250);
    }
}
