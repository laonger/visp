//! watcher 运行时测试（计划 2a，7 例 + 适配层单测）。
//!
//! 集成用例**后端无关**：只断言「有限时间轮询的最终收敛」，不含 kqueue 特化断言，
//! 可在 Linux（inotify）与 macOS（kqueue）双方言跑。
//! 关键前提：验收文件的**在监听启动之前已存在**（否则 kqueue 目录 diff 会自动补挂，
//! 让缺陷实现也通过——虚假覆盖）。

use std::path::Path;
use std::time::Duration;

use tempfile::tempdir;
use tokio::sync::mpsc::UnboundedReceiver;
use tokio::time::{Instant, timeout};

use crate::normalize::RawEvent;
use crate::target::{FilterRules, Include, WatchTarget};

use super::*;

/// 构造直接子级目标。
fn direct(root: &Path, includes: Vec<Include>, excludes: &[&str]) -> WatchTarget {
    WatchTarget {
        root: root.to_path_buf(),
        mode: WatchMode::DirectChildren,
        filter: FilterRules {
            includes,
            excludes: excludes.iter().map(|s| (*s).to_string()).collect(),
        },
    }
}

/// 构造递归目标。
fn recursive(root: &Path, includes: Vec<Include>, excludes: &[&str]) -> WatchTarget {
    WatchTarget {
        root: root.to_path_buf(),
        mode: WatchMode::Recursive,
        filter: FilterRules {
            includes,
            excludes: excludes.iter().map(|s| (*s).to_string()).collect(),
        },
    }
}

/// 事件谓词。
fn is_event(message: &WatchMessage, path: &Path, kind: EventType) -> bool {
    matches!(
        message,
        WatchMessage::Event(FileEvent { path: p, kind: k }) if p == path && *k == kind
    )
}

/// 在给定时限内轮询，直到出现满足谓词的消息。
async fn wait_event<F>(rx: &mut UnboundedReceiver<WatchMessage>, dur: Duration, pred: F) -> bool
where
    F: Fn(&WatchMessage) -> bool,
{
    let deadline = Instant::now() + dur;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return false;
        }
        match timeout(remaining, rx.recv()).await {
            Ok(Some(message)) => {
                if pred(&message) {
                    return true;
                }
            }
            Ok(None) | Err(_) => return false,
        }
    }
}

// ── 集成用例（后端无关） ──────────────────────────────────────────────

/// 2a#1 核心修复项：启动前已存在文件的原地覆写 → 「修改」。
#[tokio::test]
async fn existing_file_overwrite_emits_modified() {
    let dir = tempdir().unwrap();
    let file = dir.path().join("AGENTS.md");
    std::fs::write(&file, "before").unwrap(); // 启动前已存在

    let target = direct(dir.path(), vec![Include::FileName("AGENTS.md".into())], &[]);
    let (watcher, mut rx) = start(vec![target]).await.unwrap();

    std::fs::write(&file, "after").unwrap(); // 原地覆写

    assert!(
        wait_event(&mut rx, Duration::from_secs(5), |m| is_event(
            m,
            &file,
            EventType::Modified
        ))
        .await,
        "启动前已存在文件的原地覆写必须产生修改事件"
    );
    watcher.stop();
}

/// 2a#2 新建文件 → 「创建」。
#[tokio::test]
async fn new_file_emits_created() {
    let dir = tempdir().unwrap();
    let target = direct(dir.path(), vec![Include::Extension("md".into())], &[]);
    let (watcher, mut rx) = start(vec![target]).await.unwrap();

    let file = dir.path().join("new.md");
    std::fs::write(&file, "hello").unwrap();

    assert!(
        wait_event(&mut rx, Duration::from_secs(5), |m| is_event(
            m,
            &file,
            EventType::Created
        ))
        .await,
        "新建文件应产生创建事件"
    );
    watcher.stop();
}

/// 2a#3 删除文件 → 「删除」。
#[tokio::test]
async fn removed_file_emits_removed() {
    let dir = tempdir().unwrap();
    let file = dir.path().join("gone.md");
    std::fs::write(&file, "x").unwrap();

    let target = direct(dir.path(), vec![Include::Extension("md".into())], &[]);
    let (watcher, mut rx) = start(vec![target]).await.unwrap();

    std::fs::remove_file(&file).unwrap();

    assert!(
        wait_event(&mut rx, Duration::from_secs(5), |m| is_event(
            m,
            &file,
            EventType::Removed
        ))
        .await,
        "删除文件应产生删除事件"
    );
    watcher.stop();
}

/// 2a#4 直接子级模式下的子目录创建须上报（G5 / daemon #6 依赖）。
#[tokio::test]
async fn direct_child_subdir_creation_is_reported() {
    let dir = tempdir().unwrap();
    let target = direct(dir.path(), vec![], &[]);
    let (watcher, mut rx) = start(vec![target]).await.unwrap();

    let sub = dir.path().join("sub");
    std::fs::create_dir(&sub).unwrap();

    assert!(
        wait_event(&mut rx, Duration::from_secs(5), |m| is_event(
            m,
            &sub,
            EventType::Created
        ))
        .await,
        "直接子级模式必须上报子目录创建"
    );
    watcher.stop();
}

/// 2a#5 动态补挂：目标目录不存在 → 运行中创建 → 重扫信号 + 事件。
#[tokio::test]
async fn missing_target_dir_is_dynamically_attached() {
    let dir = tempdir().unwrap();
    let missing = dir.path().join("watched");
    let target = direct(&missing, vec![], &[]);
    let (watcher, mut rx) = start(vec![target]).await.unwrap();

    std::fs::create_dir(&missing).unwrap();
    assert!(
        wait_event(&mut rx, Duration::from_secs(5), |m| matches!(
            m,
            WatchMessage::Rescan
        ))
        .await,
        "缺目录补挂应发出一次重扫信号"
    );

    let file = missing.join("hello.md");
    std::fs::write(&file, "x").unwrap();
    assert!(
        wait_event(&mut rx, Duration::from_secs(5), |m| is_event(
            m,
            &file,
            EventType::Created
        ))
        .await,
        "补挂后写入应产生事件"
    );
    watcher.stop();
}

/// 2a#6 递归目标下子树既有文件覆写 → 「修改」。
#[tokio::test]
async fn recursive_target_overwrite_in_subtree_emits_modified() {
    let dir = tempdir().unwrap();
    let sub = dir.path().join("sub");
    std::fs::create_dir_all(&sub).unwrap();
    let file = sub.join("a.md");
    std::fs::write(&file, "before").unwrap();

    let target = recursive(dir.path(), vec![Include::Extension("md".into())], &[]);
    let (watcher, mut rx) = start(vec![target]).await.unwrap();

    std::fs::write(&file, "after").unwrap();

    assert!(
        wait_event(&mut rx, Duration::from_secs(5), |m| is_event(
            m,
            &file,
            EventType::Modified
        ))
        .await,
        "递归目标下子树既有文件覆写应产生修改事件"
    );
    watcher.stop();
}

/// 2a#7 过滤生效：未命中包含规则的事件不下发。
#[tokio::test]
async fn non_matching_events_are_filtered() {
    let dir = tempdir().unwrap();
    let target = direct(
        dir.path(),
        vec![Include::Extension("md".into())],
        &["node_modules"],
    );
    let (watcher, mut rx) = start(vec![target]).await.unwrap();

    // 无关扩展名：不命中包含规则。
    std::fs::write(dir.path().join("ignore.txt"), "x").unwrap();

    let got = timeout(Duration::from_millis(600), rx.recv()).await;
    assert!(got.is_err(), "未命中过滤规则的事件不应下发，却收到 {got:?}");
    watcher.stop();
}

// ── 适配层单测（备注 13：notify::Event → RawEvent） ──────────────────

fn notify_event(kind: EventKind, paths: &[&str]) -> NotifyEvent {
    let mut event = NotifyEvent::new(kind);
    for path in paths {
        event = event.add_path(PathBuf::from(path));
    }
    event
}

#[test]
fn adapt_create_and_remove() {
    use notify::event::{CreateKind, RemoveKind};
    assert_eq!(
        adapt(&notify_event(
            EventKind::Create(CreateKind::File),
            &["/p/a.md"]
        )),
        vec![RawEvent::Create("/p/a.md".into())]
    );
    assert_eq!(
        adapt(&notify_event(
            EventKind::Remove(RemoveKind::File),
            &["/p/a.md"]
        )),
        vec![RawEvent::Remove("/p/a.md".into())]
    );
}

#[test]
fn adapt_data_and_metadata_modify() {
    use notify::event::{DataChange, MetadataKind};
    assert_eq!(
        adapt(&notify_event(
            EventKind::Modify(ModifyKind::Data(DataChange::Any)),
            &["/p/a.md"]
        )),
        vec![RawEvent::Modify("/p/a.md".into())]
    );
    assert_eq!(
        adapt(&notify_event(
            EventKind::Modify(ModifyKind::Metadata(MetadataKind::WriteTime)),
            &["/p/a.md"]
        )),
        vec![RawEvent::Modify("/p/a.md".into())]
    );
}

#[test]
fn adapt_kqueue_rename_any_uses_old_path() {
    assert_eq!(
        adapt(&notify_event(
            EventKind::Modify(ModifyKind::Name(RenameMode::Any)),
            &["/p/old.md"]
        )),
        vec![RawEvent::RenameFrom("/p/old.md".into())]
    );
}

#[test]
fn adapt_inotify_from_to() {
    assert_eq!(
        adapt(&notify_event(
            EventKind::Modify(ModifyKind::Name(RenameMode::From)),
            &["/p/old.md"]
        )),
        vec![RawEvent::RenameFrom("/p/old.md".into())]
    );
    assert_eq!(
        adapt(&notify_event(
            EventKind::Modify(ModifyKind::Name(RenameMode::To)),
            &["/p/new.md"]
        )),
        vec![RawEvent::RenameTo("/p/new.md".into())]
    );
}

#[test]
fn adapt_inotify_both_splits_from_and_to() {
    assert_eq!(
        adapt(&notify_event(
            EventKind::Modify(ModifyKind::Name(RenameMode::Both)),
            &["/p/old.md", "/p/new.md"]
        )),
        vec![RawEvent::RenameBoth {
            from: "/p/old.md".into(),
            to: "/p/new.md".into(),
        }]
    );
}

#[test]
fn adapt_access_and_other_are_ignored() {
    use notify::event::{AccessKind, AccessMode};
    assert!(
        adapt(&notify_event(
            EventKind::Access(AccessKind::Open(AccessMode::Any)),
            &["/p/a.md"]
        ))
        .is_empty()
    );
    assert!(adapt(&notify_event(EventKind::Other, &["/p/a.md"])).is_empty());
}
