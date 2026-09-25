//! 缺目录降级链解析测试（计划 1c，5 例）。
//!
//! 存在性由谓词注入，测试用内存集合模拟，不触碰真实文件系统。

use super::*;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// 构造「仅给定路径存在」的谓词。
fn exists_in(paths: &[&str]) -> impl Fn(&Path) -> bool {
    let set: HashSet<PathBuf> = paths.iter().map(PathBuf::from).collect();
    move |p: &Path| set.contains(p)
}

#[test]
fn root_exists_watches_root_directly() {
    let plan = resolve_degradation(Path::new("/base/proj"), exists_in(&["/base/proj"]));

    assert_eq!(plan.mount, MountTarget::Direct("/base/proj".into()));
    assert_eq!(plan.ancestors, vec![PathBuf::from("/base/proj")]);
}

#[test]
fn missing_root_falls_back_to_nearest_ancestor() {
    let plan = resolve_degradation(Path::new("/base/proj"), exists_in(&["/base"]));

    assert_eq!(
        plan.mount,
        MountTarget::Fallback {
            ancestor: "/base".into(),
            pending: "proj".into(),
        }
    );
    assert_eq!(
        plan.ancestors,
        vec![PathBuf::from("/base/proj"), PathBuf::from("/base")]
    );
}

#[test]
fn multi_level_missing_ascends_each_level() {
    let plan = resolve_degradation(Path::new("/a/b/c/d"), exists_in(&["/a/b"]));

    assert_eq!(
        plan.mount,
        MountTarget::Fallback {
            ancestor: "/a/b".into(),
            pending: "c/d".into(),
        }
    );
    assert_eq!(
        plan.ancestors,
        vec![
            PathBuf::from("/a/b/c/d"),
            PathBuf::from("/a/b/c"),
            PathBuf::from("/a/b"),
        ]
    );
}

#[test]
fn no_mountable_ancestor_is_unmountable() {
    let plan = resolve_degradation(Path::new("/nowhere/x"), |_| false);

    assert_eq!(plan.mount, MountTarget::Unmountable);
    assert!(plan.ancestors.is_empty());
}

#[test]
fn ancestor_chain_is_reusable_directory_list() {
    let plan = resolve_degradation(Path::new("/a/b/c"), exists_in(&["/a"]));

    assert_eq!(
        plan.ancestors,
        vec![
            PathBuf::from("/a/b/c"),
            PathBuf::from("/a/b"),
            PathBuf::from("/a"),
        ]
    );
    // 全为绝对目录路径，且按「目标根 → 最近存在祖先」有序。
    assert!(plan.ancestors.iter().all(|p| p.is_absolute()));

    // 末项即最近存在祖先，可作挂载点；挂载点 + pending 可重建目标根。
    let MountTarget::Fallback { ancestor, pending } = &plan.mount else {
        panic!("应为降级挂载");
    };
    assert_eq!(plan.ancestors.last(), Some(ancestor));
    assert_eq!(ancestor.join(pending), PathBuf::from("/a/b/c"));
}
