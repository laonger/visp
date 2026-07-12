use rusqlite::{Connection, Result};

/// Database schema version and migration logic.
pub struct Migrator;

impl Migrator {
    /// Current schema version (incremented on each migration).
    pub const VERSION: i64 = 5;

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
            agent_name        TEXT NOT NULL DEFAULT 'default',
            parent_id         TEXT,
            permission_json   TEXT NOT NULL DEFAULT '[]',
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
            tool_calls_json       TEXT,
            tool_result_is_error  INTEGER,
            tool_result_duration_ms INTEGER,
            tool_call_count       INTEGER NOT NULL DEFAULT 0,
            estimated_tokens      INTEGER NOT NULL DEFAULT 0,
            extra_blocks          TEXT,
            provider_metadata     TEXT,
            actual_tokens_input   INTEGER,
            actual_tokens_output  INTEGER,
            actual_cache_read     INTEGER,
            actual_cache_write    INTEGER,
            actual_cost           REAL,
            skip_context          INTEGER NOT NULL DEFAULT 0,
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
        "CREATE INDEX IF NOT EXISTS idx_session_parent_id ON session(parent_id);",
    ];

    /// Run all pending migrations.
    /// Safe to call multiple times — uses PRAGMA user_version for idempotency.
    pub fn run(conn: &Connection) -> Result<()> {
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

        // Ensure all columns exist (idempotent via pragma_table_info checks).
        // Runs unconditionally — not gated on current_version — to self-heal
        // databases whose version was bumped ahead of the actual schema.

        // v1→v2: tool_calls_json
        {
            let has_column: bool = conn
                .prepare(
                    "SELECT COUNT(*) FROM pragma_table_info('message') WHERE name='tool_calls_json'",
                )
                .and_then(|mut s| s.query_row([], |row| row.get::<_, i64>(0)))
                .map(|c| c > 0)
                .unwrap_or(false);
            if !has_column {
                conn.execute_batch("ALTER TABLE message ADD COLUMN tool_calls_json TEXT;")?;
            }
        }

        // v2→v3: skip_context
        {
            let has_column: bool = conn
                .prepare(
                    "SELECT COUNT(*) FROM pragma_table_info('message') WHERE name='skip_context'",
                )
                .and_then(|mut s| s.query_row([], |row| row.get::<_, i64>(0)))
                .map(|c| c > 0)
                .unwrap_or(false);
            if !has_column {
                conn.execute_batch(
                    "ALTER TABLE message ADD COLUMN skip_context INTEGER NOT NULL DEFAULT 0;",
                )?;
            }
        }

        // v3→v4: tool_call_count (message table)
        {
            let has_column: bool = conn
                .prepare(
                    "SELECT COUNT(*) FROM pragma_table_info('message') WHERE name='tool_call_count'",
                )
                .and_then(|mut s| s.query_row([], |row| row.get::<_, i64>(0)))
                .map(|c| c > 0)
                .unwrap_or(false);
            if !has_column {
                conn.execute_batch(
                    "ALTER TABLE message ADD COLUMN tool_call_count INTEGER NOT NULL DEFAULT 0;",
                )?;
            }
        }

        // v3→v4: multi-agent columns (session table)
        {
            for (col, def) in &[
                ("agent_name", "TEXT NOT NULL DEFAULT 'default'"),
                ("parent_id", "TEXT"),
                ("permission_json", "TEXT NOT NULL DEFAULT '[]'"),
            ] {
                let has_column: bool = conn
                    .prepare(&format!(
                        "SELECT COUNT(*) FROM pragma_table_info('session') WHERE name='{col}'"
                    ))
                    .and_then(|mut s| s.query_row([], |row| row.get::<_, i64>(0)))
                    .map(|c| c > 0)
                    .unwrap_or(false);
                if !has_column {
                    conn.execute_batch(&format!("ALTER TABLE session ADD COLUMN {col} {def};"))?;
                }
            }
        }

        // Create indexes (after all columns exist from version-specific migrations)
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
#[path = "schema_tests.rs"]
mod tests;
