use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

#[derive(Debug, Clone)]
pub struct RuleFile {
    pub path: PathBuf,
    pub content: String,
}

#[derive(Debug, Clone)]
pub struct RuleSet {
    pub content: String,
    pub files: Vec<RuleFile>,
}

#[derive(Debug)]
pub struct RuleEngine {
    /// 构造时保存的项目路径，重载时据此重走完全相同的扫描。
    project_path: PathBuf,
    /// 全局 AGENTS.md 路径（构造时解析；重载复用，保证扫描逻辑等价）。
    global_agents_md: Option<PathBuf>,
    /// 全局 rules 目录（构造时解析；重载复用，保证扫描逻辑等价）。
    global_rules_dir: Option<PathBuf>,
    /// 祖先 `AGENTS.md` 向上查找的 `$HOME` 边界（构造时解析；重载复用）。
    home: Option<PathBuf>,
    rules: Arc<RwLock<RuleSet>>,
}

/// RuleEngine 重载结果（变化守卫素材）。
///
/// `ruleset.content` 即消费端实际使用的整体拼接串，调用方可据此做等值比较。
#[derive(Debug, Clone)]
pub struct ReloadResult {
    /// 重扫得到的完整 RuleSet。
    pub ruleset: RuleSet,
    /// 新拼接内容是否与重载前不同；为 `false` 时未写回，旧状态原样保留。
    pub changed: bool,
}

impl RuleEngine {
    pub fn new(project_path: &Path) -> std::io::Result<Self> {
        Self::with_global_sources(
            project_path,
            crate::path::global_agents_md(),
            crate::path::rules_dir_global(),
            crate::path::home_dir(),
        )
    }

    /// 使用显式全局来源与祖先边界构造（测试隔离钩子，参照 `load_skills` 的注入模式）。
    ///
    /// 生产路径由 [`RuleEngine::new`] 传入 `path` 模块解析出的全局路径与 `home_dir()`；
    /// 测试可传入临时目录或 `None`，以避免读取真实 `~/.config/visp` 与真实 `$HOME`。
    fn with_global_sources(
        project_path: &Path,
        global_agents_md: Option<PathBuf>,
        global_rules_dir: Option<PathBuf>,
        home: Option<PathBuf>,
    ) -> std::io::Result<Self> {
        let ruleset = build_ruleset(
            project_path,
            global_agents_md.as_deref(),
            global_rules_dir.as_deref(),
            home.as_deref(),
        )?;

        Ok(RuleEngine {
            project_path: project_path.to_path_buf(),
            global_agents_md,
            global_rules_dir,
            home,
            rules: Arc::new(RwLock::new(ruleset)),
        })
    }

    /// 重载：重走与构造完全相同的扫描，成功后整体替换当前 RuleSet。
    ///
    /// 失败语义：构建阶段任何 IO 错误（如某规则文件暂时不可读）→ 放弃本次
    /// 写回、保留旧 RuleSet，并返回错误；绝不把「好状态」变成「空状态」。
    pub fn reload(&self) -> std::io::Result<ReloadResult> {
        let new_ruleset = build_ruleset(
            &self.project_path,
            self.global_agents_md.as_deref(),
            self.global_rules_dir.as_deref(),
            self.home.as_deref(),
        )?;

        let mut guard = self.rules.write().unwrap();
        let changed = guard.content != new_ruleset.content;
        if changed {
            *guard = new_ruleset.clone();
        }

        Ok(ReloadResult {
            ruleset: new_ruleset,
            changed,
        })
    }

    pub fn get_active_rules(&self) -> String {
        self.rules.read().unwrap().content.clone()
    }
}

/// 执行完整扫描并组装 RuleSet（构造与重载共用的唯一扫描逻辑）。
///
/// 顺序：祖先 AGENTS.md（近先远后）→ 全局 AGENTS.md → 项目 `.visp/rules/`
/// → 全局 rules 目录。任何 IO 错误向上传播，调用方据此放弃写回、保留旧状态。
fn build_ruleset(
    project_path: &Path,
    global_agents_md: Option<&Path>,
    global_rules_dir: Option<&Path>,
    home: Option<&Path>,
) -> std::io::Result<RuleSet> {
    let mut files = Vec::new();

    // 1. AGENTS.md from project directory upward (bounded, closest first)
    for md in discover_agents_md(project_path, home) {
        if let Ok(content) = std::fs::read_to_string(&md) {
            let header = format!("Instructions from: {}", md.display());
            files.push(RuleFile {
                path: md,
                content: format!("{header}\n{content}"),
            });
        }
    }

    // 2. Global AGENTS.md: ~/.config/visp/AGENTS.md
    if let Some(global_agents) = global_agents_md
        && global_agents.is_file()
        && let Ok(content) = std::fs::read_to_string(global_agents)
    {
        let header = format!("Instructions from: {}", global_agents.display());
        files.push(RuleFile {
            path: global_agents.to_path_buf(),
            content: format!("{header}\n{content}"),
        });
    }

    // 3. Project rules: .visp/rules/
    let project_rules = crate::path::rules_dir_project(project_path);
    if project_rules.is_dir() {
        collect_rules(&project_rules, &mut files)?;
    }

    // 4. Global rules: ~/.config/visp/rules/
    if let Some(global_rules) = global_rules_dir
        && global_rules.is_dir()
    {
        collect_rules(global_rules, &mut files)?;
    }

    let content = files
        .iter()
        .map(|f| f.content.as_str())
        .collect::<Vec<_>>()
        .join("\n\n");

    Ok(RuleSet { content, files })
}

/// 在共享祖先链上查找存在的 `AGENTS.md`，返回路径按距离 project_path 从近到远排序。
///
/// 边界（git 根 / `$HOME` / 不在 `$HOME` 下 / `$HOME` 不可解析）由
/// [`crate::path::agents_md_ancestors_with_home`] 单点解析，加载器与监听计划共用。
fn discover_agents_md(project_path: &Path, home: Option<&Path>) -> Vec<PathBuf> {
    let mut result = Vec::new();
    for dir in crate::path::agents_md_ancestors_with_home(project_path, home) {
        let agents = dir.join("AGENTS.md");
        if agents.is_file() {
            result.push(agents);
        }
    }
    result
}

pub(crate) fn collect_rules(dir: &Path, files: &mut Vec<RuleFile>) -> std::io::Result<()> {
    let mut entries: Vec<_> = std::fs::read_dir(dir)?
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|ext| ext == "md"))
        .collect();

    entries.sort_by_key(|e| e.file_name());

    for entry in entries {
        let path = entry.path();
        let content = std::fs::read_to_string(&path)?;
        if has_always_apply_true(&content) {
            files.push(RuleFile { path, content });
        }
    }

    Ok(())
}

fn has_always_apply_true(content: &str) -> bool {
    content
        .lines()
        .take(5)
        .any(|line| line.trim().contains("alwaysApply: true"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn test_loads_always_apply_true() {
        let dir = tempdir().unwrap();
        let rules_dir = dir.path().join(".visp").join("rules");
        fs::create_dir_all(&rules_dir).unwrap();

        fs::write(
            rules_dir.join("test.md"),
            "---\nalwaysApply: true\n---\n# My Rule\nContent here\n",
        )
        .unwrap();

        let engine = RuleEngine::new(dir.path()).unwrap();
        let rules = engine.get_active_rules();
        assert!(rules.contains("# My Rule"));
        assert!(rules.contains("Content here"));
    }

    #[test]
    fn test_skips_always_apply_false() {
        let dir = tempdir().unwrap();
        let rules_dir = dir.path().join(".visp").join("rules");
        fs::create_dir_all(&rules_dir).unwrap();

        fs::write(
            rules_dir.join("test.md"),
            "---\nalwaysApply: false\n---\n# Rule\ncontent",
        )
        .unwrap();

        let engine = RuleEngine::new(dir.path()).unwrap();
        assert!(engine.get_active_rules().is_empty());
    }

    #[test]
    fn test_skips_no_marker() {
        let dir = tempdir().unwrap();
        let rules_dir = dir.path().join(".visp").join("rules");
        fs::create_dir_all(&rules_dir).unwrap();

        fs::write(
            rules_dir.join("test.md"),
            "# Just a regular file\nno marker here",
        )
        .unwrap();

        let engine = RuleEngine::new(dir.path()).unwrap();
        assert!(engine.get_active_rules().is_empty());
    }

    #[test]
    fn test_skips_non_md() {
        let dir = tempdir().unwrap();
        let rules_dir = dir.path().join(".visp").join("rules");
        fs::create_dir_all(&rules_dir).unwrap();

        fs::write(
            rules_dir.join("test.txt"),
            "alwaysApply: true\nText content",
        )
        .unwrap();
        fs::write(rules_dir.join("test.md"), "alwaysApply: true\n# Real rule").unwrap();

        let engine = RuleEngine::new(dir.path()).unwrap();
        let rules = engine.get_active_rules();
        assert!(rules.contains("# Real rule"));
        assert!(!rules.contains("Text content"));
    }

    #[test]
    fn test_missing_dir_no_error() {
        let dir = tempdir().unwrap();
        let engine = RuleEngine::new(dir.path());
        assert!(engine.is_ok());
        assert!(engine.unwrap().get_active_rules().is_empty());
    }

    #[test]
    fn test_multiple_files_sorted() {
        let dir = tempdir().unwrap();
        let rules_dir = dir.path().join(".visp").join("rules");
        fs::create_dir_all(&rules_dir).unwrap();

        fs::write(rules_dir.join("b.md"), "alwaysApply: true\n# B rule").unwrap();
        fs::write(rules_dir.join("a.md"), "alwaysApply: true\n# A rule").unwrap();
        fs::write(rules_dir.join("c.md"), "alwaysApply: true\n# C rule").unwrap();

        let engine = RuleEngine::new(dir.path()).unwrap();
        let rules = engine.get_active_rules();

        let a_pos = rules.find("# A rule").unwrap();
        let b_pos = rules.find("# B rule").unwrap();
        let c_pos = rules.find("# C rule").unwrap();

        assert!(a_pos < b_pos);
        assert!(b_pos < c_pos);
    }

    #[test]
    fn test_project_before_global_order() {
        // Test ordering via collect_rules directly, avoiding env var manipulation.
        let project_dir = tempdir().unwrap();
        let global_dir = tempdir().unwrap();

        fs::write(
            project_dir.path().join("a.md"),
            "alwaysApply: true\n# Project rule",
        )
        .unwrap();
        fs::write(
            global_dir.path().join("a.md"),
            "alwaysApply: true\n# Global rule",
        )
        .unwrap();

        let mut files = Vec::new();
        collect_rules(project_dir.path(), &mut files).unwrap();
        collect_rules(global_dir.path(), &mut files).unwrap();

        assert_eq!(files.len(), 2);
        assert!(files[0].content.contains("# Project rule"));
        assert!(files[1].content.contains("# Global rule"));
    }

    #[test]
    fn test_check_only_first_five_lines() {
        let dir = tempdir().unwrap();
        let rules_dir = dir.path().join(".visp").join("rules");
        fs::create_dir_all(&rules_dir).unwrap();

        // Marker beyond 5th line should NOT be detected
        let content = "line1\nline2\nline3\nline4\nline5\nalwaysApply: true\n# Should be ignored";
        fs::write(rules_dir.join("test.md"), content).unwrap();

        let engine = RuleEngine::new(dir.path()).unwrap();
        assert!(engine.get_active_rules().is_empty());
    }

    #[test]
    fn test_allow_whitespace_around_marker() {
        let dir = tempdir().unwrap();
        let rules_dir = dir.path().join(".visp").join("rules");
        fs::create_dir_all(&rules_dir).unwrap();

        fs::write(
            rules_dir.join("test.md"),
            "  alwaysApply: true  \n# Rule with whitespace",
        )
        .unwrap();

        let engine = RuleEngine::new(dir.path()).unwrap();
        let rules = engine.get_active_rules();
        assert!(rules.contains("# Rule with whitespace"));
    }

    #[test]
    fn test_loads_project_agents_md() {
        let dir = tempdir().unwrap();
        let agents_path = dir.path().join("AGENTS.md");
        fs::write(
            &agents_path,
            "<Role>\nYou are a Rust coding assistant.\n</Role>",
        )
        .unwrap();

        let engine = RuleEngine::new(dir.path()).unwrap();
        let rules = engine.get_active_rules();
        assert!(rules.contains("Instructions from:"));
        assert!(rules.contains("Rust coding assistant"));
    }

    #[test]
    fn test_missing_agents_md_no_error() {
        let dir = tempdir().unwrap();
        // No AGENTS.md file exists
        let engine = RuleEngine::new(dir.path()).unwrap();
        // Should contain nothing (no rules dirs either)
        assert!(engine.get_active_rules().is_empty());
    }

    #[test]
    fn test_agents_md_before_rules_order() {
        let dir = tempdir().unwrap();
        // Create AGENTS.md
        fs::write(dir.path().join("AGENTS.md"), "<Role>Agent role</Role>").unwrap();
        // Create .visp/rules/ with a rule
        let rules_dir = dir.path().join(".visp").join("rules");
        fs::create_dir_all(&rules_dir).unwrap();
        fs::write(
            rules_dir.join("test.md"),
            "alwaysApply: true\n# Custom rule",
        )
        .unwrap();

        let engine = RuleEngine::new(dir.path()).unwrap();
        let rules = engine.get_active_rules();
        // AGENTS.md should come first (higher priority)
        let agents_pos = rules.find("Agent role").unwrap();
        let rule_pos = rules.find("Custom rule").unwrap();
        assert!(agents_pos < rule_pos);
    }

    #[test]
    fn test_discover_ancestor_agents_md() {
        // Simulate: project/subdir/ with AGENTS.md in project/
        let tmp = tempdir().unwrap();
        let project = tmp.path().join("project");
        let subdir = project.join("subdir");
        fs::create_dir_all(&subdir).unwrap();

        // AGENTS.md in parent directory (project root)
        fs::write(project.join("AGENTS.md"), "Ancestor instructions").unwrap();

        // RuleEngine created from subdir should discover ancestor AGENTS.md
        // （注入 home=tmp，使 `$HOME` 边界不成为停止原因）
        let engine = isolated_engine_with_home(&subdir, Some(tmp.path()));
        let rules = engine.get_active_rules();
        assert!(rules.contains("Ancestor instructions"));
    }

    #[test]
    fn test_agents_md_closest_highest_priority() {
        // Simulate: project/AGENTS.md and project/subdir/AGENTS.md
        let tmp = tempdir().unwrap();
        let subdir = tmp.path().join("subdir");
        fs::create_dir_all(&subdir).unwrap();

        fs::write(tmp.path().join("AGENTS.md"), "Root instructions").unwrap();
        fs::write(subdir.join("AGENTS.md"), "Subdir instructions").unwrap();

        // RuleEngine created from subdir should have both, subdir first
        let engine = isolated_engine_with_home(&subdir, Some(tmp.path()));
        let rules = engine.get_active_rules();
        assert!(rules.contains("Root instructions"));
        assert!(rules.contains("Subdir instructions"));

        // Subdir instructions should come first (closer)
        let sub_pos = rules.find("Subdir instructions").unwrap();
        let root_pos = rules.find("Root instructions").unwrap();
        assert!(sub_pos < root_pos);
    }

    /// 构造一个不读取真实全局配置、且向上遍历止于注入 `home` 的引擎（测试隔离）。
    fn isolated_engine_with_home(project_path: &Path, home: Option<&Path>) -> RuleEngine {
        RuleEngine::with_global_sources(project_path, None, None, home.map(Path::to_path_buf))
            .unwrap()
    }

    /// 默认投影：`home` 不可解析语义，仅加载项目层。
    fn isolated_engine(project_path: &Path) -> RuleEngine {
        isolated_engine_with_home(project_path, None)
    }

    // ---- 步骤 2a：RuleEngine 重载 ----

    #[test]
    fn test_reload_applies_new_content() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("AGENTS.md"), "<Role>旧角色</Role>").unwrap();

        let engine = isolated_engine(dir.path());
        assert!(engine.get_active_rules().contains("旧角色"));

        fs::write(dir.path().join("AGENTS.md"), "<Role>新角色</Role>").unwrap();
        let result = engine.reload().unwrap();

        assert!(result.changed);
        assert!(result.ruleset.content.contains("新角色"));
        let rules = engine.get_active_rules();
        assert!(rules.contains("新角色"));
        assert!(!rules.contains("旧角色"));
    }

    #[test]
    fn test_reload_discovers_ancestor_agents_md_in_order() {
        // project/subdir 与 project 各有一个 AGENTS.md，重扫按「近先远后」拼接
        let tmp = tempdir().unwrap();
        let project = tmp.path().join("project");
        let subdir = project.join("subdir");
        fs::create_dir_all(&subdir).unwrap();
        fs::write(project.join("AGENTS.md"), "上级指令").unwrap();
        fs::write(subdir.join("AGENTS.md"), "项目指令").unwrap();

        let engine = isolated_engine_with_home(&subdir, Some(tmp.path()));
        let result = engine.reload().unwrap();
        let rules = &result.ruleset.content;

        let near = rules.find("项目指令").unwrap();
        let far = rules.find("上级指令").unwrap();
        assert!(near < far, "近的 AGENTS.md 应先于远的");
    }

    #[test]
    fn test_reload_io_failure_keeps_old_state() {
        let dir = tempdir().unwrap();
        let rules_dir = dir.path().join(".visp").join("rules");
        fs::create_dir_all(&rules_dir).unwrap();
        fs::write(rules_dir.join("good.md"), "alwaysApply: true\n# 旧规则").unwrap();

        let engine = isolated_engine(dir.path());
        let before = engine.get_active_rules();
        assert!(before.contains("# 旧规则"));

        // 用目录冒充 .md 文件：collect_rules 读取该条目必然 IO 失败
        fs::create_dir_all(rules_dir.join("zzz.md")).unwrap();

        let result = engine.reload();
        assert!(result.is_err(), "构建阶段 IO 失败应返回错误");
        assert_eq!(
            engine.get_active_rules(),
            before,
            "失败时必须保留旧 RuleSet，不得清空"
        );
    }

    #[test]
    fn test_reload_missing_rules_dir_is_tolerated() {
        let dir = tempdir().unwrap();
        // 无 .visp/rules 目录
        let engine = isolated_engine(dir.path());

        let result = engine.reload().unwrap();

        assert_eq!(result.ruleset.files.len(), 0);
        assert!(result.ruleset.content.is_empty());
        assert!(engine.get_active_rules().is_empty());
    }

    #[test]
    fn test_reload_exposes_concatenated_content_for_guard() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("AGENTS.md"), "守卫素材 A").unwrap();

        let engine = isolated_engine(dir.path());
        let first = engine.reload().unwrap();
        assert!(first.ruleset.content.contains("守卫素材 A"));
        assert_eq!(first.ruleset.content, engine.get_active_rules());

        fs::write(dir.path().join("AGENTS.md"), "守卫素材 B").unwrap();
        let second = engine.reload().unwrap();

        assert!(second.changed, "内容变化应被守卫素材检出");
        assert!(second.ruleset.content.contains("守卫素材 B"));
        assert_ne!(first.ruleset.content, second.ruleset.content);
        assert_eq!(second.ruleset.content, engine.get_active_rules());
    }

    // ---- 步骤 5a：AGENTS.md 向上边界 + 共享祖先链函数 ----

    /// 用例 1：某层含 `.git` 目录 → 处理完该层后停止，git 根之上不加载。
    #[test]
    fn test_ancestors_stop_at_git_root() {
        let tmp = tempdir().unwrap();
        let repo = tmp.path().join("repo");
        let sub = repo.join("sub");
        fs::create_dir_all(&sub).unwrap();
        fs::create_dir_all(repo.join(".git")).unwrap(); // `.git` 目录即边界
        fs::write(repo.join("AGENTS.md"), "仓库层指令").unwrap();
        fs::write(tmp.path().join("AGENTS.md"), "git 根之上泄漏指令").unwrap();

        let chain = crate::path::agents_md_ancestors_with_home(&sub, Some(tmp.path()));
        assert_eq!(chain, vec![sub.clone(), repo.clone()]);

        // home=tmp 使 `$HOME` 边界不成为停止原因，从而隔离验证 git 边界
        let rules = isolated_engine_with_home(&sub, Some(tmp.path())).get_active_rules();
        assert!(rules.contains("仓库层指令"));
        assert!(!rules.contains("git 根之上泄漏指令"));
    }

    /// 用例 2：monorepo 子目录项目加载至 git 根的各层。
    #[test]
    fn test_monorepo_loads_up_to_git_root() {
        let tmp = tempdir().unwrap();
        let repo = tmp.path().join("repo");
        let packages = repo.join("packages");
        let pkg = packages.join("pkg");
        fs::create_dir_all(&pkg).unwrap();
        fs::create_dir_all(repo.join(".git")).unwrap();
        fs::write(repo.join("AGENTS.md"), "monorepo 根指令").unwrap();
        fs::write(packages.join("AGENTS.md"), "包集指令").unwrap();
        fs::write(tmp.path().join("AGENTS.md"), "越界指令").unwrap();

        let chain = crate::path::agents_md_ancestors_with_home(&pkg, Some(tmp.path()));
        assert_eq!(chain, vec![pkg.clone(), packages.clone(), repo.clone()]);

        let rules = isolated_engine_with_home(&pkg, Some(tmp.path())).get_active_rules();
        assert!(rules.contains("monorepo 根指令"));
        assert!(rules.contains("包集指令"));
        assert!(!rules.contains("越界指令"));
    }

    /// 用例 3：`.git` 是文件（worktree / submodule）→ 同样视为边界。
    #[test]
    fn test_git_file_is_boundary_for_worktree() {
        let tmp = tempdir().unwrap();
        let worktree = tmp.path().join("worktree");
        let sub = worktree.join("sub");
        fs::create_dir_all(&sub).unwrap();
        fs::write(
            worktree.join(".git"),
            "gitdir: /elsewhere/.git/worktrees/x\n",
        )
        .unwrap();
        fs::write(worktree.join("AGENTS.md"), "worktree 指令").unwrap();
        fs::write(tmp.path().join("AGENTS.md"), "越界指令").unwrap();

        let chain = crate::path::agents_md_ancestors_with_home(&sub, Some(tmp.path()));
        assert_eq!(chain, vec![sub.clone(), worktree.clone()]);

        let rules = isolated_engine_with_home(&sub, Some(tmp.path())).get_active_rules();
        assert!(rules.contains("worktree 指令"));
        assert!(!rules.contains("越界指令"));
    }

    /// 用例 4：非 git 且项目在 `$HOME` 之下 → 止于 `$HOME`（该层含在内）。
    #[test]
    fn test_non_git_under_home_stops_at_home_inclusive() {
        let tmp = tempdir().unwrap();
        let home = tmp.path().join("home");
        let project = home.join("work").join("proj");
        fs::create_dir_all(&project).unwrap();
        fs::write(home.join("AGENTS.md"), "HOME 层指令").unwrap();
        fs::write(tmp.path().join("AGENTS.md"), "HOME 之上指令").unwrap();

        let chain = crate::path::agents_md_ancestors_with_home(&project, Some(&home));
        assert_eq!(
            chain,
            vec![project.clone(), home.join("work"), home.clone()]
        );

        let rules = isolated_engine_with_home(&project, Some(&home)).get_active_rules();
        assert!(rules.contains("HOME 层指令"), "$HOME 层本身应含在内");
        assert!(!rules.contains("HOME 之上指令"));
    }

    /// 用例 5：非 git 且项目不在 `$HOME` 之下 → 不向上，只加载项目层。
    #[test]
    fn test_project_outside_home_does_not_walk_up() {
        let tmp = tempdir().unwrap();
        let home = tmp.path().join("home");
        let outside = tmp.path().join("outside");
        let project = outside.join("proj");
        fs::create_dir_all(&home).unwrap();
        fs::create_dir_all(&project).unwrap();
        fs::write(project.join("AGENTS.md"), "项目层指令").unwrap();
        fs::write(outside.join("AGENTS.md"), "祖先指令").unwrap();

        let chain = crate::path::agents_md_ancestors_with_home(&project, Some(&home));
        assert_eq!(chain, vec![project.clone()]);

        let rules = isolated_engine_with_home(&project, Some(&home)).get_active_rules();
        assert!(rules.contains("项目层指令"));
        assert!(!rules.contains("祖先指令"));
    }

    /// 用例 6：`$HOME` 不可解析 → 只加载项目层。
    #[test]
    fn test_unresolvable_home_loads_only_project_layer() {
        let tmp = tempdir().unwrap();
        let parent = tmp.path().join("a");
        let project = parent.join("proj");
        fs::create_dir_all(&project).unwrap();
        fs::write(parent.join("AGENTS.md"), "祖先指令").unwrap();
        fs::write(project.join("AGENTS.md"), "项目层指令").unwrap();

        let chain = crate::path::agents_md_ancestors_with_home(&project, None);
        assert_eq!(chain, vec![project.clone()]);

        let rules = isolated_engine_with_home(&project, None).get_active_rules();
        assert!(rules.contains("项目层指令"));
        assert!(!rules.contains("祖先指令"));
    }

    /// 用例 7：祖先链函数可复用——输出为祖先目录列表（近先远后，均为目录）。
    #[test]
    fn test_ancestor_chain_resolver_is_reusable() {
        let tmp = tempdir().unwrap();
        let home = tmp.path().join("home");
        let project = home.join("a").join("b");
        fs::create_dir_all(&project).unwrap();

        let chain = crate::path::agents_md_ancestors_with_home(&project, Some(&home));
        assert_eq!(chain, vec![project.clone(), home.join("a"), home.clone()]);
        assert!(chain.iter().all(|p| p.is_dir()), "链中每项均为目录");
        assert!(
            chain
                .iter()
                .all(|p| p.file_name() != Some(std::ffi::OsStr::new("AGENTS.md"))),
            "链中不含 AGENTS.md 文件名"
        );
    }

    /// 用例 8：全局 AGENTS.md 通道不受边界改动影响（与向上遍历正交）。
    #[test]
    fn test_global_agents_md_channel_unchanged() {
        let tmp = tempdir().unwrap();
        let project = tmp.path().join("proj");
        fs::create_dir_all(&project).unwrap();
        fs::write(project.join("AGENTS.md"), "项目 AGENTS").unwrap();

        let global_dir = tmp.path().join("global");
        fs::create_dir_all(&global_dir).unwrap();
        fs::write(global_dir.join("AGENTS.md"), "全局 AGENTS").unwrap();

        let engine = RuleEngine::with_global_sources(
            &project,
            Some(global_dir.join("AGENTS.md")),
            None,
            None, // home 不可解析 → 只项目层；全局通道独立生效
        )
        .unwrap();

        let rules = engine.get_active_rules();
        assert!(rules.contains("项目 AGENTS"));
        assert!(rules.contains("全局 AGENTS"));
        assert!(
            rules.find("项目 AGENTS").unwrap() < rules.find("全局 AGENTS").unwrap(),
            "项目/祖先 AGENTS 应先于全局通道"
        );
    }
}
