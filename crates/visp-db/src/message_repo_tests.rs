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
        agent_name: "default".into(),
        parent_id: None,
        permission: vec![],
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
