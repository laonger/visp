use rusqlite::Connection;
use std::path::Path;
use std::sync::{Arc, Mutex};

use crate::graph::{Edge, EdgeKind, Symbol, SymbolKind};

pub struct Store {
    conn: Arc<Mutex<Connection>>,
}

impl Store {
    pub fn open(db_path: &Path) -> rusqlite::Result<Self> {
        if let Some(parent) = db_path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent)
                .map_err(|e| rusqlite::Error::InvalidParameterName(e.to_string()))?;
        }
        let conn = Connection::open(db_path)?;
        Self::init_schema(&conn)?;
        Ok(Store {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    pub fn init_schema(conn: &Connection) -> rusqlite::Result<()> {
        conn.execute_batch("PRAGMA journal_mode = WAL;")?;
        conn.execute_batch("PRAGMA foreign_keys = ON;")?;
        conn.execute_batch("PRAGMA case_sensitive_like = ON;")?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS symbols (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                name TEXT NOT NULL,
                kind TEXT NOT NULL,
                file_path TEXT NOT NULL,
                line INTEGER NOT NULL,
                column INTEGER NOT NULL,
                signature TEXT,
                docstring TEXT,
                UNIQUE(file_path, name)
            );
            CREATE INDEX IF NOT EXISTS idx_symbols_name ON symbols(name);
            CREATE INDEX IF NOT EXISTS idx_symbols_file ON symbols(file_path);

            CREATE TABLE IF NOT EXISTS edges (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                source_id INTEGER NOT NULL REFERENCES symbols(id) ON DELETE CASCADE,
                target_id INTEGER REFERENCES symbols(id) ON DELETE SET NULL,
                target_name TEXT,
                kind TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_edges_source ON edges(source_id);
            CREATE INDEX IF NOT EXISTS idx_edges_target ON edges(target_id);

            CREATE TABLE IF NOT EXISTS files (
                path TEXT PRIMARY KEY,
                language TEXT NOT NULL,
                symbol_count INTEGER DEFAULT 0,
                last_indexed_at INTEGER NOT NULL
            );

            CREATE TABLE IF NOT EXISTS imports (
                file_path TEXT NOT NULL,
                local_name TEXT NOT NULL,
                import_source TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_imports_file ON imports(file_path, local_name);

            CREATE TABLE IF NOT EXISTS exports (
                file_path TEXT NOT NULL,
                export_name TEXT NOT NULL,
                symbol_id INTEGER REFERENCES symbols(id) ON DELETE CASCADE,
                re_export_source TEXT
            );
            CREATE INDEX IF NOT EXISTS idx_exports_file ON exports(file_path, export_name);

            CREATE INDEX IF NOT EXISTS idx_symbols_kind ON symbols(kind);
            CREATE INDEX IF NOT EXISTS idx_symbols_lower_name ON symbols(LOWER(name));

            CREATE VIRTUAL TABLE IF NOT EXISTS symbols_fts USING fts5(
                id UNINDEXED,
                name,
                kind UNINDEXED,
                signature,
                docstring,
                content='symbols',
                content_rowid='rowid'
            );

            CREATE TRIGGER IF NOT EXISTS symbols_ai AFTER INSERT ON symbols BEGIN
                INSERT INTO symbols_fts(rowid, id, name, kind, signature, docstring)
                VALUES (NEW.rowid, NEW.id, NEW.name, NEW.kind, NEW.signature, NEW.docstring);
            END;

            CREATE TRIGGER IF NOT EXISTS symbols_ad AFTER DELETE ON symbols BEGIN
                INSERT INTO symbols_fts(symbols_fts, rowid, id, name, kind, signature, docstring)
                VALUES ('delete', OLD.rowid, OLD.id, OLD.name, OLD.kind, OLD.signature, OLD.docstring);
            END;

            CREATE TRIGGER IF NOT EXISTS symbols_au AFTER UPDATE ON symbols BEGIN
                INSERT INTO symbols_fts(symbols_fts, rowid, id, name, kind, signature, docstring)
                VALUES ('delete', OLD.rowid, OLD.id, OLD.name, OLD.kind, OLD.signature, OLD.docstring);
                INSERT INTO symbols_fts(rowid, id, name, kind, signature, docstring)
                VALUES (NEW.rowid, NEW.id, NEW.name, NEW.kind, NEW.signature, NEW.docstring);
            END;",
        )?;
        Ok(())
    }

    pub fn insert_symbols(&self, symbols: &mut [Symbol]) -> rusqlite::Result<()> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "INSERT OR REPLACE INTO symbols (name, kind, file_path, line, column, signature, docstring)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        )?;
        for sym in symbols.iter_mut() {
            stmt.execute(rusqlite::params![
                sym.name,
                symbol_kind_to_str(&sym.kind),
                sym.file_path,
                sym.line,
                sym.column,
                sym.signature,
                sym.docstring,
            ])?;
            sym.id = conn.last_insert_rowid() as u64;
        }
        Ok(())
    }

    pub fn delete_by_file(&self, path: &str) -> rusqlite::Result<()> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        tx.execute(
            "DELETE FROM symbols WHERE file_path = ?1",
            rusqlite::params![path],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn insert_edges(&self, edges: &[Edge]) -> rusqlite::Result<()> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "INSERT INTO edges (source_id, target_id, target_name, kind)
             VALUES (?1, ?2, ?3, ?4)",
        )?;
        for edge in edges {
            stmt.execute(rusqlite::params![
                edge.source_id as i64,
                edge.target_id.map(|id| id as i64),
                edge.target_name,
                edge_kind_to_str(&edge.kind),
            ])?;
        }
        Ok(())
    }

    pub fn insert_imports(
        &self,
        file_path: &str,
        imports: &[(String, String)],
    ) -> rusqlite::Result<()> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "INSERT INTO imports (file_path, local_name, import_source)
             VALUES (?1, ?2, ?3)",
        )?;
        for (local_name, import_source) in imports {
            stmt.execute(rusqlite::params![file_path, local_name, import_source])?;
        }
        Ok(())
    }

    pub fn insert_exports(
        &self,
        file_path: &str,
        exports: &[(String, Option<u64>, Option<String>)],
    ) -> rusqlite::Result<()> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "INSERT INTO exports (file_path, export_name, symbol_id, re_export_source)
             VALUES (?1, ?2, ?3, ?4)",
        )?;
        for (export_name, symbol_id, re_export_source) in exports {
            stmt.execute(rusqlite::params![
                file_path,
                export_name,
                symbol_id.map(|id| id as i64),
                re_export_source,
            ])?;
        }
        Ok(())
    }

    pub fn search_symbols(&self, prefix: &str, limit: usize) -> rusqlite::Result<Vec<Symbol>> {
        let conn = self.conn.lock().unwrap();
        let pattern = format!("{}%", prefix);
        let mut stmt = conn.prepare(
            "SELECT id, name, kind, file_path, line, column, signature, docstring
             FROM symbols WHERE name LIKE ?1 LIMIT ?2",
        )?;
        let rows = stmt.query_map(rusqlite::params![pattern, limit as i64], row_to_symbol)?;
        let mut symbols = Vec::new();
        for row in rows {
            symbols.push(row?);
        }
        Ok(symbols)
    }

    pub fn get_symbols_by_name(&self, name: &str) -> rusqlite::Result<Vec<Symbol>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, name, kind, file_path, line, column, signature, docstring
             FROM symbols WHERE name = ?1",
        )?;
        let rows = stmt.query_map(rusqlite::params![name], row_to_symbol)?;
        let mut symbols = Vec::new();
        for row in rows {
            symbols.push(row?);
        }
        Ok(symbols)
    }

    pub fn get_callers(&self, symbol_id: u64) -> rusqlite::Result<Vec<(String, String)>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT s.name, s.file_path
             FROM edges e
             JOIN symbols s ON e.source_id = s.id
             WHERE e.target_id = ?1 AND e.target_id IS NOT NULL",
        )?;
        let rows = stmt.query_map(rusqlite::params![symbol_id as i64], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        let mut result = Vec::new();
        for row in rows {
            result.push(row?);
        }
        Ok(result)
    }

    pub fn get_callees(&self, symbol_id: u64) -> rusqlite::Result<Vec<(String, String)>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT s.name, s.file_path
             FROM edges e
             JOIN symbols s ON e.target_id = s.id
             WHERE e.source_id = ?1 AND e.target_id IS NOT NULL",
        )?;
        let rows = stmt.query_map(rusqlite::params![symbol_id as i64], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        let mut result = Vec::new();
        for row in rows {
            result.push(row?);
        }
        Ok(result)
    }

    pub fn get_unresolved_edges(&self) -> rusqlite::Result<Vec<(i64, String)>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, target_name FROM edges
             WHERE target_id IS NULL AND target_name IS NOT NULL",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
        })?;
        let mut result = Vec::new();
        for row in rows {
            result.push(row?);
        }
        Ok(result)
    }

    pub fn resolve_edge(&self, edge_id: i64, target_id: u64) -> rusqlite::Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE edges SET target_id = ?1, target_name = NULL WHERE id = ?2",
            rusqlite::params![target_id as i64, edge_id],
        )?;
        Ok(())
    }

    pub fn upsert_file(
        &self,
        path: &str,
        language: &str,
        count: u32,
        timestamp: u64,
    ) -> rusqlite::Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT OR REPLACE INTO files (path, language, symbol_count, last_indexed_at)
             VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![path, language, count, timestamp as i64],
        )?;
        Ok(())
    }

    pub fn delete_file_record(&self, path: &str) -> rusqlite::Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute("DELETE FROM files WHERE path = ?1", rusqlite::params![path])?;
        Ok(())
    }

    pub fn get_symbol(&self, id: u64) -> rusqlite::Result<Option<Symbol>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, name, kind, file_path, line, column, signature, docstring
             FROM symbols WHERE id = ?1",
        )?;
        let mut rows = stmt.query_map(rusqlite::params![id as i64], row_to_symbol)?;
        match rows.next() {
            Some(Ok(sym)) => Ok(Some(sym)),
            _ => Ok(None),
        }
    }

    pub fn list_indexed_files(&self) -> rusqlite::Result<Vec<String>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare("SELECT path FROM files")?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
        let mut files = Vec::new();
        for row in rows {
            files.push(row?);
        }
        Ok(files)
    }
}

// --- Helpers ---

fn symbol_kind_to_str(kind: &SymbolKind) -> &'static str {
    match kind {
        SymbolKind::Function => "Function",
        SymbolKind::Method => "Method",
        SymbolKind::Class => "Class",
        SymbolKind::Interface => "Interface",
        SymbolKind::TypeAlias => "TypeAlias",
        SymbolKind::Variable => "Variable",
        SymbolKind::Enum => "Enum",
    }
}

fn symbol_kind_from_str(s: &str) -> Option<SymbolKind> {
    match s {
        "Function" => Some(SymbolKind::Function),
        "Method" => Some(SymbolKind::Method),
        "Class" => Some(SymbolKind::Class),
        "Interface" => Some(SymbolKind::Interface),
        "TypeAlias" => Some(SymbolKind::TypeAlias),
        "Variable" => Some(SymbolKind::Variable),
        "Enum" => Some(SymbolKind::Enum),
        _ => None,
    }
}

fn edge_kind_to_str(kind: &EdgeKind) -> &'static str {
    match kind {
        EdgeKind::Call => "Call",
        EdgeKind::Reference => "Reference",
        EdgeKind::Implementation => "Implementation",
        EdgeKind::Inheritance => "Inheritance",
    }
}

#[allow(dead_code)]
fn edge_kind_from_str(s: &str) -> Option<EdgeKind> {
    match s {
        "Call" => Some(EdgeKind::Call),
        "Reference" => Some(EdgeKind::Reference),
        "Implementation" => Some(EdgeKind::Implementation),
        "Inheritance" => Some(EdgeKind::Inheritance),
        _ => None,
    }
}

fn row_to_symbol(row: &rusqlite::Row) -> rusqlite::Result<Symbol> {
    let kind_str: String = row.get(2)?;
    let kind = symbol_kind_from_str(&kind_str).unwrap_or(SymbolKind::Variable);
    Ok(Symbol {
        id: row.get::<_, i64>(0)? as u64,
        name: row.get(1)?,
        kind,
        file_path: row.get(3)?,
        line: row.get(4)?,
        column: row.get(5)?,
        signature: row.get(6)?,
        docstring: row.get(7)?,
    })
}

// --- Tests ---

#[cfg(test)]
mod tests {
    use super::*;
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

    // --- helpers ---

    fn create_store() -> Store {
        let conn = Connection::open_in_memory().unwrap();
        Store::init_schema(&conn).unwrap();
        Store {
            conn: Arc::new(Mutex::new(conn)),
        }
    }
}
