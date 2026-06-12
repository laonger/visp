use std::collections::{HashSet, VecDeque};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::graph::Symbol;
use crate::store::{sanitize_fts_query, ScoredSymbol, Store};

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
    project_name_tokens: HashSet<String>,
}

impl QueryEngine {
    pub fn new(
        store: Arc<Store>,
        is_building: Arc<AtomicBool>,
        project_name_tokens: HashSet<String>,
    ) -> Self {
        QueryEngine {
            store,
            is_building,
            project_name_tokens,
        }
    }

    pub fn search(&self, query: &str, limit: usize) -> Result<Vec<SymbolInfo>, String> {
        if self.is_building.load(Ordering::Relaxed) {
            return Err("codegraph: index is building, please retry later".into());
        }

        let mut results: Vec<ScoredSymbol> = Vec::new();

        if !query.is_empty() {
            let fts_query = sanitize_fts_query(query);
            results = self
                .store
                .search_fts(&fts_query, limit * 5)
                .map_err(|e| e.to_string())?;
        }

        let max_fts = results
            .iter()
            .map(|r| r.score)
            .fold(f64::NEG_INFINITY, f64::max);

        if results.len() < limit {
            let like_results = self
                .store
                .search_like(query, limit)
                .map_err(|e| e.to_string())?;
            merge_dedup(&mut results, like_results, max_fts);
        }

        inject_exact(&mut results, query, &self.store, max_fts);
        score_and_sort(&mut results, query, &self.project_name_tokens);
        results.truncate(limit);

        Ok(results.into_iter().map(|rs| sym_to_info(rs.symbol)).collect())
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

// ------------------------------------------------------------------
//  Search orchestration helpers
// ------------------------------------------------------------------

/// Merge LIKE results into FTS5 results with dedup by (name, file_path).
fn merge_dedup(
    results: &mut Vec<ScoredSymbol>,
    incoming: Vec<Symbol>,
    max_fts_score: f64,
) {
    let existing: HashSet<(String, String)> = results
        .iter()
        .map(|r| (r.symbol.name.clone(), r.symbol.file_path.clone()))
        .collect();
    for sym in incoming {
        let key = (sym.name.clone(), sym.file_path.clone());
        if !existing.contains(&key) {
            results.push(ScoredSymbol {
                symbol: sym,
                score: max_fts_score,
            });
        }
    }
}

/// Ensure exact name match is always included in results.
fn inject_exact(
    results: &mut Vec<ScoredSymbol>,
    query: &str,
    store: &Store,
    max_fts_score: f64,
) {
    if query.is_empty() {
        return;
    }
    if let Ok(exacts) = store.get_symbols_by_name(query) {
        for sym in exacts {
            if !results
                .iter()
                .any(|r| r.symbol.name == sym.name && r.symbol.file_path == sym.file_path)
            {
                results.push(ScoredSymbol {
                    symbol: sym,
                    score: max_fts_score,
                });
            }
        }
    }
}

/// Score and sort results by kind bonus + name_match_bonus + path_score.
fn score_and_sort(
    results: &mut Vec<ScoredSymbol>,
    query: &str,
    project_name_tokens: &HashSet<String>,
) {
    // Compute composite score for each result
    for r in results.iter_mut() {
        let kind = kind_bonus(&r.symbol.kind);
        let name = name_match_bonus(&r.symbol.name, query);
        let path = path_score(&r.symbol.file_path, query, project_name_tokens);
        // Final score: BM25 + kind_bonus + name_bonus + path_bonus
        r.score = r.score + kind + name + path;
    }

    // Sort by score descending, then by name for determinism
    results.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.symbol.name.cmp(&b.symbol.name))
    });
}

/// Kind bonus: higher for semantically useful types.
fn kind_bonus(kind: &crate::graph::SymbolKind) -> f64 {
    match kind {
        crate::graph::SymbolKind::Function | crate::graph::SymbolKind::Method => 10.0,
        crate::graph::SymbolKind::Interface => 9.0,
        crate::graph::SymbolKind::Class => 8.0,
        crate::graph::SymbolKind::TypeAlias => 6.0,
        crate::graph::SymbolKind::Enum => 5.0,
        crate::graph::SymbolKind::Variable => 2.0,
    }
}

/// Name match bonus: exact match > prefix match > substring match.
fn name_match_bonus(name: &str, query: &str) -> f64 {
    if query.is_empty() {
        return 0.0;
    }
    let name_lower = name.to_lowercase();
    let query_lower = query.to_lowercase();

    if query_lower.len() < 2 {
        return 0.0;
    }

    // Exact match
    if name_lower == query_lower {
        return 80.0;
    }

    // Prefix match (by length ratio)
    if name_lower.starts_with(&query_lower) {
        let ratio = query_lower.len() as f64 / name_lower.len() as f64;
        return 10.0 + 30.0 * ratio;
    }

    // Substring match
    if name_lower.contains(&query_lower) {
        return 10.0;
    }

    0.0
}

/// Path score: bonus when query term appears in file path (skip project name tokens).
fn path_score(
    file_path: &str,
    query: &str,
    project_name_tokens: &HashSet<String>,
) -> f64 {
    let path_lower = file_path.to_lowercase();
    let mut score = 0.0;
    for term in query.split_whitespace().filter(|t| t.len() >= 2) {
        let term_lower = term.to_lowercase();
        if project_name_tokens.contains(&term_lower) {
            continue;
        }
        if path_lower.contains(&term_lower) {
            score += 2.0;
        }
    }
    score
}

/// Extract project name tokens from a project root path.
/// Returns a set of lowercase alphanumeric tokens with len >= 5.
pub fn get_project_name_tokens(project_path: &Path) -> HashSet<String> {
    let mut tokens = HashSet::new();
    if let Some(dir) = project_path.file_name().and_then(|n| n.to_str()) {
        let norm: String = dir
            .to_lowercase()
            .chars()
            .filter(|c| c.is_alphanumeric())
            .collect();
        if norm.len() >= 5 {
            tokens.insert(norm);
        }
    }
    tokens
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

        let engine = QueryEngine::new(store, Arc::new(AtomicBool::new(false)), HashSet::new());

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

        let engine = QueryEngine::new(store, Arc::new(AtomicBool::new(false)), HashSet::new());

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

        let engine = QueryEngine::new(store, Arc::new(AtomicBool::new(false)), HashSet::new());

        let results = engine.search("Get", 10).unwrap();
        assert_eq!(results.len(), 1, "search is case-insensitive, should find getUser");
        assert_eq!(results[0].name, "getUser");
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

        let engine = QueryEngine::new(store, Arc::new(AtomicBool::new(false)), HashSet::new());
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

        let engine = QueryEngine::new(store, Arc::new(AtomicBool::new(false)), HashSet::new());
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

        let engine = QueryEngine::new(store, Arc::new(AtomicBool::new(false)), HashSet::new());
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
        let engine = QueryEngine::new(store, is_building, HashSet::new());

        let err = engine.search("foo", 10).unwrap_err();
        assert_eq!(err, "codegraph: index is building, please retry later");
    }

    #[test]
    fn test_get_details_during_build() {
        let (store_raw, _db_dir) = create_store();
        let store = Arc::new(store_raw);
        let is_building = Arc::new(AtomicBool::new(true));
        let engine = QueryEngine::new(store, is_building, HashSet::new());

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

        let engine = QueryEngine::new(store, Arc::new(AtomicBool::new(false)), HashSet::new());
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

        let engine = QueryEngine::new(store, Arc::new(AtomicBool::new(false)), HashSet::new());
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

        let engine = QueryEngine::new(store, Arc::new(AtomicBool::new(false)), HashSet::new());
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

        let engine = QueryEngine::new(store, Arc::new(AtomicBool::new(false)), HashSet::new());
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

        let engine = QueryEngine::new(store, Arc::new(AtomicBool::new(false)), HashSet::new());

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

        let engine = QueryEngine::new(store, Arc::new(AtomicBool::new(false)), HashSet::new());
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

        let engine = QueryEngine::new(store, Arc::new(AtomicBool::new(false)), HashSet::new());
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
        let engine = QueryEngine::new(Arc::new(store_raw), Arc::new(AtomicBool::new(false)), HashSet::new());
        let err = engine.impact("Nonexistent", 1).unwrap_err();
        assert!(err.contains("not found"));
    }

    // --- Step 2a: Search orchestration tests ---

    #[test]
    fn test_new_accepts_3params() {
        let (store_raw, _db_dir) = create_store();
        let engine = QueryEngine::new(
            Arc::new(store_raw),
            Arc::new(AtomicBool::new(false)),
            HashSet::new(),
        );
        // Compile-time check: 3-param constructor works
        let _ = engine.search("x", 10);
    }

    #[test]
    fn test_search_empty_query() {
        let (store_raw, _db_dir) = create_store();
        let store = Arc::new(store_raw);
        insert_symbol(&store, "foo", SymbolKind::Function, "a.rs", 1, 1, None, None);
        let engine = QueryEngine::new(store, Arc::new(AtomicBool::new(false)), HashSet::new());

        let results = engine.search("", 10).unwrap();
        // Empty query returns LIKE % results (matches everything)
        assert!(!results.is_empty(), "empty query should return results via LIKE");
    }

    #[test]
    fn test_search_merge_dedup() {
        let (store_raw, _db_dir) = create_store();
        let store = Arc::new(store_raw);
        insert_symbol(&store, "getUser", SymbolKind::Function, "a.rs", 1, 1, None, None);
        insert_symbol(&store, "getConfig", SymbolKind::Function, "b.rs", 1, 1, None, None);
        let engine = QueryEngine::new(store, Arc::new(AtomicBool::new(false)), HashSet::new());

        let results = engine.search("get", 10).unwrap();
        // Should find both without duplicates
        assert_eq!(results.len(), 2);
        let names: Vec<&str> = results.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"getUser"));
        assert!(names.contains(&"getConfig"));
    }

    #[test]
    fn test_search_truncate() {
        let (store_raw, _db_dir) = create_store();
        let store = Arc::new(store_raw);
        for i in 0..10 {
            insert_symbol(
                &store,
                &format!("foo{i}"),
                SymbolKind::Function,
                "a.rs",
                i + 1,
                1,
                None,
                None,
            );
        }
        let engine = QueryEngine::new(store, Arc::new(AtomicBool::new(false)), HashSet::new());

        let results = engine.search("foo", 3).unwrap();
        assert!(results.len() <= 3, "results should be truncated to limit");
    }

    #[test]
    fn test_inject_exact_works() {
        let (store_raw, _db_dir) = create_store();
        let store = Arc::new(store_raw);
        insert_symbol(&store, "getUser", SymbolKind::Function, "a.rs", 1, 1, None, None);
        insert_symbol(&store, "getter", SymbolKind::Function, "b.rs", 2, 1, None, None);
        let engine = QueryEngine::new(store, Arc::new(AtomicBool::new(false)), HashSet::new());

        // "getUser" as query: exact match should be included
        let results = engine.search("getUser", 10).unwrap();
        assert!(
            results.iter().any(|r| r.name == "getUser"),
            "exact match getUser should be in results"
        );
    }

    #[test]
    fn test_project_name_tokens_empty() {
        let (store_raw, _db_dir) = create_store();
        let engine = QueryEngine::new(
            Arc::new(store_raw),
            Arc::new(AtomicBool::new(false)),
            HashSet::new(),
        );
        // Empty tokens should not affect search
        let _ = engine.search("foo", 10);
    }

    // --- Step 2b: Scoring tests ---

    #[test]
    fn test_kind_fn_method() {
        assert_eq!(kind_bonus(&SymbolKind::Function), 10.0);
        assert_eq!(kind_bonus(&SymbolKind::Method), 10.0);
    }

    #[test]
    fn test_kind_interface() {
        assert_eq!(kind_bonus(&SymbolKind::Interface), 9.0);
    }

    #[test]
    fn test_kind_class() {
        assert_eq!(kind_bonus(&SymbolKind::Class), 8.0);
    }

    #[test]
    fn test_kind_typealias() {
        assert_eq!(kind_bonus(&SymbolKind::TypeAlias), 6.0);
    }

    #[test]
    fn test_kind_enum() {
        assert_eq!(kind_bonus(&SymbolKind::Enum), 5.0);
    }

    #[test]
    fn test_kind_variable() {
        assert_eq!(kind_bonus(&SymbolKind::Variable), 2.0);
    }

    #[test]
    fn test_name_exact_match() {
        assert_eq!(name_match_bonus("getUser", "getUser"), 80.0);
    }

    #[test]
    fn test_name_starts_with_ratio() {
        let score = name_match_bonus("getUser", "get");
        // "get" is 3 chars, "getUser" is 7 chars: ratio = 3/7 ≈ 0.4286
        // score = 10 + 30 * 3/7 ≈ 22.86
        assert!((score - 22.857).abs() < 0.01, "expected ~22.86, got {score}");
    }

    #[test]
    fn test_name_substring() {
        assert_eq!(name_match_bonus("UserService", "ser"), 10.0);
    }

    #[test]
    fn test_name_no_match() {
        assert_eq!(name_match_bonus("foo", "xyz"), 0.0);
    }

    #[test]
    fn test_name_short_query() {
        // query < 2 chars → 0
        assert_eq!(name_match_bonus("foo", "x"), 0.0);
    }

    #[test]
    fn test_path_match() {
        let tokens = HashSet::new();
        let score = path_score("src/auth/login.rs", "auth", &tokens);
        assert_eq!(score, 2.0);
    }

    #[test]
    fn test_path_no_match() {
        let tokens = HashSet::new();
        let score = path_score("src/auth/login.rs", "db", &tokens);
        assert_eq!(score, 0.0);
    }

    #[test]
    fn test_path_short_term() {
        let tokens = HashSet::new();
        // single-char terms should not contribute
        let score = path_score("src/a.rs", "a", &tokens);
        assert_eq!(score, 0.0);
    }

    #[test]
    fn test_path_project_name_skip() {
        let mut tokens = HashSet::new();
        tokens.insert("visp".to_string());
        // "visp" appears in path but is a project name token → skip
        let score = path_score("visp-core/src/lib.rs", "visp", &tokens);
        assert_eq!(score, 0.0);
    }

    #[test]
    fn test_score_and_sort_function_before_variable() {
        let mut results = vec![
            ScoredSymbol {
                symbol: Symbol { id: 1, name: "var".into(), kind: SymbolKind::Variable, file_path: "a.rs".into(), line: 1, column: 1, signature: None, docstring: None },
                score: 0.0,
            },
            ScoredSymbol {
                symbol: Symbol { id: 2, name: "func".into(), kind: SymbolKind::Function, file_path: "a.rs".into(), line: 1, column: 1, signature: None, docstring: None },
                score: 0.0,
            },
        ];
        score_and_sort(&mut results, "test", &HashSet::new());
        // Function (10 bonus) should sort before Variable (2 bonus)
        assert_eq!(results[0].symbol.kind, SymbolKind::Function);
        assert_eq!(results[1].symbol.kind, SymbolKind::Variable);
    }

    #[test]
    fn test_score_and_sort_exact_match_first() {
        let mut results = vec![
            ScoredSymbol {
                symbol: Symbol { id: 1, name: "getter".into(), kind: SymbolKind::Function, file_path: "a.rs".into(), line: 1, column: 1, signature: None, docstring: None },
                score: 0.0,
            },
            ScoredSymbol {
                symbol: Symbol { id: 2, name: "getConfig".into(), kind: SymbolKind::Function, file_path: "b.rs".into(), line: 1, column: 1, signature: None, docstring: None },
                score: 0.0,
            },
        ];
        score_and_sort(&mut results, "getConfig", &HashSet::new());
        // Exact match "getConfig" (80 bonus) should be first
        assert_eq!(results[0].symbol.name, "getConfig");
    }

    #[test]
    fn test_get_project_name_tokens_from_path() {
        use std::path::Path;
        let tokens = get_project_name_tokens(Path::new("/home/user/projects/visp-core"));
        assert!(tokens.contains("vispcore"), "vispcore should be extracted as token");
        assert_eq!(tokens.len(), 1, "only one token expected");
    }

    #[test]
    fn test_get_project_name_tokens_short_name() {
        use std::path::Path;
        // Short names (< 5 chars) should be excluded
        let tokens = get_project_name_tokens(Path::new("/home/user/projects/ab"));
        assert!(tokens.is_empty(), "short names should be excluded");
    }
}
