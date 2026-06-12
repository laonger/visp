use rusqlite::{Connection, Result, params};
use visp_core::message::{Message, MessageType, Role};

/// Message table DAO (Data Access Object).
/// All methods receive a `&rusqlite::Connection` and operate on the `message` table.
pub struct MessageRepo;

impl MessageRepo {
    /// Insert a new message row. Returns the auto-generated id.
    pub fn insert(conn: &Connection, session_id: &str, msg: &Message) -> Result<i64> {
        let now = msg
            .created_at
            .unwrap_or_else(|| chrono::Utc::now().timestamp_millis());
        let role_str = match msg.role {
            Role::System => "system",
            Role::User => "user",
            Role::Assistant => "assistant",
            Role::Tool => "tool",
        };
        let type_str = match msg.kind {
            MessageType::Text => "text",
            MessageType::Thinking => "thinking",
            MessageType::ToolCall => "tool_call",
            MessageType::ToolResult => "tool_result",
            MessageType::Error => "error",
            MessageType::Status => "status",
            MessageType::System => "system",
            MessageType::User => "user",
        };

        // tool_name and tool_arguments from the first tool_call if present
        let (tool_name, tool_arguments) = if let Some(ref calls) = msg.tool_calls {
            if let Some(first) = calls.first() {
                (Some(first.name.clone()), Some(first.arguments.clone()))
            } else {
                (None, None)
            }
        } else {
            (None, None)
        };

        let extra_blocks = msg
            .extra_blocks
            .as_ref()
            .map(|v| serde_json::to_string(v).unwrap_or_default());
        let provider_metadata = msg
            .provider_metadata
            .as_ref()
            .map(|v| serde_json::to_string(v).unwrap_or_default());

        conn.execute(
            "INSERT INTO message (session_id, role, type, content, tool_call_id, tool_name, tool_arguments, tool_result_is_error, tool_result_duration_ms, estimated_tokens, extra_blocks, provider_metadata, actual_tokens_input, actual_tokens_output, actual_cache_read, actual_cache_write, actual_cost, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18)",
            params![
                session_id,
                role_str,
                type_str,
                msg.content,
                msg.tool_call_id,
                tool_name,
                tool_arguments,
                msg.tool_result_is_error.map(|v| v as i64),
                msg.tool_result_duration_ms.map(|v| v as i64),
                msg.estimated_tokens,
                extra_blocks,
                provider_metadata,
                msg.actual_tokens_input,
                msg.actual_tokens_output,
                msg.actual_cache_read,
                msg.actual_cache_write,
                msg.actual_cost,
                now,
            ],
        )?;
        Ok(conn.last_insert_rowid())
    }

    /// Get all messages for a session, ordered by id (insertion order).
    pub fn get_by_session(conn: &Connection, session_id: &str) -> Result<Vec<Message>> {
        let mut stmt = conn.prepare(
            "SELECT id, role, type, content, tool_call_id, tool_name, tool_arguments, tool_result_is_error, tool_result_duration_ms, estimated_tokens, extra_blocks, provider_metadata, actual_tokens_input, actual_tokens_output, actual_cache_read, actual_cache_write, actual_cost, created_at
             FROM message WHERE session_id = ?1 ORDER BY id ASC",
        )?;

        let messages = stmt
            .query_map(params![session_id], |row| {
                let _id: i64 = row.get(0)?;
                let role_str: String = row.get(1)?;
                let type_str: String = row.get(2)?;
                let content: String = row.get(3)?;
                let tool_call_id: Option<String> = row.get(4)?;
                let tool_name: Option<String> = row.get(5)?;
                let tool_arguments: Option<String> = row.get(6)?;
                let tool_result_is_error: Option<i64> = row.get(7)?;
                let tool_result_duration_ms: Option<i64> = row.get(8)?;
                let estimated_tokens: u32 = row.get(9)?;
                let extra_blocks_str: Option<String> = row.get(10)?;
                let provider_metadata_str: Option<String> = row.get(11)?;
                let actual_tokens_input: Option<u32> = row.get(12)?;
                let actual_tokens_output: Option<u32> = row.get(13)?;
                let actual_cache_read: Option<u32> = row.get(14)?;
                let actual_cache_write: Option<u32> = row.get(15)?;
                let actual_cost: Option<f64> = row.get(16)?;
                let created_at: Option<i64> = row.get(17)?;

                let role = match role_str.as_str() {
                    "user" => Role::User,
                    "assistant" => Role::Assistant,
                    "tool" => Role::Tool,
                    _ => Role::System,
                };

                let kind = match type_str.as_str() {
                    "thinking" => MessageType::Thinking,
                    "tool_call" => MessageType::ToolCall,
                    "tool_result" => MessageType::ToolResult,
                    "error" => MessageType::Error,
                    "status" => MessageType::Status,
                    "system" => MessageType::System,
                    "user" => MessageType::User,
                    _ => MessageType::Text,
                };

                let extra_blocks = extra_blocks_str
                    .and_then(|s| serde_json::from_str::<Vec<serde_json::Value>>(&s).ok());

                let provider_metadata = provider_metadata_str
                    .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok());

                // Reconstruct tool_calls from tool_name + tool_arguments if type is tool_call
                let tool_calls = if kind == MessageType::ToolCall {
                    if let (Some(name), Some(args)) = (&tool_name, &tool_arguments) {
                        Some(vec![visp_core::message::ToolCallRequest {
                            id: tool_call_id.clone().unwrap_or_default(),
                            name: name.clone(),
                            arguments: args.clone(),
                        }])
                    } else {
                        None
                    }
                } else {
                    None
                };

                Ok(Message {
                    role,
                    kind,
                    content,
                    tool_call_id,
                    tool_calls,
                    extra_blocks,
                    skip_context: false,
                    estimated_tokens,
                    actual_tokens_input,
                    actual_tokens_output,
                    actual_cache_read,
                    actual_cache_write,
                    actual_cost,
                    provider_metadata,
                    tool_result_is_error: tool_result_is_error.map(|v| v != 0),
                    tool_result_duration_ms: tool_result_duration_ms.map(|v| v as u64),
                    created_at,
                })
            })?
            .collect::<Result<Vec<_>>>()?;

        Ok(messages)
    }

    /// Delete all messages for a session.
    pub fn delete_by_session(conn: &Connection, session_id: &str) -> Result<usize> {
        conn.execute(
            "DELETE FROM message WHERE session_id = ?1",
            params![session_id],
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::Migrator;
    use crate::session_repo::SessionRepo;
    use std::collections::HashSet;
    use visp_core::provider::LlmConfig;
    use visp_core::session::{Session, SessionStatus};

    fn setup() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        Migrator::run(&conn).unwrap();
        conn
    }

    fn insert_session(conn: &Connection, id: &str) {
        let session = Session {
            id: id.to_string(),
            project_path: "/tmp".into(),
            status: SessionStatus::Idle,
            created_at: std::time::Instant::now(),
            created_at_unix: Some(1700000000000),
            history: vec![],
            config: LlmConfig::default(),
            system_prompt_template: "default".into(),
            approved_tools: HashSet::new(),
        };
        SessionRepo::insert(conn, &session).unwrap();
    }

    #[test]
    fn test_insert_message() {
        let conn = setup();
        insert_session(&conn, "ses-msg-1");

        let msg = Message::user("hello");
        let id = MessageRepo::insert(&conn, "ses-msg-1", &msg).unwrap();
        assert!(id > 0);
    }

    #[test]
    fn test_get_messages() {
        let conn = setup();
        insert_session(&conn, "ses-msg-2");

        let msg1 = Message::user("hello");
        let msg2 = Message::assistant("world");
        MessageRepo::insert(&conn, "ses-msg-2", &msg1).unwrap();
        MessageRepo::insert(&conn, "ses-msg-2", &msg2).unwrap();

        let messages = MessageRepo::get_by_session(&conn, "ses-msg-2").unwrap();
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].content, "hello");
        assert_eq!(messages[1].content, "world");
    }

    #[test]
    fn test_get_messages_empty() {
        let conn = setup();
        insert_session(&conn, "ses-empty");
        let messages = MessageRepo::get_by_session(&conn, "ses-empty").unwrap();
        assert!(messages.is_empty());
    }

    #[test]
    fn test_get_messages_unknown_session() {
        let conn = setup();
        let messages = MessageRepo::get_by_session(&conn, "nonexistent").unwrap();
        assert!(messages.is_empty());
    }

    #[test]
    fn test_messages_ordered_by_id() {
        let conn = setup();
        insert_session(&conn, "ses-order");

        let msgs = vec![
            Message::user("first"),
            Message::assistant("second"),
            Message::user("third"),
        ];
        for msg in &msgs {
            MessageRepo::insert(&conn, "ses-order", msg).unwrap();
        }

        let loaded = MessageRepo::get_by_session(&conn, "ses-order").unwrap();
        assert_eq!(loaded.len(), 3);
        assert_eq!(loaded[0].content, "first");
        assert_eq!(loaded[1].content, "second");
        assert_eq!(loaded[2].content, "third");
    }

    #[test]
    fn test_message_type_preserved() {
        let conn = setup();
        insert_session(&conn, "ses-type");

        let msgs: Vec<Message> = vec![
            Message::user("user msg"),
            Message::system("system msg"),
            Message::assistant("assistant msg"),
            Message::tool("tool result", "call_1"),
            Message::thinking("thinking content"),
            Message::tool_call(vec![]),
            Message::error("error msg"),
            Message::status("status msg"),
        ];

        let kinds: Vec<MessageType> = msgs.iter().map(|m| m.kind.clone()).collect();

        for msg in &msgs {
            MessageRepo::insert(&conn, "ses-type", msg).unwrap();
        }

        let loaded = MessageRepo::get_by_session(&conn, "ses-type").unwrap();
        let loaded_kinds: Vec<MessageType> = loaded.iter().map(|m| m.kind.clone()).collect();

        assert_eq!(loaded_kinds, kinds);
    }

    #[test]
    fn test_delete_by_session() {
        let conn = setup();
        insert_session(&conn, "ses-del");

        MessageRepo::insert(&conn, "ses-del", &Message::user("m1")).unwrap();
        MessageRepo::insert(&conn, "ses-del", &Message::user("m2")).unwrap();
        assert_eq!(
            MessageRepo::get_by_session(&conn, "ses-del").unwrap().len(),
            2
        );

        MessageRepo::delete_by_session(&conn, "ses-del").unwrap();
        assert_eq!(
            MessageRepo::get_by_session(&conn, "ses-del").unwrap().len(),
            0
        );
    }
}
