use super::*;
use crate::app::AppState;
use crate::notify::{NotifyEngine, NotifyKind};
use std::time::Duration;
use visp_proto::visp::{Done, Error, ServerMessage, UsageDelta, server_message};

fn make_done_msg(sid: &str) -> ServerMessage {
    ServerMessage {
        payload: Some(server_message::Payload::Done(Done {
            session_id: sid.into(),
        })),
    }
}

fn make_error_msg(sid: &str, code: &str, msg: &str) -> ServerMessage {
    ServerMessage {
        payload: Some(server_message::Payload::Error(Error {
            code: code.into(),
            message: msg.into(),
            session_id: sid.into(),
            agent_name: String::new(),
        })),
    }
}

/// Bug: 子 agent Done 时不应影响主 agent 的 generating 状态。
/// 修复前 set_generating(false) 作用于 active tab，切换 tab 后会误清其他 tab。
#[test]
fn test_sub_done_does_not_clear_main_generating() {
    let mut app = AppState::new("main".into(), "m".into(), "".into(), String::new());
    let chat = ChatHandle::new_mock("main");

    // 主 agent 正在运行
    app.tab_bar.tabs[0].generating = true;
    app.current_request_id = Some("req-1".to_string());

    // 创建子 agent tab（正在运行）
    app.tab_bar.insert_sub_agent("sub1", "agentA", false);
    app.tab_bar.tabs[1].generating = true;

    // 切换到子 agent tab
    app.tab_bar.activate(1);

    // 子 agent 完成
    handle_grpc_message(make_done_msg("sub1"), &mut app, &chat);

    // 子 tab generating 应为 false
    assert!(
        !app.tab_bar.tabs[1].generating,
        "sub tab generating should be false after its Done"
    );
    // 主 tab generating 仍应为 true
    assert!(
        app.tab_bar.tabs[0].generating,
        "main tab generating should remain true — sub Done must not affect it"
    );
    // 主 tab 的 current_request_id 不应被子 agent Done 清除
    assert_eq!(
        app.current_request_id,
        Some("req-1".to_string()),
        "current_request_id should not be cleared by sub agent Done"
    );
}

/// Bug: 主 agent Done 时不应影响子 agent 的 generating 状态。
#[tokio::test]
async fn test_main_done_does_not_clear_sub_generating() {
    let mut app = AppState::new("main".into(), "m".into(), "".into(), String::new());
    let chat = ChatHandle::new_mock("main");

    // 主 agent 正在运行
    app.tab_bar.tabs[0].generating = true;
    app.current_request_id = Some("req-1".to_string());

    // 创建子 agent tab（正在运行）
    app.tab_bar.insert_sub_agent("sub1", "agentA", false);
    app.tab_bar.tabs[1].generating = true;

    // 停留在子 agent tab
    app.tab_bar.activate(1);

    // 主 agent 完成
    handle_grpc_message(make_done_msg("main"), &mut app, &chat);

    // 主 tab generating 应为 false
    assert!(
        !app.tab_bar.tabs[0].generating,
        "main tab generating should be false after its Done"
    );
    // 子 tab generating 仍应为 true
    assert!(
        app.tab_bar.tabs[1].generating,
        "sub tab generating should remain true — main Done must not affect it"
    );
    // 主 tab 的 current_request_id 应被清除
    assert!(
        app.current_request_id.is_none(),
        "current_request_id should be cleared by main agent Done"
    );
}

/// 子 agent Error 不应清除主 agent 的 current_request_id。
#[test]
fn test_sub_error_does_not_clear_main_request_id() {
    let mut app = AppState::new("main".into(), "m".into(), "".into(), String::new());
    let chat = ChatHandle::new_mock("main");

    app.tab_bar.tabs[0].generating = true;
    app.current_request_id = Some("req-1".to_string());

    app.tab_bar.insert_sub_agent("sub1", "agentA", false);
    app.tab_bar.tabs[1].generating = true;

    // 停留在主 tab
    app.tab_bar.activate(0);

    // 子 agent 出错
    handle_grpc_message(
        make_error_msg("sub1", "ProviderError", "timeout"),
        &mut app,
        &chat,
    );

    // 子 tab generating 应为 false
    assert!(!app.tab_bar.tabs[1].generating);
    // 主 tab 的 current_request_id 不应被清除
    assert_eq!(
        app.current_request_id,
        Some("req-1".to_string()),
        "current_request_id should not be cleared by sub agent Error"
    );
}

/// stale_done_expected 只应跳过主 session 的 Done/Error，
/// 不应被子 session 的 Done/Error 消耗。
#[test]
fn test_stale_done_not_consumed_by_sub_done() {
    let mut app = AppState::new("main".into(), "m".into(), "".into(), String::new());
    let chat = ChatHandle::new_mock("main");

    // 模拟 Ctrl+C 后的 stale 状态
    app.stale_done_expected = true;
    app.tab_bar.tabs[0].generating = true;

    app.tab_bar.insert_sub_agent("sub1", "agentA", false);
    app.tab_bar.tabs[1].generating = true;

    // 子 agent Done 到来
    handle_grpc_message(make_done_msg("sub1"), &mut app, &chat);

    // stale_done_expected 仍应为 true（被子 Done 消耗了就错）
    assert!(
        app.stale_done_expected,
        "stale_done_expected should remain true — sub Done must not consume it"
    );
    // 子 tab generating 应为 false
    assert!(!app.tab_bar.tabs[1].generating);

    // 接着主 agent Done 到来 — 应被 stale 跳过
    handle_grpc_message(make_done_msg("main"), &mut app, &chat);
    assert!(
        !app.stale_done_expected,
        "stale_done_expected should be consumed by main Done"
    );
}

fn make_usage_delta_msg(sid: &str, output_tokens: u32) -> ServerMessage {
    ServerMessage {
        payload: Some(server_message::Payload::UsageDelta(UsageDelta {
            input_tokens: 0,
            output_tokens,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 0,
            session_id: sid.into(),
        })),
    }
}

/// UsageDelta 必须被分发到实时速率统计（此前落入 `_ => {}` 被忽略），
/// 且不参与结算累加。
#[test]
fn test_usage_delta_is_applied_to_stream_rate() {
    let mut app = AppState::new("main".into(), "m".into(), "".into(), String::new());
    let chat = ChatHandle::new_mock("main");

    handle_grpc_message(make_usage_delta_msg("main", 12), &mut app, &chat);
    handle_grpc_message(make_usage_delta_msg("main", 8), &mut app, &chat);

    assert_eq!(app.tab_bar.tabs[0].stream_output_tokens, 20);
    assert_eq!(app.total_output_tokens, 0, "实时增量不得计入结算总量");
    assert!(app.active_tab().pending_usage.is_none());
}

// ── 通知挂接点（Wave 2）：Done 分支在 stale 守卫之后触发 BEL ──────────

#[test]
fn test_done_hookup_rings_for_main_session() {
    let mut app = AppState::new("main".into(), "m".into(), "".into(), String::new());
    app.notify = NotifyEngine::new(true, None, true, true, true, Duration::ZERO);
    let chat = ChatHandle::new_mock("main");

    handle_grpc_message(make_done_msg("main"), &mut app, &chat);

    assert!(
        app.notify.last_sent_at(NotifyKind::Done).is_some(),
        "main session Done should trigger a notification"
    );
}

#[test]
fn test_done_hookup_skips_sub_session() {
    let mut app = AppState::new("main".into(), "m".into(), "".into(), String::new());
    app.notify = NotifyEngine::new(true, None, true, true, true, Duration::ZERO);
    let chat = ChatHandle::new_mock("main");
    app.tab_bar.insert_sub_agent("sub1", "agentA", false);

    handle_grpc_message(make_done_msg("sub1"), &mut app, &chat);

    assert!(
        app.notify.last_sent_at(NotifyKind::Done).is_none(),
        "sub session Done must not ring"
    );
}
