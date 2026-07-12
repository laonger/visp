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

    /// List child sessions by parent_id.
    pub fn list_child_sessions(conn: &Connection, parent_id: &str) -> Result<Vec<Session>> {
        let mut stmt = conn.prepare(
            "SELECT id, project_path, status, model, system_prompt_template, config_json, approved_tools, agent_name, parent_id, permission_json, created_at
             FROM session WHERE parent_id = ?1 AND id != ?1 ORDER BY created_at ASC",
        )?;

        let mut sessions = stmt
            .query_map(params![parent_id], Self::row_to_session)?
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
#[path = "session_repo_tests.rs"]
mod tests;
