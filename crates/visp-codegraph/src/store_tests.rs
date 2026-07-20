#![cfg(test)]
use crate::graph::{Edge, EdgeKind, Symbol, SymbolKind};
use crate::store::*;
use rusqlite::Connection;

// Step 3a: Schema + table creation tests

#[test]
fn test_init_creates_tables() {
    let conn = Connection::open_in_memory().unwrap();
    Store::init_schema(&conn).unwrap();

    let mut stmt = conn
        .prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
        .unwrap();
    let tables: Vec<String> = stmt
        .query_map([], |row| row.get(0))
        .unwrap()
        .filter_map(|r| r.ok())
        .collect();

    assert!(tables.contains(&"symbols".into()));
    assert!(tables.contains(&"edges".into()));
    assert!(tables.contains(&"files".into()));
    assert!(tables.contains(&"imports".into()));
    assert!(tables.contains(&"exports".into()));
}

#[test]
fn test_init_idempotent() {
    let conn = Connection::open_in_memory().unwrap();
    Store::init_schema(&conn).unwrap();
    // Second initialization should not error
    Store::init_schema(&conn).unwrap();
}

// Step 3b: CRUD tests

#[test]
fn test_insert_symbols() {
    let store = create_store();
    let mut symbols = vec![
        Symbol {
            id: 0,
            name: "foo".into(),
            kind: SymbolKind::Function,
            file_path: "a.rs".into(),
            line: 10,
            column: 4,
            signature: Some("fn foo()".into()),
            docstring: None,
        },
        Symbol {
            id: 0,
            name: "bar".into(),
            kind: SymbolKind::Variable,
            file_path: "a.rs".into(),
            line: 20,
            column: 8,
            signature: None,
            docstring: Some("A bar variable".into()),
        },
    ];
    store.insert_symbols(&mut symbols).unwrap();

    // ids should be backfilled (non-zero)
    assert!(symbols[0].id > 0);
    assert!(symbols[1].id > 0);
    assert_ne!(symbols[0].id, symbols[1].id);

    // verify via search
    let results = store.search_symbols("foo", 10).unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].name, "foo");
    assert_eq!(results[0].line, 10);
}

#[test]
fn test_insert_replace() {
    let store = create_store();
    let mut syms = vec![Symbol {
        id: 0,
        name: "foo".into(),
        kind: SymbolKind::Function,
        file_path: "lib.rs".into(),
        line: 10,
        column: 4,
        signature: None,
        docstring: None,
    }];
    store.insert_symbols(&mut syms).unwrap();

    // Replace with different data (same file_path + name)
    let mut syms2 = vec![Symbol {
        id: 0,
        name: "foo".into(),
        kind: SymbolKind::Variable,
        file_path: "lib.rs".into(),
        line: 20,
        column: 8,
        signature: None,
        docstring: None,
    }];
    store.insert_symbols(&mut syms2).unwrap();

    let fetched = store.get_symbols_by_name("foo").unwrap();
    assert_eq!(fetched.len(), 1);
    assert_eq!(fetched[0].kind, SymbolKind::Variable);
    assert_eq!(fetched[0].line, 20);
}

#[test]
fn test_delete_by_file() {
    let store = create_store();
    let mut symbols = vec![
        Symbol {
            id: 0,
            name: "a".into(),
            kind: SymbolKind::Function,
            file_path: "x.rs".into(),
            line: 1,
            column: 1,
            signature: None,
            docstring: None,
        },
        Symbol {
            id: 0,
            name: "b".into(),
            kind: SymbolKind::Function,
            file_path: "y.rs".into(),
            line: 1,
            column: 1,
            signature: None,
            docstring: None,
        },
    ];
    store.insert_symbols(&mut symbols).unwrap();

    store.delete_by_file("x.rs").unwrap();

    let results = store.get_symbols_by_name("a").unwrap();
    assert_eq!(results.len(), 0);

    let results = store.get_symbols_by_name("b").unwrap();
    assert_eq!(results.len(), 1);
}

#[test]
fn test_insert_edges() {
    let store = create_store();
    let mut symbols = vec![
        Symbol {
            id: 0,
            name: "src".into(),
            kind: SymbolKind::Function,
            file_path: "a.rs".into(),
            line: 1,
            column: 1,
            signature: None,
            docstring: None,
        },
        Symbol {
            id: 0,
            name: "dst".into(),
            kind: SymbolKind::Function,
            file_path: "b.rs".into(),
            line: 1,
            column: 1,
            signature: None,
            docstring: None,
        },
    ];
    store.insert_symbols(&mut symbols).unwrap();

    let edges = vec![
        Edge {
            source_id: symbols[0].id,
            target_id: Some(symbols[1].id),
            target_name: None,
            kind: EdgeKind::Call,
        },
        Edge {
            source_id: symbols[0].id,
            target_id: None,
            target_name: Some("unknown".into()),
            kind: EdgeKind::Reference,
        },
    ];
    store.insert_edges(&edges).unwrap();

    let callers = store.get_callers(symbols[1].id).unwrap();
    assert_eq!(callers.len(), 1);
    assert_eq!(callers[0], ("src".into(), "a.rs".into()));

    let unresolved = store.get_unresolved_edges().unwrap();
    assert_eq!(unresolved.len(), 1);
    assert_eq!(unresolved[0].1, "unknown");
}

#[test]
fn test_search_symbols() {
    let store = create_store();
    let mut symbols = vec![
        Symbol {
            id: 0,
            name: "foo".into(),
            kind: SymbolKind::Function,
            file_path: "a.rs".into(),
            line: 1,
            column: 1,
            signature: None,
            docstring: None,
        },
        Symbol {
            id: 0,
            name: "foobar".into(),
            kind: SymbolKind::Function,
            file_path: "b.rs".into(),
            line: 1,
            column: 1,
            signature: None,
            docstring: None,
        },
        Symbol {
            id: 0,
            name: "bar".into(),
            kind: SymbolKind::Function,
            file_path: "c.rs".into(),
            line: 1,
            column: 1,
            signature: None,
            docstring: None,
        },
    ];
    store.insert_symbols(&mut symbols).unwrap();

    let results = store.search_symbols("foo", 10).unwrap();
    assert_eq!(results.len(), 2);
    assert!(results.iter().any(|s| s.name == "foo"));
    assert!(results.iter().any(|s| s.name == "foobar"));

    // limit
    let results = store.search_symbols("foo", 1).unwrap();
    assert_eq!(results.len(), 1);
}

#[test]
fn test_get_callers() {
    let store = create_store();
    let mut symbols = vec![
        Symbol {
            id: 0,
            name: "callee".into(),
            kind: SymbolKind::Function,
            file_path: "a.rs".into(),
            line: 1,
            column: 1,
            signature: None,
            docstring: None,
        },
        Symbol {
            id: 0,
            name: "caller1".into(),
            kind: SymbolKind::Function,
            file_path: "b.rs".into(),
            line: 1,
            column: 1,
            signature: None,
            docstring: None,
        },
        Symbol {
            id: 0,
            name: "caller2".into(),
            kind: SymbolKind::Function,
            file_path: "c.rs".into(),
            line: 1,
            column: 1,
            signature: None,
            docstring: None,
        },
    ];
    store.insert_symbols(&mut symbols).unwrap();

    let edges = vec![
        Edge {
            source_id: symbols[1].id,
            target_id: Some(symbols[0].id),
            target_name: None,
            kind: EdgeKind::Call,
        },
        Edge {
            source_id: symbols[2].id,
            target_id: Some(symbols[0].id),
            target_name: None,
            kind: EdgeKind::Call,
        },
    ];
    store.insert_edges(&edges).unwrap();

    let callers = store.get_callers(symbols[0].id).unwrap();
    assert_eq!(callers.len(), 2);
    assert!(callers.contains(&("caller1".into(), "b.rs".into())));
    assert!(callers.contains(&("caller2".into(), "c.rs".into())));
}

#[test]
fn test_get_callees() {
    let store = create_store();
    let mut symbols = vec![
        Symbol {
            id: 0,
            name: "callee1".into(),
            kind: SymbolKind::Function,
            file_path: "a.rs".into(),
            line: 1,
            column: 1,
            signature: None,
            docstring: None,
        },
        Symbol {
            id: 0,
            name: "callee2".into(),
            kind: SymbolKind::Function,
            file_path: "b.rs".into(),
            line: 1,
            column: 1,
            signature: None,
            docstring: None,
        },
        Symbol {
            id: 0,
            name: "caller".into(),
            kind: SymbolKind::Function,
            file_path: "c.rs".into(),
            line: 1,
            column: 1,
            signature: None,
            docstring: None,
        },
    ];
    store.insert_symbols(&mut symbols).unwrap();

    let edges = vec![
        Edge {
            source_id: symbols[2].id,
            target_id: Some(symbols[0].id),
            target_name: None,
            kind: EdgeKind::Call,
        },
        Edge {
            source_id: symbols[2].id,
            target_id: Some(symbols[1].id),
            target_name: None,
            kind: EdgeKind::Call,
        },
    ];
    store.insert_edges(&edges).unwrap();

    let callees = store.get_callees(symbols[2].id).unwrap();
    assert_eq!(callees.len(), 2);
    assert!(callees.contains(&("callee1".into(), "a.rs".into())));
    assert!(callees.contains(&("callee2".into(), "b.rs".into())));
}

#[test]
fn test_resolve_edges() {
    let store = create_store();
    let mut symbols = vec![
        Symbol {
            id: 0,
            name: "target".into(),
            kind: SymbolKind::Function,
            file_path: "a.rs".into(),
            line: 1,
            column: 1,
            signature: None,
            docstring: None,
        },
        Symbol {
            id: 0,
            name: "source".into(),
            kind: SymbolKind::Function,
            file_path: "b.rs".into(),
            line: 1,
            column: 1,
            signature: None,
            docstring: None,
        },
    ];
    store.insert_symbols(&mut symbols).unwrap();

    let edges = vec![Edge {
        source_id: symbols[1].id,
        target_id: None,
        target_name: Some("target".into()),
        kind: EdgeKind::Call,
    }];
    store.insert_edges(&edges).unwrap();

    let unresolved = store.get_unresolved_edges().unwrap();
    assert_eq!(unresolved.len(), 1);
    let (edge_id, _) = unresolved[0];

    store.resolve_edge(edge_id, symbols[0].id).unwrap();

    let unresolved = store.get_unresolved_edges().unwrap();
    assert_eq!(unresolved.len(), 0);

    let callers = store.get_callers(symbols[0].id).unwrap();
    assert_eq!(callers.len(), 1);
}

#[test]
fn test_insert_exports() {
    let store = create_store();
    let mut symbols = vec![Symbol {
        id: 0,
        name: "foo".into(),
        kind: SymbolKind::Function,
        file_path: "a.rs".into(),
        line: 1,
        column: 1,
        signature: None,
        docstring: None,
    }];
    store.insert_symbols(&mut symbols).unwrap();

    store
        .insert_exports(
            "a.rs",
            &[
                ("Foo".into(), Some(symbols[0].id), None),
                ("Bar".into(), None, Some("other::bar".into())),
            ],
        )
        .unwrap();

    // verify via direct query
    let conn = store.conn.lock().unwrap();
    let mut stmt = conn
        .prepare("SELECT export_name, re_export_source FROM exports WHERE file_path = ?1 ORDER BY export_name")
        .unwrap();
    let rows: Vec<(String, Option<String>)> = stmt
        .query_map(rusqlite::params!["a.rs"], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })
        .unwrap()
        .filter_map(|r| r.ok())
        .collect();

    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].0, "Bar");
    assert_eq!(rows[1].0, "Foo");
}

#[test]
fn test_get_symbols_by_name() {
    let store = create_store();
    let mut symbols = vec![
        Symbol {
            id: 0,
            name: "foo".into(),
            kind: SymbolKind::Function,
            file_path: "a.rs".into(),
            line: 1,
            column: 1,
            signature: None,
            docstring: None,
        },
        Symbol {
            id: 0,
            name: "foo".into(),
            kind: SymbolKind::Variable,
            file_path: "b.rs".into(),
            line: 2,
            column: 2,
            signature: None,
            docstring: None,
        },
        Symbol {
            id: 0,
            name: "bar".into(),
            kind: SymbolKind::Class,
            file_path: "c.rs".into(),
            line: 3,
            column: 3,
            signature: None,
            docstring: None,
        },
    ];
    store.insert_symbols(&mut symbols).unwrap();

    let results = store.get_symbols_by_name("foo").unwrap();
    assert_eq!(results.len(), 2);
    assert!(results.iter().all(|s| s.name == "foo"));

    let results = store.get_symbols_by_name("bar").unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].kind, SymbolKind::Class);

    let results = store.get_symbols_by_name("nonexistent").unwrap();
    assert_eq!(results.len(), 0);
}

#[test]
fn test_list_indexed_files() {
    let store = create_store();
    store.upsert_file("a.rs", "rust", 5, 1000).unwrap();
    store.upsert_file("b.rs", "rust", 3, 2000).unwrap();

    let files = store.list_indexed_files().unwrap();
    assert_eq!(files.len(), 2);
    assert!(files.contains(&"a.rs".into()));
    assert!(files.contains(&"b.rs".into()));

    // also test delete and upsert replace
    store.delete_file_record("a.rs").unwrap();
    let files = store.list_indexed_files().unwrap();
    assert_eq!(files.len(), 1);
    assert_eq!(files[0], "b.rs");
}

// --- Step 1a: FTS5 schema + auxiliary index tests ---

#[test]
fn test_fts_table_exists() {
    let conn = Connection::open_in_memory().unwrap();
    Store::init_schema(&conn).unwrap();
    let count: i64 = conn
        .query_row(
            "SELECT count(*) FROM sqlite_master WHERE name='symbols_fts'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count, 1, "symbols_fts virtual table should exist");
}

#[test]
fn test_fts_triggers_exist() {
    let conn = Connection::open_in_memory().unwrap();
    Store::init_schema(&conn).unwrap();
    for trigger_name in &["symbols_ai", "symbols_ad", "symbols_au"] {
        let count: i64 = conn
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE type='trigger' AND name=?1",
                rusqlite::params![trigger_name],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 1, "trigger {trigger_name} should exist");
    }
}

#[test]
fn test_fts_backfill_on_insert() {
    let store = create_store();
    let mut symbols = vec![Symbol {
        id: 0,
        name: "test_func".into(),
        kind: SymbolKind::Function,
        file_path: "lib.rs".into(),
        line: 10,
        column: 4,
        signature: Some("fn test_func()".into()),
        docstring: Some("A test function".into()),
    }];
    store.insert_symbols(&mut symbols).unwrap();

    let conn = store.conn.lock().unwrap();
    let count: i64 = conn
        .query_row(
            "SELECT count(*) FROM symbols_fts WHERE name = 'test_func'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count, 1, "FTS5 should have the inserted symbol");

    let name: String = conn
        .query_row(
            "SELECT name FROM symbols_fts WHERE name = 'test_func'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(name, "test_func");
}

#[test]
fn test_fts_backfill_on_update() {
    let store = create_store();
    let mut symbols = vec![Symbol {
        id: 0,
        name: "old_name".into(),
        kind: SymbolKind::Function,
        file_path: "lib.rs".into(),
        line: 10,
        column: 4,
        signature: None,
        docstring: None,
    }];
    store.insert_symbols(&mut symbols).unwrap();
    let sym_id = symbols[0].id;

    // Direct SQL update to test the update trigger
    let conn = store.conn.lock().unwrap();
    conn.execute(
        "UPDATE symbols SET name = ?1 WHERE id = ?2",
        rusqlite::params!["new_name", sym_id as i64],
    )
    .unwrap();
    drop(conn);

    // OLD name should be gone from FTS5
    let conn = store.conn.lock().unwrap();
    let count_old: i64 = conn
        .query_row(
            "SELECT count(*) FROM symbols_fts WHERE name = 'old_name'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count_old, 0, "OLD name should be deleted from FTS5");

    // NEW name should be in FTS5
    let count_new: i64 = conn
        .query_row(
            "SELECT count(*) FROM symbols_fts WHERE name = 'new_name'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count_new, 1, "NEW name should be in FTS5");
}

#[test]
fn test_fts_backfill_on_delete() {
    let store = create_store();
    let mut symbols = vec![Symbol {
        id: 0,
        name: "to_delete".into(),
        kind: SymbolKind::Function,
        file_path: "lib.rs".into(),
        line: 10,
        column: 4,
        signature: None,
        docstring: None,
    }];
    store.insert_symbols(&mut symbols).unwrap();

    store.delete_by_file("lib.rs").unwrap();

    let conn = store.conn.lock().unwrap();
    let count: i64 = conn
        .query_row(
            "SELECT count(*) FROM symbols_fts WHERE name = 'to_delete'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count, 0, "FTS5 should have deleted the symbol");
}

#[test]
fn test_idx_kind_exists() {
    let conn = Connection::open_in_memory().unwrap();
    Store::init_schema(&conn).unwrap();
    let count: i64 = conn
        .query_row(
            "SELECT count(*) FROM sqlite_master WHERE type='index' AND name='idx_symbols_kind'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count, 1, "idx_symbols_kind index should exist");
}

#[test]
fn test_idx_lower_name_exists() {
    let conn = Connection::open_in_memory().unwrap();
    Store::init_schema(&conn).unwrap();
    let count: i64 = conn
        .query_row(
            "SELECT count(*) FROM sqlite_master WHERE type='index' AND name='idx_symbols_lower_name'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count, 1, "idx_symbols_lower_name index should exist");
}

#[test]
fn test_schema_idempotent() {
    // Already covered by test_init_idempotent, verify FTS5 is re-creatable
    let conn = Connection::open_in_memory().unwrap();
    Store::init_schema(&conn).unwrap();
    // Second initialization should not error
    let result = Store::init_schema(&conn);
    assert!(result.is_ok(), "init_schema should be idempotent");
}

// --- Step 1b: search_fts tests ---

#[test]
fn test_fts_basic_search() {
    let store = create_store();
    let mut symbols = vec![
        Symbol {
            id: 0,
            name: "getUser".into(),
            kind: SymbolKind::Function,
            file_path: "a.ts".into(),
            line: 1,
            column: 1,
            signature: Some("getUser(id: number)".into()),
            docstring: None,
        },
        Symbol {
            id: 0,
            name: "getConfig".into(),
            kind: SymbolKind::Function,
            file_path: "b.ts".into(),
            line: 1,
            column: 1,
            signature: None,
            docstring: None,
        },
        Symbol {
            id: 0,
            name: "setValue".into(),
            kind: SymbolKind::Function,
            file_path: "c.ts".into(),
            line: 1,
            column: 1,
            signature: None,
            docstring: None,
        },
    ];
    store.insert_symbols(&mut symbols).unwrap();

    let results = store.search_fts("\"get\"*", 10).unwrap();
    assert!(results.len() >= 2, "should find getUser and getConfig");
    assert!(results.iter().any(|r| r.symbol.name == "getUser"));
    assert!(results.iter().any(|r| r.symbol.name == "getConfig"));
}

#[test]
fn test_fts_has_reasonable_score() {
    let store = create_store();
    let mut symbols = vec![
        Symbol {
            id: 0,
            name: "auth".into(),
            kind: SymbolKind::Function,
            file_path: "a.rs".into(),
            line: 1,
            column: 1,
            signature: None,
            docstring: Some("authenticate user".into()),
        },
        Symbol {
            id: 0,
            name: "auth_user".into(),
            kind: SymbolKind::Function,
            file_path: "b.rs".into(),
            line: 1,
            column: 1,
            signature: None,
            docstring: None,
        },
    ];
    store.insert_symbols(&mut symbols).unwrap();

    let results = store.search_fts("\"auth\"*", 10).unwrap();
    assert!(!results.is_empty(), "should find results");
    // BM25 typically returns negative scores; any non-NaN value is fine
    assert!(!results[0].score.is_nan(), "score should be valid");
}

#[test]
fn test_fts_empty_query() {
    let store = create_store();
    let mut symbols = vec![Symbol {
        id: 0,
        name: "foo".into(),
        kind: SymbolKind::Function,
        file_path: "a.rs".into(),
        line: 1,
        column: 1,
        signature: None,
        docstring: None,
    }];
    store.insert_symbols(&mut symbols).unwrap();

    // Empty FTS query returns empty results via search_fts with empty string
    // The search_fts method delegates to FTS5 which errors on empty query,
    // so our caller (search) should prevent this case.
    // Here we just verify the raw method behavior:
    let result = store.search_fts("", 10);
    assert!(result.is_err() || result.unwrap().is_empty());
}

#[test]
fn test_fts_sanitize_boolean_ops() {
    let store = create_store();
    let mut symbols = vec![Symbol {
        id: 0,
        name: "bar".into(),
        kind: SymbolKind::Function,
        file_path: "a.rs".into(),
        line: 1,
        column: 1,
        signature: None,
        docstring: None,
    }];
    store.insert_symbols(&mut symbols).unwrap();

    // "AND OR NOT" should be stripped; "bar" remains
    let sanitized = sanitize_fts_query("NOT AND OR NEAR bar");
    assert!(!sanitized.contains("NOT"), "boolean ops should be removed");
    assert!(sanitized.contains("bar"), "bar should survive");
    let results = store.search_fts(&sanitized, 10).unwrap();
    assert!(!results.is_empty(), "should find bar");
}

#[test]
fn test_fts_query_limit() {
    let store = create_store();
    let mut symbols = vec![
        Symbol {
            id: 0,
            name: "foo1".into(),
            kind: SymbolKind::Function,
            file_path: "a.rs".into(),
            line: 1,
            column: 1,
            signature: None,
            docstring: None,
        },
        Symbol {
            id: 0,
            name: "foo2".into(),
            kind: SymbolKind::Function,
            file_path: "b.rs".into(),
            line: 1,
            column: 1,
            signature: None,
            docstring: None,
        },
        Symbol {
            id: 0,
            name: "foo3".into(),
            kind: SymbolKind::Function,
            file_path: "c.rs".into(),
            line: 1,
            column: 1,
            signature: None,
            docstring: None,
        },
    ];
    store.insert_symbols(&mut symbols).unwrap();

    let results = store.search_fts("\"foo\"*", 2).unwrap();
    assert!(results.len() <= 2, "limit should cap results");
}

// --- Step 1c: search_like tests ---

#[test]
fn test_like_substring() {
    let store = create_store();
    let mut symbols = vec![
        Symbol {
            id: 0,
            name: "my_getter".into(),
            kind: SymbolKind::Function,
            file_path: "a.rs".into(),
            line: 1,
            column: 1,
            signature: None,
            docstring: None,
        },
        Symbol {
            id: 0,
            name: "getter".into(),
            kind: SymbolKind::Function,
            file_path: "b.rs".into(),
            line: 1,
            column: 1,
            signature: None,
            docstring: None,
        },
        Symbol {
            id: 0,
            name: "getUser".into(),
            kind: SymbolKind::Function,
            file_path: "c.rs".into(),
            line: 1,
            column: 1,
            signature: None,
            docstring: None,
        },
        Symbol {
            id: 0,
            name: "setter".into(),
            kind: SymbolKind::Function,
            file_path: "d.rs".into(),
            line: 1,
            column: 1,
            signature: None,
            docstring: None,
        },
    ];
    store.insert_symbols(&mut symbols).unwrap();

    let results = store.search_like("get", 10).unwrap();
    assert_eq!(results.len(), 3, "should find my_getter, getter, getUser");
    let names: Vec<&str> = results.iter().map(|s| s.name.as_str()).collect();
    assert!(names.contains(&"my_getter"));
    assert!(names.contains(&"getter"));
    assert!(names.contains(&"getUser"));
}

#[test]
fn test_like_case_insensitive() {
    let store = create_store();
    let mut symbols = vec![Symbol {
        id: 0,
        name: "getUser".into(),
        kind: SymbolKind::Function,
        file_path: "a.rs".into(),
        line: 1,
        column: 1,
        signature: None,
        docstring: None,
    }];
    store.insert_symbols(&mut symbols).unwrap();

    let results = store.search_like("getuser", 10).unwrap();
    assert_eq!(
        results.len(),
        1,
        "case-insensitive search should match getUser"
    );
    assert_eq!(results[0].name, "getUser");
}

#[test]
fn test_like_search_signature() {
    let store = create_store();
    let mut symbols = vec![
        Symbol {
            id: 0,
            name: "parse".into(),
            kind: SymbolKind::Function,
            file_path: "a.rs".into(),
            line: 1,
            column: 1,
            signature: Some("fn parse() -> i32".into()),
            docstring: None,
        },
        Symbol {
            id: 0,
            name: "run".into(),
            kind: SymbolKind::Function,
            file_path: "b.rs".into(),
            line: 1,
            column: 1,
            signature: Some("fn run()".into()),
            docstring: None,
        },
    ];
    store.insert_symbols(&mut symbols).unwrap();

    let results = store.search_like("i32", 10).unwrap();
    assert_eq!(results.len(), 1, "should find symbol with i32 in signature");
    assert_eq!(results[0].name, "parse");
}

#[test]
fn test_like_escape_wildcard() {
    let store = create_store();
    let mut symbols = vec![Symbol {
        id: 0,
        name: "foo_bar".into(),
        kind: SymbolKind::Function,
        file_path: "a.rs".into(),
        line: 1,
        column: 1,
        signature: None,
        docstring: None,
    }];
    store.insert_symbols(&mut symbols).unwrap();

    // _ should be literal, not single-char wildcard
    let results = store.search_like("foo_bar", 10).unwrap();
    assert_eq!(results.len(), 1, "_ should be literal, so foo_bar matches");
}

#[test]
fn test_like_escape_percent() {
    let store = create_store();
    let mut symbols = vec![Symbol {
        id: 0,
        name: "100%".into(),
        kind: SymbolKind::Function,
        file_path: "a.rs".into(),
        line: 1,
        column: 1,
        signature: None,
        docstring: None,
    }];
    store.insert_symbols(&mut symbols).unwrap();

    // % should be literal
    let results = store.search_like("100%", 10).unwrap();
    assert_eq!(results.len(), 1, "% should be literal");
}

#[test]
fn test_like_empty_query() {
    let store = create_store();
    let mut symbols = vec![Symbol {
        id: 0,
        name: "foo".into(),
        kind: SymbolKind::Function,
        file_path: "a.rs".into(),
        line: 1,
        column: 1,
        signature: None,
        docstring: None,
    }];
    store.insert_symbols(&mut symbols).unwrap();

    // empty query -> pattern "%%" -> matches everything
    let results = store.search_like("", 10).unwrap();
    assert!(!results.is_empty(), "empty LIKE should match all");
}

#[test]
fn test_like_limit() {
    let store = create_store();
    let mut symbols = vec![
        Symbol {
            id: 0,
            name: "foo1".into(),
            kind: SymbolKind::Function,
            file_path: "a.rs".into(),
            line: 1,
            column: 1,
            signature: None,
            docstring: None,
        },
        Symbol {
            id: 0,
            name: "foo2".into(),
            kind: SymbolKind::Function,
            file_path: "b.rs".into(),
            line: 1,
            column: 1,
            signature: None,
            docstring: None,
        },
        Symbol {
            id: 0,
            name: "foo3".into(),
            kind: SymbolKind::Function,
            file_path: "c.rs".into(),
            line: 1,
            column: 1,
            signature: None,
            docstring: None,
        },
    ];
    store.insert_symbols(&mut symbols).unwrap();

    let results = store.search_like("foo", 2).unwrap();
    assert_eq!(results.len(), 2, "limit should cap results");
}

// --- Step 1d: backfill_fts tests ---

#[test]
fn test_backfill_existing_data() {
    let conn = Connection::open_in_memory().unwrap();
    Store::init_schema(&conn).unwrap();
    let store = Store {
        conn: Arc::new(Mutex::new(conn)),
    };

    // Insert symbols (triggers populate FTS5)
    let mut symbols = vec![
        Symbol {
            id: 0,
            name: "foo".into(),
            kind: SymbolKind::Function,
            file_path: "a.rs".into(),
            line: 1,
            column: 1,
            signature: None,
            docstring: None,
        },
        Symbol {
            id: 0,
            name: "bar".into(),
            kind: SymbolKind::Variable,
            file_path: "b.rs".into(),
            line: 2,
            column: 2,
            signature: None,
            docstring: None,
        },
    ];
    store.insert_symbols(&mut symbols).unwrap();

    // Backfill should be safe (INSERT OR IGNORE prevents duplicates)
    store.backfill_fts().unwrap();

    // Verify FTS5 still has the expected rows (no duplicates)
    {
        let conn = store.conn.lock().unwrap();
        let count: i64 = conn
            .query_row("SELECT count(*) FROM symbols_fts", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 2, "FTS5 should have 2 rows after backfill");
    }

    // Verify FTS5 search works
    let results = store.search_fts("\"foo\"*", 10).unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].symbol.name, "foo");
}

#[test]
fn test_backfill_idempotent() {
    let store = create_store();

    let mut symbols = vec![Symbol {
        id: 0,
        name: "foo".into(),
        kind: SymbolKind::Function,
        file_path: "a.rs".into(),
        line: 1,
        column: 1,
        signature: None,
        docstring: None,
    }];
    store.insert_symbols(&mut symbols).unwrap();

    // After trigger-based insert, FTS5 already has the row
    // Backfill uses INSERT OR IGNORE, so it should be idempotent
    store.backfill_fts().unwrap();
    store.backfill_fts().unwrap();

    let conn = store.conn.lock().unwrap();
    let count: i64 = conn
        .query_row("SELECT count(*) FROM symbols_fts", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 1, "backfill should be idempotent");
}

// --- helpers ---

fn create_store() -> Store {
    let conn = Connection::open_in_memory().unwrap();
    Store::init_schema(&conn).unwrap();
    Store {
        conn: Arc::new(Mutex::new(conn)),
    }
}
