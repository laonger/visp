use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::mpsc::UnboundedReceiver;
use visp_fs::normalize::EventType;
use visp_fs::runtime::{self, FsWatcher, WatchMessage};
use visp_fs::target::{FilterRules, Include, WatchMode, WatchTarget};

use crate::index::{CodeGraphConfig, FileEvent as IndexEvent, Indexer};

/// debounce 窗口：同一路径的快速连续变更合并为最终状态。
const DEBOUNCE: Duration = Duration::from_millis(500);

/// Watches a project directory recursively (through `visp-fs`) and triggers
/// incremental re-indexing through the provided `Indexer`.
///
/// codegraph 是**纯增量消费者**：仅消费文件事件，明确忽略 `visp-fs` 的重扫信号
/// （设计 §4.7 决策——它没有全量重扫能力，由 lazy 启动 + `build_full` 兜底）。
pub struct Watcher {
    watcher: FsWatcher,
    task: tokio::task::JoinHandle<()>,
}

impl Watcher {
    /// Start watching `project_path`.
    ///
    /// Returns immediately; file processing runs on a background tokio task.
    pub async fn start(
        project_path: &Path,
        indexer: Arc<Indexer>,
        config: CodeGraphConfig,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        // 一个递归目标 + 包含（扩展名，兼容含点/不含点）+ 排除（路径组件级目录名）。
        let target = WatchTarget {
            root: project_path.to_path_buf(),
            mode: WatchMode::Recursive,
            filter: FilterRules {
                includes: config
                    .supported_extensions
                    .into_iter()
                    .map(Include::Extension)
                    .collect(),
                excludes: config.exclude_dirs,
            },
        };

        let (watcher, rx) = runtime::start(vec![target]).await?;
        let project = project_path.to_path_buf();
        let task = tokio::spawn(consume_events(rx, indexer, project));

        Ok(Self { watcher, task })
    }

    /// Stop watching: abort the consumer task and release `visp-fs` watches.
    pub fn stop(self) {
        self.task.abort();
        self.watcher.stop();
    }
}

/// 消费 `visp-fs` 事件：按路径 debounce 后做增量索引。
///
/// - `Rescan`：显式忽略（设计 §4.7 决策）；
/// - `Degraded`：仅告警，不阻断（由 `build_full` 兜底）；
/// - 重命名的「删除(旧) + 创建(新)」同批次内先删后建，使跨文件引用可被重新解析。
async fn consume_events(
    mut rx: UnboundedReceiver<WatchMessage>,
    indexer: Arc<Indexer>,
    project: PathBuf,
) {
    loop {
        // 取首个文件事件；重扫 / 降级消息不驱动索引。
        let first = loop {
            match rx.recv().await {
                Some(WatchMessage::Event(event)) => break event,
                Some(WatchMessage::Rescan) => {}
                Some(WatchMessage::Degraded(reason)) => {
                    eprintln!("[visp-codegraph] watcher degraded: {:?}", reason);
                }
                None => return,
            }
        };

        let mut batch: HashMap<PathBuf, IndexEvent> = HashMap::new();
        batch.insert(first.path.clone(), classify(&first.path, first.kind));

        // 收集 debounce 窗口内的后续事件；每次到达都重置窗口。
        let sleep = tokio::time::sleep(DEBOUNCE);
        tokio::pin!(sleep);
        loop {
            tokio::select! {
                biased;
                message = rx.recv() => {
                    match message {
                        Some(WatchMessage::Event(event)) => {
                            batch.insert(event.path.clone(), classify(&event.path, event.kind));
                        }
                        Some(WatchMessage::Rescan) => {}
                        Some(WatchMessage::Degraded(reason)) => {
                            eprintln!("[visp-codegraph] watcher degraded: {:?}", reason);
                        }
                        None => break,
                    }
                    sleep.as_mut().reset(tokio::time::Instant::now() + DEBOUNCE);
                }
                _ = &mut sleep => break,
            }
        }

        // 先处理删除（旧路径），再处理创建/修改（新路径）：重命名批次内
        // 跨文件引用在「重插」后被 `resolve_cross_file_edges` 重新解析。
        let mut entries: Vec<(PathBuf, IndexEvent)> = batch.into_iter().collect();
        entries.sort_by_key(|(_, event)| usize::from(*event != IndexEvent::Removed));
        for (path, event) in entries {
            if let Err(error) = indexer.update_file(&project, &path, event) {
                eprintln!("[visp-codegraph] watcher error: {}", error);
            }
        }
    }
}

/// `visp-fs` 事件类型 → 索引器事件类型。
///
/// 额外容错：**递归目标**下覆盖同名路径的原子替换（tmp+rename）时，`visp-fs`
/// 只投递 `Removed`（设计 §4.3 细则 2a 的存在性探测仅覆盖文件级监听），而路径
/// 实际仍存在。消费者按 §4.6「删除(同名) + 创建(同名)」语义，将其还原为
/// 「创建」重插，避免索引因原子替换而丢失该文件。
fn classify(path: &Path, kind: EventType) -> IndexEvent {
    match kind {
        EventType::Created => IndexEvent::Created,
        EventType::Modified => IndexEvent::Modified,
        EventType::Removed if path.exists() => IndexEvent::Created,
        EventType::Removed => IndexEvent::Removed,
    }
}

#[cfg(test)]
#[path = "watcher_tests.rs"]
mod watcher_tests;
