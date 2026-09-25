//! 监听目标声明与匹配/排除规则测试（计划 1a，6 例）。
//!
//! 判定为纯函数，不触碰文件系统，路径素材全为字符串构造。

use super::*;
use std::path::Path;

/// 构造目标，省去重复字段。
fn target(root: &str, mode: WatchMode, includes: Vec<Include>, excludes: Vec<&str>) -> WatchTarget {
    WatchTarget {
        root: root.into(),
        mode,
        filter: FilterRules {
            includes,
            excludes: excludes.into_iter().map(String::from).collect(),
        },
    }
}

#[test]
fn two_modes_express_scope_including_directories() {
    let root = "/proj";
    let recursive = target(root, WatchMode::Recursive, vec![], vec![]);
    let direct = target(root, WatchMode::DirectChildren, vec![], vec![]);

    // 递归：根及整个子树。
    assert!(recursive.matches(Path::new("/proj/src/lib.rs")));
    assert!(recursive.matches(Path::new("/proj/src")));
    // 直接子级：直接子文件与直接子目录均命中（模式必须含目录）。
    assert!(direct.matches(Path::new("/proj/main.rs")));
    assert!(direct.matches(Path::new("/proj/src")));
    // 直接子级不下探更深层。
    assert!(!direct.matches(Path::new("/proj/src/lib.rs")));
    // 根自身不是「直接子项」。
    assert!(!direct.matches(Path::new("/proj")));
}

#[test]
fn include_filename_matches_exactly() {
    let t = target(
        "/proj",
        WatchMode::DirectChildren,
        vec![Include::FileName("AGENTS.md".into())],
        vec![],
    );

    assert!(t.matches(Path::new("/proj/AGENTS.md")));
    assert!(!t.matches(Path::new("/proj/README.md")));
    // 大小写精确：不承诺大小写不敏感平台。
    assert!(!t.matches(Path::new("/proj/agents.md")));
}

#[test]
fn include_extension_accepts_dot_and_bare() {
    for spec in [".md", "md"] {
        let t = target(
            "/proj",
            WatchMode::Recursive,
            vec![Include::Extension(spec.into())],
            vec![],
        );
        assert!(
            t.matches(Path::new("/proj/docs/a.md")),
            "书写 `{spec}` 应命中 .md"
        );
        assert!(
            !t.matches(Path::new("/proj/docs/a.rs")),
            "书写 `{spec}` 不应命中 .rs"
        );
    }
}

#[test]
fn include_prefix_matches_subtree() {
    let t = target(
        "/proj",
        WatchMode::Recursive,
        vec![Include::Prefix("rules".into())],
        vec![],
    );

    assert!(t.matches(Path::new("/proj/rules/a.md")));
    assert!(t.matches(Path::new("/proj/rules/nested/b.md")));
    assert!(!t.matches(Path::new("/proj/other/a.md")));
    // 组件级前缀：`rules-old` 不应命中 `rules`。
    assert!(!t.matches(Path::new("/proj/rules-old/a.md")));
}

#[test]
fn exclude_by_path_component() {
    let t = target(
        "/proj",
        WatchMode::Recursive,
        vec![],
        vec!["node_modules", ".git"],
    );

    assert!(!t.matches(Path::new("/proj/node_modules/pkg/a.js")));
    assert!(!t.matches(Path::new("/proj/.git/config")));
    assert!(t.matches(Path::new("/proj/src/a.js")));
    // 组件名精确：`node_modules_extra` 不被排除。
    assert!(t.matches(Path::new("/proj/node_modules_extra/a.js")));
}

#[test]
fn excludes_take_priority_over_includes_and_case_is_exact() {
    let t = target(
        "/proj",
        WatchMode::Recursive,
        vec![Include::Extension(".md".into())],
        vec!["node_modules"],
    );

    // 排除优先：命中包含但被排除 → 不命中。
    assert!(!t.matches(Path::new("/proj/node_modules/readme.md")));
    assert!(t.matches(Path::new("/proj/docs/readme.md")));
    assert!(!t.matches(Path::new("/proj/docs/main.rs")));

    // 排除大小写严格：`Node_Modules` 不排除 `node_modules`。
    let upper = target("/proj", WatchMode::Recursive, vec![], vec!["Node_Modules"]);
    assert!(upper.matches(Path::new("/proj/node_modules/a.js")));
}
