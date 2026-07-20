#![cfg(test)]
use crate::*;
use std::path::Path;
use std::sync::atomic::Ordering;

use tempfile::TempDir;

fn setup_project() -> (TempDir, std::path::PathBuf) {
    let tmp = TempDir::new().unwrap();
    let project = tmp.path().join("test_project");
    std::fs::create_dir_all(&project).unwrap();
    (tmp, project)
}

fn write_ts_file(root: &std::path::Path, rel: &str, content: &str) {
    let full = root.join(rel);
    if let Some(parent) = full.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(&full, content).unwrap();
}

#[test]
fn test_codegraph_open() {
    let (_tmp, project) = setup_project();
    let cg = CodeGraph::open(&project).unwrap();

    let db_path = project.join(".visp").join("codegraph.db");
    assert!(db_path.exists(), "Database file should exist after open");
    assert!(!cg.is_building.load(Ordering::SeqCst));
}

#[tokio::test]
async fn test_codegraph_build_and_search() {
    let (_tmp, project) = setup_project();
    let cg = CodeGraph::open(&project).unwrap();

    write_ts_file(
        &project,
        "src/main.ts",
        "export function hello() { return 1; }\n",
    );

    let config = CodeGraphConfig::default();
    cg.build_full(&project, &config).await.unwrap();

    let results = cg.search("hello", 10).unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].name, "hello");
}

#[tokio::test]
async fn test_codegraph_start_watching() {
    let (_tmp, project) = setup_project();
    let mut cg = CodeGraph::open(&project).unwrap();

    let config = CodeGraphConfig::default();
    let result = cg.start_watching(&project, config).await;
    assert!(result.is_ok(), "start_watching should succeed");
}

#[tokio::test]
async fn test_codegraph_shutdown() {
    let (_tmp, project) = setup_project();
    let mut cg = CodeGraph::open(&project).unwrap();

    let config = CodeGraphConfig::default();
    cg.start_watching(&project, config).await.unwrap();

    // Shutdown should not panic
    cg.shutdown();

    // Database file should still exist after shutdown
    let db_path = project.join(".visp").join("codegraph.db");
    assert!(
        db_path.exists(),
        "Database file should persist after shutdown"
    );
}

#[tokio::test]
async fn test_multi_language_indexing() {
    let (_tmp, project) = setup_project();
    let cg = CodeGraph::open(&project).unwrap();

    for (path, content) in [
        ("src/lib.rs", "pub fn add(a: i32) -> i32 { a }\n"),
        ("script.py", "def hello(): pass\n"),
        ("main.ts", "export function greet(): void {}\n"),
    ] {
        let full = project.join(path);
        std::fs::create_dir_all(full.parent().unwrap()).unwrap();
        std::fs::write(full, content).unwrap();
    }

    cg.build_full(&project, &CodeGraphConfig::default())
        .await
        .unwrap();
    let results = cg.search("", 100).unwrap();
    assert_eq!(
        results.len(),
        3,
        "expected 3 symbols, got {}",
        results.len()
    );
}

/// Index the visp project itself so you can inspect .visp/codegraph.db directly.
#[tokio::test]
async fn test_index_visp() {
    let cg = CodeGraph::open(Path::new(".")).unwrap();
    let config = CodeGraphConfig::default();
    cg.build_full(Path::new("."), &config).await.unwrap();
    let results = cg.search("", 200).unwrap();
    eprintln!("[INDEX] indexed {} symbols:", results.len());
    for s in &results {
        eprintln!("  {} ({})  {}", s.name, s.kind, s.file_path);
    }
}
