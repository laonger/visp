//! 监听计划构建测试（步骤 6a）。
//!
//! 计划构建是纯函数（只读传入路径的存在性），用 tempdir 构造存在性组合即可，
//! 不触碰真实 `~/.config/visp`，无需 env 隔离与 `#[serial]`。

use super::*;
use std::fs;

use tempfile::TempDir;

/// 取出指定目录的监听条目。
fn entry<'a>(plan: &'a WatchPlan, dir: &Path) -> &'a WatchEntry {
    plan.entries
        .iter()
        .find(|entry| entry.dir == dir)
        .unwrap_or_else(|| panic!("缺少监听条目 {dir:?}"))
}

/// 分类结果领域集合。
fn domains(plan: &WatchPlan, path: &Path) -> Option<Vec<ReloadDomain>> {
    plan.classify(path)
        .map(|classification| classification.domains)
}

/// 构造一个「项目与全局目录素材齐备」的组合。
fn full_setup() -> (TempDir, TempDir) {
    let project = TempDir::new().unwrap();
    let global = TempDir::new().unwrap();
    for sub in ["rules", "agents", "skills"] {
        fs::create_dir_all(project.path().join(".visp").join(sub)).unwrap();
        fs::create_dir_all(global.path().join(sub)).unwrap();
    }
    (project, global)
}

#[test]
fn nine_category_paths_map_to_domains() {
    let (project, global) = full_setup();
    let plan = WatchPlan::build(project.path(), Some(global.path()));

    // #1 祖先 / project 根：非递归。
    assert!(!entry(&plan, project.path()).recursive);
    // #2 全局根：非递归。
    assert!(!entry(&plan, global.path()).recursive);
    // #6 项目 `.visp`：非递归。
    assert!(!entry(&plan, &project.path().join(".visp")).recursive);
    // #3/#4/#5 全局 rules/agents/skills：递归。
    for sub in ["rules", "agents", "skills"] {
        assert!(
            entry(&plan, &global.path().join(sub)).recursive,
            "全局 {sub} 应递归监听"
        );
        // #7/#8/#9 项目 rules/agents/skills：递归。
        assert!(
            entry(&plan, &project.path().join(".visp").join(sub)).recursive,
            "项目 {sub} 应递归监听"
        );
    }

    let project = project.path();
    let global = global.path();
    // rules 三类来源
    assert_eq!(
        domains(&plan, &project.join("AGENTS.md")),
        Some(vec![ReloadDomain::Rules])
    );
    assert_eq!(
        domains(&plan, &project.join(".visp/rules/a.md")),
        Some(vec![ReloadDomain::Rules])
    );
    assert_eq!(
        domains(&plan, &global.join("rules/a.md")),
        Some(vec![ReloadDomain::Rules])
    );
    // agents 两类来源
    assert_eq!(
        domains(&plan, &project.join(".visp/agents/a.md")),
        Some(vec![ReloadDomain::Agents])
    );
    assert_eq!(
        domains(&plan, &global.join("agents/a.md")),
        Some(vec![ReloadDomain::Agents])
    );
    // skills 两类来源（任意事件，含 SKILL.md）
    assert_eq!(
        domains(&plan, &project.join(".visp/skills/my-skill/SKILL.md")),
        Some(vec![ReloadDomain::Skills])
    );
    assert_eq!(
        domains(&plan, &global.join("skills/my-skill/SKILL.md")),
        Some(vec![ReloadDomain::Skills])
    );
    // 全局顶层 AGENTS.md
    assert_eq!(
        domains(&plan, &global.join("AGENTS.md")),
        Some(vec![ReloadDomain::Rules])
    );
}

#[test]
fn ancestor_chain_is_non_recursive() {
    let base = TempDir::new().unwrap();
    let project = base.path().join("a/b/c");
    fs::create_dir_all(&project).unwrap();
    let plan = WatchPlan::build(&project, None);

    for ancestor in ancestors_inclusive(&project) {
        assert!(
            !entry(&plan, &ancestor).recursive,
            "祖先 {ancestor:?} 应非递归监听"
        );
    }

    // project 根这一层含 `.visp` 子树规则：`.visp` 创建可触发补挂校准（空领域），
    // `.visp/rules` 命中 rules。
    assert_eq!(
        plan.classify(&project.join(".visp")),
        Some(Classification { domains: vec![] })
    );
    assert_eq!(
        domains(&plan, &project.join(".visp/rules/a.md")),
        Some(vec![ReloadDomain::Rules])
    );
}

#[test]
fn fresh_project_without_visp_uses_root_fallback() {
    let base = TempDir::new().unwrap();
    let project = base.path().join("fresh");
    fs::create_dir_all(&project).unwrap();
    assert!(!project.join(".visp").exists());

    let plan = WatchPlan::build(&project, None);

    // 降级：project 根非递归监听，`.visp` 子树前缀规则命中补挂链。
    assert!(!entry(&plan, &project).recursive);
    assert_eq!(
        plan.classify(&project.join(".visp")),
        Some(Classification { domains: vec![] })
    );
    assert_eq!(
        domains(&plan, &project.join(".visp/agents/reviewer.md")),
        Some(vec![ReloadDomain::Agents])
    );
    assert_eq!(
        domains(&plan, &project.join(".visp/skills/x/SKILL.md")),
        Some(vec![ReloadDomain::Skills])
    );

    // `.visp` 尚不存在，但计划中已保留其递归目标（供运行中补挂 + 重扫）。
    let visp_agents = entry(&plan, &project.join(".visp/agents"));
    assert!(visp_agents.recursive);
    assert_eq!(visp_agents.rescan_domains, vec![ReloadDomain::Agents]);
}

#[test]
fn missing_subdirs_fall_back_to_nearest_ancestor() {
    let project = TempDir::new().unwrap();
    let global = TempDir::new().unwrap();
    // 只建 `.visp` 与全局根，不建 rules/agents/skills。
    fs::create_dir_all(project.path().join(".visp")).unwrap();

    let plan = WatchPlan::build(project.path(), Some(global.path()));

    // `.visp` 存在 → 其子目录创建事件直接分流（无需 project 根前缀规则参与）。
    assert_eq!(
        domains(&plan, &project.path().join(".visp/rules")),
        Some(vec![ReloadDomain::Rules])
    );
    assert_eq!(
        domains(&plan, &project.path().join(".visp/agents")),
        Some(vec![ReloadDomain::Agents])
    );
    assert_eq!(
        domains(&plan, &project.path().join(".visp/skills")),
        Some(vec![ReloadDomain::Skills])
    );

    // 全局根存在 → 三个子目录创建事件分流。
    assert_eq!(
        domains(&plan, &global.path().join("rules")),
        Some(vec![ReloadDomain::Rules])
    );
    assert_eq!(
        domains(&plan, &global.path().join("agents")),
        Some(vec![ReloadDomain::Agents])
    );
    assert_eq!(
        domains(&plan, &global.path().join("skills")),
        Some(vec![ReloadDomain::Skills])
    );

    // 目标目录仍作为递归补挂目标保留。
    assert!(
        entry(&plan, &project.path().join(".visp/rules")).recursive,
        "缺失子目录应保留递归补挂目标"
    );
    assert!(
        entry(&plan, &global.path().join("rules")).recursive,
        "缺失全局子目录应保留递归补挂目标"
    );
}

#[test]
fn excluded_paths_produce_no_domain() {
    let (project, global) = full_setup();
    let plan = WatchPlan::build(project.path(), Some(global.path()));
    let project = project.path();
    let global = global.path();

    let excluded = [
        project.join(".visp/system-prompt.md"),
        project.join(".visp/daemon.toml"),
        project.join(".visp/webfetch.toml"),
        project.join(".visp/codegraph.db"),
        project.join(".visp/logs/daemon.log"),
        project.join(".visp/.startup-error"),
        global.join("system-prompt.md"),
        global.join("daemon.toml"),
        global.join("webfetch.toml"),
        global.join("logs/daemon.log"),
    ];
    for path in excluded {
        assert_eq!(plan.classify(&path), None, "{path:?} 不应命中任何领域");
    }

    // 对照：非排除文件正常命中。
    assert_eq!(
        domains(&plan, &project.join(".visp/rules/a.md")),
        Some(vec![ReloadDomain::Rules])
    );
}

#[test]
fn global_root_children_dispatch_by_subdir() {
    let global = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    let plan = WatchPlan::build(project.path(), Some(global.path()));
    let global = global.path();

    assert_eq!(
        domains(&plan, &global.join("AGENTS.md")),
        Some(vec![ReloadDomain::Rules])
    );
    assert_eq!(
        domains(&plan, &global.join("rules")),
        Some(vec![ReloadDomain::Rules])
    );
    assert_eq!(
        domains(&plan, &global.join("agents")),
        Some(vec![ReloadDomain::Agents])
    );
    assert_eq!(
        domains(&plan, &global.join("skills")),
        Some(vec![ReloadDomain::Skills])
    );
    // 顶层其他文件（daemon.toml 等）不命中。
    assert_eq!(plan.classify(&global.join("daemon.toml")), None);
}
