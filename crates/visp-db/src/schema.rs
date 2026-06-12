use rusqlite::{Connection, Result};

/// Database schema version and migration logic.
pub struct Migrator;

impl Migrator {
    /// Current schema version (incremented on each migration).
    pub const VERSION: i64 = 1;

    /// SQL to create the session table.
    const CREATE_SESSION: &'static str = r#"
        CREATE TABLE IF NOT EXISTS session (
            id                TEXT PRIMARY KEY,
            project_path      TEXT NOT NULL,
            title             TEXT NOT NULL DEFAULT '',
            status            TEXT NOT NULL DEFAULT 'idle',
            model             TEXT NOT NULL DEFAULT '',
            system_prompt_template TEXT NOT NULL DEFAULT '',
            config_json       TEXT NOT NULL DEFAULT '{}',
            approved_tools    TEXT NOT NULL DEFAULT '[]',
            created_at        INTEGER NOT NULL,
            updated_at        INTEGER NOT NULL
        );
    "#;

    /// SQL to create the message table.
    const CREATE_MESSAGE: &'static str = r#"
        CREATE TABLE IF NOT EXISTS message (
            id                    INTEGER PRIMARY KEY AUTOINCREMENT,
            session_id            TEXT NOT NULL REFERENCES session(id) ON DELETE CASCADE,
            role                  TEXT NOT NULL,
            type                  TEXT NOT NULL,
            content               TEXT NOT NULL DEFAULT '',
            tool_call_id          TEXT,
            tool_name             TEXT,
            tool_arguments        TEXT,
            tool_result_is_error  INTEGER,
            tool_result_duration_ms INTEGER,
            estimated_tokens      INTEGER NOT NULL DEFAULT 0,
            extra_blocks          TEXT,
            provider_metadata     TEXT,
            actual_tokens_input   INTEGER,
            actual_tokens_output  INTEGER,
            actual_cache_read     INTEGER,
            actual_cache_write    INTEGER,
            actual_cost           REAL,
            created_at            INTEGER NOT NULL
        );
    "#;

    /// Indexes for performance.
    const INDEXES: &'static [&'static str] = &[
        "CREATE INDEX IF NOT EXISTS idx_message_session ON message(session_id, id);",
        "CREATE INDEX IF NOT EXISTS idx_message_session_role ON message(session_id, role);",
        "CREATE INDEX IF NOT EXISTS idx_message_tool_call ON message(tool_call_id);",
        "CREATE INDEX IF NOT EXISTS idx_session_project ON session(project_path, created_at);",
        "CREATE INDEX IF NOT EXISTS idx_session_updated ON session(updated_at);",
    ];

    /// Run all pending migrations.
    /// Safe to call multiple times — uses PRAGMA user_version for idempotency.
    pub fn run(conn: &Connection) -> Result<()> {
        let current_version: i64 =
            conn.pragma_query_value(None, "user_version", |row| row.get(0))?;

        if current_version >= Self::VERSION {
            return Ok(());
        }

        // PRAGMA configuration (must be outside transaction)
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;
             PRAGMA busy_timeout = 5000;
             PRAGMA foreign_keys = ON;
             PRAGMA cache_size = -64000;",
        )?;

        // Schema changes in a single transaction
        conn.execute_batch("BEGIN TRANSACTION;")?;

        // Create tables
        conn.execute_batch(Self::CREATE_SESSION)?;
        conn.execute_batch(Self::CREATE_MESSAGE)?;

        // Create indexes
        for idx in Self::INDEXES {
            conn.execute_batch(idx)?;
        }

        // Update version
        conn.pragma_update(None, "user_version", Self::VERSION)?;

        conn.execute_batch("COMMIT;")?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        Migrator::run(&conn).unwrap();
        conn
    }

    #[test]
    fn test_migrate_creates_tables() {
        let conn = setup_db();

        // Verify session table exists
        let tables: Vec<String> = conn
            .prepare("SELECT name FROM sqlite_master WHERE type='table' AND name IN ('session', 'message') ORDER BY name")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(tables, vec!["message", "session"]);
    }

    #[test]
    fn test_migrate_creates_indexes() {
        let conn = setup_db();

        let indexes: Vec<String> = conn
            .prepare("SELECT name FROM sqlite_master WHERE type='index' AND name LIKE 'idx_%' ORDER BY name")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        let expected: Vec<&str> = vec![
            "idx_message_session",
            "idx_message_session_role",
            "idx_message_tool_call",
            "idx_session_project",
            "idx_session_updated",
        ];
        assert_eq!(indexes, expected);
    }

    #[test]
    fn test_migrate_idempotent() {
        let conn = setup_db();
        // Second run should not error
        Migrator::run(&conn).unwrap();
        // Verify still has the expected tables (ignore sqlite_* system tables)
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 2);
    }

    #[test]
    fn test_migrate_version() {
        let conn = setup_db();
        let version: i64 = conn
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
        assert_eq!(version, 1);
    }

    #[test]
    fn test_migrate_pragma() {
        let conn = Connection::open_in_memory().unwrap();
        Migrator::run(&conn).unwrap();

        // journal_mode should be WAL (in-memory may differ, so just check no error)
        let journal_mode: String = conn
            .pragma_query_value(None, "journal_mode", |row| row.get(0))
            .unwrap();
        // In-memory databases may not support WAL, so just verify it ran
        // synchronous
        let sync: i64 = conn
            .pragma_query_value(None, "synchronous", |row| row.get(0))
            .unwrap();
        assert_eq!(sync, 1); // NORMAL = 1

        // foreign_keys
        let fk: bool = conn
            .pragma_query_value(None, "foreign_keys", |row| row.get(0))
            .unwrap();
        assert!(fk);
    }
}
