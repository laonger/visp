use rusqlite::Connection;
use std::path::Path;
use std::sync::{Arc, Mutex};

use crate::graph::{Edge, EdgeKind, Symbol, SymbolKind};

#[derive(Debug, Clone)]
pub struct ScoredSymbol {
    pub symbol: Symbol,
    pub score: f64,
}

/// Sanitize a user query string into a safe FTS5 query.
/// Escapes special chars, removes boolean operators, and uses prefix matching.
pub fn sanitize_fts_query(query: &str) -> String {
    query
        .replace("::", " ")
        .replace(['\'', '"', '*', '(', ')', ':', '^'], "")
        .split_whitespace()
        .filter(|t| !["AND", "OR", "NOT", "NEAR"].contains(&t.to_uppercase().as_str()))
        .map(|t| format!("\"{}\"*", t))
        .collect::<Vec<_>>()
        .join(" OR ")
}

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

    /// Search using FTS5 BM25 scoring. fts_query should already be sanitized.
    pub fn search_fts(&self, fts_query: &str, limit: usize) -> rusqlite::Result<Vec<ScoredSymbol>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT symbols.*, bm25(symbols_fts, 0, 20, 0, 5, 1) as score
             FROM symbols_fts
             JOIN symbols ON symbols_fts.rowid = symbols.rowid
             WHERE symbols_fts MATCH ?1
             ORDER BY score DESC
             LIMIT ?2",
        )?;
        let rows = stmt.query_map(rusqlite::params![fts_query, limit as i64], |row| {
            let sym = row_to_symbol(row)?;
            let score: f64 = row.get(8)?;
            Ok(ScoredSymbol { symbol: sym, score })
        })?;
        let mut results = Vec::new();
        for row in rows {
            results.push(row?);
        }
        Ok(results)
    }

    /// LIKE-based substring search (fallback when FTS5 returns too few results).
    pub fn search_like(&self, query: &str, limit: usize) -> rusqlite::Result<Vec<Symbol>> {
        let conn = self.conn.lock().unwrap();
        let escaped = query
            .replace('\\', "\\\\")
            .replace('_', "\\_")
            .replace('%', "\\%");
        let pattern = format!("%{}%", escaped);
        let mut stmt = conn.prepare(
            "SELECT id, name, kind, file_path, line, column, signature, docstring
             FROM symbols
             WHERE LOWER(name) LIKE LOWER(?1) ESCAPE '\\'
                OR LOWER(signature) LIKE LOWER(?1) ESCAPE '\\'
             LIMIT ?2",
        )?;
        let rows = stmt.query_map(rusqlite::params![pattern, limit as i64], row_to_symbol)?;
        let mut symbols = Vec::new();
        for row in rows {
            symbols.push(row?);
        }
        Ok(symbols)
    }

    /// Back-fill FTS5 index with data from the symbols table (for existing DBs).
    pub fn backfill_fts(&self) -> rusqlite::Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute_batch(
            "INSERT OR IGNORE INTO symbols_fts(rowid, id, name, kind, signature, docstring)
             SELECT rowid, id, name, kind, signature, docstring FROM symbols;",
        )?;
        Ok(())
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
#[path = "store_tests.rs"]
mod store_tests;
