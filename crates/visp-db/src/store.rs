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
        let expanded = if let Some(rest) = path.strip_prefix("~/") {
            let home = visp_core::session::home_dir()
                .ok_or_else(|| SessionError::Other("HOME not set".into()))?;
            home.join(rest)
        } else {
            std::path::PathBuf::from(path)
        };

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
        SessionRepo::get(&conn, session_id)
            .map_err(|e| SessionError::Other(format!("Get session failed: {e}")))?
            .ok_or_else(|| SessionError::NotFound(session_id.to_string()))
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;
    use std::path::Path;
    use visp_core::provider::LlmConfig;
    use visp_core::session::{Session, SessionStatus};

    fn setup() -> SqliteSessionStore {
        SqliteSessionStore::open_in_memory().unwrap()
    }

    fn sample_session(id: &str) -> Session {
        Session {
            id: id.to_string(),
            project_path: "/tmp".into(),
            status: SessionStatus::Idle,
            created_at: std::time::Instant::now(),
            created_at_unix: Some(1700000000000),
            history: vec![],
            config: LlmConfig::default(),
            system_prompt_template: "default".into(),
            approved_tools: HashSet::new(),
        }
    }

    #[test]
    fn test_store_create_and_get() {
        let mut store = setup();
        let session = sample_session("s1");
        store.create(session.clone()).unwrap();

        let got = store.get("s1").unwrap();
        assert_eq!(got.id, "s1");
        assert_eq!(got.project_path, Path::new("/tmp"));
        assert!(got.history.is_empty());
    }

    #[test]
    fn test_store_get_not_found() {
        let store = setup();
        let err = store.get("nonexistent").unwrap_err();
        assert!(matches!(err, SessionError::NotFound(_)));
    }

    #[test]
    fn test_store_list() {
        let mut store = setup();
        store.create(sample_session("a")).unwrap();
        store.create(sample_session("b")).unwrap();

        let list = store.list().unwrap();
        assert_eq!(list.len(), 2);
    }

    #[test]
    fn test_store_update() {
        let mut store = setup();
        store.create(sample_session("u1")).unwrap();

        let mut updated = sample_session("u1");
        updated.system_prompt_template = "changed".into();
        store.update(updated).unwrap();

        let got = store.get("u1").unwrap();
        assert_eq!(got.system_prompt_template, "changed");
    }

    #[test]
    fn test_store_delete() {
        let mut store = setup();
        store.create(sample_session("d1")).unwrap();
        assert!(store.get("d1").is_ok());

        store.delete("d1").unwrap();
        assert!(store.get("d1").is_err());
    }

    #[test]
    fn test_store_append_and_get_messages() {
        let mut store = setup();
        store.create(sample_session("m1")).unwrap();

        store.append_message("m1", Message::user("hello")).unwrap();
        store
            .append_message("m1", Message::assistant("world"))
            .unwrap();

        let messages = store.get_messages("m1").unwrap();
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].content, "hello");
        assert_eq!(messages[1].content, "world");
    }

    #[test]
    fn test_store_list_by_project() {
        let mut store = setup();

        let mut s1 = sample_session("p1");
        s1.project_path = "/proj/x".into();
        store.create(s1).unwrap();

        let mut s2 = sample_session("p2");
        s2.project_path = "/proj/x".into();
        store.create(s2).unwrap();

        let mut s3 = sample_session("p3");
        s3.project_path = "/proj/y".into();
        store.create(s3).unwrap();

        let list = store.list_by_project("/proj/x").unwrap();
        assert_eq!(list.len(), 2);

        let list_y = store.list_by_project("/proj/y").unwrap();
        assert_eq!(list_y.len(), 1);
    }

    #[test]
    fn test_store_create_already_exists() {
        let mut store = setup();
        store.create(sample_session("dup")).unwrap();
        let err = store.create(sample_session("dup")).unwrap_err();
        assert!(matches!(err, SessionError::AlreadyExists(_)));
    }

    #[test]
    fn test_store_delete_cascade() {
        let mut store = setup();
        store.create(sample_session("c1")).unwrap();

        // Append a message
        store
            .append_message("c1", Message::user("cascade test"))
            .unwrap();

        // Delete session — message should cascade
        store.delete("c1").unwrap();

        // Verify messages are gone
        let messages = store.get_messages("c1").unwrap();
        assert!(messages.is_empty());
    }
}
