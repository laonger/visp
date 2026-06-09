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
mod tests {
    use super::*;
    use crate::store::Store;
    use std::sync::Arc;

    // ------------------------------------------------------------------
    //  Helpers
    // ------------------------------------------------------------------

    fn setup() -> (
        tempfile::TempDir,
        PathBuf,
        Arc<Store>,
        Arc<Indexer>,
        CodeGraphConfig,
    ) {
        let tmp = tempfile::tempdir().unwrap();
        let project = tmp.path().join("project");
        std::fs::create_dir_all(&project).unwrap();

        let db_path = tmp.path().join("test.db");
        let store = Arc::new(Store::open(&db_path).unwrap());
        let indexer = Arc::new(Indexer::new(store.clone()));
        let config = CodeGraphConfig::default();

        (tmp, project, store, indexer, config)
    }

    fn valid_ts(name: &str) -> String {
        format!("export function {}() {{}}\n", name)
    }

    // ------------------------------------------------------------------
    //  1. File create triggers event processing
    // ------------------------------------------------------------------

    #[tokio::test]
    async fn test_watcher_file_create() {
        let (_tmp, project, store, indexer, config) = setup();

        let _watcher = Watcher::start(&project, indexer, config).await.unwrap();
        tokio::time::sleep(Duration::from_millis(300)).await; // let watcher settle

        let file_path = project.join("a.ts");
        std::fs::write(&file_path, &valid_ts("created_fn")).unwrap();

        // Wait for debounce (500 ms) + processing time
        tokio::time::sleep(Duration::from_millis(1500)).await;

        let symbols = store.search_symbols("", 100).unwrap();
        let names: Vec<&str> = symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(
            names.contains(&"created_fn"),
            "expected created_fn to be indexed, got {:?}",
            names
        );
    }

    // ------------------------------------------------------------------
    //  2. File modify triggers event processing
    // ------------------------------------------------------------------

    #[tokio::test]
    async fn test_watcher_file_modify() {
        let (_tmp, project, store, indexer, config) = setup();

        let _watcher = Watcher::start(&project, indexer, config).await.unwrap();
        tokio::time::sleep(Duration::from_millis(300)).await;

        let file_path = project.join("a.ts");
        std::fs::write(&file_path, &valid_ts("original")).unwrap();
        tokio::time::sleep(Duration::from_millis(1500)).await;

        // Modify the file with a new symbol
        std::fs::write(&file_path, &valid_ts("modified_fn")).unwrap();
        tokio::time::sleep(Duration::from_millis(1500)).await;

        let symbols = store.search_symbols("", 100).unwrap();
        let names: Vec<&str> = symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(
            names.contains(&"modified_fn"),
            "expected modified_fn to be indexed, got {:?}",
            names
        );
        assert!(
            !names.contains(&"original"),
            "original should have been replaced"
        );
    }

    // ------------------------------------------------------------------
    //  3. Debounce: rapid modifications result in a single update
    // ------------------------------------------------------------------

    #[tokio::test]
    async fn test_watcher_debounce() {
        let (_tmp, project, store, indexer, config) = setup();

        let _watcher = Watcher::start(&project, indexer, config).await.unwrap();
        tokio::time::sleep(Duration::from_millis(300)).await;

        let file_path = project.join("a.ts");

        // Three rapid writes within the 500 ms debounce window
        std::fs::write(&file_path, &valid_ts("v1")).unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;
        std::fs::write(&file_path, &valid_ts("v2")).unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;
        std::fs::write(&file_path, &valid_ts("v3")).unwrap();

        // Without debounce each event would be processed after its own
        // 500 ms sleep → at least 3×500=1500 ms to finish all three.
        // With debounce the batch fires once after 500 ms of silence.
        // Wait 1200 ms: enough for the debounced batch, not enough for
        // three sequential non-debounced events.
        tokio::time::sleep(Duration::from_millis(1200)).await;

        let symbols = store.search_symbols("", 100).unwrap();
        let names: Vec<&str> = symbols.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(
            symbols.len(),
            1,
            "expected exactly 1 symbol (debounced), got {:?}",
            names
        );
        assert_eq!(
            names[0], "v3",
            "expected only the final symbol v3, got {:?}",
            names
        );
    }

    // ------------------------------------------------------------------
    //  4. Excluded directories are filtered out
    // ------------------------------------------------------------------

    #[tokio::test]
    async fn test_watcher_excludes_dirs() {
        let (_tmp, project, store, indexer, config) = setup();

        // Create directories before watcher start so kqueue watches them.
        std::fs::create_dir_all(project.join("node_modules")).unwrap();
        std::fs::create_dir_all(project.join("src")).unwrap();

        let _watcher = Watcher::start(&project, indexer, config).await.unwrap();
        tokio::time::sleep(Duration::from_millis(300)).await;

        // File inside node_modules should be ignored
        let ignored_path = project.join("node_modules").join("ignored.ts");
        std::fs::write(&ignored_path, &valid_ts("ignored_fn")).unwrap();

        // Valid file outside excluded dirs should be picked up
        let valid_path = project.join("src").join("valid.ts");
        std::fs::write(&valid_path, &valid_ts("valid_fn")).unwrap();

        tokio::time::sleep(Duration::from_millis(1500)).await;

        let symbols = store.search_symbols("", 100).unwrap();
        let names: Vec<&str> = symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(
            names.contains(&"valid_fn"),
            "expected valid_fn, got {:?}",
            names
        );
        assert!(
            !names.contains(&"ignored_fn"),
            "ignored_fn should not be indexed (in node_modules), got {:?}",
            names
        );
    }

    // ------------------------------------------------------------------
    //  5. Unsupported extensions are filtered out
    // ------------------------------------------------------------------

    #[tokio::test]
    async fn test_watcher_unsupported_extension() {
        let (_tmp, project, store, indexer, config) = setup();

        let _watcher = Watcher::start(&project, indexer, config).await.unwrap();
        tokio::time::sleep(Duration::from_millis(300)).await;

        // .json file should be ignored
        let json_path = project.join("data.json");
        std::fs::write(&json_path, "{}\n").unwrap();

        // .ts file should be picked up
        let ts_path = project.join("main.ts");
        std::fs::write(&ts_path, &valid_ts("main_fn")).unwrap();

        tokio::time::sleep(Duration::from_millis(1500)).await;

        let symbols = store.search_symbols("", 100).unwrap();
        let names: Vec<&str> = symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(
            names.contains(&"main_fn"),
            "expected main_fn, got {:?}",
            names
        );
        assert_eq!(
            symbols.len(),
            1,
            "expected only the .ts file's symbol, got {:?}",
            names
        );
    }

    // ------------------------------------------------------------------
    //  Unit tests for filtering helpers
    // ------------------------------------------------------------------

    #[test]
    fn test_is_excluded_matches_directory() {
        let exclude = vec!["node_modules".to_string(), ".git".to_string()];
        assert!(is_excluded(
            Path::new("/project/node_modules/pkg/a.ts"),
            &exclude
        ));
        assert!(!is_excluded(Path::new("/project/src/a.ts"), &exclude));
        assert!(is_excluded(Path::new("/project/.git/config"), &exclude));
    }

    #[test]
    fn test_has_supported_ext_matches() {
        let exts = vec![".ts".to_string(), ".tsx".to_string()];
        assert!(has_supported_ext(Path::new("a.ts"), &exts));
        assert!(has_supported_ext(Path::new("a.tsx"), &exts));
        assert!(!has_supported_ext(Path::new("a.json"), &exts));
        assert!(!has_supported_ext(Path::new("a"), &exts));
    }
}
