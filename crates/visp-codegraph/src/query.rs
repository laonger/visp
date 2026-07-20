use std::collections::{HashSet, VecDeque};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::graph::Symbol;
use crate::store::{ScoredSymbol, Store, sanitize_fts_query};

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

        let max_fts = results.iter().map(|r| r.score).fold(0.0, f64::max);

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

        Ok(results
            .into_iter()
            .map(|rs| sym_to_info(rs.symbol))
            .collect())
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
fn merge_dedup(results: &mut Vec<ScoredSymbol>, incoming: Vec<Symbol>, max_fts_score: f64) {
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
fn inject_exact(results: &mut Vec<ScoredSymbol>, query: &str, store: &Store, max_fts_score: f64) {
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
    results: &mut [ScoredSymbol],
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
fn path_score(file_path: &str, query: &str, project_name_tokens: &HashSet<String>) -> f64 {
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
#[path = "query_tests.rs"]
mod query_tests;
