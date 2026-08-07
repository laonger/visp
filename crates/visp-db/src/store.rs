use std::sync::Mutex;

use visp_core::error::SessionError;
use visp_core::message::Message;
use visp_core::session::{Session, SessionStore};

use crate::message_repo::MessageRepo;
use crate::schema::Migrator;
use crate::session_repo::SessionRepo;

/// SQLite-backed session store implementing `SessionStore` trait.
pub struct SqliteSessionStore {
    conn: Mutex<rusqlite::Connection>,
}

impl SqliteSessionStore {
    /// Open (or create) a SQLite database at the given path, run migrations.
    pub fn open(path: &str) -> Result<Self, SessionError> {
        let expanded = visp_config::path::expand_home(path);

        // Ensure parent directory exists
        if let Some(parent) = expanded.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| SessionError::Other(format!("Failed to create db dir: {e}")))?;
        }

        let conn = rusqlite::Connection::open(&expanded)
            .map_err(|e| SessionError::Other(format!("Failed to open db: {e}")))?;

        // Set restrictive permissions on new DB file
        #[cfg(unix)]
        {
            use std::fs::set_permissions;
            use std::os::unix::fs::PermissionsExt;
            let _ = set_permissions(&expanded, std::fs::Permissions::from_mode(0o600));
        }

        // Run migration
        Migrator::run(&conn).map_err(|e| SessionError::Other(format!("Migration failed: {e}")))?;

        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    /// Open an in-memory database (for testing).
    pub fn open_in_memory() -> Result<Self, SessionError> {
        let conn = rusqlite::Connection::open_in_memory()
            .map_err(|e| SessionError::Other(format!("Failed to open in-memory db: {e}")))?;

        Migrator::run(&conn).map_err(|e| SessionError::Other(format!("Migration failed: {e}")))?;

        Ok(Self {
            conn: Mutex::new(conn),
        })
    }
}

impl SessionStore for SqliteSessionStore {
    fn create(&mut self, session: Session) -> Result<(), SessionError> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| SessionError::Other(format!("Lock error: {e}")))?;
        SessionRepo::insert(&conn, &session).map_err(|e| {
            // Map UNIQUE constraint violation to AlreadyExists
            if let rusqlite::Error::SqliteFailure(err, _) = &e
                && err.code == rusqlite::ErrorCode::ConstraintViolation
            {
                return SessionError::AlreadyExists(session.id);
            }
            SessionError::Other(format!("Insert session failed: {e}"))
        })
    }

    fn get(&self, session_id: &str) -> Result<Session, SessionError> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| SessionError::Other(format!("Lock error: {e}")))?;
        let mut session = SessionRepo::get(&conn, session_id)
            .map_err(|e| SessionError::Other(format!("Get session failed: {e}")))?
            .ok_or_else(|| SessionError::NotFound(session_id.to_string()))?;
        // Load messages from the message table to populate history
        session.history = MessageRepo::get_by_session(&conn, session_id)
            .map_err(|e| SessionError::Other(format!("Get messages failed: {e}")))?;
        Ok(session)
    }

    fn list(&self) -> Result<Vec<Session>, SessionError> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| SessionError::Other(format!("Lock error: {e}")))?;
        SessionRepo::list(&conn)
            .map_err(|e| SessionError::Other(format!("List sessions failed: {e}")))
    }

    fn delete(&mut self, session_id: &str) -> Result<(), SessionError> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| SessionError::Other(format!("Lock error: {e}")))?;
        // Delete cascade handled by DB foreign key
        SessionRepo::delete(&conn, session_id)
            .map_err(|e| SessionError::Other(format!("Delete session failed: {e}")))?;
        Ok(())
    }

    fn update(&mut self, session: Session) -> Result<(), SessionError> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| SessionError::Other(format!("Lock error: {e}")))?;
        SessionRepo::update(&conn, &session)
            .map_err(|e| SessionError::Other(format!("Update session failed: {e}")))?;
        Ok(())
    }

    fn get_messages(&self, session_id: &str) -> Result<Vec<Message>, SessionError> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| SessionError::Other(format!("Lock error: {e}")))?;
        MessageRepo::get_by_session(&conn, session_id)
            .map_err(|e| SessionError::Other(format!("Get messages failed: {e}")))
    }

    fn append_message(&mut self, session_id: &str, message: Message) -> Result<(), SessionError> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| SessionError::Other(format!("Lock error: {e}")))?;
        MessageRepo::insert(&conn, session_id, &message)
            .map_err(|e| SessionError::Other(format!("Append message failed: {e}")))?;
        Ok(())
    }

    fn list_by_project(&self, project_path: &str) -> Result<Vec<Session>, SessionError> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| SessionError::Other(format!("Lock error: {e}")))?;
        SessionRepo::list_by_project(&conn, project_path)
            .map_err(|e| SessionError::Other(format!("List by project failed: {e}")))
    }

    fn list_child_sessions(&self, parent_id: &str) -> Result<Vec<Session>, SessionError> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| SessionError::Other(format!("Lock error: {e}")))?;
        let mut sessions = SessionRepo::list_child_sessions(&conn, parent_id)
            .map_err(|e| SessionError::Other(format!("List child sessions failed: {e}")))?;
        // Load history for each session (required by replay_session_history)
        for session in &mut sessions {
            session.history = MessageRepo::get_by_session(&conn, &session.id)
                .map_err(|e| SessionError::Other(format!("Get messages failed: {e}")))?;
        }
        Ok(sessions)
    }
}

#[cfg(test)]
#[path = "store_tests.rs"]
mod tests;
