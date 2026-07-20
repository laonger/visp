pub mod graph;
pub mod index;
pub mod parser;
pub mod query;
pub mod store;
pub mod watcher;

pub use crate::index::CodeGraphConfig;

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use crate::index::Indexer;
use crate::query::{
    ImpactResult, QueryEngine, SymbolDetails, SymbolInfo, TraceHop, get_project_name_tokens,
};
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
        // Back-fill FTS5 for existing databases (safe no-op if already populated)
        store.backfill_fts().map_err(|e| e.to_string())?;
        let is_building = Arc::new(AtomicBool::new(false));
        let project_name_tokens = get_project_name_tokens(project_path);
        let query_engine =
            QueryEngine::new(store.clone(), is_building.clone(), project_name_tokens);
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

    /// 调用路径追踪：查找从 from 到 to 的最短调用路径
    pub fn trace(&self, from: &str, to: &str) -> Result<Vec<TraceHop>, String> {
        self.query_engine.trace(from, to)
    }

    /// 影响分析：获取符号的调用者和被调用者（支持指定深度）
    pub fn impact(&self, symbol: &str, depth: usize) -> Result<ImpactResult, String> {
        self.query_engine.impact(symbol, depth)
    }

    /// 关闭
    pub fn shutdown(mut self) {
        if let Some(w) = self.watcher.take() {
            w.stop();
        }
    }
}

#[cfg(test)]
#[path = "lib_tests.rs"]
mod lib_tests;
