//! 底层事件类型归一化（设计 §4.6）。
//!
//! 输入后端原始形态的事件，输出零或多个 `(路径, 类型)`；纯函数、无 IO。
//! 「重命名」不是独立事件类型，而是语义糖：**删除(旧) + 创建(新)**。

use std::collections::HashSet;
use std::path::PathBuf;

/// 归一化后的事件类型（设计 §4.6）：创建 / 修改 / 删除。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventType {
    /// 新建（或原子替换后的重新出现）。
    Created,
    /// 内容写入或元数据变更（保守归一化）。
    Modified,
    /// 删除（或重命名的旧路径）。
    Removed,
}

/// 后端原始事件形态（三类后端差异集中于此）。
///
/// - kqueue：重命名报 [`RawEvent::RenameFrom`]（`Any`，单事件仅旧路径）；
/// - inotify：`From` → [`RawEvent::RenameFrom`]、`To` → [`RawEvent::RenameTo`]、
///   `Both` → [`RawEvent::RenameBoth`]；
/// - Windows：仅 `From` / `To`，**无 `Both`**。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RawEvent {
    /// 新建文件 / 目录。
    Create(PathBuf),
    /// 内容写入（write / truncate / O_TRUNC）或元数据变更（chmod / mtime）。
    Modify(PathBuf),
    /// 删除。
    Remove(PathBuf),
    /// 重命名的旧路径（kqueue `Any` / inotify·Windows `From`）。
    RenameFrom(PathBuf),
    /// 重命名的新路径（inotify·Windows `To`）。
    RenameTo(PathBuf),
    /// inotify `Both`：单事件同时含旧、新路径。
    RenameBoth { from: PathBuf, to: PathBuf },
}

/// 归一化单个底层事件（纯函数）。
///
/// `mounted_dirs` 为**模块直接挂载的目录**集合；命中其的 Modify 视为目录自身的
/// 条目级变更而抑制。判定只用该集合，**不得**用 `path.is_dir()`。
///
/// **范围限定**：抑制仅保证覆盖模块直接挂载的目录；递归目标的子树目录不在集合中，
/// 其条目级 Modify 不抑制、按「修改」下发。
pub fn normalize(event: &RawEvent, mounted_dirs: &HashSet<PathBuf>) -> Vec<(PathBuf, EventType)> {
    match event {
        RawEvent::Create(path) => vec![(path.clone(), EventType::Created)],
        RawEvent::Remove(path) => vec![(path.clone(), EventType::Removed)],
        RawEvent::Modify(path) => {
            if mounted_dirs.contains(path) {
                Vec::new()
            } else {
                vec![(path.clone(), EventType::Modified)]
            }
        }
        RawEvent::RenameFrom(old) => vec![(old.clone(), EventType::Removed)],
        RawEvent::RenameTo(new) => vec![(new.clone(), EventType::Created)],
        RawEvent::RenameBoth { from, to } => vec![
            (from.clone(), EventType::Removed),
            (to.clone(), EventType::Created),
        ],
    }
}

#[cfg(test)]
#[path = "normalize_tests.rs"]
mod tests;
