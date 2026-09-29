use super::*;
use crate::app::{AppState, LineType};
use crate::connection::{ConnState, Recovered};
use crate::notify::{NotifyEngine, NotifyKind};
use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use std::time::Duration;
use visp_proto::visp::{
    Done, Error, ServerMessage, UsageDelta, UserMessage, UserQuery, reload_config_response,
    server_message,
};

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

fn make_user_query_msg(sid: &str, message: &str) -> ServerMessage {
    ServerMessage {
        payload: Some(server_message::Payload::UserQuery(UserQuery {
            query_id: "q-1".into(),
            message: message.into(),
            session_id: sid.into(),
            options: Vec::new(),
            allow_other: false,
        })),
    }
}

// ── 通知挂接点（计划 5a）：stale Done 不响铃、enabled=false 不响铃 ──

#[test]
fn test_done_hookup_skips_stale_done() {
    let mut app = AppState::new("main".into(), "m".into(), "".into(), String::new());
    app.notify = NotifyEngine::new(true, None, true, true, true, Duration::ZERO);
    let chat = ChatHandle::new_mock("main");

    // 模拟 Ctrl+C 后的 stale 状态：主 session Done 应被跳过且不响铃
    app.stale_done_expected = true;

    handle_grpc_message(make_done_msg("main"), &mut app, &chat);

    assert!(
        app.notify.last_sent_at(NotifyKind::Done).is_none(),
        "stale Done（Cancel 语义）不得触发通知"
    );
    assert!(
        !app.stale_done_expected,
        "stale_done_expected 应被主 session Done 清除"
    );
}

#[test]
fn test_done_hookup_respects_disabled_engine() {
    let mut app = AppState::new("main".into(), "m".into(), "".into(), String::new());
    app.notify = NotifyEngine::new(true, None, false, true, true, Duration::ZERO);
    let chat = ChatHandle::new_mock("main");

    handle_grpc_message(make_done_msg("main"), &mut app, &chat);

    assert!(
        app.notify.last_sent_at(NotifyKind::Done).is_none(),
        "enabled=false 时主 session Done 不得触发通知"
    );
}

// ── 通知挂接点（计划 5b）：UserQuery 仅主会话响铃，正文取 uq.message ──

#[test]
fn test_user_query_hookup_rings_for_main_session() {
    let mut app = AppState::new("main".into(), "m".into(), "".into(), String::new());
    app.notify = NotifyEngine::new(true, None, true, true, true, Duration::ZERO);
    let chat = ChatHandle::new_mock("main");

    handle_grpc_message(make_user_query_msg("main", "approve?"), &mut app, &chat);

    assert!(
        app.notify.last_sent_at(NotifyKind::UserQuery).is_some(),
        "主 session 的 UserQuery 应触发通知"
    );
}

#[test]
fn test_user_query_hookup_rings_for_empty_session() {
    let mut app = AppState::new("main".into(), "m".into(), "".into(), String::new());
    app.notify = NotifyEngine::new(true, None, true, true, true, Duration::ZERO);
    let chat = ChatHandle::new_mock("main");

    handle_grpc_message(make_user_query_msg("", "approve?"), &mut app, &chat);

    assert!(
        app.notify.last_sent_at(NotifyKind::UserQuery).is_some(),
        "session_id 为空的 UserQuery 应按主会话处理并触发通知"
    );
}

#[test]
fn test_user_query_hookup_skips_sub_session() {
    let mut app = AppState::new("main".into(), "m".into(), "".into(), String::new());
    app.notify = NotifyEngine::new(true, None, true, true, true, Duration::ZERO);
    let chat = ChatHandle::new_mock("main");
    app.tab_bar.insert_sub_agent("sub1", "agentA", false);

    handle_grpc_message(make_user_query_msg("sub1", "approve?"), &mut app, &chat);

    assert!(
        app.notify.last_sent_at(NotifyKind::UserQuery).is_none(),
        "子 session 的 UserQuery 不得触发通知"
    );
}

#[test]
fn test_user_query_hookup_still_builds_confirm_state() {
    let mut app = AppState::new("main".into(), "m".into(), "".into(), String::new());
    app.notify = NotifyEngine::new(true, None, true, true, true, Duration::ZERO);
    let chat = ChatHandle::new_mock("main");

    handle_grpc_message(make_user_query_msg("main", "approve?"), &mut app, &chat);

    // 回归：通知挂接不得破坏 ConfirmState 构造
    let confirm = app
        .confirm
        .as_ref()
        .expect("UserQuery 应正常构造 ConfirmState");
    assert_eq!(confirm.query_id, "q-1");
    assert_eq!(confirm.message, "approve?");
    assert!(confirm.options.is_empty());
    assert_eq!(confirm.selected_index, 0);
    assert!(!confirm.other_active);
}

#[test]
fn test_user_query_hookup_uses_message_body() {
    let mut app = AppState::new("main".into(), "m".into(), "".into(), String::new());
    app.notify = NotifyEngine::new(true, None, true, true, true, Duration::ZERO);
    let chat = ChatHandle::new_mock("main");

    handle_grpc_message(
        make_user_query_msg("main", "需要审批：执行 bash 命令"),
        &mut app,
        &chat,
    );

    assert!(
        app.notify.last_sent_at(NotifyKind::UserQuery).is_some(),
        "非空 uq.message 应作为通知正文发出"
    );
}

// ── /reload 命令接入（计划 5a）──────────────────────────────

fn make_reload_item(category: &str, success: bool, message: &str) -> reload_config_response::Item {
    reload_config_response::Item {
        category: category.into(),
        success,
        message: message.into(),
        added: 0,
        modified: 0,
        deleted: 0,
        skipped: 0,
    }
}

/// 5a-1：`/reload` 置 pending 标志并在状态行提示 Reloading。
#[test]
fn test_reload_command_sets_pending_and_status() {
    let mut app = AppState::new("main".into(), "m".into(), "".into(), String::new());
    let mut chat = ChatHandle::new_mock("main");

    crate::command::handle("/reload", &mut app, &mut chat);

    assert!(app.pending_reload, "/reload 应置 pending_reload 标志");
    assert!(
        app.messages()
            .iter()
            .any(|m| matches!(m.line_type, LineType::Status) && m.content.contains("Reloading")),
        "状态行应提示 Reloading"
    );
}

/// 5a-2：逐项结果渲染——成功用 Status、失败用 Error，并清除标志。
#[test]
fn test_reload_results_render_per_item_styles() {
    let mut app = AppState::new("main".into(), "m".into(), "".into(), String::new());
    app.pending_reload = true;

    let items = vec![
        make_reload_item("rules", true, "3 files"),
        make_reload_item("agents", false, "bad file"),
    ];
    apply_reload_results(&mut app, Ok(items));

    assert!(!app.pending_reload, "结果渲染后应清除 pending_reload 标志");
    assert!(
        app.messages()
            .iter()
            .any(|m| matches!(m.line_type, LineType::Status)
                && m.content.contains("rules")
                && m.content.contains("3 files")),
        "成功条目应以 Status 样式渲染"
    );
    assert!(
        app.messages()
            .iter()
            .any(|m| matches!(m.line_type, LineType::Error)
                && m.content.contains("agents")
                && m.content.contains("bad file")),
        "失败条目应以 Error 样式渲染"
    );
}

/// 5a-3：Tab 补全清单含 /reload。
#[test]
fn test_tab_completion_lists_reload() {
    let mut app = AppState::new("main".into(), "m".into(), "".into(), String::new());
    let mut chat = ChatHandle::new_mock("main");
    app.textarea = AppState::new_textarea();
    app.textarea.insert_str("/");

    handle_key_event(
        Event::Key(KeyEvent::from(KeyCode::Tab)),
        &mut app,
        &mut chat,
    );

    let tc = app
        .tab_completion
        .as_ref()
        .expect("输入 / 后按 Tab 应产生补全候选");
    assert!(
        tc.matches.iter().any(|c| c == "/reload"),
        "补全清单应含 /reload：{:?}",
        tc.matches
    );
}

/// 5a-5：断线期调用报错提示、无自动重试（单条错误 + 标志清除）。
#[test]
fn test_reload_rpc_error_reports_without_retry() {
    let mut app = AppState::new("main".into(), "m".into(), "".into(), String::new());
    app.pending_reload = true;

    apply_reload_results(&mut app, Err("daemon unreachable".into()));

    assert!(
        !app.pending_reload,
        "失败后应清除 pending_reload（不自动重试）"
    );
    let errors: Vec<_> = app
        .messages()
        .iter()
        .filter(|m| matches!(m.line_type, LineType::Error))
        .collect();
    assert_eq!(errors.len(), 1, "只应报一条错误，不得自动重试");
    assert!(errors[0].content.contains("daemon unreachable"));
}

// ── 重连状态机接线（计划 7b）────────────────────────────────

fn make_user_message(sid: &str, content: &str) -> ServerMessage {
    ServerMessage {
        payload: Some(server_message::Payload::UserMessage(UserMessage {
            content: content.into(),
            session_id: sid.into(),
        })),
    }
}

/// 7b-1：recv()==None 不再退出，而是进入 Reconnecting（方向翻转）。
#[test]
fn test_stream_close_enters_reconnecting_not_quit() {
    let mut app = AppState::new("main".into(), "m".into(), "".into(), String::new());

    on_disconnect(&mut app, DisconnectReason::StreamClosed);

    assert!(!app.should_quit, "断线不得直接退出");
    assert_eq!(
        app.connection_state,
        ConnState::Reconnecting { attempt: 0 },
        "干净关闭应进入 Reconnecting"
    );
    assert!(
        app.messages()
            .iter()
            .any(|m| matches!(m.line_type, LineType::Status) && m.content.contains("reconnecting")),
        "状态行应提示正在重连"
    );
}

/// 7b-2：重连成功 → send_join 回放帧渲染进主 tab。
#[test]
fn test_recovery_replay_renders_into_main_tab() {
    let mut app = AppState::new("main".into(), "m".into(), "".into(), String::new());
    let chat = ChatHandle::new_mock("main");

    apply_recovery(&mut app, Recovered::default());
    assert_eq!(app.connection_state, ConnState::Connected);

    // 新流经由 send_join 回放的历史帧
    handle_grpc_message(make_user_message("main", "历史回放内容"), &mut app, &chat);

    assert!(
        app.tab_bar.tabs[0]
            .messages
            .iter()
            .any(|m| matches!(m.line_type, LineType::User) && m.content.contains("历史回放内容")),
        "回放帧应渲染进主 tab"
    );
}

/// 7b-3：get_session 刷新 available_models / model_keys。
#[test]
fn test_recovery_refreshes_models() {
    let mut app = AppState::new("main".into(), "m".into(), "".into(), String::new());
    app.available_models = vec!["old".into()];

    apply_recovery(
        &mut app,
        Recovered {
            available_models: vec!["gpt-new".into()],
            model_keys: vec!["gpt-new-key".into()],
        },
    );

    assert_eq!(app.available_models, vec!["gpt-new".to_string()]);
    assert_eq!(app.model_keys, vec!["gpt-new-key".to_string()]);
}

/// 7b-4：重建流后本地 streaming 残留清零。
#[test]
fn test_recovery_clears_streaming_residue() {
    let mut app = AppState::new("main".into(), "m".into(), "".into(), String::new());
    {
        let tab = &mut app.tab_bar.tabs[0];
        tab.streaming_text = "残留文本".into();
        tab.generating = true;
        tab.stream_started_at = Some(std::time::Instant::now());
        tab.stream_output_tokens = 42;
        tab.pending_usage = Some((1, 2, 3, 4, 5));
        tab.last_stream_tps = Some(1.0);
        tab.last_stream_elapsed = Some(2.0);
    }
    app.current_request_id = Some("rid".into());

    apply_recovery(&mut app, Recovered::default());

    let tab = &app.tab_bar.tabs[0];
    assert!(tab.streaming_text.is_empty(), "streaming_text 应清零");
    assert!(!tab.generating, "generating 应复位");
    assert!(tab.stream_started_at.is_none());
    assert_eq!(tab.stream_output_tokens, 0);
    assert!(tab.pending_usage.is_none(), "pending_usage 应清零");
    assert!(tab.last_stream_tps.is_none());
    assert!(tab.last_stream_elapsed.is_none());
    assert!(app.current_request_id.is_none());
}

/// 7b-5：重连期间 Enter 提交 → 提示不发送不清空；重连成功后可正常发送。
#[test]
fn test_input_blocked_while_reconnecting() {
    let mut app = AppState::new("main".into(), "m".into(), "".into(), String::new());
    let mut chat = ChatHandle::new_mock("main");
    app.connection_state = ConnState::Reconnecting { attempt: 0 };
    app.textarea = AppState::new_textarea();
    app.textarea.insert_str("hello daemon");

    handle_key_event(
        Event::Key(KeyEvent::from(KeyCode::Enter)),
        &mut app,
        &mut chat,
    );

    assert!(
        app.messages().iter().any(
            |m| matches!(m.line_type, LineType::Error) && m.content.contains("未连接到 daemon")
        ),
        "重连期间 Enter 应提示未连接"
    );
    assert_eq!(
        app.textarea.lines().join("\n"),
        "hello daemon",
        "重连期间输入不得被清空"
    );
    assert!(!app.generating(), "重连期间不得发送（generating 不应置位）");
}

#[tokio::test]
async fn test_input_works_after_reconnected() {
    let mut app = AppState::new("main".into(), "m".into(), "".into(), String::new());
    let mut chat = ChatHandle::new_mock("main");
    app.connection_state = ConnState::Connected;
    app.textarea = AppState::new_textarea();
    app.textarea.insert_str("hello daemon");

    handle_key_event(
        Event::Key(KeyEvent::from(KeyCode::Enter)),
        &mut app,
        &mut chat,
    );

    assert!(app.generating(), "连接正常时应发送输入");
    assert!(
        app.textarea.lines().join("\n").is_empty(),
        "发送后应清空输入"
    );
}

/// 7b-6：断线时正在 generating → 显示「连接中断，生成已终止」。
#[test]
fn test_generation_interrupted_hint_on_disconnect() {
    let mut app = AppState::new("main".into(), "m".into(), "".into(), String::new());
    app.tab_bar.tabs[0].generating = true;
    app.tab_bar.tabs[0].streaming_text = "半截输出".into();

    on_disconnect(&mut app, DisconnectReason::StreamClosed);

    assert!(!app.tab_bar.tabs[0].generating, "断线应终止本地 generating");
    assert!(
        app.messages()
            .iter()
            .any(|m| matches!(m.line_type, LineType::Error)
                && m.content.contains("连接中断，生成已终止")),
        "断线时正在生成应提示生成已终止"
    );
}

// ── 步骤 4b：SessionBusy 触发窗口（取消/重连）特征化（非红）──────

/// 特征化：Ctrl+C 取消后输入门禁确实放开（B1a/B1b 窗口存在性）。
#[tokio::test]
async fn test_ctrl_c_releases_input_gate() {
    let mut app = AppState::new("main".into(), "m".into(), "".into(), String::new());
    let mut chat = ChatHandle::new_mock("main");
    app.tab_bar.tabs[0].generating = true;
    app.current_request_id = Some("req-1".into());

    handle_key_event(
        Event::Key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
        &mut app,
        &mut chat,
    );

    assert!(!app.generating(), "Ctrl+C 后输入门禁应放开");
    assert!(app.stale_done_expected, "应标记 stale Done");
    assert!(
        app.current_request_id.is_none(),
        "应清除 current_request_id"
    );
}

/// 特征化：Esc 关闭确认框并取消后输入门禁确实放开（B1c 窗口存在性）。
#[tokio::test]
async fn test_esc_on_confirm_releases_input_gate() {
    let mut app = AppState::new("main".into(), "m".into(), "".into(), String::new());
    let mut chat = ChatHandle::new_mock("main");
    app.tab_bar.tabs[0].generating = true;
    app.current_request_id = Some("req-1".into());
    app.confirm = Some(crate::app::ConfirmState {
        query_id: "q-1".into(),
        message: "approve?".into(),
        options: Vec::new(),
        selected_index: 0,
        other_active: false,
    });

    handle_key_event(
        Event::Key(KeyEvent::from(KeyCode::Esc)),
        &mut app,
        &mut chat,
    );

    assert!(app.confirm.is_none(), "Esc 应关闭确认框");
    assert!(!app.generating(), "Esc 取消后输入门禁应放开");
    assert!(app.stale_done_expected);
}

// ── idle 看门狗接线（计划 7c）────────────────────────────────

/// 7c-2：探测失败/超时 → 进入 Reconnecting（看门狗入口）。
#[test]
fn test_watchdog_probe_failure_enters_reconnecting() {
    let mut app = AppState::new("main".into(), "m".into(), "".into(), String::new());

    let outcome = apply_probe_result(&mut app, false);

    assert_eq!(outcome, ProbeOutcome::Reconnect);
    assert_eq!(app.connection_state, ConnState::Reconnecting { attempt: 0 });
    assert!(
        app.messages()
            .iter()
            .any(|m| matches!(m.line_type, LineType::Status)
                && m.content.contains("Daemon unresponsive")),
        "探测失败应提示 daemon 无响应并进入重连"
    );
}

/// 7c-3：探测成功 → 重置 idle 计时、不迁移状态（周期探测稳态）。
#[test]
fn test_watchdog_probe_success_resets_without_migration() {
    let mut app = AppState::new("main".into(), "m".into(), "".into(), String::new());

    let outcome = apply_probe_result(&mut app, true);

    assert_eq!(outcome, ProbeOutcome::Reset);
    assert_eq!(
        app.connection_state,
        ConnState::Connected,
        "探测成功不得重建"
    );
}

/// 7c 负向：重连期间到达的陈旧探测结果被忽略（不产生第三路径）。
#[test]
fn test_watchdog_stale_probe_result_ignored() {
    let mut app = AppState::new("main".into(), "m".into(), "".into(), String::new());
    app.connection_state = ConnState::Reconnecting { attempt: 3 };

    assert_eq!(apply_probe_result(&mut app, true), ProbeOutcome::Ignored);
    assert_eq!(
        app.connection_state,
        ConnState::Reconnecting { attempt: 3 },
        "重连期间的陈旧探测成功不得迁回 Connected"
    );
}
