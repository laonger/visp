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
fn test_store_get_system_prompt() {
    let mut store = setup();
    let mut session = sample_session("sp1");
    session.system_prompt_template = "custom-template".into();
    store.create(session).unwrap();

    let template = store.get_system_prompt("sp1").unwrap();
    assert_eq!(template, "custom-template");
}

#[test]
fn test_store_get_system_prompt_not_found() {
    let store = setup();
    let err = store.get_system_prompt("nonexistent").unwrap_err();
    assert!(matches!(err, SessionError::NotFound(_)));
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

#[test]
fn test_store_list_child_sessions() {
    let mut store = setup();

    let mut child1 = sample_session("schild1");
    child1.parent_id = Some("sparent".into());
    child1.created_at_unix = Some(100);
    store.create(child1).unwrap();

    let mut child2 = sample_session("schild2");
    child2.parent_id = Some("sparent".into());
    child2.created_at_unix = Some(200);
    store.create(child2).unwrap();

    // Append messages to verify history is loaded
    store
        .append_message("schild1", Message::user("hello from child1"))
        .unwrap();
    store
        .append_message("schild2", Message::assistant("response from child2"))
        .unwrap();

    // Session for another parent should not leak
    let mut other = sample_session("sother");
    other.parent_id = Some("sother-parent".into());
    store.create(other).unwrap();

    let children = store.list_child_sessions("sparent").unwrap();
    assert_eq!(children.len(), 2);
    let ids: Vec<&str> = children.iter().map(|s| s.id.as_str()).collect();
    assert_eq!(ids, vec!["schild1", "schild2"]);

    // Verify history is loaded
    let c1 = children.iter().find(|s| s.id == "schild1").unwrap();
    assert_eq!(c1.history.len(), 1);
    assert_eq!(c1.history[0].content, "hello from child1");

    let c2 = children.iter().find(|s| s.id == "schild2").unwrap();
    assert_eq!(c2.history.len(), 1);
    assert_eq!(c2.history[0].content, "response from child2");
}
