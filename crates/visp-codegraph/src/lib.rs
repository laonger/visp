pub mod graph;
pub mod index;
pub mod parser;
pub mod query;
pub mod store;
pub mod watcher;

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use crate::index::{CodeGraphConfig, Indexer};
use crate::query::{QueryEngine, SymbolDetails, SymbolInfo};
use crate::store::Store;
use crate::watcher::Watcher;

pub struct CodeGraph {
    #[allow(dead_code)]
    store: Arc<Store>,
    query_engine: QueryEngine,
    indexer: Arc<Indexer>,
    watcher: Option<Watcher>,
    is_building: Arc<AtomicBool>,
}

impl CodeGraph {
    /// 打开/创建数据库，返回 CodeGraph 实例
    pub fn open(project_path: &Path) -> Result<Self, String> {
        let db_path = project_path.join(".visp").join("codegraph.db");
        let store = Arc::new(Store::open(&db_path).map_err(|e| e.to_string())?);
        let is_building = Arc::new(AtomicBool::new(false));
        let query_engine = QueryEngine::new(store.clone(), is_building.clone());
        let indexer = Arc::new(Indexer::new(store.clone()));
        Ok(Self {
            store,
            query_engine,
            indexer,
            watcher: None,
            is_building,
        })
    }

    /// 全量索引构建
    pub async fn build_full(
        &self,
        project_path: &Path,
        config: &CodeGraphConfig,
    ) -> Result<(), String> {
        self.is_building
            .store(true, std::sync::atomic::Ordering::SeqCst);
        let result = self
            .indexer
            .build_full(project_path, config)
            .map_err(|e| e.to_string());
        self.is_building
            .store(false, std::sync::atomic::Ordering::SeqCst);
        result
    }

    /// 启动文件监听
    pub async fn start_watching(
        &mut self,
        project_path: &Path,
        config: CodeGraphConfig,
    ) -> Result<(), String> {
        let watcher = Watcher::start(project_path, self.indexer.clone(), config)
            .await
            .map_err(|e| e.to_string())?;
        self.watcher = Some(watcher);
        Ok(())
    }

    /// 符号搜索
    pub fn search(&self, query: &str, limit: usize) -> Result<Vec<SymbolInfo>, String> {
        self.query_engine.search(query, limit)
    }

    /// 符号详情
    pub fn get_details(&self, name: &str) -> Result<Vec<SymbolDetails>, String> {
        self.query_engine.get_details(name)
    }

    /// 关闭
    pub fn shutdown(mut self) {
        if let Some(w) = self.watcher.take() {
            w.stop();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
}
