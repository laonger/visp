//! `[hooks]` 配置节：规则模型、合并与校验（设计 §7）。
//!
//! 本模块只负责**配置形态**：反序列化、全局/项目合并（`scope` 标注）、
//! 规则校验。规则匹配/执行属 `visp-hooks` 执行器，不在此处。

use std::collections::HashMap;
use std::fmt;

use serde::{Deserialize, Serialize};

pub use visp_hooks::HookEventName;

/// 每条规则的默认超时（毫秒）。
///
/// 设计 §7.2 要求「每规则强制 `timeout_ms`」，但未给出具体默认值；
/// 一期锁定为 60s（与 Claude 族 hook 惯例一致）。
pub const DEFAULT_HOOK_TIMEOUT_MS: u64 = 60_000;

/// 规则作用域来源：由加载来源标注，**不由用户在 TOML 中书写**。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HookScope {
    /// 全局配置（`~/.config/visp/daemon.toml`）。
    #[default]
    Global,
    /// 项目配置（`{project}/.visp/daemon.toml`）。
    Project,
}

/// 规则队列溢出策略（设计 D4 / §7.2）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OnFull {
    /// 丢新（默认）。
    #[default]
    DropNew,
    /// 丢旧。
    DropOld,
    /// 合并为最新（状态型规则用）。
    CoalesceLatest,
}

/// 单条 hook 规则（字段严格对齐设计 §7.2）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HookRule {
    /// 规则唯一名（内置规则以 `builtin:` 前缀）；决定同事件多规则顺序（字典序）。
    pub id: String,
    /// 显式顺序覆盖（缺省时按 `id` 字典序）。
    #[serde(default)]
    pub order: Option<i64>,
    /// 命中事件名（可多选）。
    pub event: Vec<HookEventName>,
    /// 正则匹配 `tool_name`/`source`/`kind`；缺省（None）= 全匹配。
    #[serde(default)]
    pub matcher: Option<String>,
    /// argv 首元素（无 shell）；项目级须位于 `.visp/hooks/` 内。
    pub command: String,
    /// argv 参数。
    #[serde(default)]
    pub args: Vec<String>,
    /// 规则显式注入的环境变量（叠加白名单）。
    #[serde(default)]
    pub env: HashMap<String, String>,
    /// 工作目录（缺省时由执行器取 project_path）。
    #[serde(default)]
    pub cwd: Option<String>,
    /// 强制超时（毫秒）。
    #[serde(default = "default_timeout_ms")]
    pub timeout_ms: u64,
    /// 是否启用。
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// 队列溢出策略。
    #[serde(default)]
    pub on_full: OnFull,
    /// 放开同规则并发（放弃默认保序）。
    #[serde(default)]
    pub parallel: bool,
    /// 最小触发间隔（毫秒）。
    #[serde(default)]
    pub cooldown_ms: u64,
    /// 载荷可选字段（`prompt`/`tool_input`/`tool_response`）；空 = 默认脱敏。
    #[serde(default)]
    pub include: Vec<String>,
    /// 来源作用域；由加载来源标注（`#[serde(skip)]`，非用户可写）。
    #[serde(skip)]
    pub scope: HookScope,
}

fn default_timeout_ms() -> u64 {
    DEFAULT_HOOK_TIMEOUT_MS
}

fn default_true() -> bool {
    true
}

/// `[hooks]` 配置节。
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct HooksConfig {
    #[serde(default)]
    pub rules: Vec<HookRule>,
}

impl HooksConfig {
    /// 是否无任何规则（用于序列化时省略空 `[hooks]` 节）。
    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }

    /// 校验规则集合：空 `id`、重复 `id`、空 `command`。
    ///
    /// 未知 `event` 由 [`HookEventName`] 的反序列化（契约枚举）直接拒绝，
    /// 缺 `command` 由 serde 的 `missing field` 拒绝，均不在此重复。
    pub fn validate(&self) -> Result<(), HookConfigError> {
        let mut seen: HashMap<&str, usize> = HashMap::new();
        for (index, rule) in self.rules.iter().enumerate() {
            if rule.id.trim().is_empty() {
                return Err(HookConfigError::EmptyId { index });
            }
            if seen.insert(rule.id.as_str(), index).is_some() {
                return Err(HookConfigError::DuplicateId {
                    id: rule.id.clone(),
                });
            }
            if rule.command.trim().is_empty() {
                return Err(HookConfigError::EmptyCommand {
                    id: rule.id.clone(),
                });
            }
        }
        Ok(())
    }
}

/// `[hooks]` 校验错误。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HookConfigError {
    /// 规则缺少 `id`。
    EmptyId { index: usize },
    /// 规则 `id` 重复。
    DuplicateId { id: String },
    /// 规则缺少 `command`。
    EmptyCommand { id: String },
}

impl fmt::Display for HookConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            HookConfigError::EmptyId { index } => {
                write!(f, "hook rule at index {index} has an empty id")
            }
            HookConfigError::DuplicateId { id } => {
                write!(f, "duplicate hook rule id `{id}`")
            }
            HookConfigError::EmptyCommand { id } => {
                write!(f, "hook rule `{id}` has an empty command")
            }
        }
    }
}

impl std::error::Error for HookConfigError {}

/// 把项目级 hook 合并进全局规则集（设计 §7.3）。
///
/// - 同 `id`：项目覆盖全局（原地替换，保持位置）。
/// - 新 `id`：追加。
/// - 合并进来的项目规则 `scope` 统一标注为 [`HookScope::Project`]；
///   全局规则保持 [`HookScope::Global`]。
pub fn merge_hooks(global: &mut HooksConfig, project: &HooksConfig) {
    for project_rule in &project.rules {
        let mut incoming = project_rule.clone();
        incoming.scope = HookScope::Project;
        match global.rules.iter_mut().find(|r| r.id == incoming.id) {
            Some(existing) => *existing = incoming,
            None => global.rules.push(incoming),
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::config::{DaemonConfig, load_config};
    use crate::hooks::*;
    use std::io::Write;

    #[test]
    fn hooks_full_field_parse() {
        let toml = r#"
[daemon]
listen_addr = "[::1]:50051"

[hooks]

[[hooks.rules]]
id = "notify-done"
order = 10
event = ["Stop", "StopFailure"]
matcher = "^Bash$"
command = "/usr/local/bin/notify.sh"
args = ["--title", "done"]
cwd = "/tmp/work"
timeout_ms = 5000
enabled = false
on_full = "coalesce_latest"
parallel = true
cooldown_ms = 250
include = ["prompt", "tool_input"]

[hooks.rules.env]
FOO = "bar"
BAZ = "qux"
"#;
        let config: DaemonConfig = toml::from_str(toml).unwrap();
        assert_eq!(config.hooks.rules.len(), 1);
        let rule = &config.hooks.rules[0];

        assert_eq!(rule.id, "notify-done");
        assert_eq!(rule.order, Some(10));
        assert_eq!(
            rule.event,
            vec![HookEventName::Stop, HookEventName::StopFailure]
        );
        assert_eq!(rule.matcher.as_deref(), Some("^Bash$"));
        assert_eq!(rule.command, "/usr/local/bin/notify.sh");
        assert_eq!(rule.args, vec!["--title".to_string(), "done".to_string()]);
        assert_eq!(rule.env.get("FOO").map(String::as_str), Some("bar"));
        assert_eq!(rule.env.get("BAZ").map(String::as_str), Some("qux"));
        assert_eq!(rule.cwd.as_deref(), Some("/tmp/work"));
        assert_eq!(rule.timeout_ms, 5000);
        assert!(!rule.enabled);
        assert_eq!(rule.on_full, OnFull::CoalesceLatest);
        assert!(rule.parallel);
        assert_eq!(rule.cooldown_ms, 250);
        assert_eq!(
            rule.include,
            vec!["prompt".to_string(), "tool_input".to_string()]
        );
        // scope 为来源派生字段，解析后恒为 global（项目加载时由合并标注）。
        assert_eq!(rule.scope, HookScope::Global);
    }

    #[test]
    fn hooks_absent_defaults_empty() {
        let config: DaemonConfig = toml::from_str("").unwrap();
        assert!(config.hooks.rules.is_empty());
        assert!(config.hooks.is_empty());
        assert_eq!(config.hooks, HooksConfig::default());
    }

    #[test]
    fn merge_hooks_project_overrides_appends_and_scopes() {
        let global_toml = r#"
[hooks]

[[hooks.rules]]
id = "a"
event = ["Stop"]
command = "/g/a"

[[hooks.rules]]
id = "shared"
event = ["Stop"]
command = "/g/shared"
"#;
        let project_toml = r#"
[hooks]

[[hooks.rules]]
id = "shared"
event = ["StopFailure"]
command = "/p/shared"

[[hooks.rules]]
id = "b"
event = ["SessionEnd"]
command = "/p/b"
"#;
        let mut global: DaemonConfig = toml::from_str(global_toml).unwrap();
        let project: DaemonConfig = toml::from_str(project_toml).unwrap();

        merge_hooks(&mut global.hooks, &project.hooks);

        assert_eq!(global.hooks.rules.len(), 3);

        let a = global.hooks.rules.iter().find(|r| r.id == "a").unwrap();
        assert_eq!(a.command, "/g/a");
        assert_eq!(a.scope, HookScope::Global);

        let shared = global
            .hooks
            .rules
            .iter()
            .find(|r| r.id == "shared")
            .unwrap();
        assert_eq!(shared.command, "/p/shared");
        assert_eq!(shared.event, vec![HookEventName::StopFailure]);
        assert_eq!(shared.scope, HookScope::Project);

        let b = global.hooks.rules.iter().find(|r| r.id == "b").unwrap();
        assert_eq!(b.command, "/p/b");
        assert_eq!(b.scope, HookScope::Project);
    }

    #[test]
    fn invalid_rules_report_clear_errors() {
        // 未知 event：契约枚举在反序列化阶段即拒绝，错误信息含事件名。
        let unknown_event = toml::from_str::<DaemonConfig>(
            r#"
[hooks]
[[hooks.rules]]
id = "x"
event = ["NotAnEvent"]
command = "/x"
"#,
        )
        .unwrap_err();
        assert!(
            unknown_event.to_string().contains("NotAnEvent"),
            "unexpected error: {unknown_event}"
        );

        // 缺 command：反序列化阶段报 missing field。
        let missing_command = toml::from_str::<DaemonConfig>(
            r#"
[hooks]
[[hooks.rules]]
id = "x"
event = ["Stop"]
"#,
        )
        .unwrap_err();
        assert!(
            missing_command.to_string().contains("command"),
            "unexpected error: {missing_command}"
        );

        // 重复 id：语法合法，靠 validate 明确报错。
        let dup: DaemonConfig = toml::from_str(
            r#"
[hooks]
[[hooks.rules]]
id = "dup"
event = ["Stop"]
command = "/a"
[[hooks.rules]]
id = "dup"
event = ["Stop"]
command = "/b"
"#,
        )
        .unwrap();
        let err = dup.hooks.validate().unwrap_err();
        assert_eq!(
            err,
            HookConfigError::DuplicateId {
                id: "dup".to_string()
            }
        );
        assert!(err.to_string().contains("dup"));

        // 空 command 亦视为缺失。
        let empty_command: DaemonConfig = toml::from_str(
            r#"
[hooks]
[[hooks.rules]]
id = "empty"
event = ["Stop"]
command = ""
"#,
        )
        .unwrap();
        assert_eq!(
            empty_command.hooks.validate().unwrap_err(),
            HookConfigError::EmptyCommand {
                id: "empty".to_string()
            }
        );
    }

    #[test]
    fn hooks_rule_defaults_locked() {
        let config: DaemonConfig = toml::from_str(
            r#"
[hooks]
[[hooks.rules]]
id = "r"
event = ["Stop"]
command = "/x"
"#,
        )
        .unwrap();
        let rule = &config.hooks.rules[0];

        assert_eq!(rule.on_full, OnFull::DropNew);
        assert!(rule.enabled);
        assert!(!rule.parallel);
        assert_eq!(rule.cooldown_ms, 0);
        assert_eq!(rule.timeout_ms, DEFAULT_HOOK_TIMEOUT_MS);
        // matcher 缺省 = 全匹配（None）。
        assert_eq!(rule.matcher, None);
        assert_eq!(rule.order, None);
        assert!(rule.args.is_empty());
        assert!(rule.env.is_empty());
        assert_eq!(rule.cwd, None);
        assert!(rule.include.is_empty());
        assert_eq!(rule.scope, HookScope::Global);
    }

    #[test]
    fn load_config_without_hooks_is_unchanged() {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        write!(
            file,
            r#"
[daemon]
listen_addr = "127.0.0.1:9090"

[llm]

[tools]

[agent]
"#
        )
        .unwrap();

        let config = load_config(Some(file.path())).unwrap();
        assert!(config.hooks.rules.is_empty());
        assert_eq!(config.daemon.listen_addr, "127.0.0.1:9090");
    }
}
