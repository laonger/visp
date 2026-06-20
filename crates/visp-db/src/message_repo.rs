use rusqlite::{Connection, OptionalExtension, Result, params};
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

        // Full tool_calls serialized as JSON (preserves all calls including their IDs)
        let tool_calls_json = msg
            .tool_calls
            .as_ref()
            .map(|calls| serde_json::to_string(calls).unwrap_or_default());

        let extra_blocks = msg
            .extra_blocks
            .as_ref()
            .map(|v| serde_json::to_string(v).unwrap_or_default());
        let provider_metadata = msg
            .provider_metadata
            .as_ref()
            .map(|v| serde_json::to_string(v).unwrap_or_default());

        conn.execute(
            "INSERT INTO message (session_id, role, type, content, tool_call_id, tool_name, tool_arguments, tool_calls_json, tool_result_is_error, tool_result_duration_ms, tool_call_count, estimated_tokens, extra_blocks, provider_metadata, actual_tokens_input, actual_tokens_output, actual_cache_read, actual_cache_write, actual_cost, skip_context, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21)",
            params![
                session_id,
                role_str,
                type_str,
                msg.content,
                msg.tool_call_id,
                tool_name,
                tool_arguments,
                tool_calls_json,
                msg.tool_result_is_error.map(|v| v as i64),
                msg.tool_result_duration_ms.map(|v| v as i64),
                msg.tool_calls.as_ref().map(|c| c.len() as i64).unwrap_or(0),
                msg.estimated_tokens,
                extra_blocks,
                provider_metadata,
                msg.actual_tokens_input,
                msg.actual_tokens_output,
                msg.actual_cache_read,
                msg.actual_cache_write,
                msg.actual_cost,
                msg.skip_context as i64,
                now,
            ],
        )?;
        Ok(conn.last_insert_rowid())
    }

    /// Get all messages for a session, ordered by id (insertion order).
    pub fn get_by_session(conn: &Connection, session_id: &str) -> Result<Vec<Message>> {
        let mut stmt = conn.prepare(
            "SELECT id, role, type, content, tool_call_id, tool_name, tool_arguments, tool_calls_json, tool_result_is_error, tool_result_duration_ms, tool_call_count, estimated_tokens, extra_blocks, provider_metadata, actual_tokens_input, actual_tokens_output, actual_cache_read, actual_cache_write, actual_cost, skip_context, created_at
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
                let tool_calls_json: Option<String> = row.get(7)?;
                let tool_result_is_error: Option<i64> = row.get(8)?;
                let tool_result_duration_ms: Option<i64> = row.get(9)?;
                let tool_call_count: i64 = row.get(10)?;
                let estimated_tokens: u32 = row.get(11)?;
                let extra_blocks_str: Option<String> = row.get(12)?;
                let provider_metadata_str: Option<String> = row.get(13)?;
                let actual_tokens_input: Option<u32> = row.get(14)?;
                let actual_tokens_output: Option<u32> = row.get(15)?;
                let actual_cache_read: Option<u32> = row.get(16)?;
                let actual_cache_write: Option<u32> = row.get(17)?;
                let actual_cost: Option<f64> = row.get(18)?;
                let skip_context_int: i64 = row.get(19)?;
                let created_at: Option<i64> = row.get(20)?;

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

                // Reconstruct tool_calls: prefer tool_calls_json if present (v2+),
                // fall back to tool_name + tool_arguments (v1 compat)
                let tool_calls = if kind == MessageType::ToolCall {
                    if let Some(ref json) = tool_calls_json {
                        // v2+: deserialize from JSON (preserves all calls & their IDs)
                        serde_json::from_str(json).ok()
                    } else if let (Some(name), Some(args)) = (&tool_name, &tool_arguments) {
                        // v1 fallback: reconstruct from tool_name + tool_arguments
                        // v1 data has empty tool_call_id, which is a known bug.
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
                    skip_context: skip_context_int != 0,
                    estimated_tokens,
                    actual_tokens_input,
                    actual_tokens_output,
                    actual_cache_read,
                    actual_cache_write,
                    actual_cost,
                    provider_metadata,
                    tool_result_is_error: tool_result_is_error.map(|v| v != 0),
                    tool_result_duration_ms: tool_result_duration_ms.map(|v| v as u64),
                    tool_call_count: Some(tool_call_count as u32).filter(|&c| c > 0),
                    created_at,
                })
            })?
            .collect::<Result<Vec<_>>>()?;

        Ok(messages)
    }

    /// 获取某个 session 的最后一条用户消息内容（截断到 80 字符）
    pub fn get_last_user_message(conn: &Connection, session_id: &str) -> Result<Option<String>> {
        let mut stmt = conn.prepare(
            "SELECT content FROM message WHERE session_id = ?1 AND role = 'user' ORDER BY id DESC LIMIT 1",
        )?;
        let result: Option<String> = stmt
            .query_row(params![session_id], |row| row.get(0))
            .optional()?;
        Ok(result.map(|s| {
            if s.chars().count() > 80 {
                format!("{}...", s.chars().take(80).collect::<String>())
            } else {
                s
            }
        }))
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
            last_user_message: None,
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

    #[test]
    fn test_tool_calls_json_roundtrip() {
        let conn = setup();
        insert_session(&conn, "ses-tcj-1");

        let msg = Message {
            role: Role::Assistant,
            kind: MessageType::ToolCall,
            content: String::new(),
            tool_call_id: None,
            tool_calls: Some(vec![
                visp_core::message::ToolCallRequest {
                    id: "call_abc123".to_string(),
                    name: "search".to_string(),
                    arguments: r#"{"query":"test"}"#.to_string(),
                },
                visp_core::message::ToolCallRequest {
                    id: "call_def456".to_string(),
                    name: "read_file".to_string(),
                    arguments: r#"{"path":"/tmp/test"}"#.to_string(),
                },
            ]),
            tool_call_count: None,
            extra_blocks: None,
            skip_context: false,
            estimated_tokens: 0,
            actual_tokens_input: None,
            actual_tokens_output: None,
            actual_cache_read: None,
            actual_cache_write: None,
            actual_cost: None,
            provider_metadata: None,
            tool_result_is_error: None,
            tool_result_duration_ms: None,
            created_at: None,
        };

        MessageRepo::insert(&conn, "ses-tcj-1", &msg).unwrap();
        let loaded = MessageRepo::get_by_session(&conn, "ses-tcj-1").unwrap();
        assert_eq!(loaded.len(), 1);

        let restored = &loaded[0];
        let calls = restored.tool_calls.as_ref().unwrap();
        assert_eq!(calls.len(), 2, "should restore both tool_calls");
        assert_eq!(calls[0].id, "call_abc123");
        assert_eq!(calls[0].name, "search");
        assert_eq!(calls[0].arguments, r#"{"query":"test"}"#);
        assert_eq!(calls[1].id, "call_def456");
        assert_eq!(calls[1].name, "read_file");
        assert_eq!(calls[1].arguments, r#"{"path":"/tmp/test"}"#);
    }

    #[test]
    fn test_tool_calls_json_empty_calls() {
        let conn = setup();
        insert_session(&conn, "ses-tcj-2");

        // Message with kind=ToolCall but no tool_calls
        let msg = Message {
            role: Role::Assistant,
            kind: MessageType::ToolCall,
            content: "text".to_string(),
            tool_call_id: None,
            tool_calls: None,
            tool_call_count: None,
            extra_blocks: None,
            skip_context: false,
            estimated_tokens: 0,
            actual_tokens_input: None,
            actual_tokens_output: None,
            actual_cache_read: None,
            actual_cache_write: None,
            actual_cost: None,
            provider_metadata: None,
            tool_result_is_error: None,
            tool_result_duration_ms: None,
            created_at: None,
        };

        MessageRepo::insert(&conn, "ses-tcj-2", &msg).unwrap();
        let loaded = MessageRepo::get_by_session(&conn, "ses-tcj-2").unwrap();
        assert!(loaded[0].tool_calls.is_none());
    }

    #[test]
    fn test_tool_calls_json_v1_fallback() {
        let conn = Connection::open_in_memory().unwrap();
        Migrator::run(&conn).unwrap();
        insert_session(&conn, "ses-tcj-3");

        // Insert manually using raw SQL to simulate v1 data (no tool_calls_json)
        conn.execute(
            "INSERT INTO message (session_id, role, type, content, tool_call_id, tool_name, tool_arguments, tool_calls_json, tool_result_is_error, tool_result_duration_ms, estimated_tokens, extra_blocks, provider_metadata, actual_tokens_input, actual_tokens_output, actual_cache_read, actual_cache_write, actual_cost, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, NULL, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18)",
            rusqlite::params![
                "ses-tcj-3",
                "assistant",
                "tool_call",
                "",
                Option::<String>::None, // tool_call_id
                "search",
                r#"{"query":"test"}"#,
                Option::<i64>::None, // tool_result_is_error
                Option::<i64>::None, // tool_result_duration_ms
                0_i64,              // estimated_tokens
                Option::<String>::None, // extra_blocks
                Option::<String>::None, // provider_metadata
                Option::<i64>::None, // actual_tokens_input
                Option::<i64>::None, // actual_tokens_output
                Option::<i64>::None, // actual_cache_read
                Option::<i64>::None, // actual_cache_write
                Option::<f64>::None, // actual_cost
                1700000000000_i64,   // created_at
            ],
        )
        .unwrap();

        // Load via normal API (should fall back to v1 logic)
        let loaded = MessageRepo::get_by_session(&conn, "ses-tcj-3").unwrap();
        assert_eq!(loaded.len(), 1);
        let calls = loaded[0].tool_calls.as_ref().unwrap();
        assert_eq!(calls.len(), 1, "v1 fallback should reconstruct single call");
        assert_eq!(calls[0].name, "search");
        assert_eq!(calls[0].arguments, r#"{"query":"test"}"#);
        // v1 data has empty tool_call_id
        assert_eq!(calls[0].id, "");
    }

    #[test]
    fn test_get_last_user_message() {
        let conn = setup();
        insert_session(&conn, "ses-last-msg");

        // 插入一些消息
        MessageRepo::insert(&conn, "ses-last-msg", &Message::user("hello")).unwrap();
        MessageRepo::insert(&conn, "ses-last-msg", &Message::assistant("hi there")).unwrap();
        MessageRepo::insert(&conn, "ses-last-msg", &Message::user("second question")).unwrap();

        let last = MessageRepo::get_last_user_message(&conn, "ses-last-msg").unwrap();
        assert_eq!(last, Some("second question".to_string()));
    }

    #[test]
    fn test_get_last_user_message_empty() {
        let conn = setup();
        insert_session(&conn, "ses-empty-msg");

        let last = MessageRepo::get_last_user_message(&conn, "ses-empty-msg").unwrap();
        assert_eq!(last, None);
    }

    #[test]
    fn test_get_last_user_message_truncated() {
        let conn = setup();
        insert_session(&conn, "ses-long-msg");

        let long_msg = "a".repeat(100);
        MessageRepo::insert(&conn, "ses-long-msg", &Message::user(&long_msg)).unwrap();

        let last = MessageRepo::get_last_user_message(&conn, "ses-long-msg").unwrap();
        let expected = format!("{}...", "a".repeat(80));
        assert_eq!(last, Some(expected));
    }

    #[test]
    fn test_skip_context_roundtrip() {
        let conn = setup();
        insert_session(&conn, "ses-sk-1");

        // Insert a message with skip_context: true
        let skip_msg = Message {
            skip_context: true,
            ..Message::user("skip me")
        };
        MessageRepo::insert(&conn, "ses-sk-1", &skip_msg).unwrap();

        // Insert a normal message with skip_context: false (default)
        let normal_msg = Message::user("keep me");
        MessageRepo::insert(&conn, "ses-sk-1", &normal_msg).unwrap();

        // Load and verify
        let loaded = MessageRepo::get_by_session(&conn, "ses-sk-1").unwrap();
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded[0].content, "skip me");
        assert!(
            loaded[0].skip_context,
            "first message should have skip_context=true"
        );
        assert_eq!(loaded[1].content, "keep me");
        assert!(
            !loaded[1].skip_context,
            "second message should have skip_context=false"
        );
    }

    #[test]
    fn test_skip_context_v2_default() {
        let conn = setup();
        insert_session(&conn, "ses-sk-2");

        // Insert raw SQL without skip_context column — v2 backward compat
        conn.execute(
            "INSERT INTO message (session_id, role, type, content, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params!["ses-sk-2", "user", "user", "legacy msg", 1700000000000_i64],
        )
        .unwrap();

        // Load and verify skip_context defaulted to false
        let loaded = MessageRepo::get_by_session(&conn, "ses-sk-2").unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].content, "legacy msg");
        assert!(
            !loaded[0].skip_context,
            "v2 legacy data should default to false"
        );
    }
}
