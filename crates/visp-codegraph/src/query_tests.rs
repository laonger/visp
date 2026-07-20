#![cfg(test)]
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

#[allow(clippy::too_many_arguments)]
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
    assert_eq!(
        results.len(),
        1,
        "search is case-insensitive, should find getUser"
    );
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
    let engine = QueryEngine::new(
        Arc::new(store_raw),
        Arc::new(AtomicBool::new(false)),
        HashSet::new(),
    );
    let err = engine.impact("Nonexistent", 1).unwrap_err();
    assert!(err.contains("not found"));
}

// --- Step 2a: Search orchestration tests ---

#[test]
fn test_search_empty_query() {
    let (store_raw, _db_dir) = create_store();
    let store = Arc::new(store_raw);
    insert_symbol(
        &store,
        "foo",
        SymbolKind::Function,
        "a.rs",
        1,
        1,
        None,
        None,
    );
    let engine = QueryEngine::new(store, Arc::new(AtomicBool::new(false)), HashSet::new());

    let results = engine.search("", 10).unwrap();
    // Empty query returns LIKE % results (matches everything)
    assert!(
        !results.is_empty(),
        "empty query should return results via LIKE"
    );
}

#[test]
fn test_search_fts_empty_like_fallback() {
    let (store_raw, _db_dir) = create_store();
    let store = Arc::new(store_raw);
    // Insert a camelCase symbol. FTS5 unicode61 tokenizer doesn't split
    // camelCase, so "fooBarBaz" is stored as a single token.
    // Searching "arBa": FTS5 does prefix match "arba"* → 0 results
    // ("foobarbaz" doesn't start with "arba").
    // LIKE does substring match %arBa% → 1 result.
    insert_symbol(
        &store,
        "fooBarBaz",
        SymbolKind::Function,
        "a.rs",
        1,
        1,
        None,
        None,
    );

    let engine = QueryEngine::new(store, Arc::new(AtomicBool::new(false)), HashSet::new());
    let results = engine.search("arBa", 10).unwrap();

    // Must find the symbol via LIKE fallback when FTS returns 0.
    // (Before the fix, max_fts = NEG_INFINITY caused all fallback
    // scores to be NEG_INFINITY, breaking sort — but results would
    // still be returned. This assertion ensures the FTS→LIKE path.)
    assert_eq!(results.len(), 1, "LIKE fallback should find 'fooBarBaz'");
}

#[test]
fn test_search_merge_dedup() {
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
        "getConfig",
        SymbolKind::Function,
        "b.rs",
        1,
        1,
        None,
        None,
    );
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
        "getter",
        SymbolKind::Function,
        "b.rs",
        2,
        1,
        None,
        None,
    );
    let engine = QueryEngine::new(store, Arc::new(AtomicBool::new(false)), HashSet::new());

    // "getUser" as query: exact match should be included
    let results = engine.search("getUser", 10).unwrap();
    assert!(
        results.iter().any(|r| r.name == "getUser"),
        "exact match getUser should be in results"
    );
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
    assert!(
        (score - 22.857).abs() < 0.01,
        "expected ~22.86, got {score}"
    );
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
            symbol: Symbol {
                id: 1,
                name: "var".into(),
                kind: SymbolKind::Variable,
                file_path: "a.rs".into(),
                line: 1,
                column: 1,
                signature: None,
                docstring: None,
            },
            score: 0.0,
        },
        ScoredSymbol {
            symbol: Symbol {
                id: 2,
                name: "func".into(),
                kind: SymbolKind::Function,
                file_path: "a.rs".into(),
                line: 1,
                column: 1,
                signature: None,
                docstring: None,
            },
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
            symbol: Symbol {
                id: 1,
                name: "getter".into(),
                kind: SymbolKind::Function,
                file_path: "a.rs".into(),
                line: 1,
                column: 1,
                signature: None,
                docstring: None,
            },
            score: 0.0,
        },
        ScoredSymbol {
            symbol: Symbol {
                id: 2,
                name: "getConfig".into(),
                kind: SymbolKind::Function,
                file_path: "b.rs".into(),
                line: 1,
                column: 1,
                signature: None,
                docstring: None,
            },
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
    assert!(
        tokens.contains("vispcore"),
        "vispcore should be extracted as token"
    );
    assert_eq!(tokens.len(), 1, "only one token expected");
}

#[test]
fn test_get_project_name_tokens_short_name() {
    use std::path::Path;
    // Short names (< 5 chars) should be excluded
    let tokens = get_project_name_tokens(Path::new("/home/user/projects/ab"));
    assert!(tokens.is_empty(), "short names should be excluded");
}
