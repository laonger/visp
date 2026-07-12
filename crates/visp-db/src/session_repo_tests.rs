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

#[test]
fn test_list_child_sessions_returns_children() {
    let conn = setup();
    let mut child1 = sample_session("child-1");
    child1.parent_id = Some("parent-1".into());
    child1.created_at_unix = Some(100);
    SessionRepo::insert(&conn, &child1).unwrap();
    let mut child2 = sample_session("child-2");
    child2.parent_id = Some("parent-1".into());
    child2.created_at_unix = Some(200);
    SessionRepo::insert(&conn, &child2).unwrap();

    let children = SessionRepo::list_child_sessions(&conn, "parent-1").unwrap();
    assert_eq!(children.len(), 2);
}

#[test]
fn test_list_child_sessions_empty_when_no_children() {
    let conn = setup();
    let parent = sample_session("lonely");
    SessionRepo::insert(&conn, &parent).unwrap();

    let children = SessionRepo::list_child_sessions(&conn, "lonely").unwrap();
    assert!(children.is_empty());
}

#[test]
fn test_list_child_sessions_orders_by_created_at_asc() {
    let conn = setup();
    for i in 0..3 {
        let mut child = sample_session(&format!("child-order-{i}"));
        child.parent_id = Some("parent-order".into());
        child.created_at_unix = Some(3000 - i * 1000); // 3000, 2000, 1000 — reversed insertion
        SessionRepo::insert(&conn, &child).unwrap();
    }

    let children = SessionRepo::list_child_sessions(&conn, "parent-order").unwrap();
    assert_eq!(children.len(), 3);
    assert!(children[0].created_at_unix.unwrap() <= children[1].created_at_unix.unwrap());
    assert!(children[1].created_at_unix.unwrap() <= children[2].created_at_unix.unwrap());
}

#[test]
fn test_list_child_sessions_excludes_parent_self() {
    let conn = setup();
    // A session where parent_id equals its own id
    let mut self_ref = sample_session("self-ref");
    self_ref.parent_id = Some("self-ref".into());
    self_ref.created_at_unix = Some(100);
    SessionRepo::insert(&conn, &self_ref).unwrap();
    // A real child
    let mut child = sample_session("real-child");
    child.parent_id = Some("self-ref".into());
    child.created_at_unix = Some(200);
    SessionRepo::insert(&conn, &child).unwrap();

    let children = SessionRepo::list_child_sessions(&conn, "self-ref").unwrap();
    // Should only return the real child, not the self-referential session
    assert_eq!(children.len(), 1);
    assert_eq!(children[0].id, "real-child");
}

#[test]
fn test_list_child_sessions_does_not_return_other_parents_children() {
    let conn = setup();
    let mut c1 = sample_session("c1");
    c1.parent_id = Some("p1".into());
    c1.created_at_unix = Some(100);
    SessionRepo::insert(&conn, &c1).unwrap();
    let mut c2 = sample_session("c2");
    c2.parent_id = Some("p2".into());
    c2.created_at_unix = Some(100);
    SessionRepo::insert(&conn, &c2).unwrap();

    let children = SessionRepo::list_child_sessions(&conn, "p1").unwrap();
    assert_eq!(children.len(), 1);
    assert_eq!(children[0].id, "c1");
}

#[test]
fn test_list_child_sessions_only_returns_direct_children() {
    let conn = setup();
    // parent → child → grandchild
    let mut child = sample_session("child");
    child.parent_id = Some("parent".into());
    child.created_at_unix = Some(100);
    SessionRepo::insert(&conn, &child).unwrap();
    let mut grandchild = sample_session("grandchild");
    grandchild.parent_id = Some("child".into());
    grandchild.created_at_unix = Some(200);
    SessionRepo::insert(&conn, &grandchild).unwrap();

    // Querying parent should only return child (not grandchild)
    let children = SessionRepo::list_child_sessions(&conn, "parent").unwrap();
    assert_eq!(children.len(), 1);
    assert_eq!(children[0].id, "child");
}
