use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use notify::{Event, EventKind, RecursiveMode, Watcher as NotifyWatcher};
use tokio::sync::mpsc;

use crate::index::{CodeGraphConfig, FileEvent, Indexer};

/// Watches a project directory recursively and triggers incremental
/// re-indexing through the provided `Indexer`.
///
/// Events are debounced with a 500 ms window: rapid changes to the same
/// file are merged so that only the final state is forwarded.
pub struct Watcher {
    watcher: notify::RecommendedWatcher,
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
        let (tx, mut rx) = mpsc::unbounded_channel::<(PathBuf, FileEvent)>();

        let exclude_dirs = config.exclude_dirs.clone();
        let supported_extensions = config.supported_extensions.clone();

        let mut watcher =
            notify::recommended_watcher(move |event: Result<Event, notify::Error>| {
                let event = match event {
                    Ok(e) => e,
                    Err(_) => return,
                };
                let kind = match event.kind {
                    EventKind::Create(_) => FileEvent::Created,
                    EventKind::Modify(_) => FileEvent::Modified,
                    EventKind::Remove(_) => FileEvent::Removed,
                    _ => return,
                };
                for path in event.paths {
                    if !is_excluded(&path, &exclude_dirs)
                        && has_supported_ext(&path, &supported_extensions)
                    {
                        let _ = tx.send((path, kind));
                    }
                }
            })?;

        watcher.watch(project_path, RecursiveMode::Recursive)?;

        let project = project_path.to_path_buf();
        tokio::spawn(async move {
            loop {
                // Wait for the first event in a batch
                let first = match rx.recv().await {
                    Some(e) => e,
                    None => return,
                };

                let mut batch: HashMap<PathBuf, FileEvent> = HashMap::new();
                batch.insert(first.0, first.1);

                // Collect subsequent events within a 500 ms debounce window.
                let sleep = tokio::time::sleep(Duration::from_millis(500));
                tokio::pin!(sleep);

                loop {
                    tokio::select! {
                        biased;
                        Some((path, kind)) = rx.recv() => {
                            batch.insert(path, kind);
                            sleep.as_mut().reset(tokio::time::Instant::now() + Duration::from_millis(500));
                        }
                        _ = &mut sleep => break,
                    }
                }

                // Process the batch – one `update_file` per changed path
                // with the latest event kind.
                for (path, kind) in batch {
                    if let Err(e) = indexer.update_file(&project, &path, kind) {
                        eprintln!("[visp-codegraph] watcher error: {}", e);
                    }
                }
            }
        });

        Ok(Self { watcher })
    }

    /// Stop watching by dropping the underlying notify watcher.
    /// The background task will exit once the event channel is closed.
    pub fn stop(self) {
        drop(self.watcher);
    }
}

/// Returns `true` if `path` contains an excluded directory component
/// (e.g. `node_modules`, `.git`).
pub(crate) fn is_excluded(path: &Path, exclude_dirs: &[String]) -> bool {
    path.components().any(|comp| {
        if let std::path::Component::Normal(name) = comp {
            name.to_str()
                .is_some_and(|s| exclude_dirs.iter().any(|d| d == s))
        } else {
            false
        }
    })
}

/// Returns `true` if `path` has a supported file extension (e.g. `.ts`, `.tsx`).
pub(crate) fn has_supported_ext(path: &Path, supported_extensions: &[String]) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|ext| {
            let dot_ext = format!(".{}", ext);
            supported_extensions.contains(&dot_ext)
                || supported_extensions.contains(&ext.to_string())
        })
}

#[cfg(test)]
#[path = "watcher_tests.rs"]
mod watcher_tests;
