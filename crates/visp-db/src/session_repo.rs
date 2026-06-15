use rusqlite::{Connection, Result, params};
use visp_core::provider::LlmConfig;
use visp_core::session::{Session, SessionStatus};

use crate::message_repo::MessageRepo;

/// Session table DAO (Data Access Object).
/// All methods receive a `&rusqlite::Connection` and operate on the `session` table.
pub struct SessionRepo;

impl SessionRepo {
    /// Insert a new session row.
    pub fn insert(conn: &Connection, session: &Session) -> Result<()> {
        let now = chrono::Utc::now().timestamp_millis();
        let created_at_unix = session.created_at_unix.unwrap_or(now);
        let updated_at = now;
        let config_json = serde_json::to_string(&session.config).unwrap_or_default();
        let approved_tools: Vec<&str> = session.approved_tools.iter().map(|s| s.as_str()).collect();
        let approved_json = serde_json::to_string(&approved_tools).unwrap_or_default();
        let permission_json = serde_json::to_string(&session.permission).unwrap_or_default();
        let status = match session.status {
            SessionStatus::Idle => "idle",
            SessionStatus::Running => "running",
            SessionStatus::Completed => "completed",
            SessionStatus::Error => "error",
        };

        conn.execute(
            "INSERT INTO session (id, project_path, title, status, model, system_prompt_template, config_json, approved_tools, agent_name, parent_id, permission_json, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
            params![
                session.id,
                session.project_path.to_string_lossy().as_ref(),
                "",
                status,
                session.config.model,
                session.system_prompt_template,
                config_json,
                approved_json,
                session.agent_name,
                session.parent_id,
                permission_json,
                created_at_unix,
                updated_at,
            ],
        )?;
        Ok(())
    }

    /// Get a session by ID. Returns `None` if not found.
    /// The returned session has `history` empty.
    pub fn get(conn: &Connection, session_id: &str) -> Result<Option<Session>> {
        let mut stmt = conn.prepare(
            "SELECT id, project_path, status, model, system_prompt_template, config_json, approved_tools, agent_name, parent_id, permission_json, created_at, updated_at
             FROM session WHERE id = ?1",
        )?;

        let mut rows = stmt.query(params![session_id])?;
        match rows.next()? {
            Some(row) => {
                let session = Self::row_to_session(row)?;
                Ok(Some(session))
            }
            None => Ok(None),
        }
    }

    /// List all sessions. Returns sessions with empty history.
    pub fn list(conn: &Connection) -> Result<Vec<Session>> {
        let mut stmt = conn.prepare(
            "SELECT id, project_path, status, model, system_prompt_template, config_json, approved_tools, agent_name, parent_id, permission_json, created_at
             FROM session ORDER BY created_at DESC",
        )?;

        let mut sessions = stmt
            .query_map([], Self::row_to_session)?
            .collect::<Result<Vec<_>>>()?;

        for session in &mut sessions {
            session.last_user_message =
                MessageRepo::get_last_user_message(conn, &session.id).unwrap_or(None);
        }

        Ok(sessions)
    }

    /// List sessions filtered by project path.
    pub fn list_by_project(conn: &Connection, project_path: &str) -> Result<Vec<Session>> {
        let mut stmt = conn.prepare(
            "SELECT id, project_path, status, model, system_prompt_template, config_json, approved_tools, agent_name, parent_id, permission_json, created_at
             FROM session WHERE project_path = ?1 ORDER BY created_at DESC",
        )?;

        let mut sessions = stmt
            .query_map(params![project_path], Self::row_to_session)?
            .collect::<Result<Vec<_>>>()?;

        for session in &mut sessions {
            session.last_user_message =
                MessageRepo::get_last_user_message(conn, &session.id).unwrap_or(None);
        }

        Ok(sessions)
    }

    /// Update a session. Returns the number of rows affected.
    pub fn update(conn: &Connection, session: &Session) -> Result<usize> {
        let now = chrono::Utc::now().timestamp_millis();
        let config_json = serde_json::to_string(&session.config).unwrap_or_default();
        let approved_tools: Vec<&str> = session.approved_tools.iter().map(|s| s.as_str()).collect();
        let approved_json = serde_json::to_string(&approved_tools).unwrap_or_default();
        let permission_json = serde_json::to_string(&session.permission).unwrap_or_default();
        let status = match session.status {
            SessionStatus::Idle => "idle",
            SessionStatus::Running => "running",
            SessionStatus::Completed => "completed",
            SessionStatus::Error => "error",
        };

        conn.execute(
            "UPDATE session SET project_path = ?1, status = ?2, model = ?3, system_prompt_template = ?4, config_json = ?5, approved_tools = ?6, agent_name = ?7, parent_id = ?8, permission_json = ?9, updated_at = ?10 WHERE id = ?11",
            params![
                session.project_path.to_string_lossy().as_ref(),
                status,
                session.config.model,
                session.system_prompt_template,
                config_json,
                approved_json,
                session.agent_name,
                session.parent_id,
                permission_json,
                now,
                session.id,
            ],
        )
    }

    /// Delete a session by ID. Returns the number of rows affected.
    pub fn delete(conn: &Connection, session_id: &str) -> Result<usize> {
        conn.execute("DELETE FROM session WHERE id = ?1", params![session_id])
    }

    /// Convert a SQLite row into a `Session`.
    /// Expects columns in order: id, project_path, status, model, system_prompt_template,
    /// config_json, approved_tools, agent_name, parent_id, permission_json, created_at.
    fn row_to_session(row: &rusqlite::Row) -> Result<Session> {
        let id: String = row.get(0)?;
        let project_path: String = row.get(1)?;
        let status_str: String = row.get(2)?;
        let _model: String = row.get(3)?;
        let prompt: String = row.get(4)?;
        let config_json: String = row.get(5)?;
        let approved_json: String = row.get(6)?;
        let agent_name: String = row.get(7)?;
        let parent_id: Option<String> = row.get(8)?;
        let permission_json: String = row.get(9)?;
        let created_at_unix: i64 = row.get(10)?;

        let status = match status_str.as_str() {
            "running" => SessionStatus::Running,
            "completed" => SessionStatus::Completed,
            "error" => SessionStatus::Error,
            _ => SessionStatus::Idle,
        };

        let config: LlmConfig = serde_json::from_str(&config_json).unwrap_or_default();
        let approved_tools: Vec<String> = serde_json::from_str(&approved_json).unwrap_or_default();
        let permission: Vec<visp_core::agent_definition::PermissionRule> =
            serde_json::from_str(&permission_json).unwrap_or_default();

        Ok(Session {
            id,
            project_path: project_path.into(),
            status,
            created_at: std::time::Instant::now(),
            created_at_unix: Some(created_at_unix),
            history: vec![],
            last_user_message: None,
            config,
            system_prompt_template: prompt,
            approved_tools: approved_tools.into_iter().collect(),
            agent_name,
            parent_id,
            permission,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::Migrator;
    use std::collections::HashSet;

    fn setup() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        Migrator::run(&conn).unwrap();
        conn
    }

    fn sample_session(id: &str) -> Session {
        Session {
            id: id.to_string(),
            project_path: "/tmp".into(),
            status: SessionStatus::Idle,
            created_at: std::time::Instant::now(),
            created_at_unix: Some(1700000000000),
            history: vec![],
            last_user_message: None,
            config: LlmConfig::default(),
            system_prompt_template: "default".into(),
            approved_tools: HashSet::new(),
            agent_name: "default".into(),
            parent_id: None,
            permission: vec![],
        }
    }

    #[test]
    fn test_insert_session() {
        let conn = setup();
        let session = sample_session("ses-1");
        SessionRepo::insert(&conn, &session).unwrap();

        let got = SessionRepo::get(&conn, "ses-1").unwrap().unwrap();
        assert_eq!(got.id, "ses-1");
        assert_eq!(got.project_path, std::path::PathBuf::from("/tmp"));
    }

    #[test]
    fn test_get_session_found() {
        let conn = setup();
        let session = sample_session("ses-2");
        SessionRepo::insert(&conn, &session).unwrap();

        let got = SessionRepo::get(&conn, "ses-2").unwrap().unwrap();
        assert_eq!(got.id, "ses-2");
        // history is always empty when loaded from DB
        assert!(got.history.is_empty());
    }

    #[test]
    fn test_get_session_not_found() {
        let conn = setup();
        let got = SessionRepo::get(&conn, "nonexistent").unwrap();
        assert!(got.is_none());
    }

    #[test]
    fn test_list_sessions() {
        let conn = setup();
        SessionRepo::insert(&conn, &sample_session("ses-a")).unwrap();
        SessionRepo::insert(&conn, &sample_session("ses-b")).unwrap();

        let list = SessionRepo::list(&conn).unwrap();
        assert_eq!(list.len(), 2);
    }

    #[test]
    fn test_list_by_project() {
        let conn = setup();
        let mut s1 = sample_session("s1");
        s1.project_path = "/proj/a".into();
        let mut s2 = sample_session("s2");
        s2.project_path = "/proj/a".into();
        let mut s3 = sample_session("s3");
        s3.project_path = "/proj/b".into();

        SessionRepo::insert(&conn, &s1).unwrap();
        SessionRepo::insert(&conn, &s2).unwrap();
        SessionRepo::insert(&conn, &s3).unwrap();

        let list = SessionRepo::list_by_project(&conn, "/proj/a").unwrap();
        assert_eq!(list.len(), 2);

        let list_b = SessionRepo::list_by_project(&conn, "/proj/b").unwrap();
        assert_eq!(list_b.len(), 1);

        let list_empty = SessionRepo::list_by_project(&conn, "/nonexistent").unwrap();
        assert_eq!(list_empty.len(), 0);
    }

    #[test]
    fn test_update_session() {
        let conn = setup();
        let session = sample_session("ses-u");
        SessionRepo::insert(&conn, &session).unwrap();

        let mut updated = session.clone();
        updated.system_prompt_template = "updated prompt".into();
        SessionRepo::update(&conn, &updated).unwrap();

        let got = SessionRepo::get(&conn, "ses-u").unwrap().unwrap();
        assert_eq!(got.system_prompt_template, "updated prompt");
    }

    #[test]
    fn test_delete_session() {
        let conn = setup();
        SessionRepo::insert(&conn, &sample_session("ses-d")).unwrap();
        assert!(SessionRepo::get(&conn, "ses-d").unwrap().is_some());

        SessionRepo::delete(&conn, "ses-d").unwrap();
        assert!(SessionRepo::get(&conn, "ses-d").unwrap().is_none());
    }

    #[test]
    fn test_delete_session_cascade() {
        let conn = setup();
        SessionRepo::insert(&conn, &sample_session("ses-c")).unwrap();

        // Insert a message referencing this session
        conn.execute(
            "INSERT INTO message (session_id, role, type, content, created_at) VALUES (?1, 'user', 'user', 'hello', 1700000000000)",
            params!["ses-c"],
        ).unwrap();

        // Verify message exists
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM message WHERE session_id = ?1",
                params!["ses-c"],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 1);

        // Delete session (should cascade)
        SessionRepo::delete(&conn, "ses-c").unwrap();

        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM message WHERE session_id = ?1",
                params!["ses-c"],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn test_list_populates_last_user_message() {
        let conn = setup();
        SessionRepo::insert(&conn, &sample_session("ses-list-msg")).unwrap();

        // Insert messages, last one being a user message
        conn.execute(
            "INSERT INTO message (session_id, role, type, content, created_at) VALUES (?1, 'user', 'user', 'first question', 1700000000001)",
            params!["ses-list-msg"],
        ).unwrap();
        conn.execute(
            "INSERT INTO message (session_id, role, type, content, created_at) VALUES (?1, 'assistant', 'text', 'some answer', 1700000000002)",
            params!["ses-list-msg"],
        ).unwrap();
        conn.execute(
            "INSERT INTO message (session_id, role, type, content, created_at) VALUES (?1, 'user', 'user', 'second question', 1700000000003)",
            params!["ses-list-msg"],
        ).unwrap();

        let list = SessionRepo::list(&conn).unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(
            list[0].last_user_message,
            Some("second question".to_string())
        );
    }

    #[test]
    fn test_list_last_user_message_none_when_no_messages() {
        let conn = setup();
        SessionRepo::insert(&conn, &sample_session("ses-no-msg")).unwrap();

        let list = SessionRepo::list(&conn).unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].last_user_message, None);
    }

    #[test]
    fn test_list_by_project_populates_last_user_message() {
        let conn = setup();
        let mut s = sample_session("ses-proj-msg");
        s.project_path = "/my-project".into();
        SessionRepo::insert(&conn, &s).unwrap();

        conn.execute(
            "INSERT INTO message (session_id, role, type, content, created_at) VALUES (?1, 'user', 'user', 'project question', 1700000000000)",
            params!["ses-proj-msg"],
        ).unwrap();

        let list = SessionRepo::list_by_project(&conn, "/my-project").unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(
            list[0].last_user_message,
            Some("project question".to_string())
        );

        // Other project should have no sessions
        let other = SessionRepo::list_by_project(&conn, "/other").unwrap();
        assert!(other.is_empty());
    }
}
