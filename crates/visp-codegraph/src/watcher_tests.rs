#![cfg(test)]
use super::*;
use crate::store::Store;

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
    std::fs::write(&file_path, valid_ts("created_fn")).unwrap();

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

    let _watcher = Watcher::start(&project, indexer.clone(), config).await.unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;

    let file_path = project.join("a.ts");
    std::fs::write(&file_path, valid_ts("original_fn")).unwrap();
    tokio::time::sleep(Duration::from_millis(1500)).await;

    // Modify the file
    std::fs::write(&file_path, valid_ts("modified_fn")).unwrap();
    tokio::time::sleep(Duration::from_millis(1500)).await;

    let symbols = store.search_symbols("", 100).unwrap();
    let names: Vec<&str> = symbols.iter().map(|s| s.name.as_str()).collect();
    assert!(
        names.contains(&"modified_fn"),
        "expected modified_fn to be indexed, got {:?}",
        names
    );
}

// ------------------------------------------------------------------
//  3. Debounce: rapid changes merge into one event
// ------------------------------------------------------------------

#[tokio::test]
async fn test_watcher_debounce_rapid_changes() {
    let (_tmp, project, store, indexer, config) = setup();

    let _watcher = Watcher::start(&project, indexer, config).await.unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;

    let file_path = project.join("a.ts");
    // Rapid writes within debounce window
    std::fs::write(&file_path, valid_ts("first")).unwrap();
    std::fs::write(&file_path, valid_ts("second")).unwrap();
    std::fs::write(&file_path, valid_ts("third")).unwrap();
    tokio::time::sleep(Duration::from_millis(1500)).await;

    let symbols = store.search_symbols("", 100).unwrap();
    let names: Vec<&str> = symbols.iter().map(|s| s.name.as_str()).collect();
    assert!(
        names.contains(&"third"),
        "expected third (last write) to be indexed, got {:?}",
        names
    );
}

// ------------------------------------------------------------------
//  4. Excluded directories are skipped
// ------------------------------------------------------------------

#[tokio::test]
async fn test_watcher_excluded_dir() {
    let (_tmp, project, store, indexer, config) = setup();

    let _watcher = Watcher::start(&project, indexer, config).await.unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;

    // Write inside node_modules (should be excluded)
    let excluded_path = project.join("node_modules").join("ignored.ts");
    std::fs::create_dir_all(excluded_path.parent().unwrap()).unwrap();
    std::fs::write(&excluded_path, valid_ts("ignored_fn")).unwrap();

    tokio::time::sleep(Duration::from_millis(1500)).await;

    let symbols = store.search_symbols("", 100).unwrap();
    let names: Vec<&str> = symbols.iter().map(|s| s.name.as_str()).collect();
    assert!(
        !names.contains(&"ignored_fn"),
        "ignored_fn should not appear (excluded dir), got {:?}",
        names
    );
}

// ------------------------------------------------------------------
//  5. Unsupported file extensions are skipped
// ------------------------------------------------------------------

#[tokio::test]
async fn test_watcher_unsupported_extension() {
    let (_tmp, project, store, indexer, config) = setup();

    let _watcher = Watcher::start(&project, indexer, config).await.unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;

    let file_path = project.join("a.json");
    std::fs::write(&file_path, "{}").unwrap();
    tokio::time::sleep(Duration::from_millis(1500)).await;

    let symbols = store.search_symbols("", 100).unwrap();
    assert!(
        symbols.is_empty(),
        "no symbols should be indexed for .json files"
    );
}

// ------------------------------------------------------------------
//  6. Watcher.stop() stops processing
// ------------------------------------------------------------------

#[tokio::test]
async fn test_watcher_stop() {
    let (_tmp, project, store, indexer, config) = setup();

    let watcher = Watcher::start(&project, indexer, config).await.unwrap();
    watcher.stop(); // Should not panic

    // Write after stop should not be indexed
    let file_path = project.join("a.ts");
    std::fs::write(&file_path, valid_ts("after_stop")).unwrap();
    tokio::time::sleep(Duration::from_millis(1500)).await;

    let symbols = store.search_symbols("", 100).unwrap();
    assert!(
        symbols.is_empty(),
        "no symbols should appear after watcher stopped"
    );
}

// ------------------------------------------------------------------
//  7. File delete triggers event processing (tombstone)
// ------------------------------------------------------------------

#[tokio::test]
async fn test_watcher_file_delete() {
    let (_tmp, project, store, indexer, config) = setup();

    let _watcher = Watcher::start(&project, indexer, config).await.unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;

    let file_path = project.join("to_delete.ts");
    std::fs::write(&file_path, valid_ts("will_be_deleted")).unwrap();
    tokio::time::sleep(Duration::from_millis(1500)).await;

    // Confirm it was indexed
    let symbols_before = store.search_symbols("", 100).unwrap();
    assert_eq!(symbols_before.len(), 1);

    // Delete the file
    std::fs::remove_file(&file_path).unwrap();
    tokio::time::sleep(Duration::from_millis(1500)).await;

    // Deletion via watcher just triggers re-index; in this test the file
    // is gone, so search returns empty (re-indexing an absent file produces
    // no symbols).
    let symbols_after = store.search_symbols("", 100).unwrap();
    assert!(
        symbols_after.is_empty(),
        "symbols should be removed after file delete"
    );
}

// ------------------------------------------------------------------
//  8. is_excluded helper tests
// ------------------------------------------------------------------

#[test]
fn test_is_excluded_matches_directory() {
    let exclude = vec!["node_modules".to_string(), ".git".into()];
    assert!(is_excluded(
        Path::new("/project/node_modules/pkg/a.ts"),
        &exclude
    ));
    assert!(!is_excluded(Path::new("/project/src/a.ts"), &exclude));
    assert!(is_excluded(Path::new("/project/.git/config"), &exclude));
}

#[test]
fn test_has_supported_ext_matches() {
    let exts = vec![".ts".into(), ".tsx".into()];
    assert!(has_supported_ext(Path::new("a.ts"), &exts));
    assert!(has_supported_ext(Path::new("a.tsx"), &exts));
    assert!(!has_supported_ext(Path::new("a.json"), &exts));
    assert!(!has_supported_ext(Path::new("a"), &exts));
}
