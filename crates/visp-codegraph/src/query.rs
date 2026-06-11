use std::collections::{HashSet, VecDeque};
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

#[derive(Debug, Clone)]
pub struct TraceHop {
    pub name: String,
    pub file_path: String,
    pub line: u32,
    pub signature: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ImpactResult {
    pub symbol_name: String,
    pub callers: Vec<ImpactSymbol>,
    pub callees: Vec<ImpactSymbol>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ImpactSymbol {
    pub name: String,
    pub file_path: String,
    pub depth: usize,
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

    pub fn trace(&self, from: &str, to: &str) -> Result<Vec<TraceHop>, String> {
        if self.is_building.load(Ordering::Relaxed) {
            return Err("codegraph: index is building, please retry later".into());
        }

        let from_symbols = self
            .store
            .get_symbols_by_name(from)
            .map_err(|e| e.to_string())?;
        if from_symbols.is_empty() {
            return Ok(vec![]);
        }
        if from_symbols.len() > 1 {
            let matches: Vec<String> = from_symbols
                .iter()
                .map(|s| format!("{} ({}:{})", s.name, s.file_path, s.line))
                .collect();
            return Err(format!(
                "ambiguous symbol '{}': found {} matches: {}",
                from,
                from_symbols.len(),
                matches.join(", ")
            ));
        }

        let from_sym = &from_symbols[0];
        let to_symbols = self
            .store
            .get_symbols_by_name(to)
            .map_err(|e| e.to_string())?;
        if to_symbols.is_empty() {
            return Ok(vec![]);
        }
        if to_symbols.len() > 1 {
            let matches: Vec<String> = to_symbols
                .iter()
                .map(|s| format!("{} ({}:{})", s.name, s.file_path, s.line))
                .collect();
            return Err(format!(
                "ambiguous symbol '{}': found {} matches: {}",
                to,
                to_symbols.len(),
                matches.join(", ")
            ));
        }

        let to_name = &to_symbols[0].name;
        let start_hop = TraceHop {
            name: from_sym.name.clone(),
            file_path: from_sym.file_path.clone(),
            line: from_sym.line,
            signature: from_sym.signature.clone(),
        };

        // Self-reference
        if from_sym.name == *to_name {
            return Ok(vec![start_hop]);
        }

        let mut visited = HashSet::new();
        visited.insert(from_sym.id);

        let mut queue = VecDeque::new();
        queue.push_back((from_sym.id, vec![start_hop]));

        while let Some((current_id, path)) = queue.pop_front() {
            let callees = self
                .store
                .get_callees(current_id)
                .map_err(|e| e.to_string())?;
            for (callee_name, callee_file) in callees {
                let callee_sym =
                    match find_symbol_by_name_and_file(&self.store, &callee_name, &callee_file) {
                        Ok(Some(s)) => s,
                        _ => continue,
                    };

                if !visited.insert(callee_sym.id) {
                    continue;
                }

                let hop = TraceHop {
                    name: callee_sym.name.clone(),
                    file_path: callee_sym.file_path.clone(),
                    line: callee_sym.line,
                    signature: callee_sym.signature.clone(),
                };

                let mut new_path = path.clone();
                new_path.push(hop);

                if callee_name == *to_name {
                    return Ok(new_path);
                }

                queue.push_back((callee_sym.id, new_path));
            }
        }

        Ok(vec![])
    }

    pub fn impact(&self, symbol: &str, depth: usize) -> Result<ImpactResult, String> {
        if self.is_building.load(Ordering::Relaxed) {
            return Err("codegraph: index is building, please retry later".into());
        }

        let symbols = self
            .store
            .get_symbols_by_name(symbol)
            .map_err(|e| e.to_string())?;
        if symbols.is_empty() {
            return Err(format!("symbol '{}' not found", symbol));
        }
        if symbols.len() > 1 {
            let matches: Vec<String> = symbols
                .iter()
                .map(|s| format!("{} ({}:{})", s.name, s.file_path, s.line))
                .collect();
            return Err(format!(
                "ambiguous symbol '{}': found {} matches: {}",
                symbol,
                symbols.len(),
                matches.join(", ")
            ));
        }

        let target = &symbols[0];
        let mut visited = HashSet::new();
        visited.insert(target.id);

        let callers = self.expand_impact_dir(target.id, true, 1, depth, &mut visited)?;
        let callees = self.expand_impact_dir(target.id, false, 1, depth, &mut visited)?;

        Ok(ImpactResult {
            symbol_name: target.name.clone(),
            callers,
            callees,
        })
    }

    fn expand_impact_dir(
        &self,
        sym_id: u64,
        is_caller: bool,
        current_depth: usize,
        max_depth: usize,
        visited: &mut HashSet<u64>,
    ) -> Result<Vec<ImpactSymbol>, String> {
        if current_depth > max_depth {
            return Ok(vec![]);
        }

        let neighbors = if is_caller {
            self.store.get_callers(sym_id).map_err(|e| e.to_string())?
        } else {
            self.store.get_callees(sym_id).map_err(|e| e.to_string())?
        };

        let mut result = Vec::new();
        for (name, file_path) in neighbors {
            let symbols = self
                .store
                .get_symbols_by_name(&name)
                .map_err(|e| e.to_string())?;
            let sym = match symbols.into_iter().find(|s| s.file_path == file_path) {
                Some(s) => s,
                None => continue,
            };

            if !visited.insert(sym.id) {
                continue;
            }

            result.push(ImpactSymbol {
                name,
                file_path: file_path.clone(),
                depth: current_depth,
            });

            let sub =
                self.expand_impact_dir(sym.id, is_caller, current_depth + 1, max_depth, visited)?;
            result.extend(sub);
        }

        Ok(result)
    }
}

fn find_symbol_by_name_and_file(
    store: &Store,
    name: &str,
    file_path: &str,
) -> Result<Option<Symbol>, String> {
    let symbols = store.get_symbols_by_name(name).map_err(|e| e.to_string())?;
    Ok(symbols.into_iter().find(|s| s.file_path == file_path))
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

    // --- trace tests ---

    #[test]
    fn test_trace_direct_call() {
        let (store_raw, _db_dir) = create_store();
        let store = Arc::new(store_raw);

        let a_id = insert_symbol(
            &store,
            "A",
            SymbolKind::Function,
            "src/a.ts",
            1,
            1,
            None,
            None,
        );
        let b_id = insert_symbol(
            &store,
            "B",
            SymbolKind::Function,
            "src/b.ts",
            1,
            1,
            Some("fn B()"),
            None,
        );
        store
            .insert_edges(&[Edge {
                source_id: a_id,
                target_id: Some(b_id),
                target_name: None,
                kind: EdgeKind::Call,
            }])
            .unwrap();

        let engine = QueryEngine::new(store, Arc::new(AtomicBool::new(false)));
        let path = engine.trace("A", "B").unwrap();

        assert_eq!(path.len(), 2);
        assert_eq!(path[0].name, "A");
        assert_eq!(path[1].name, "B");
        assert_eq!(path[1].signature, Some("fn B()".into()));
    }

    #[test]
    fn test_trace_multi_hop() {
        let (store_raw, _db_dir) = create_store();
        let store = Arc::new(store_raw);

        let a_id = insert_symbol(&store, "A", SymbolKind::Function, "a.rs", 1, 1, None, None);
        let b_id = insert_symbol(&store, "B", SymbolKind::Function, "b.rs", 1, 1, None, None);
        let c_id = insert_symbol(&store, "C", SymbolKind::Function, "c.rs", 1, 1, None, None);
        store
            .insert_edges(&[
                Edge {
                    source_id: a_id,
                    target_id: Some(b_id),
                    target_name: None,
                    kind: EdgeKind::Call,
                },
                Edge {
                    source_id: b_id,
                    target_id: Some(c_id),
                    target_name: None,
                    kind: EdgeKind::Call,
                },
            ])
            .unwrap();

        let engine = QueryEngine::new(store, Arc::new(AtomicBool::new(false)));
        let path = engine.trace("A", "C").unwrap();

        assert_eq!(path.len(), 3);
        assert_eq!(path[0].name, "A");
        assert_eq!(path[1].name, "B");
        assert_eq!(path[2].name, "C");
    }

    #[test]
    fn test_trace_no_path() {
        let (store_raw, _db_dir) = create_store();
        let store = Arc::new(store_raw);

        let a_id = insert_symbol(&store, "A", SymbolKind::Function, "a.rs", 1, 1, None, None);
        let b_id = insert_symbol(&store, "B", SymbolKind::Function, "b.rs", 1, 1, None, None);
        let _c_id = insert_symbol(&store, "C", SymbolKind::Function, "c.rs", 1, 1, None, None);
        let _d_id = insert_symbol(&store, "D", SymbolKind::Function, "d.rs", 1, 1, None, None);

        store
            .insert_edges(&[Edge {
                source_id: a_id,
                target_id: Some(b_id),
                target_name: None,
                kind: EdgeKind::Call,
            }])
            .unwrap();
        // C and D exist but are disconnected from A/B

        let engine = QueryEngine::new(store, Arc::new(AtomicBool::new(false)));
        let path = engine.trace("C", "D").unwrap();
        assert!(path.is_empty());
    }

    #[test]
    fn test_trace_with_cycle() {
        let (store_raw, _db_dir) = create_store();
        let store = Arc::new(store_raw);

        let a_id = insert_symbol(&store, "A", SymbolKind::Function, "a.rs", 1, 1, None, None);
        let b_id = insert_symbol(&store, "B", SymbolKind::Function, "b.rs", 1, 1, None, None);
        let c_id = insert_symbol(&store, "C", SymbolKind::Function, "c.rs", 1, 1, None, None);

        // A → B → C → A (cycle)
        store
            .insert_edges(&[
                Edge {
                    source_id: a_id,
                    target_id: Some(b_id),
                    target_name: None,
                    kind: EdgeKind::Call,
                },
                Edge {
                    source_id: b_id,
                    target_id: Some(c_id),
                    target_name: None,
                    kind: EdgeKind::Call,
                },
                Edge {
                    source_id: c_id,
                    target_id: Some(a_id),
                    target_name: None,
                    kind: EdgeKind::Call,
                },
            ])
            .unwrap();

        let engine = QueryEngine::new(store, Arc::new(AtomicBool::new(false)));
        let path = engine.trace("A", "B").unwrap();

        // Should find A→B directly, not go through the cycle
        assert_eq!(path.len(), 2);
        assert_eq!(path[0].name, "A");
        assert_eq!(path[1].name, "B");
    }

    #[test]
    fn test_trace_not_found() {
        let (store_raw, _db_dir) = create_store();
        let store = Arc::new(store_raw);

        insert_symbol(&store, "A", SymbolKind::Function, "a.rs", 1, 1, None, None);

        let engine = QueryEngine::new(store, Arc::new(AtomicBool::new(false)));

        // from not found
        let path = engine.trace("X", "A").unwrap();
        assert!(path.is_empty());

        // to not found
        let path = engine.trace("A", "Y").unwrap();
        assert!(path.is_empty());
    }

    // --- impact tests ---

    #[test]
    fn test_impact_depth_1() {
        let (store_raw, _db_dir) = create_store();
        let store = Arc::new(store_raw);

        let a_id = insert_symbol(&store, "A", SymbolKind::Function, "a.rs", 1, 1, None, None);
        let u_id = insert_symbol(&store, "U", SymbolKind::Function, "u.rs", 1, 1, None, None);
        let v_id = insert_symbol(&store, "V", SymbolKind::Function, "v.rs", 1, 1, None, None);
        let x_id = insert_symbol(&store, "X", SymbolKind::Function, "x.rs", 1, 1, None, None);

        // U → A, V → A, A → X
        store
            .insert_edges(&[
                Edge {
                    source_id: u_id,
                    target_id: Some(a_id),
                    target_name: None,
                    kind: EdgeKind::Call,
                },
                Edge {
                    source_id: v_id,
                    target_id: Some(a_id),
                    target_name: None,
                    kind: EdgeKind::Call,
                },
                Edge {
                    source_id: a_id,
                    target_id: Some(x_id),
                    target_name: None,
                    kind: EdgeKind::Call,
                },
            ])
            .unwrap();

        let engine = QueryEngine::new(store, Arc::new(AtomicBool::new(false)));
        let impact = engine.impact("A", 1).unwrap();

        assert_eq!(impact.symbol_name, "A");

        let caller_names: Vec<&str> = impact.callers.iter().map(|s| s.name.as_str()).collect();
        assert!(caller_names.contains(&"U"));
        assert!(caller_names.contains(&"V"));
        assert_eq!(impact.callers.len(), 2);
        for c in &impact.callers {
            assert_eq!(c.depth, 1);
        }

        assert_eq!(impact.callees.len(), 1);
        assert_eq!(impact.callees[0].name, "X");
        assert_eq!(impact.callees[0].depth, 1);
    }

    #[test]
    fn test_impact_depth_2() {
        let (store_raw, _db_dir) = create_store();
        let store = Arc::new(store_raw);

        let a_id = insert_symbol(&store, "A", SymbolKind::Function, "a.rs", 1, 1, None, None);
        let b_id = insert_symbol(&store, "B", SymbolKind::Function, "b.rs", 1, 1, None, None);
        let c_id = insert_symbol(&store, "C", SymbolKind::Function, "c.rs", 1, 1, None, None);

        // A → B → C
        store
            .insert_edges(&[
                Edge {
                    source_id: a_id,
                    target_id: Some(b_id),
                    target_name: None,
                    kind: EdgeKind::Call,
                },
                Edge {
                    source_id: b_id,
                    target_id: Some(c_id),
                    target_name: None,
                    kind: EdgeKind::Call,
                },
            ])
            .unwrap();

        let engine = QueryEngine::new(store, Arc::new(AtomicBool::new(false)));
        let impact = engine.impact("A", 2).unwrap();

        assert_eq!(impact.symbol_name, "A");
        assert!(impact.callers.is_empty());

        assert_eq!(impact.callees.len(), 2);
        let b = impact.callees.iter().find(|s| s.name == "B").unwrap();
        assert_eq!(b.depth, 1);
        let c = impact.callees.iter().find(|s| s.name == "C").unwrap();
        assert_eq!(c.depth, 2);
    }

    #[test]
    fn test_impact_not_found() {
        let (store_raw, _db_dir) = create_store();
        let engine = QueryEngine::new(Arc::new(store_raw), Arc::new(AtomicBool::new(false)));
        let err = engine.impact("Nonexistent", 1).unwrap_err();
        assert!(err.contains("not found"));
    }
}
