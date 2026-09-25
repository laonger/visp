//! watcher 运行时：notify 接线、目录/文件双监听、动态补挂与适配层
//! （设计 §4.3 实现策略、§4.5 接口职责、§4.6 归一化）。
//!
//! 分层：本模块封装对 `notify` 的使用，对消费者只暴露「路径 + 事件类型」的
//! [`WatchMessage`]。纯逻辑（划定范围、归一化、降级链）仍由 sibling 模块提供，
//! 本模块只做「运行时编排 + 后端适配」。
//!
//! 事件在 notify 回调线程上仅做非阻塞投递，真正的挂载/卸载/补挂发生在后台
//! 任务中（从 notify 事件循环之外调用 `watch`/`unwatch`，避免自锁）。

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex as StdMutex};

use notify::event::{ModifyKind, RenameMode};
use notify::{Event as NotifyEvent, EventKind, RecommendedWatcher, RecursiveMode, Watcher as _};
use tokio::sync::mpsc::UnboundedReceiver;

use crate::degrade::{MountTarget, resolve_degradation};
use crate::normalize::{EventType, RawEvent, normalize};
use crate::target::{WatchMode, WatchTarget};

/// 投递给消费者的文件事件（路径 + 类型）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileEvent {
    /// 事件路径。
    pub path: PathBuf,
    /// 事件类型。
    pub kind: EventType,
}

/// 运行时投递给消费者的消息。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WatchMessage {
    /// 文件事件。
    Event(FileEvent),
    /// 缺目录补挂后的重扫信号（G5）；消费者幂等处理。
    Rescan,
}

/// 挂载类型：目录递归 / 目录非递归 / 文件级。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MountKind {
    /// 递归目录。
    DirRecursive,
    /// 非递归目录（直接子级目标的根、缺目录降级挂载点）。
    DirShallow,
    /// 文件级监听（直接子级目标下的匹配文件）。
    File,
}

impl MountKind {
    /// 映射为 notify 的递归模式。
    fn recursive_mode(self) -> RecursiveMode {
        match self {
            MountKind::DirRecursive => RecursiveMode::Recursive,
            _ => RecursiveMode::NonRecursive,
        }
    }
}

/// 挂载后端抽象：生产为 notify，测试可注入假体。
pub(crate) trait MountBackend {
    /// 挂载（目录按 [`MountKind`] 决定递归与否；文件一律非递归）。
    fn watch(&mut self, path: &Path, kind: MountKind) -> Result<(), notify::Error>;
    /// 卸载；`watch_not_found` 由调用方按细则 1 忽略。
    fn unwatch(&mut self, path: &Path, kind: MountKind) -> Result<(), notify::Error>;
}

/// `notify::Event` → 自定义 [`RawEvent`] 的适配（备注 13）。
///
/// 后端差异集中于此：
/// - kqueue `RenameMode::Any`（单路径旧）→ [`RawEvent::RenameFrom`]；
/// - inotify `From` / `To` / **`Both`**（单事件旧 + 新）→ 对应三种形态；
/// - Windows 仅 `From` / `To`（**无 `Both`**）。
pub fn adapt(event: &NotifyEvent) -> Vec<RawEvent> {
    let mut out = Vec::new();
    match event.kind {
        EventKind::Create(_) => out.extend(event.paths.iter().cloned().map(RawEvent::Create)),
        EventKind::Remove(_) => out.extend(event.paths.iter().cloned().map(RawEvent::Remove)),
        EventKind::Modify(ModifyKind::Name(mode)) => match mode {
            // kqueue `Any`（单路径旧）/ inotify·Windows `From` / 未知 `Other`：旧路径。
            RenameMode::From | RenameMode::Any | RenameMode::Other => {
                if let Some(path) = event.paths.first() {
                    out.push(RawEvent::RenameFrom(path.clone()));
                }
            }
            RenameMode::To => {
                if let Some(path) = event.paths.first() {
                    out.push(RawEvent::RenameTo(path.clone()));
                }
            }
            RenameMode::Both => {
                if let (Some(from), Some(to)) = (event.paths.first(), event.paths.get(1)) {
                    out.push(RawEvent::RenameBoth {
                        from: from.clone(),
                        to: to.clone(),
                    });
                }
            }
        },
        // 内容写入 / 元数据变更 / `ModifyKind::Any`（kqueue `Vnode::Link`）：保守「修改」。
        EventKind::Modify(_) => out.extend(event.paths.iter().cloned().map(RawEvent::Modify)),
        // `EventKind::Any` 为不精确后端的 catch-all，保守按「修改」处理。
        EventKind::Any => out.extend(event.paths.iter().cloned().map(RawEvent::Modify)),
        // Access（开闭/读取）不在「创建/修改/删除」契约内；Other 由 `need_rescan` 另行处理。
        EventKind::Access(_) | EventKind::Other => {}
    }
    out
}

/// 生产挂载后端：包装共享的 notify watcher。
struct NotifyBackend {
    watcher: Arc<StdMutex<RecommendedWatcher>>,
}

impl MountBackend for NotifyBackend {
    fn watch(&mut self, path: &Path, kind: MountKind) -> Result<(), notify::Error> {
        self.watcher
            .lock()
            .unwrap()
            .watch(path, kind.recursive_mode())
    }

    fn unwatch(&mut self, path: &Path, _kind: MountKind) -> Result<(), notify::Error> {
        self.watcher.lock().unwrap().unwatch(path)
    }
}

/// 运行时核心：维护监听集合并把后端事件归一化投递。
///
/// 泛型后端与存在性谓词便于以假体注入做确定性的细则单测（计划 2b）。
struct Core<B, E> {
    targets: Vec<WatchTarget>,
    backend: B,
    exists: E,
    /// 已直接挂载的目录 → 挂载类型（用于 Modify 抑制与卸载）。
    dirs: HashMap<PathBuf, MountKind>,
    /// 已挂载的文件级路径。
    files: HashSet<PathBuf>,
    /// 已知「根已直接挂载」的目标根，用于 G5 重扫信号去重。
    known_direct: HashSet<PathBuf>,
    /// 首次 reconcile（启动挂载）不计重扫信号。
    initialized: bool,
}

impl<B: MountBackend, E: Fn(&Path) -> bool> Core<B, E> {
    fn new(targets: Vec<WatchTarget>, backend: B, exists: E) -> Self {
        Self {
            targets,
            backend,
            exists,
            dirs: HashMap::new(),
            files: HashSet::new(),
            known_direct: HashSet::new(),
            initialized: false,
        }
    }

    /// 已直接挂载目录集合（Modify 抑制的判定依据）。
    fn dir_path_set(&self) -> HashSet<PathBuf> {
        self.dirs.keys().cloned().collect()
    }

    /// 路径是否命中任一目标。
    fn matches_any(&self, path: &Path) -> bool {
        self.targets.iter().any(|target| target.matches(path))
    }

    /// 路径是否属于某直接子级目标的直接子项范围。
    fn is_direct_child_scope(&self, path: &Path) -> bool {
        self.targets
            .iter()
            .any(|target| target.mode == WatchMode::DirectChildren && target.matches(path))
    }

    /// 是否为文件级重挂目标：已监听文件，或直接子级范围内的现存文件。
    fn is_remount_target(&self, path: &Path) -> bool {
        self.files.contains(path) || (self.is_direct_child_scope(path) && path.is_file())
    }

    /// 处理单个后端原始事件：归一化 + 必要重挂 + 过滤投递。
    fn process(&mut self, raw: RawEvent) -> Vec<WatchMessage> {
        let mut messages = Vec::new();
        for (path, kind) in normalize(&raw, &self.dir_path_set()) {
            // Create / Rename（新名）涉及的直接子文件：无条件「先卸后挂」（幂等）。
            if kind == EventType::Created && self.is_remount_target(&path) {
                self.remount_file(&path);
            }
            if self.matches_any(&path) {
                messages.push(WatchMessage::Event(FileEvent { path, kind }));
            }
        }
        messages
    }

    /// 对单个文件无条件「先卸后挂」（幂等；幂等目标是不产生重复状态而非跳过重挂）。
    fn remount_file(&mut self, path: &Path) {
        let _ = self.backend.unwatch(path, MountKind::File);
        match self.backend.watch(path, MountKind::File) {
            Ok(()) => {
                self.files.insert(path.to_path_buf());
            }
            Err(error) => {
                self.files.remove(path);
                tracing::warn!(path = %path.display(), %error, "文件级监听重挂失败，忽略");
            }
        }
    }

    /// 收敛目录/文件监听集合、执行缺目录补挂，并产出重扫信号。
    fn reconcile(&mut self) -> Vec<WatchMessage> {
        let mut messages = Vec::new();

        // 期望的目录挂载：降级解析 + 模式映射。
        let mut desired: HashMap<PathBuf, MountKind> = HashMap::new();
        for target in &self.targets {
            match resolve_degradation(&target.root, &self.exists).mount {
                MountTarget::Direct(root) => {
                    let kind = match target.mode {
                        WatchMode::Recursive => MountKind::DirRecursive,
                        WatchMode::DirectChildren => MountKind::DirShallow,
                    };
                    merge_dir(&mut desired, root, kind);
                }
                MountTarget::Fallback { ancestor, .. } => {
                    merge_dir(&mut desired, ancestor, MountKind::DirShallow);
                }
                // 无任何可挂载祖先：2b 起显式降级上报；2a 阶段跳过。
                MountTarget::Unmountable => {}
            }
        }

        // 卸载不再需要的目录。
        let stale: Vec<PathBuf> = self
            .dirs
            .keys()
            .filter(|path| !desired.contains_key(*path))
            .cloned()
            .collect();
        for path in stale {
            let kind = self.dirs[&path];
            let _ = self.backend.unwatch(&path, kind);
            self.dirs.remove(&path);
        }

        // 挂载 / 重挂期望目录（目录重挂仅在模式变化或新出现时发生）。
        let mut entries: Vec<(PathBuf, MountKind)> = desired.into_iter().collect();
        entries.sort_by(|a, b| a.0.cmp(&b.0));
        for (path, kind) in entries {
            if self.dirs.get(&path) == Some(&kind) {
                continue;
            }
            if self.dirs.contains_key(&path) {
                let old = self.dirs[&path];
                let _ = self.backend.unwatch(&path, old);
                self.dirs.remove(&path);
            }
            match self.backend.watch(&path, kind) {
                Ok(()) => {
                    self.dirs.insert(path, kind);
                }
                Err(error) => {
                    tracing::warn!(path = %path.display(), %error, "目录监听挂载失败");
                }
            }
        }

        // 直接子级目标下的文件级监听收敛。
        let desired_files = self.desired_files();
        let stale_files: Vec<PathBuf> = self
            .files
            .iter()
            .filter(|path| !desired_files.contains(*path))
            .cloned()
            .collect();
        for path in stale_files {
            let _ = self.backend.unwatch(&path, MountKind::File);
            self.files.remove(&path);
        }
        let mut new_files: Vec<PathBuf> = desired_files
            .into_iter()
            .filter(|path| !self.files.contains(path))
            .collect();
        new_files.sort();
        for path in new_files {
            match self.backend.watch(&path, MountKind::File) {
                Ok(()) => {
                    self.files.insert(path);
                }
                Err(error) => {
                    tracing::warn!(path = %path.display(), %error, "文件级监听挂载失败，忽略");
                }
            }
        }

        // G5：目标根从「未挂载」变为「已直接挂载」⇒ 一次重扫信号（启动首挂不计）。
        let now_direct: HashSet<PathBuf> = self
            .targets
            .iter()
            .filter(|target| (self.exists)(&target.root) && self.dirs.contains_key(&target.root))
            .map(|target| target.root.clone())
            .collect();
        if self.initialized {
            let mut appeared: Vec<PathBuf> = now_direct
                .iter()
                .filter(|root| !self.known_direct.contains(*root))
                .cloned()
                .collect();
            appeared.sort();
            messages.extend(appeared.into_iter().map(|_| WatchMessage::Rescan));
        }
        self.known_direct = now_direct;

        messages
    }

    /// 直接子级目标下当前存在的匹配文件集合。
    fn desired_files(&self) -> HashSet<PathBuf> {
        let mut files = HashSet::new();
        for target in &self.targets {
            if target.mode != WatchMode::DirectChildren || !(self.exists)(&target.root) {
                continue;
            }
            let Ok(entries) = std::fs::read_dir(&target.root) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                let is_file = entry
                    .file_type()
                    .map(|kind| kind.is_file())
                    .unwrap_or(false);
                if is_file && target.matches(&path) {
                    files.insert(path);
                }
            }
        }
        files
    }
}

/// 合并目录挂载需求；同路径递归优先。
fn merge_dir(map: &mut HashMap<PathBuf, MountKind>, path: PathBuf, kind: MountKind) {
    map.entry(path)
        .and_modify(|existing| {
            if kind == MountKind::DirRecursive {
                *existing = kind;
            }
        })
        .or_insert(kind);
}

/// 文件监听句柄（设计 §4.5）。
///
/// 句柄不直接持有 notify watcher——watcher 由后台任务持有；`stop` 通知任务退出，
/// 任务析构其 `Core` 时释放 watcher 与全部监听。
pub struct FsWatcher {
    shutdown: Option<tokio::sync::oneshot::Sender<()>>,
    task: Option<tokio::task::JoinHandle<()>>,
}

impl FsWatcher {
    /// 停止监听：通知后台任务退出（消费后释放 notify watcher 与全部监听）。
    pub fn stop(mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

/// 启动监听：返回句柄与消息接收端（设计 §4.5）。
///
/// 目标根不存在时按 G5 自动挂载最近存在祖先；根出现后补挂并发重扫信号。
/// 需在 tokio 运行时内调用（后台任务由 `tokio::spawn` 驱动）。
pub async fn start(
    targets: Vec<WatchTarget>,
) -> Result<(FsWatcher, UnboundedReceiver<WatchMessage>), notify::Error> {
    let (raw_tx, mut raw_rx) = tokio::sync::mpsc::unbounded_channel::<NotifyEvent>();
    let watcher = notify::recommended_watcher(
        move |result: Result<NotifyEvent, notify::Error>| match result {
            Ok(event) => {
                let _ = raw_tx.send(event);
            }
            Err(error) => tracing::warn!(%error, "文件监听回调错误"),
        },
    )?;
    let watcher = Arc::new(StdMutex::new(watcher));

    let backend = NotifyBackend {
        watcher: watcher.clone(),
    };
    let mut core = Core::new(targets, backend, |path: &Path| path.exists());

    let (out_tx, out_rx) = tokio::sync::mpsc::unbounded_channel::<WatchMessage>();
    // 启动时一次性挂载（同步完成，先于任何事件）。
    for message in core.reconcile() {
        let _ = out_tx.send(message);
    }
    core.initialized = true;

    let (shutdown_tx, mut shutdown_rx) = tokio::sync::oneshot::channel::<()>();
    let task = tokio::spawn(async move {
        loop {
            tokio::select! {
                biased;
                _ = &mut shutdown_rx => break,
                event = raw_rx.recv() => {
                    let Some(event) = event else { break };
                    if event.need_rescan() && out_tx.send(WatchMessage::Rescan).is_err() {
                        return;
                    }
                    let mut messages: Vec<WatchMessage> = Vec::new();
                    for raw in adapt(&event) {
                        messages.extend(core.process(raw));
                    }
                    messages.extend(core.reconcile());
                    for message in messages {
                        if out_tx.send(message).is_err() {
                            // 接收端被 drop：静默停止投递（设计 §4.5）。
                            return;
                        }
                    }
                }
            }
        }
    });

    Ok((
        FsWatcher {
            shutdown: Some(shutdown_tx),
            task: Some(task),
        },
        out_rx,
    ))
}

#[cfg(test)]
#[path = "runtime_tests.rs"]
mod tests;
