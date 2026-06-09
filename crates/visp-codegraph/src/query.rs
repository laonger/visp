use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::graph::Symbol;
use crate::store::Store;

#[derive(Debug, Clone)]
pub struct SymbolInfo {
    pub name: String,
    pub kind: String,
    pub file_path: String,
    pub line: u32,
    pub column: u32,
    pub signature: Option<String>,
}

#[derive(Debug, Clone)]
pub struct SymbolDetails {
    pub name: String,
    pub kind: String,
    pub file_path: String,
    pub line: u32,
    pub column: u32,
    pub signature: Option<String>,
    pub docstring: Option<String>,
    pub source: String,
    pub callers: Vec<String>,
    pub callees: Vec<String>,
}

pub struct QueryEngine {
    store: Arc<Store>,
    is_building: Arc<AtomicBool>,
}

impl QueryEngine {
    pub fn new(store: Arc<Store>, is_building: Arc<AtomicBool>) -> Self {
        QueryEngine { store, is_building }
    }

    pub fn search(&self, query: &str, limit: usize) -> Result<Vec<SymbolInfo>, String> {
        if self.is_building.load(Ordering::Relaxed) {
            return Err("codegraph: index is building, please retry later".into());
        }
        let symbols = self
            .store
            .search_symbols(query, limit)
            .map_err(|e| e.to_string())?;
        Ok(symbols.into_iter().map(sym_to_info).collect())
    }

    pub fn get_details(&self, name: &str) -> Result<Vec<SymbolDetails>, String> {
        if self.is_building.load(Ordering::Relaxed) {
            return Err("codegraph: index is building, please retry later".into());
        }
        let symbols = self
            .store
            .get_symbols_by_name(name)
            .map_err(|e| e.to_string())?;
        let mut result = Vec::with_capacity(symbols.len());
        for sym in symbols {
            let callers = self
                .store
                .get_callers(sym.id)
                .map_err(|e| e.to_string())?
                .into_iter()
                .map(|(n, p)| format!("{} ({})", n, p))
                .collect();
            let callees = self
                .store
                .get_callees(sym.id)
                .map_err(|e| e.to_string())?
                .into_iter()
                .map(|(n, p)| format!("{} ({})", n, p))
                .collect();
            let source = read_source(&sym.file_path, sym.line);
            result.push(SymbolDetails {
                name: sym.name,
                kind: format!("{:?}", sym.kind),
                file_path: sym.file_path,
                line: sym.line,
                column: sym.column,
                signature: sym.signature,
                docstring: sym.docstring,
                source,
                callers,
                callees,
            });
        }
        Ok(result)
    }
}

fn sym_to_info(sym: Symbol) -> SymbolInfo {
    SymbolInfo {
        name: sym.name,
        kind: format!("{:?}", sym.kind),
        file_path: sym.file_path,
        line: sym.line,
        column: sym.column,
        signature: sym.signature,
    }
}

fn read_source(file_path: &str, line: u32) -> String {
    let content = match std::fs::read_to_string(file_path) {
        Ok(c) => c,
        Err(_) => return String::new(),
    };
    // Find byte offset for the given 1-indexed line
    let mut offset = 0;
    for _ in 1..line {
        match content[offset..].find('\n') {
            Some(pos) => offset += pos + 1,
            None => break,
        }
    }
    content[offset..].chars().take(500).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::{Edge, EdgeKind, SymbolKind};
    use tempfile::TempDir;

    /// Create a Store backed by a temp file. Returns (Store, TempDir) — the
    /// TempDir must be kept alive for the Store to remain valid.
    fn create_store() -> (Store, TempDir) {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("test.db");
        let store = Store::open(&path).unwrap();
        (store, dir)
    }

    fn insert_symbol(
        store: &Store,
        name: &str,
        kind: SymbolKind,
        file_path: &str,
        line: u32,
        col: u32,
        sig: Option<&str>,
        doc: Option<&str>,
    ) -> u64 {
        let mut sym = Symbol {
            id: 0,
            name: name.into(),
            kind,
            file_path: file_path.into(),
            line,
            column: col,
            signature: sig.map(|s| s.into()),
            docstring: doc.map(|s| s.into()),
        };
        store
            .insert_symbols(std::slice::from_mut(&mut sym))
            .unwrap();
        sym.id
    }

    fn make_src_file(dir: &TempDir, rel_path: &str, content: &str) -> String {
        let path = dir.path().join(rel_path);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&path, content).unwrap();
        path.to_string_lossy().to_string()
    }

    // --- test_search_prefix ---
    #[test]
    fn test_search_prefix() {
        let (store_raw, _db_dir) = create_store();
        let store = Arc::new(store_raw);
        insert_symbol(
            &store,
            "getUser",
            SymbolKind::Function,
            "a.rs",
            1,
            1,
            None,
            None,
        );
        insert_symbol(
            &store,
            "getName",
            SymbolKind::Function,
            "a.rs",
            2,
            1,
            None,
            None,
        );
        insert_symbol(
            &store,
            "setName",
            SymbolKind::Function,
            "a.rs",
            3,
            1,
            None,
            None,
        );

        let engine = QueryEngine::new(store, Arc::new(AtomicBool::new(false)));

        let results = engine.search("get", 10).unwrap();
        assert_eq!(results.len(), 2);
        let names: Vec<&str> = results.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"getUser"));
        assert!(names.contains(&"getName"));
    }

    // --- test_search_no_match ---
    #[test]
    fn test_search_no_match() {
        let (store_raw, _db_dir) = create_store();
        let store = Arc::new(store_raw);
        insert_symbol(
            &store,
            "getUser",
            SymbolKind::Function,
            "a.rs",
            1,
            1,
            None,
            None,
        );

        let engine = QueryEngine::new(store, Arc::new(AtomicBool::new(false)));

        let results = engine.search("zzz", 10).unwrap();
        assert!(results.is_empty());
    }

    // --- test_search_case_sensitive ---
    #[test]
    fn test_search_case_sensitive() {
        let (store_raw, _db_dir) = create_store();
        let store = Arc::new(store_raw);
        insert_symbol(
            &store,
            "getUser",
            SymbolKind::Function,
            "a.rs",
            1,
            1,
            None,
            None,
        );

        let engine = QueryEngine::new(store, Arc::new(AtomicBool::new(false)));

        let results = engine.search("Get", 10).unwrap();
        assert!(results.is_empty(), "LIKE should be case-sensitive");
    }

    // --- test_get_details_single ---
    #[test]
    fn test_get_details_single() {
        let (store_raw, _db_dir) = create_store();
        let store = Arc::new(store_raw);
        let src_dir = TempDir::new().unwrap();
        let src_path = make_src_file(
            &src_dir,
            "src/main.ts",
            "export function foo() {\n  return 42;\n}\n",
        );

        let foo_id = insert_symbol(
            &store,
            "foo",
            SymbolKind::Function,
            &src_path,
            1,
            9,
            Some("function foo()"),
            Some("A foo function"),
        );
        let bar_id = insert_symbol(
            &store,
            "bar",
            SymbolKind::Function,
            "src/bar.ts",
            1,
            1,
            None,
            None,
        );
        store
            .insert_edges(&[Edge {
                source_id: bar_id,
                target_id: Some(foo_id),
                target_name: None,
                kind: EdgeKind::Call,
            }])
            .unwrap();

        let engine = QueryEngine::new(store, Arc::new(AtomicBool::new(false)));
        let details = engine.get_details("foo").unwrap();

        assert_eq!(details.len(), 1);
        assert_eq!(details[0].name, "foo");
        assert_eq!(details[0].kind, "Function");
        assert_eq!(details[0].signature, Some("function foo()".into()));
        assert_eq!(details[0].docstring, Some("A foo function".into()));
        assert!(details[0].source.contains("export function foo()"));
        assert_eq!(details[0].callers.len(), 1);
        assert!(details[0].callers[0].starts_with("bar"));
        assert!(details[0].callees.is_empty());
    }

    // --- test_get_details_multiple ---
    #[test]
    fn test_get_details_multiple() {
        let (store_raw, _db_dir) = create_store();
        let store = Arc::new(store_raw);
        insert_symbol(
            &store,
            "foo",
            SymbolKind::Function,
            "src/a.ts",
            1,
            1,
            None,
            None,
        );
        insert_symbol(
            &store,
            "foo",
            SymbolKind::Variable,
            "src/b.ts",
            5,
            1,
            None,
            None,
        );

        let engine = QueryEngine::new(store, Arc::new(AtomicBool::new(false)));
        let details = engine.get_details("foo").unwrap();

        assert_eq!(details.len(), 2);
        let paths: Vec<&str> = details.iter().map(|d| d.file_path.as_str()).collect();
        assert!(paths.contains(&"src/a.ts"));
        assert!(paths.contains(&"src/b.ts"));
    }

    // --- test_get_details_caller_format ---
    #[test]
    fn test_get_details_caller_format() {
        let (store_raw, _db_dir) = create_store();
        let store = Arc::new(store_raw);

        let foo_id = insert_symbol(
            &store,
            "foo",
            SymbolKind::Function,
            "src/main.ts",
            1,
            1,
            None,
            None,
        );
        let bar_id = insert_symbol(
            &store,
            "bar",
            SymbolKind::Function,
            "src/utils.ts",
            1,
            1,
            None,
            None,
        );
        store
            .insert_edges(&[Edge {
                source_id: bar_id,
                target_id: Some(foo_id),
                target_name: None,
                kind: EdgeKind::Call,
            }])
            .unwrap();

        let engine = QueryEngine::new(store, Arc::new(AtomicBool::new(false)));
        let details = engine.get_details("foo").unwrap();

        assert_eq!(details.len(), 1);
        let fmts: Vec<&str> = details[0].callers.iter().map(|s| s.as_str()).collect();
        // Format: "funcName (file/path.ts)"
        assert!(fmts.contains(&"bar (src/utils.ts)"));
    }

    // --- test_search_during_build ---
    #[test]
    fn test_search_during_build() {
        let (store_raw, _db_dir) = create_store();
        let store = Arc::new(store_raw);
        let is_building = Arc::new(AtomicBool::new(true));
        let engine = QueryEngine::new(store, is_building);

        let err = engine.search("foo", 10).unwrap_err();
        assert_eq!(err, "codegraph: index is building, please retry later");
    }

    #[test]
    fn test_get_details_during_build() {
        let (store_raw, _db_dir) = create_store();
        let store = Arc::new(store_raw);
        let is_building = Arc::new(AtomicBool::new(true));
        let engine = QueryEngine::new(store, is_building);

        let err = engine.get_details("foo").unwrap_err();
        assert_eq!(err, "codegraph: index is building, please retry later");
    }
}
