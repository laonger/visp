use super::*;
use tokio::sync::mpsc;

fn make_agent(session_id: &str, parent_id: Option<&str>) -> ActiveAgent {
    let (_tx, _rx) = mpsc::channel(16);
    ActiveAgent {
        session_id: session_id.to_string(),
        parent_session_id: parent_id.map(|s| s.to_string()),
        agent_name: "test".to_string(),
        cancel_token: CancellationToken::new(),
        inbox: _tx,
        pending_call_id: None,
        started_at: Instant::now(),
    }
}

#[test]
fn test_register_and_get() {
    let mut reg = ActiveAgentRegistry::new();
    let agent = make_agent("sess-1", None);
    reg.register(agent);
    assert!(reg.get("sess-1").is_some());
}

#[test]
fn test_register_overwrites() {
    let mut reg = ActiveAgentRegistry::new();
    reg.register(make_agent("sess-1", None));
    reg.register(make_agent("sess-1", Some("parent")));
    assert_eq!(
        reg.get("sess-1").unwrap().parent_session_id,
        Some("parent".to_string())
    );
}

#[test]
fn test_remove_returns_none() {
    let mut reg = ActiveAgentRegistry::new();
    reg.register(make_agent("sess-1", None));
    reg.remove("sess-1");
    assert!(reg.get("sess-1").is_none());
}

#[test]
fn test_children_of() {
    let mut reg = ActiveAgentRegistry::new();
    reg.register(make_agent("root", None));
    reg.register(make_agent("child-1", Some("root")));
    reg.register(make_agent("child-2", Some("root")));
    reg.register(make_agent("other", None));
    let children = reg.children_of("root");
    assert_eq!(children.len(), 2);
    assert!(children.iter().any(|a| a.session_id == "child-1"));
    assert!(children.iter().any(|a| a.session_id == "child-2"));
}

#[test]
fn test_children_of_empty() {
    let mut reg = ActiveAgentRegistry::new();
    reg.register(make_agent("root", None));
    assert!(reg.children_of("root").is_empty());
}

#[test]
fn test_descendants_of_two_generations() {
    let mut reg = ActiveAgentRegistry::new();
    reg.register(make_agent("A", None));
    reg.register(make_agent("B", Some("A")));
    reg.register(make_agent("C", Some("B")));
    let desc = reg.descendants_of("A");
    assert_eq!(desc.len(), 2);
    assert!(desc.iter().any(|a| a.session_id == "B"));
    assert!(desc.iter().any(|a| a.session_id == "C"));
}

#[test]
fn test_descendants_of_none() {
    let mut reg = ActiveAgentRegistry::new();
    reg.register(make_agent("A", None));
    assert!(reg.descendants_of("A").is_empty());
}

#[test]
fn test_compute_depth_root() {
    let mut reg = ActiveAgentRegistry::new();
    reg.register(make_agent("root", None));
    assert_eq!(reg.compute_depth("root"), 0);
}

#[test]
fn test_compute_depth_child() {
    let mut reg = ActiveAgentRegistry::new();
    reg.register(make_agent("root", None));
    reg.register(make_agent("child", Some("root")));
    assert_eq!(reg.compute_depth("child"), 1);
}

#[test]
fn test_compute_depth_grandchild() {
    let mut reg = ActiveAgentRegistry::new();
    reg.register(make_agent("root", None));
    reg.register(make_agent("child", Some("root")));
    reg.register(make_agent("grand", Some("child")));
    assert_eq!(reg.compute_depth("grand"), 2);
}

#[test]
fn test_len_and_is_empty() {
    let mut reg = ActiveAgentRegistry::new();
    assert!(reg.is_empty());
    assert_eq!(reg.len(), 0);
    reg.register(make_agent("sess-1", None));
    assert!(!reg.is_empty());
    assert_eq!(reg.len(), 1);
}
