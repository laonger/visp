//! 事件类型归一化测试（计划 1b，11 例）。
//!
//! 覆盖三类后端的事件形态 → 「创建 / 修改 / 删除」的映射，
//! 以及目录条目级 Modify 的范围限定抑制。纯函数、无 IO。

use super::*;
use std::collections::HashSet;
use std::path::PathBuf;

/// 构造「已直接挂载目录」集合。
fn mounted(paths: &[&str]) -> HashSet<PathBuf> {
    paths.iter().map(PathBuf::from).collect()
}

/// 期望输出。
fn ev(path: &str, ty: EventType) -> (PathBuf, EventType) {
    (PathBuf::from(path), ty)
}

#[test]
fn normalized_create() {
    let out = normalize(
        &RawEvent::Create("/proj/new.txt".into()),
        &mounted(&["/proj"]),
    );
    assert_eq!(out, vec![ev("/proj/new.txt", EventType::Created)]);
}

#[test]
fn content_write_is_modified() {
    // write / truncate / O_TRUNC 在归一化层同为「修改」。
    let out = normalize(
        &RawEvent::Modify("/proj/file.txt".into()),
        &mounted(&["/proj"]),
    );
    assert_eq!(out, vec![ev("/proj/file.txt", EventType::Modified)]);
}

#[test]
fn metadata_change_is_modified() {
    // chmod / mtime（如 touch）保守归一化为「修改」。
    let out = normalize(
        &RawEvent::Modify("/proj/file.txt".into()),
        &mounted(&["/proj"]),
    );
    assert_eq!(out, vec![ev("/proj/file.txt", EventType::Modified)]);
}

#[test]
fn normalized_remove() {
    let out = normalize(
        &RawEvent::Remove("/proj/gone.txt".into()),
        &mounted(&["/proj"]),
    );
    assert_eq!(out, vec![ev("/proj/gone.txt", EventType::Removed)]);
}

#[test]
fn kqueue_any_rename_yields_removed_old() {
    // kqueue `Any`：单事件、仅携带旧路径。
    let out = normalize(
        &RawEvent::RenameFrom("/proj/old.txt".into()),
        &mounted(&["/proj"]),
    );
    assert_eq!(out, vec![ev("/proj/old.txt", EventType::Removed)]);
}

#[test]
fn inotify_from_to_yields_removed_and_created() {
    // inotify `From`(旧) / `To`(新)：两事件分别归一化。
    let out = normalize(
        &RawEvent::RenameFrom("/proj/old.txt".into()),
        &mounted(&["/proj"]),
    );
    assert_eq!(out, vec![ev("/proj/old.txt", EventType::Removed)]);

    let out = normalize(
        &RawEvent::RenameTo("/proj/new.txt".into()),
        &mounted(&["/proj"]),
    );
    assert_eq!(out, vec![ev("/proj/new.txt", EventType::Created)]);
}

#[test]
fn inotify_both_yields_removed_and_created() {
    // inotify `Both`：单事件同时含旧 + 新，拆为删除 + 创建。
    let out = normalize(
        &RawEvent::RenameBoth {
            from: "/proj/old.txt".into(),
            to: "/proj/new.txt".into(),
        },
        &mounted(&["/proj"]),
    );
    assert_eq!(
        out,
        vec![
            ev("/proj/old.txt", EventType::Removed),
            ev("/proj/new.txt", EventType::Created),
        ]
    );
}

#[test]
fn windows_from_to_yields_removed_and_created() {
    // Windows 仅 `From` / `To`（无 `Both`），形态与 inotify 的两事件一致。
    let out = normalize(
        &RawEvent::RenameFrom("/proj/old.txt".into()),
        &mounted(&["/proj"]),
    );
    assert_eq!(out, vec![ev("/proj/old.txt", EventType::Removed)]);

    let out = normalize(
        &RawEvent::RenameTo("/proj/new.txt".into()),
        &mounted(&["/proj"]),
    );
    assert_eq!(out, vec![ev("/proj/new.txt", EventType::Created)]);
}

#[test]
fn dir_entry_modify_in_mounted_set_is_suppressed() {
    // 模块直接挂载的目录自身的条目级 Modify → 抑制、不下发。
    let out = normalize(&RawEvent::Modify("/proj".into()), &mounted(&["/proj"]));
    assert!(out.is_empty());
}

#[test]
fn dir_entry_modify_outside_mounted_set_is_emitted() {
    // 递归目标的子树目录不在集合中 → 不抑制，作为「修改」下发。
    let out = normalize(
        &RawEvent::Modify("/proj/subdir".into()),
        &mounted(&["/proj"]),
    );
    assert_eq!(out, vec![ev("/proj/subdir", EventType::Modified)]);
}

#[test]
fn temp_file_events_not_specially_suppressed() {
    // 命中过滤的临时文件事件按普通事件处理，不做基于文件名的特殊抑制。
    let m = mounted(&["/proj"]);

    let out = normalize(&RawEvent::Create("/proj/AGENTS.md.tmp".into()), &m);
    assert_eq!(out, vec![ev("/proj/AGENTS.md.tmp", EventType::Created)]);

    let out = normalize(&RawEvent::Modify("/proj/.AGENTS.md.swp".into()), &m);
    assert_eq!(out, vec![ev("/proj/.AGENTS.md.swp", EventType::Modified)]);

    let out = normalize(&RawEvent::Remove("/proj/AGENTS.md.tmp".into()), &m);
    assert_eq!(out, vec![ev("/proj/AGENTS.md.tmp", EventType::Removed)]);
}
