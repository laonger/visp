//! 项目级 hook 信任（设计 D7 / §8.1 / §8.2；实施计划步骤 2a）。
//!
//! 信任绑定**三要素**，任一变更即失效（需重新信任）：
//!
//! 1. **canonical 项目路径**（symlink / `..` 解析后的真实路径，作为记录键）；
//! 2. **`.visp/hooks/` 目录递归内容快照**（相对路径 + 文件内容哈希，含符号链接目标）；
//! 3. **生效的项目级规则全集**（`HookScope::Project` 规则的 `id`/`command`/`args`/`env`）。
//!
//! 信任存储位于**全局数据目录**（[`crate::path::hook_trust_file`]，默认 `~/.visp/hook-trust.toml`），
//! **绝不写入项目仓库**。
//!
//! 安全边界（设计 §8.1/§8.2）：
//!
//! - 项目级 `sh -c`（shell + `-c`）一律拒绝，**即使已信任**；
//! - 项目级 `command` 必须 canonicalize 到 `.visp/hooks/` 目录内的文件；
//! - `.visp/hooks/` 内指向目录外的符号链接一律拒绝（canonicalize 后比较）；
//! - 项目路径 canonicalize 失败（不存在/权限）→ **保守判为不信任**；
//! - **全局规则不经本门控**（全局 hook 默认可信）。

use std::collections::BTreeMap;
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::hooks::{HookRule, HookScope};
use crate::path::{hook_trust_file, hooks_dir_project};

/// 被视为 shell 的命令 basename；项目级不得以 `-c` 调用它们。
const SHELL_COMMANDS: [&str; 7] = ["sh", "bash", "dash", "zsh", "ksh", "csh", "fish"];

/// 项目级 hook 信任的拒绝/错误原因。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrustError {
    /// 项目路径 canonicalize 失败（不存在 / 权限）。
    CanonicalizeFailed { path: PathBuf, reason: String },
    /// 项目级规则以 shell + `-c` 调用（禁止）。
    ShellInvocation { rule_id: String, command: String },
    /// 项目级规则的 `command` 未落在 `.visp/hooks/` 目录内（或无法解析）。
    CommandOutsideHooksDir { rule_id: String, command: String },
    /// `.visp/hooks/` 内符号链接指向目录外（或悬空）。
    SymlinkEscape { path: PathBuf, target: PathBuf },
    /// 快照读取期间的 I/O 失败。
    Io { path: PathBuf, reason: String },
}

impl fmt::Display for TrustError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TrustError::CanonicalizeFailed { path, reason } => {
                write!(f, "cannot canonicalize `{}`: {reason}", path.display())
            }
            TrustError::ShellInvocation { rule_id, command } => {
                write!(
                    f,
                    "project hook rule `{rule_id}` invokes shell `{command}` with -c, which is forbidden"
                )
            }
            TrustError::CommandOutsideHooksDir { rule_id, command } => {
                write!(
                    f,
                    "project hook rule `{rule_id}` command `{command}` does not resolve inside .visp/hooks/"
                )
            }
            TrustError::SymlinkEscape { path, target } => {
                write!(
                    f,
                    "symlink `{}` escapes .visp/hooks/ (resolves to `{}`)",
                    path.display(),
                    target.display()
                )
            }
            TrustError::Io { path, reason } => {
                write!(f, "cannot read `{}`: {reason}", path.display())
            }
        }
    }
}

impl std::error::Error for TrustError {}

/// 判定不信任的原因。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrustReason {
    /// 无信任记录。
    NoRecord,
    /// `.visp/hooks/` 快照与记录不符（新增/修改/删除文件）。
    SnapshotChanged,
    /// 项目级规则全集与记录不符（command/args/env 变更）。
    RulesChanged,
    /// 项目级规则本身非法（`sh -c` / 命令在 hooks 目录外）——即使有记录也拒绝。
    InvalidRules(TrustError),
    /// `.visp/hooks/` 目录被拒（符号链接逃逸 / I/O）。
    DirectoryRejected(TrustError),
    /// 项目路径 canonicalize 失败，保守判为不信任。
    CanonicalizeFailed(TrustError),
}

impl fmt::Display for TrustReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TrustReason::NoRecord => write!(f, "no trust record"),
            TrustReason::SnapshotChanged => write!(f, "hooks directory snapshot changed"),
            TrustReason::RulesChanged => write!(f, "project rule set changed"),
            TrustReason::InvalidRules(error) => write!(f, "invalid project rules: {error}"),
            TrustReason::DirectoryRejected(error) => {
                write!(f, "hooks directory rejected: {error}")
            }
            TrustReason::CanonicalizeFailed(error) => {
                write!(f, "project path not canonicalizable: {error}")
            }
        }
    }
}

/// 项目级 hook 信任判定结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrustStatus {
    /// 无需门控：规则集内无项目级规则（全局规则恒可运行）。
    NotRequired,
    /// 已信任。
    Trusted,
    /// 未信任；原因见 [`TrustReason`]。
    Untrusted(TrustReason),
}

impl TrustStatus {
    /// 是否可放行（[`TrustStatus::Trusted`] 或 [`TrustStatus::NotRequired`]）。
    pub fn is_trusted(&self) -> bool {
        matches!(self, TrustStatus::Trusted | TrustStatus::NotRequired)
    }
}

impl fmt::Display for TrustStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TrustStatus::NotRequired => write!(f, "not required"),
            TrustStatus::Trusted => write!(f, "trusted"),
            TrustStatus::Untrusted(reason) => write!(f, "untrusted: {reason}"),
        }
    }
}

/// 单条信任记录（可持久化）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HookTrustRecord {
    /// canonical 项目路径。
    pub project: PathBuf,
    /// `.visp/hooks/` 递归内容快照哈希。
    pub hooks_snapshot_hash: String,
    /// 项目级规则全集哈希。
    pub rules_hash: String,
}

/// 项目级 hook 信任存储，键为 canonical 项目路径。
///
/// 位于**全局数据目录**（[`HookTrustStore::load`] / [`HookTrustStore::save`]），
/// 不写入仓库（设计 §8.2）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HookTrustStore {
    #[serde(default)]
    records: BTreeMap<String, HookTrustRecord>,
}

impl HookTrustStore {
    /// 从全局信任文件加载；文件缺失或损坏时降级为空存储并 `warn`。
    pub fn load() -> Self {
        let Some(path) = hook_trust_file() else {
            return Self::default();
        };
        if !path.exists() {
            return Self::default();
        }
        match Self::load_from(&path) {
            Ok(store) => store,
            Err(error) => {
                tracing::warn!(path = %path.display(), %error, "hook 信任文件损坏，视为空");
                Self::default()
            }
        }
    }

    /// 从指定路径加载信任存储。
    pub fn load_from(path: &Path) -> io::Result<Self> {
        let text = fs::read_to_string(path)?;
        toml::from_str(&text).map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
    }

    /// 保存到全局信任文件（自动创建父目录）。
    pub fn save(&self) -> io::Result<()> {
        let path = hook_trust_file().ok_or_else(|| {
            io::Error::new(io::ErrorKind::NotFound, "global data dir unavailable")
        })?;
        self.save_to(&path)
    }

    /// 保存到指定路径。
    pub fn save_to(&self, path: &Path) -> io::Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let text = toml::to_string_pretty(self)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        fs::write(path, text)
    }

    /// 全部信任记录。
    pub fn records(&self) -> impl Iterator<Item = &HookTrustRecord> {
        self.records.values()
    }

    /// 查询指定项目的信任记录（按 canonical 路径）。
    pub fn record_for(&self, project: &Path) -> Option<&HookTrustRecord> {
        let key = canonical_key(project)?;
        self.records.get(&key)
    }

    /// 信任项目：canonicalize + 校验项目规则 + 记录当前快照与规则哈希。
    ///
    /// 项目规则非法（`sh -c` / 命令在 hooks 目录外 / 符号链接逃逸）时返回错误且**不记录**。
    pub fn trust(&mut self, project: &Path, rules: &[HookRule]) -> Result<(), TrustError> {
        let canonical = canonical_project(project)?;
        validate_project_rules(&canonical, rules)?;
        let snapshot = compute_snapshot(&canonical)?;
        let rules_hash = compute_rules_hash(rules);
        let key = canonical.to_string_lossy().into_owned();
        self.records.insert(
            key,
            HookTrustRecord {
                project: canonical,
                hooks_snapshot_hash: snapshot,
                rules_hash,
            },
        );
        Ok(())
    }

    /// 撤销项目信任；返回是否确有记录被移除。
    pub fn revoke(&mut self, project: &Path) -> bool {
        match canonical_key(project) {
            Some(key) => self.records.remove(&key).is_some(),
            None => false,
        }
    }

    /// 判定：给定项目路径与规则集，项目级规则是否可运行。
    ///
    /// 顺序：canonicalize → 无项目规则则 [`TrustStatus::NotRequired`] → 校验项目规则
    /// → 计算快照/规则哈希并与记录比对。任一环节失败即不信任。
    pub fn status(&self, project: &Path, rules: &[HookRule]) -> TrustStatus {
        let canonical = match canonical_project(project) {
            Ok(path) => path,
            Err(error) => return TrustStatus::Untrusted(TrustReason::CanonicalizeFailed(error)),
        };
        if project_rules(rules).next().is_none() {
            return TrustStatus::NotRequired;
        }
        if let Err(error) = validate_project_rules(&canonical, rules) {
            return TrustStatus::Untrusted(TrustReason::InvalidRules(error));
        }
        let snapshot = match compute_snapshot(&canonical) {
            Ok(snapshot) => snapshot,
            Err(error) => return TrustStatus::Untrusted(TrustReason::DirectoryRejected(error)),
        };
        let rules_hash = compute_rules_hash(rules);
        let key = canonical.to_string_lossy().into_owned();
        let Some(record) = self.records.get(&key) else {
            return TrustStatus::Untrusted(TrustReason::NoRecord);
        };
        if record.hooks_snapshot_hash != snapshot {
            return TrustStatus::Untrusted(TrustReason::SnapshotChanged);
        }
        if record.rules_hash != rules_hash {
            return TrustStatus::Untrusted(TrustReason::RulesChanged);
        }
        TrustStatus::Trusted
    }

    /// [`HookTrustStore::status`] 的布尔便捷入口。
    pub fn is_trusted(&self, project: &Path, rules: &[HookRule]) -> bool {
        self.status(project, rules).is_trusted()
    }
}

/// 执行前校验与 `doctor` 共用的单一入口（计划 2a 重构项）。
pub fn verify(project: &Path, rules: &[HookRule], store: &HookTrustStore) -> TrustStatus {
    store.status(project, rules)
}

/// 过滤项目级规则。
fn project_rules(rules: &[HookRule]) -> impl Iterator<Item = &HookRule> {
    rules.iter().filter(|rule| rule.scope == HookScope::Project)
}

/// canonicalize 项目路径；失败即保守判为不信任。
fn canonical_project(project: &Path) -> Result<PathBuf, TrustError> {
    project
        .canonicalize()
        .map_err(|error| TrustError::CanonicalizeFailed {
            path: project.to_path_buf(),
            reason: error.to_string(),
        })
}

/// canonical 键；canonicalize 失败返回 `None`。
fn canonical_key(project: &Path) -> Option<String> {
    project
        .canonicalize()
        .ok()
        .map(|path| path.to_string_lossy().into_owned())
}

/// 校验项目级规则：禁 `sh -c`；`command` 必须落在 `.visp/hooks/` 内。
fn validate_project_rules(project: &Path, rules: &[HookRule]) -> Result<(), TrustError> {
    // 目录不存在时 canonicalize 失败 → 所有项目规则均落在目录外。
    let hooks_dir = hooks_dir_project(project).canonicalize().ok();
    for rule in project_rules(rules) {
        if is_shell_invocation(&rule.command, &rule.args) {
            return Err(TrustError::ShellInvocation {
                rule_id: rule.id.clone(),
                command: rule.command.clone(),
            });
        }
        let candidate = if Path::new(&rule.command).is_absolute() {
            PathBuf::from(&rule.command)
        } else {
            project.join(&rule.command)
        };
        let resolved =
            candidate
                .canonicalize()
                .map_err(|_| TrustError::CommandOutsideHooksDir {
                    rule_id: rule.id.clone(),
                    command: rule.command.clone(),
                })?;
        if !hooks_dir
            .as_ref()
            .is_some_and(|dir| resolved.starts_with(dir))
        {
            return Err(TrustError::CommandOutsideHooksDir {
                rule_id: rule.id.clone(),
                command: rule.command.clone(),
            });
        }
    }
    Ok(())
}

/// 是否为 shell + `-c` 调用（按命令 basename 判定）。
fn is_shell_invocation(command: &str, args: &[String]) -> bool {
    let basename = Path::new(command)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(command);
    SHELL_COMMANDS.contains(&basename) && args.iter().any(|arg| arg.as_str() == "-c")
}

/// `.visp/hooks/` 递归内容快照哈希。
///
/// 缺失目录视为空快照；符号链接作为叶子计入（不跟随目录，避免环与逃逸）。
fn compute_snapshot(project: &Path) -> Result<String, TrustError> {
    let raw_hooks = hooks_dir_project(project);
    let mut entries = Vec::new();
    if raw_hooks.exists() {
        let canonical_hooks = raw_hooks
            .canonicalize()
            .map_err(|error| io_err(&raw_hooks, error))?;
        // `.visp/hooks/` 自身指向项目外 → 逃逸。
        if !canonical_hooks.starts_with(project) {
            return Err(TrustError::SymlinkEscape {
                path: raw_hooks,
                target: canonical_hooks,
            });
        }
        collect_entries(&canonical_hooks, &canonical_hooks, &mut entries)?;
        entries.sort();
    }

    let mut hasher = Sha256::new();
    hasher.update(b"visp-hook-snapshot-v1\n");
    for entry in &entries {
        hasher.update(entry.as_bytes());
        hasher.update(b"\n");
    }
    Ok(hex(&hasher.finalize()))
}

/// 递归收集快照条目（相对路径 + 类型 + 内容/链接目标哈希）。
fn collect_entries(root: &Path, dir: &Path, out: &mut Vec<String>) -> Result<(), TrustError> {
    let read = fs::read_dir(dir).map_err(|error| io_err(dir, error))?;
    for entry in read {
        let entry = entry.map_err(|error| io_err(dir, error))?;
        let path = entry.path();
        let rel = path
            .strip_prefix(root)
            .unwrap_or(&path)
            .to_string_lossy()
            .replace('\\', "/");
        let meta = fs::symlink_metadata(&path).map_err(|error| io_err(&path, error))?;
        let file_type = meta.file_type();
        if file_type.is_symlink() {
            let target_raw = fs::read_link(&path).map_err(|error| io_err(&path, error))?;
            // 防逃逸：canonical 目标必须仍在 hooks 目录内（悬空链接亦拒绝）。
            let target = path.canonicalize().map_err(|_| TrustError::SymlinkEscape {
                path: path.clone(),
                target: target_raw,
            })?;
            if !target.starts_with(root) {
                return Err(TrustError::SymlinkEscape { path, target });
            }
            out.push(format!("link\t{rel}\t{}", target.to_string_lossy()));
        } else if file_type.is_dir() {
            out.push(format!("dir\t{rel}"));
            collect_entries(root, &path, out)?;
        } else if file_type.is_file() {
            let bytes = fs::read(&path).map_err(|error| io_err(&path, error))?;
            out.push(format!("file\t{rel}\t{}", hex(&Sha256::digest(&bytes))));
        } else {
            out.push(format!("other\t{rel}"));
        }
    }
    Ok(())
}

/// 项目级规则全集哈希：按配置顺序，逐条哈希 `id`/`command`/`args`/`env`（env 键排序）。
fn compute_rules_hash(rules: &[HookRule]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"visp-hook-project-rules-v1\n");
    for rule in project_rules(rules) {
        hasher.update(b"rule\t");
        hasher.update(rule.id.as_bytes());
        hasher.update(b"\ncommand\t");
        hasher.update(rule.command.as_bytes());
        hasher.update(b"\n");
        for arg in &rule.args {
            hasher.update(b"arg\t");
            hasher.update(arg.as_bytes());
            hasher.update(b"\n");
        }
        let mut env: Vec<(&String, &String)> = rule.env.iter().collect();
        env.sort_by(|a, b| a.0.cmp(b.0));
        for (key, value) in env {
            hasher.update(b"env\t");
            hasher.update(key.as_bytes());
            hasher.update(b"\t");
            hasher.update(value.as_bytes());
            hasher.update(b"\n");
        }
    }
    hex(&hasher.finalize())
}

/// 构造 [`TrustError::Io`]。
fn io_err(path: &Path, error: io::Error) -> TrustError {
    TrustError::Io {
        path: path.to_path_buf(),
        reason: error.to_string(),
    }
}

/// 字节序列转小写十六进制。
fn hex(bytes: &[u8]) -> String {
    const TABLE: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(TABLE[(byte >> 4) as usize] as char);
        out.push(TABLE[(byte & 0x0f) as usize] as char);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hooks::{DEFAULT_HOOK_TIMEOUT_MS, HookEventName, HookRule, HookScope, OnFull};
    use std::collections::HashMap;
    use std::fs;
    use std::path::Path;

    fn project_rule(id: &str, command: &str) -> HookRule {
        HookRule {
            id: id.to_string(),
            order: None,
            event: vec![HookEventName::Stop],
            matcher: None,
            command: command.to_string(),
            args: Vec::new(),
            env: HashMap::new(),
            cwd: None,
            timeout_ms: DEFAULT_HOOK_TIMEOUT_MS,
            enabled: true,
            on_full: OnFull::DropNew,
            parallel: false,
            cooldown_ms: 0,
            include: Vec::new(),
            scope: HookScope::Project,
        }
    }

    fn global_rule(id: &str, command: &str) -> HookRule {
        let mut rule = project_rule(id, command);
        rule.scope = HookScope::Global;
        rule
    }

    fn setup_project() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join(".visp/hooks")).unwrap();
        dir
    }

    fn write_hook(project: &Path, name: &str, body: &str) {
        fs::write(project.join(".visp/hooks").join(name), body).unwrap();
    }

    /// 1. 信任绑定 canonical 项目路径（symlink / `..` 解析一致）。
    #[cfg(unix)]
    #[test]
    fn canonical_project_path_binding() {
        let dir = setup_project();
        write_hook(dir.path(), "a.sh", "#!/bin/sh\n");
        let real = dir.path().canonicalize().unwrap();
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();

        let rules = vec![project_rule("a", ".visp/hooks/a.sh")];
        let mut store = HookTrustStore::default();
        store.trust(&link, &rules).unwrap();

        assert_eq!(store.status(&real, &rules), TrustStatus::Trusted);
        assert_eq!(store.status(&link, &rules), TrustStatus::Trusted);
        let via_dotdot = real.join("..").join(real.file_name().unwrap());
        assert_eq!(store.status(&via_dotdot, &rules), TrustStatus::Trusted);
    }

    /// 2. 递归快照：`.visp/hooks/` 内新增/修改/删除文件 → 信任失效。
    #[test]
    fn recursive_snapshot_invalidates_on_add_modify_delete() {
        let dir = setup_project();
        write_hook(dir.path(), "a.sh", "v1");
        let project = dir.path();
        let rules = vec![project_rule("a", ".visp/hooks/a.sh")];
        let mut store = HookTrustStore::default();
        store.trust(project, &rules).unwrap();
        assert_eq!(store.status(project, &rules), TrustStatus::Trusted);

        write_hook(project, "a.sh", "v2");
        assert_eq!(
            store.status(project, &rules),
            TrustStatus::Untrusted(TrustReason::SnapshotChanged)
        );
        store.trust(project, &rules).unwrap();

        write_hook(project, "b.sh", "new");
        assert_eq!(
            store.status(project, &rules),
            TrustStatus::Untrusted(TrustReason::SnapshotChanged)
        );
        store.trust(project, &rules).unwrap();

        fs::remove_file(project.join(".visp/hooks/b.sh")).unwrap();
        assert_eq!(
            store.status(project, &rules),
            TrustStatus::Untrusted(TrustReason::SnapshotChanged)
        );
    }

    /// 3. 规则全集快照：command/args/env 变更 → 失效。
    #[test]
    fn rules_full_set_change_invalidates() {
        let dir = setup_project();
        write_hook(dir.path(), "a.sh", "a");
        write_hook(dir.path(), "b.sh", "b");
        let project = dir.path();
        let mut rules = vec![project_rule("r", ".visp/hooks/a.sh")];
        let mut store = HookTrustStore::default();
        store.trust(project, &rules).unwrap();
        assert_eq!(store.status(project, &rules), TrustStatus::Trusted);

        // command 变更（快照不变：a.sh / b.sh 均在场）。
        rules[0].command = ".visp/hooks/b.sh".to_string();
        assert_eq!(
            store.status(project, &rules),
            TrustStatus::Untrusted(TrustReason::RulesChanged)
        );
        store.trust(project, &rules).unwrap();

        // args 变更。
        rules[0].args = vec!["--x".to_string()];
        assert_eq!(
            store.status(project, &rules),
            TrustStatus::Untrusted(TrustReason::RulesChanged)
        );
        store.trust(project, &rules).unwrap();

        // env 变更。
        rules[0].env.insert("K".to_string(), "V".to_string());
        assert_eq!(
            store.status(project, &rules),
            TrustStatus::Untrusted(TrustReason::RulesChanged)
        );
    }

    /// 4. 符号链接防逃逸：`.visp/hooks/` 内指向目录外的 symlink → 拒绝。
    #[cfg(unix)]
    #[test]
    fn symlink_escape_is_rejected() {
        let dir = setup_project();
        write_hook(dir.path(), "a.sh", "a");
        let project = dir.path();
        std::os::unix::fs::symlink("/bin/ls", project.join(".visp/hooks/evil")).unwrap();

        // 未被规则引用的逃逸链接，也拒绝整个目录。
        let rules = vec![project_rule("a", ".visp/hooks/a.sh")];
        let mut store = HookTrustStore::default();
        let err = store.trust(project, &rules).unwrap_err();
        assert!(
            matches!(err, TrustError::SymlinkEscape { .. }),
            "unexpected: {err:?}"
        );
        match store.status(project, &rules) {
            TrustStatus::Untrusted(TrustReason::DirectoryRejected(TrustError::SymlinkEscape {
                ..
            })) => {}
            other => panic!("unexpected status: {other:?}"),
        }

        // 规则直接引用逃逸链接 → 命令落在 hooks 目录外。
        let evil = vec![project_rule("evil", ".visp/hooks/evil")];
        match store.status(project, &evil) {
            TrustStatus::Untrusted(TrustReason::InvalidRules(
                TrustError::CommandOutsideHooksDir { .. },
            )) => {}
            other => panic!("unexpected status: {other:?}"),
        }
    }

    /// 5. 项目级 `sh -c` → 拒绝（即使已有匹配的信任记录）。
    #[test]
    fn project_shell_invocation_rejected_even_when_trusted() {
        let dir = setup_project();
        write_hook(dir.path(), "a.sh", "a");
        let project = dir.path();
        let mut rules = vec![project_rule("sh-rule", "sh")];
        rules[0].args = vec!["-c".to_string(), "echo pwned".to_string()];

        let mut store = HookTrustStore::default();
        let err = store.trust(project, &rules).unwrap_err();
        assert!(
            matches!(err, TrustError::ShellInvocation { .. }),
            "unexpected: {err:?}"
        );

        // 手工植入与当前状态匹配的信任记录，证明「即使已信任」也拒绝。
        let canonical = canonical_project(project).unwrap();
        let record = HookTrustRecord {
            project: canonical.clone(),
            hooks_snapshot_hash: compute_snapshot(&canonical).unwrap(),
            rules_hash: compute_rules_hash(&rules),
        };
        store
            .records
            .insert(canonical.to_string_lossy().into_owned(), record);

        match store.status(project, &rules) {
            TrustStatus::Untrusted(TrustReason::InvalidRules(TrustError::ShellInvocation {
                ..
            })) => {}
            other => panic!("unexpected status: {other:?}"),
        }
    }

    /// 6. canonicalize 失败 → 保守判为不信任。
    #[test]
    fn canonicalize_failure_is_untrusted() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("does-not-exist");
        let rules = vec![project_rule("a", ".visp/hooks/a.sh")];
        let mut store = HookTrustStore::default();

        let err = store.trust(&missing, &rules).unwrap_err();
        assert!(
            matches!(err, TrustError::CanonicalizeFailed { .. }),
            "unexpected: {err:?}"
        );
        match store.status(&missing, &rules) {
            TrustStatus::Untrusted(TrustReason::CanonicalizeFailed(
                TrustError::CanonicalizeFailed { .. },
            )) => {}
            other => panic!("unexpected status: {other:?}"),
        }
    }

    /// 7. 未信任项目 hook 默认不运行（惰性）。
    #[test]
    fn untrusted_project_is_lazy_by_default() {
        let dir = setup_project();
        write_hook(dir.path(), "a.sh", "a");
        let project = dir.path();
        let rules = vec![project_rule("a", ".visp/hooks/a.sh")];

        let store = HookTrustStore::default();
        assert!(!store.is_trusted(project, &rules));
        assert_eq!(
            store.status(project, &rules),
            TrustStatus::Untrusted(TrustReason::NoRecord)
        );

        let mut store = HookTrustStore::default();
        store.trust(project, &rules).unwrap();
        assert!(store.is_trusted(project, &rules));
        assert_eq!(store.status(project, &rules), TrustStatus::Trusted);
    }

    /// 8. 信任存储不写入仓库（位置为全局数据目录）。
    #[test]
    #[serial_test::serial]
    fn trust_store_lives_in_global_data_dir_not_repo() {
        let data = tempfile::tempdir().unwrap();
        let previous = std::env::var("VISP_DATA_DIR").ok();
        unsafe { std::env::set_var("VISP_DATA_DIR", data.path()) };

        let trust_file = hook_trust_file().expect("global data dir configured");
        assert_eq!(trust_file, data.path().join("hook-trust.toml"));
        assert!(trust_file.starts_with(data.path()));

        let dir = setup_project();
        write_hook(dir.path(), "a.sh", "a");
        let project = dir.path();
        let rules = vec![project_rule("a", ".visp/hooks/a.sh")];
        let mut store = HookTrustStore::default();
        store.trust(project, &rules).unwrap();
        store.save().unwrap();

        assert!(trust_file.exists());
        assert!(!project.join("hook-trust.toml").exists());
        assert!(!project.join(".visp/hook-trust.toml").exists());

        // 往返：重新加载后仍信任。
        let loaded = HookTrustStore::load();
        assert_eq!(loaded.status(project, &rules), TrustStatus::Trusted);

        match previous {
            Some(value) => unsafe { std::env::set_var("VISP_DATA_DIR", value) },
            None => unsafe { std::env::remove_var("VISP_DATA_DIR") },
        }
    }

    /// 9. 全局 hook 默认可运行（回归：不经项目信任门）。
    #[test]
    fn global_hooks_run_without_trust() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path();
        let rules = vec![global_rule("g", "/usr/bin/true")];
        let store = HookTrustStore::default();
        assert_eq!(store.status(project, &rules), TrustStatus::NotRequired);
        assert!(store.is_trusted(project, &rules));
    }

    /// 10. 未配置项目 hook 时行为不变。
    #[test]
    fn no_project_hooks_behavior_unchanged() {
        let dir = setup_project();
        let project = dir.path();
        let store = HookTrustStore::default();

        assert_eq!(store.status(project, &[]), TrustStatus::NotRequired);
        assert!(store.is_trusted(project, &[]));

        let rules = vec![global_rule("g", "/usr/bin/true")];
        assert_eq!(store.status(project, &rules), TrustStatus::NotRequired);
    }
}
