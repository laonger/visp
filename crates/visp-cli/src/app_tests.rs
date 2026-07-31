use super::*;

#[test]
fn test_spinner_glyph_cycles_points_frames() {
    let mut app = AppState::new("sid".into(), "m".into(), "".into(), String::new());
    // Points 四帧循环：∙∙∙ / ●∙∙ / ∙●∙ / ∙∙●
    let expected = [
        "\u{2219}\u{2219}\u{2219}",
        "\u{25cf}\u{2219}\u{2219}",
        "\u{2219}\u{25cf}\u{2219}",
        "\u{2219}\u{2219}\u{25cf}",
    ];
    for (i, want) in expected.iter().enumerate() {
        app.spinner_frame = i;
        assert_eq!(app.spinner_glyph(), *want);
    }
    // 回绕：第 5 帧回到第 1 帧
    app.spinner_frame = 4;
    assert_eq!(app.spinner_glyph(), expected[0]);
}

// ════════════════════════════════════════════════════════════
// wrap_text 测试
// ════════════════════════════════════════════════════════════

#[test]
fn test_wrap_text_exact_fit() {
    // 刚好填满一行，无截断问题
    let result = wrap_text("12345", 5);
    assert_eq!(result, vec!["12345"]);
}

#[test]
fn test_wrap_text_word_boundary() {
    // 单词边界折行：hello word, width=7
    // 修复前：["hello w", "ord"]（单词被截断）
    // 修复后：["hello", "word"]
    let result = wrap_text("hello word", 7);
    assert_eq!(result, vec!["hello", "word"]);
}

#[test]
fn test_wrap_text_word_boundary_exact() {
    // 前一个单词刚好填满，后续还有单词
    let result = wrap_text("hello word", 5);
    assert_eq!(result, vec!["hello", "word"]);
}

#[test]
fn test_wrap_text_long_word_breaks_char() {
    // 单词超过一行宽度，允许字符级断行
    let result = wrap_text("Helloworld", 5);
    assert_eq!(result, vec!["Hello", "world"]);
}

#[test]
fn test_wrap_text_multi_word_boundary() {
    // 多个单词，每次都在单词边界折行
    let result = wrap_text("This is a test hello", 8);
    assert_eq!(result, vec!["This is", "a test", "hello"]);
}

#[test]
fn test_wrap_text_chinese_english_mixed() {
    // 中文 + 英文混合
    let result = wrap_text("这是一个test", 8);
    assert_eq!(result, vec!["这是一个", "test"]);
}

#[test]
fn test_wrap_text_newline_paragraphs() {
    // 显式换行符
    let result = wrap_text("hello\nworld", 10);
    assert_eq!(result, vec!["hello", "world"]);
}

#[test]
fn test_wrap_text_empty() {
    // 空字符串返回一个空行（split('\n') 行为）
    let result = wrap_text("", 10);
    assert_eq!(result, vec![""]);
}

#[test]
fn test_wrap_text_empty_line() {
    let result = wrap_text("\n", 10);
    assert_eq!(result, vec!["", ""]);
}

#[test]
fn test_wrap_text_zero_width() {
    let result = wrap_text("hello", 0);
    assert!(result.is_empty());
}

#[test]
fn test_wrap_text_word_not_truncated_at_exact_fill() {
    // 核心场景：单词紧贴右边界时不截断
    // "abcde fghij" width=5
    // "abcde" 占满5，后面有空格，应在空格处折行
    let result = wrap_text("abcde fghij", 5);
    assert_eq!(result, vec!["abcde", "fghij"]);
}

#[test]
fn test_stale_done_expected_default() {
    let app = AppState::new("s".into(), "m".into(), "".into(), String::new());
    assert!(!app.stale_done_expected);
}

#[test]
fn test_app_state_new() {
    let app = AppState::new(
        "test-session".into(),
        "deepseek-v4-flash".into(),
        "".into(),
        String::new(),
    );
    assert_eq!(app.session_id, "test-session");
    assert_eq!(app.model, "deepseek-v4-flash");
    assert!(app.messages().is_empty());
    assert!(app.streaming_is_empty());
    assert!(!app.generating());
    assert!(app.confirm.is_none());
    assert!(!app.should_quit);
    assert!(app.scroll_following);
    assert_eq!(app.scroll_state.x, 0);
    assert_eq!(app.scroll_state.y, 0);
}

#[test]
fn test_add_message() {
    let mut app = AppState::new("s".into(), "m".into(), "".into(), String::new());
    app.add_message(LineType::User, "hello".into());
    assert_eq!(app.messages().len(), 1);
    assert_eq!(app.messages()[0].content, "hello");
    assert_eq!(app.messages()[0].line_type, LineType::User);
}

#[test]
fn test_add_message_id_increments() {
    let mut app = AppState::new("s".into(), "m".into(), "".into(), String::new());
    app.add_message(LineType::User, "a".into());
    app.add_message(LineType::Assistant, "b".into());
    assert_eq!(app.messages()[0].id, 0);
    assert_eq!(app.messages()[1].id, 1);
}

#[test]
fn test_add_message_version_initial() {
    let mut app = AppState::new("s".into(), "m".into(), "".into(), String::new());
    app.add_message(LineType::User, "hello".into());
    assert_eq!(app.messages()[0].version, 0);
}

#[test]
fn test_streaming_text() {
    let mut app = AppState::new("s".into(), "m".into(), "".into(), String::new());
    app.append_streaming("Hello ");
    app.append_streaming("world");
    assert_eq!(app.streaming_text(), "Hello world");
    app.flush_streaming();
    assert!(app.streaming_is_empty());
    assert_eq!(app.messages().len(), 1);
    assert_eq!(app.messages()[0].line_type, LineType::Assistant);
    assert_eq!(app.messages()[0].content, "Hello world");
}

#[test]
fn test_update_message_increments_version() {
    let mut app = AppState::new("s".into(), "m".into(), "".into(), String::new());
    app.add_message(LineType::Assistant, "original".into());
    let id = app.messages()[0].id;
    app.update_message(id, "updated".into());
    assert_eq!(app.messages()[0].version, 1);
    assert_eq!(app.messages()[0].content, "updated");
}

#[test]
fn test_update_message_id_not_found_does_nothing() {
    let mut app = AppState::new("s".into(), "m".into(), "".into(), String::new());
    app.add_message(LineType::Assistant, "original".into());
    let original_version = app.messages()[0].version;
    app.update_message(999, "nope".into());
    assert_eq!(app.messages()[0].version, original_version);
    assert_eq!(app.messages()[0].content, "original");
}

#[test]
fn test_clear_messages() {
    let mut app = AppState::new("s".into(), "m".into(), "".into(), String::new());
    app.add_message(LineType::User, "hello".into());
    app.add_message(LineType::Assistant, "world".into());
    assert_eq!(app.messages().len(), 2);
    app.clear_messages();
    assert!(app.messages().is_empty());
}

#[test]
fn test_message_cache_creation() {
    let msg = ChatLine {
        id: 0,
        version: 0,
        line_type: LineType::User,
        content: "hello world".into(),
        call_id: None,
        tool_result: None,
        tool_error: false,
        sub_session_id: None,
    };
    let cache = MessageCache::from_message(&msg, 80, false);
    assert_eq!(cache.msg_id, 0);
    assert_eq!(cache.msg_version, 0);
    assert_eq!(cache.width, 80);
    assert!(cache.line_count > 0);
    assert!(!cache.lines.is_empty());
}

#[test]
fn test_message_cache_matches() {
    let msg = ChatLine {
        id: 0,
        version: 0,
        line_type: LineType::User,
        content: "hello".into(),
        call_id: None,
        tool_result: None,
        tool_error: false,
        sub_session_id: None,
    };
    let cache = MessageCache::from_message(&msg, 80, false);
    assert!(cache.matches(&msg, 80, false));
    // 不同 version 不匹配
    let mut msg2 = msg.clone();
    msg2.version = 1;
    assert!(!cache.matches(&msg2, 80, false));
    // 不同 width 不匹配
    assert!(!cache.matches(&msg, 40, false));
    // 不同 id 不匹配
    let msg3 = ChatLine {
        id: 1,
        version: 0,
        line_type: LineType::User,
        content: "hello".into(),
        call_id: None,
        tool_result: None,
        tool_error: false,
        sub_session_id: None,
    };
    assert!(!cache.matches(&msg3, 80, false));
}

#[test]
fn test_cache_user_message_has_top_bottom_padding() {
    let msg = ChatLine {
        id: 0,
        version: 0,
        line_type: LineType::User,
        content: "hello".into(),
        call_id: None,
        tool_result: None,
        tool_error: false,
        sub_session_id: None,
    };
    let cache = MessageCache::from_message(&msg, 80, false);
    // User 消息背景由 bg_fill 处理，行级不带 padding
    assert!(cache.line_count >= 1);
}

#[test]
fn test_cache_tool_call_truncation() {
    let msg = ChatLine {
        id: 0,
        version: 0,
        line_type: LineType::ToolCall {
            name: "bash".into(),
        },
        content: r#"{"cmd":"echo hello"}"#.into(),
        call_id: None,
        tool_result: None,
        tool_error: false,
        sub_session_id: None,
    };
    let cache = MessageCache::from_message(&msg, 80, false);
    // With collapsible design: icon + name + formatted args (fits on one line)
    assert_eq!(cache.line_count, 1);
}

#[test]
fn test_clear_messages_also_clears_caches() {
    let mut app = AppState::new("s".into(), "m".into(), "".into(), String::new());
    app.add_message(LineType::User, "hello".into());
    // 手动添加一个 cache 模拟渲染后的状态
    app.message_caches
        .push(MessageCache::from_message(&app.messages()[0], 80, false));
    assert_eq!(app.message_caches.len(), 1);
    app.clear_messages();
    assert!(app.messages().is_empty());
    assert!(app.message_caches.is_empty());
}

#[test]
fn test_add_tool_line_stores_call_id() {
    let mut app = AppState::new("s".into(), "m".into(), "".into(), String::new());
    app.add_tool_line(
        LineType::ToolCall {
            name: "test".into(),
        },
        "cmd".into(),
        "tc_1",
    );
    assert_eq!(app.messages()[0].call_id.as_deref(), Some("tc_1"));
}

#[test]
fn test_insert_tool_result_appends_to_matching_call() {
    let mut app = AppState::new("s".into(), "m".into(), "".into(), String::new());
    app.add_tool_line(
        LineType::ToolCall {
            name: "test".into(),
        },
        "cmd1".into(),
        "id1",
    );
    app.add_tool_line(
        LineType::ToolCall {
            name: "test".into(),
        },
        "cmd2".into(),
        "id2",
    );
    app.insert_tool_result("id1", "result1".into());
    // result 存储在 tool_result 字段
    assert_eq!(app.messages()[0].content, "cmd1");
    assert_eq!(app.messages()[0].tool_result.as_deref(), Some("result1"));
    assert_eq!(app.messages()[1].content, "cmd2");
    assert!(matches!(
        app.messages()[1].line_type,
        LineType::ToolCall { .. }
    ));
}

#[test]
fn test_insert_tool_result_without_matching_call_appends() {
    let mut app = AppState::new("s".into(), "m".into(), "".into(), String::new());
    app.add_tool_line(
        LineType::ToolCall {
            name: "test".into(),
        },
        "cmd".into(),
        "id1",
    );
    app.insert_tool_result("nonexistent", "result".into());
    // 没有匹配的 call_id，作为新的 ToolCall 追加到末尾
    assert_eq!(app.messages().len(), 2);
    assert_eq!(app.messages()[1].content, "result");
}

#[test]
fn test_multiple_tool_calls_grouped() {
    let mut app = AppState::new("s".into(), "m".into(), "".into(), String::new());
    app.add_tool_line(
        LineType::ToolCall {
            name: "test".into(),
        },
        "cmd1".into(),
        "a",
    );
    app.add_tool_line(
        LineType::ToolCall {
            name: "test".into(),
        },
        "cmd2".into(),
        "b",
    );
    app.insert_tool_result("b", "result2".into());
    app.insert_tool_result("a", "result1".into());
    // result 存储在各自的 tool_result 字段
    assert_eq!(app.messages()[0].content, "cmd1");
    assert_eq!(app.messages()[0].tool_result.as_deref(), Some("result1"));
    assert_eq!(app.messages()[1].content, "cmd2");
    assert_eq!(app.messages()[1].tool_result.as_deref(), Some("result2"));
}

#[test]
fn test_confirm_state_new() {
    let cs = ConfirmState {
        query_id: "q1".into(),
        message: "test?".into(),
        options: vec!["Yes".into(), "No".into()],
        selected_index: 0,
        other_active: false,
    };
    assert_eq!(cs.selected_index, 0);
    assert!(!cs.other_active);
}

#[test]
fn test_confirm_state_tool_approval() {
    let cs = ConfirmState {
        query_id: "q1".into(),
        message: "Allow tool?".into(),
        options: vec![],
        selected_index: 0,
        other_active: false,
    };
    assert!(cs.options.is_empty());
}

#[test]
fn test_confirm_state_other_mode() {
    let mut cs = ConfirmState {
        query_id: "q1".into(),
        message: "Choose?".into(),
        options: vec!["Yes".into(), "No".into()],
        selected_index: 0,
        other_active: false,
    };
    assert!(!cs.other_active);
    cs.other_active = true;
    assert!(cs.other_active);
    cs.other_active = false;
    // selected_index 指向 "Other"（index == options.len()）
    cs.selected_index = cs.options.len();
    assert_eq!(cs.selected_index, 2);
}

// ════════════════════════════════════════════════════════════
// AgentStatus / TabEntry / TabBar 测试
// ════════════════════════════════════════════════════════════

#[test]
fn test_agent_status_default_is_running() {
    let tab = TabEntry::new("sid", "agent");
    assert_eq!(tab.status, AgentStatus::Running);
}

#[test]
fn test_tab_entry_new_with_session_and_name() {
    let tab = TabEntry::new("sid-1", "agent-A");
    assert_eq!(tab.session_id, "sid-1");
    assert_eq!(tab.agent_name, "agent-A");
}

#[test]
fn test_tab_entry_initial_empty() {
    let tab = TabEntry::new("sid", "agent");
    assert!(tab.frames.is_empty());
    assert!(tab.messages.is_empty());
    assert!(tab.streaming_text.is_empty());
    assert_eq!(tab.rendered_up_to, 0);
}

#[test]
fn test_tab_entry_default_per_tab_state() {
    let tab = TabEntry::new("sid", "agent");
    assert!(!tab.generating);
    assert!(tab.pending_usage.is_none());
    assert_eq!(tab.scroll, 0);
}

// ── AgentStatus::ViewOnly ──────────────────────────────────

#[test]
fn agent_status_view_only_variant_exists() {
    let status = AgentStatus::ViewOnly;
    match status {
        AgentStatus::ViewOnly => {} // 命中正确分支
        _ => panic!("Expected ViewOnly variant"),
    }
}

#[test]
fn agent_status_all_variants_display() {
    // 所有变体都能 match 不 panic
    let variants = [
        AgentStatus::Running,
        AgentStatus::Done,
        AgentStatus::Error,
        AgentStatus::ViewOnly,
    ];
    for v in &variants {
        match v {
            AgentStatus::Running => {}
            AgentStatus::Done => {}
            AgentStatus::Error => {}
            AgentStatus::ViewOnly => {}
        }
    }
}

// ── TabEntry::new_view_only ────────────────────────────────

#[test]
fn tab_entry_new_view_only_has_view_only_status() {
    let tab = TabEntry::new_view_only("sid", "name");
    assert_eq!(tab.status, AgentStatus::ViewOnly);
}

#[test]
fn tab_entry_new_keeps_running_status() {
    let tab = TabEntry::new("sid", "name");
    assert_eq!(tab.status, AgentStatus::Running);
}

#[test]
fn tab_entry_new_view_only_other_fields_default() {
    let new_tab = TabEntry::new("sid", "name");
    let vo_tab = TabEntry::new_view_only("sid", "name");

    // 与 new() 一致的默认值
    assert!(vo_tab.frames.is_empty());
    assert!(vo_tab.messages.is_empty());
    assert_eq!(vo_tab.scroll, 0);
    assert_eq!(vo_tab.rendered_up_to, new_tab.rendered_up_to);
    assert_eq!(vo_tab.streaming_text, new_tab.streaming_text);
    assert!(!vo_tab.generating);
    assert_eq!(vo_tab.pending_usage, new_tab.pending_usage);
    assert_eq!(vo_tab.next_message_id, new_tab.next_message_id);
    assert_eq!(vo_tab.session_id, new_tab.session_id);
    assert_eq!(vo_tab.agent_name, new_tab.agent_name);
}

#[test]
fn test_tabbar_new_creates_default_tab() {
    let bar = TabBar::new("main-sid".into());
    assert_eq!(bar.tabs.len(), 1);
    assert_eq!(bar.tabs[0].agent_name, "default");
    assert_eq!(bar.tabs[0].session_id, "main-sid");
    assert_eq!(bar.active, 0);
    assert_eq!(bar.page_start, 0);
}

#[test]
fn test_tabbar_new_has_last_term_width_zero() {
    let bar = TabBar::new("main-sid".into());
    assert_eq!(bar.last_term_width, 0);
}

#[test]
fn test_tabbar_insert_sub_agent_at_index_1() {
    let mut bar = TabBar::new("main".into());
    bar.insert_sub_agent("sub1", "agentA", false);
    assert_eq!(bar.tabs.len(), 2);
    assert_eq!(bar.tabs[1].session_id, "sub1");
}

#[test]
fn test_tabbar_insert_two_sub_agents_newer_first() {
    let mut bar = TabBar::new("main".into());
    bar.insert_sub_agent("sub1", "A", false);
    bar.insert_sub_agent("sub2", "B", false);
    assert_eq!(bar.tabs.len(), 3);
    assert_eq!(bar.tabs[1].session_id, "sub2");
    assert_eq!(bar.tabs[2].session_id, "sub1");
}

#[test]
fn test_tabbar_insert_does_not_change_active() {
    let mut bar = TabBar::new("main".into());
    assert_eq!(bar.active, 0);
    bar.insert_sub_agent("sub1", "A", false);
    assert_eq!(bar.active, 0);
}

#[test]
fn test_tabbar_insert_when_active_geq_1_shifts_active_plus_1() {
    let mut bar = TabBar::new("main".into());
    bar.insert_sub_agent("sub1", "A", false);
    bar.active = 1;
    bar.insert_sub_agent("sub2", "B", false);
    assert_eq!(bar.active, 2);
    // Still pointing to sub1 (now at index 2)
    assert_eq!(bar.tabs[bar.active].session_id, "sub1");
}

#[test]
fn test_tabbar_find_index_by_session() {
    let mut bar = TabBar::new("main".into());
    bar.insert_sub_agent("sub1", "A", false);
    bar.insert_sub_agent("sub2", "B", false);
    assert_eq!(bar.find_index_by_session("sub1"), Some(2));
    assert_eq!(bar.find_index_by_session("sub2"), Some(1));
    assert_eq!(bar.find_index_by_session("nonexistent"), None);
}

#[test]
fn test_tabbar_find_or_insert_creates_when_missing() {
    let mut bar = TabBar::new("main".into());
    let idx = bar.find_or_insert("new-sid", "agentX");
    assert_eq!(idx, 1);
    assert_eq!(bar.tabs.len(), 2);
}

#[test]
fn test_tabbar_find_or_insert_returns_existing() {
    let mut bar = TabBar::new("main".into());
    bar.insert_sub_agent("sub1", "A", false);
    let len_before = bar.tabs.len();
    let idx = bar.find_or_insert("sub1", "A");
    assert_eq!(idx, 1);
    assert_eq!(bar.tabs.len(), len_before);
}

// ════════════════════════════════════════════════════════════
// TabEntry::render_pending 测试
// ════════════════════════════════════════════════════════════

fn td(delta: &str) -> visp_proto::visp::ServerMessage {
    visp_proto::visp::ServerMessage {
        payload: Some(visp_proto::visp::server_message::Payload::TextDelta(
            visp_proto::visp::TextDelta {
                delta: delta.into(),
                session_id: String::new(),
                agent_name: String::new(),
            },
        )),
    }
}

fn tool_call(name: &str, call_id: &str, args: &str) -> visp_proto::visp::ServerMessage {
    visp_proto::visp::ServerMessage {
        payload: Some(visp_proto::visp::server_message::Payload::ToolCall(
            visp_proto::visp::ToolCall {
                tool_name: name.into(),
                call_id: call_id.into(),
                arguments: args.into(),
                session_id: String::new(),
                agent_name: String::new(),
            },
        )),
    }
}

fn tool_result(
    call_id: &str,
    tool_name: &str,
    content: &str,
    is_error: bool,
) -> visp_proto::visp::ServerMessage {
    visp_proto::visp::ServerMessage {
        payload: Some(visp_proto::visp::server_message::Payload::ToolResult(
            visp_proto::visp::ToolResult {
                call_id: call_id.into(),
                tool_name: tool_name.into(),
                content: content.into(),
                is_error,
                session_id: String::new(),
                agent_name: String::new(),
            },
        )),
    }
}

fn done_msg() -> visp_proto::visp::ServerMessage {
    visp_proto::visp::ServerMessage {
        payload: Some(visp_proto::visp::server_message::Payload::Done(
            visp_proto::visp::Done {
                session_id: String::new(),
            },
        )),
    }
}

fn error_msg(code: &str, message: &str) -> visp_proto::visp::ServerMessage {
    visp_proto::visp::ServerMessage {
        payload: Some(visp_proto::visp::server_message::Payload::Error(
            visp_proto::visp::Error {
                code: code.into(),
                message: message.into(),
                session_id: String::new(),
                agent_name: String::new(),
            },
        )),
    }
}

#[test]
fn test_render_pending_empty_frames_noop() {
    let mut tab = TabEntry::new("sid", "agent");
    tab.render_pending();
    assert_eq!(tab.rendered_up_to, 0);
    assert!(tab.messages.is_empty());
}

#[test]
fn test_render_pending_text_delta_appends_streaming() {
    let mut tab = TabEntry::new("sid", "agent");
    tab.frames.push(td("hello"));
    tab.frames.push(td(" world"));
    tab.render_pending();
    assert_eq!(tab.streaming_text, "hello world");
    assert!(tab.messages.is_empty());
    assert_eq!(tab.rendered_up_to, 2);
}

#[test]
fn test_render_pending_tool_call_flushes_streaming() {
    let mut tab = TabEntry::new("sid", "agent");
    tab.frames.push(td("hi"));
    tab.frames.push(tool_call("bash", "c1", r#"{}"#));
    tab.render_pending();
    // streaming flushed
    assert!(tab.streaming_text.is_empty());
    // 2 messages: Assistant("hi") + ToolCall
    assert_eq!(tab.messages.len(), 2);
    assert_eq!(tab.messages[0].line_type, LineType::Assistant);
    assert_eq!(tab.messages[0].content, "hi");
    assert_eq!(
        tab.messages[1].line_type,
        LineType::ToolCall {
            name: "bash".into()
        }
    );
    assert_eq!(tab.messages[1].call_id.as_deref(), Some("c1"));
}

#[test]
fn test_render_pending_tool_result_finds_tool_name_within_tab() {
    let mut tab = TabEntry::new("sid", "agent");
    tab.frames.push(tool_call("bash", "c1", r#"{}"#));
    tab.frames.push(tool_result("c1", "", "ok", false));
    tab.render_pending();
    // ToolResult is merged into the ToolCall message
    assert_eq!(tab.messages.len(), 1);
    assert_eq!(
        tab.messages[0].line_type,
        LineType::ToolCall {
            name: "bash".into()
        }
    );
    assert_eq!(tab.messages[0].tool_result.as_deref(), Some("ok"));
}

#[test]
fn test_render_pending_idempotent() {
    let mut tab = TabEntry::new("sid", "agent");
    tab.frames.push(td("hello"));
    tab.render_pending();
    assert_eq!(tab.streaming_text, "hello");
    assert_eq!(tab.rendered_up_to, 1);
    // second call: no new frames, should be noop
    tab.render_pending();
    assert_eq!(tab.streaming_text, "hello");
    assert_eq!(tab.rendered_up_to, 1);
}

#[test]
fn test_render_pending_increments_rendered_up_to() {
    let mut tab = TabEntry::new("sid", "agent");
    tab.frames.push(td("a"));
    tab.frames.push(td("b"));
    tab.frames.push(td("c"));
    tab.render_pending();
    assert_eq!(tab.rendered_up_to, 3);
}

#[test]
fn test_render_pending_done_running_to_done() {
    let mut tab = TabEntry::new("sid", "agent");
    tab.generating = true;
    tab.frames.push(done_msg());
    tab.render_pending();
    assert_eq!(tab.status, AgentStatus::Done);
    assert!(!tab.generating);
}

#[test]
fn test_render_pending_done_does_not_overwrite_error() {
    let mut tab = TabEntry::new("sid", "agent");
    tab.status = AgentStatus::Error;
    tab.generating = true;
    tab.frames.push(done_msg());
    tab.render_pending();
    assert_eq!(tab.status, AgentStatus::Error);
    assert!(!tab.generating);
}

#[test]
fn test_render_pending_done_does_not_overwrite_done() {
    let mut tab = TabEntry::new("sid", "agent");
    tab.status = AgentStatus::Done;
    tab.frames.push(done_msg());
    tab.render_pending();
    assert_eq!(tab.status, AgentStatus::Done);
}

#[test]
fn test_render_pending_error_event_updates_status_to_error() {
    let mut tab = TabEntry::new("sid", "agent");
    tab.generating = true;
    tab.frames.push(error_msg("X", "boom"));
    tab.render_pending();
    assert_eq!(tab.status, AgentStatus::Error);
    assert!(!tab.generating);
    assert_eq!(tab.messages.len(), 1);
    assert_eq!(tab.messages[0].line_type, LineType::Error);
}

#[test]
fn test_render_pending_error_then_done_status_remains_error() {
    let mut tab = TabEntry::new("sid", "agent");
    tab.frames.push(error_msg("X", "boom"));
    tab.frames.push(done_msg());
    tab.render_pending();
    assert_eq!(tab.status, AgentStatus::Error);
    assert!(!tab.generating);
}

#[test]
fn test_render_pending_done_clears_generating() {
    let mut tab = TabEntry::new("sid", "agent");
    tab.generating = true;
    tab.frames.push(done_msg());
    tab.render_pending();
    assert!(!tab.generating);
    assert_eq!(tab.status, AgentStatus::Done);
}

// ════════════════════════════════════════════════════════════
// Step 5: Message API 重构 — 类型 A（default tab）vs 类型 B（session 路由）
// ════════════════════════════════════════════════════════════

#[test]
fn test_add_message_writes_to_default_tab() {
    let mut app = AppState::new("sid".into(), "m".into(), "".into(), String::new());
    app.tab_bar.insert_sub_agent("sub-1", "agentA", false);
    app.add_message(LineType::User, "hi".into());
    assert_eq!(app.tab_bar.tabs[0].messages.len(), 1);
    assert_eq!(app.tab_bar.tabs[1].messages.len(), 0);
    assert_eq!(app.tab_bar.tabs[0].messages[0].content, "hi");
}

#[test]
fn test_add_message_writes_to_default_when_active_is_sub() {
    let mut app = AppState::new("sid".into(), "m".into(), "".into(), String::new());
    app.tab_bar.insert_sub_agent("sub-1", "agentA", false);
    // Switch active to sub tab
    app.tab_bar.active = 1;
    app.add_message(LineType::User, "hello".into());
    // Default tab gets the message
    assert_eq!(app.tab_bar.tabs[0].messages.len(), 1);
    assert_eq!(app.tab_bar.tabs[0].messages[0].content, "hello");
    // Sub tab remains empty
    assert_eq!(app.tab_bar.tabs[1].messages.len(), 0);
}

#[test]
fn test_add_message_to_session_routes_to_correct_tab() {
    let mut app = AppState::new("sid".into(), "m".into(), "".into(), String::new());
    app.tab_bar.insert_sub_agent("sub-1", "agentA", false);
    app.tab_bar.insert_sub_agent("sub-2", "agentB", false);
    app.add_message_to_session("sub-1", LineType::Assistant, "from agent".into());
    // sub-2 at index 1 (newest first), sub-1 at index 2
    assert_eq!(app.tab_bar.tabs[0].messages.len(), 0);
    assert_eq!(app.tab_bar.tabs[1].messages.len(), 0);
    assert_eq!(app.tab_bar.tabs[2].messages.len(), 1);
    assert_eq!(app.tab_bar.tabs[2].messages[0].content, "from agent");
}

#[test]
fn test_add_message_to_session_unknown_falls_back_to_default() {
    let mut app = AppState::new("sid".into(), "m".into(), "".into(), String::new());
    app.tab_bar.insert_sub_agent("sub-1", "agentA", false);
    app.add_message_to_session("unknown-sid", LineType::Status, "fallback".into());
    // Default tab gets the fallback
    assert_eq!(app.tab_bar.tabs[0].messages.len(), 1);
    assert_eq!(app.tab_bar.tabs[0].messages[0].content, "fallback");
    // Sub tab unchanged
    assert_eq!(app.tab_bar.tabs[1].messages.len(), 0);
}

#[test]
fn test_add_tool_line_to_session_routes_by_session_id() {
    let mut app = AppState::new("sid".into(), "m".into(), "".into(), String::new());
    app.tab_bar.insert_sub_agent("sub-1", "agentA", false);
    app.add_tool_line_to_session(
        "sub-1",
        LineType::ToolCall {
            name: "bash".into(),
        },
        "echo hi".into(),
        "tc_1",
    );
    assert_eq!(app.tab_bar.tabs[0].messages.len(), 0);
    assert_eq!(app.tab_bar.tabs[1].messages.len(), 1);
    assert_eq!(
        app.tab_bar.tabs[1].messages[0].call_id.as_deref(),
        Some("tc_1")
    );
}

#[test]
fn test_update_thinking_to_session_routes_by_session_id() {
    let mut app = AppState::new("sid".into(), "m".into(), "".into(), String::new());
    app.tab_bar.insert_sub_agent("sub-1", "agentA", false);
    app.update_thinking_to_session("sub-1", "thinking...".into());
    assert_eq!(app.tab_bar.tabs[0].messages.len(), 0);
    assert_eq!(app.tab_bar.tabs[1].messages.len(), 1);
    assert_eq!(
        app.tab_bar.tabs[1].messages[0].line_type,
        LineType::Thinking
    );
    assert_eq!(app.tab_bar.tabs[1].messages[0].content, "thinking...");
    // Update existing thinking
    app.update_thinking_to_session("sub-1", "updated thinking".into());
    assert_eq!(app.tab_bar.tabs[1].messages.len(), 1);
    assert_eq!(app.tab_bar.tabs[1].messages[0].content, "updated thinking");
}

#[test]
fn test_append_streaming_to_session_routes_by_session_id() {
    let mut app = AppState::new("sid".into(), "m".into(), "".into(), String::new());
    app.tab_bar.insert_sub_agent("sub-1", "agentA", false);
    app.append_streaming_to_session("sub-1", "Hello ");
    app.append_streaming_to_session("sub-1", "world");
    assert_eq!(app.tab_bar.tabs[0].streaming_text, "");
    assert_eq!(app.tab_bar.tabs[1].streaming_text, "Hello world");
}

#[test]
fn test_flush_streaming_to_session_routes_by_session_id() {
    let mut app = AppState::new("sid".into(), "m".into(), "".into(), String::new());
    app.tab_bar.insert_sub_agent("sub-1", "agentA", false);
    app.append_streaming_to_session("sub-1", "Hello world");
    app.flush_streaming_to_session("sub-1");
    // After flush, streaming_text is cleared and a message is added
    assert_eq!(app.tab_bar.tabs[0].streaming_text, "");
    assert_eq!(app.tab_bar.tabs[0].messages.len(), 0);
    assert_eq!(app.tab_bar.tabs[1].streaming_text, "");
    assert_eq!(app.tab_bar.tabs[1].messages.len(), 1);
    assert_eq!(
        app.tab_bar.tabs[1].messages[0].line_type,
        LineType::Assistant
    );
    assert_eq!(app.tab_bar.tabs[1].messages[0].content, "Hello world");
}

// ════════════════════════════════════════════════════════════
// Step 6: route_frame tests
// ════════════════════════════════════════════════════════════

fn make_text_delta_frame(sid: &str, agent_name: &str, delta: &str) -> ServerMessage {
    ServerMessage {
        payload: Some(visp_proto::visp::server_message::Payload::TextDelta(
            visp_proto::visp::TextDelta {
                delta: delta.into(),
                session_id: sid.into(),
                agent_name: agent_name.into(),
            },
        )),
    }
}

fn make_tool_call_frame(
    sid: &str,
    agent_name: &str,
    call_id: &str,
    tool: &str,
    args: &str,
) -> ServerMessage {
    ServerMessage {
        payload: Some(visp_proto::visp::server_message::Payload::ToolCall(
            visp_proto::visp::ToolCall {
                call_id: call_id.into(),
                tool_name: tool.into(),
                arguments: args.into(),
                session_id: sid.into(),
                agent_name: agent_name.into(),
            },
        )),
    }
}

fn make_done_frame(sid: &str) -> ServerMessage {
    ServerMessage {
        payload: Some(visp_proto::visp::server_message::Payload::Done(
            visp_proto::visp::Done {
                session_id: sid.into(),
            },
        )),
    }
}

fn make_status_update_frame_with_view_only(
    sid: &str,
    agent_name: &str,
    msg: &str,
    view_only: bool,
) -> ServerMessage {
    ServerMessage {
        payload: Some(server_message::Payload::StatusUpdate(
            visp_proto::visp::StatusUpdate {
                message: msg.into(),
                session_id: sid.into(),
                agent_name: agent_name.into(),
                user_inputs: vec![],
                view_only,
            },
        )),
    }
}

fn make_status_update_frame(sid: &str, agent_name: &str, msg: &str) -> ServerMessage {
    ServerMessage {
        payload: Some(visp_proto::visp::server_message::Payload::StatusUpdate(
            visp_proto::visp::StatusUpdate {
                message: msg.into(),
                session_id: sid.into(),
                agent_name: agent_name.into(),
                user_inputs: vec![],
                view_only: false,
            },
        )),
    }
}

#[test]
fn test_route_frame_text_delta_main_session() {
    let mut app = AppState::new("main-sid".into(), "m".into(), "".into(), String::new());
    let frame = make_text_delta_frame("main-sid", "", "hello");
    app.route_frame(frame);
    assert_eq!(app.tab_bar.tabs[0].frames.len(), 1);
    assert_eq!(app.tab_bar.tabs[0].streaming_text, "hello");
}

#[test]
fn test_route_frame_text_delta_sub_session_creates_tab() {
    let mut app = AppState::new("main-sid".into(), "m".into(), "".into(), String::new());
    let frame = make_text_delta_frame("sub-1", "explorer", "hello");
    app.route_frame(frame);
    // 子 agent 帧路由到 hidden_tabs，不自动创建活跃 tab
    assert_eq!(app.tab_bar.tabs.len(), 1);
    assert_eq!(app.tab_bar.hidden_tabs.len(), 1);
    let tab = &app.tab_bar.hidden_tabs[0];
    assert_eq!(tab.session_id, "sub-1");
    assert_eq!(tab.agent_name, "explorer");
    assert_eq!(tab.frames.len(), 1);
    assert!(tab.messages.is_empty());
}

#[test]
fn test_route_frame_tool_call_routes() {
    let mut app = AppState::new("main-sid".into(), "m".into(), "".into(), String::new());
    app.tab_bar.insert_sub_agent("sub-1", "agentA", false);
    let frame = make_tool_call_frame("sub-1", "agentA", "c1", "bash", "{}");
    app.route_frame(frame);
    assert_eq!(app.tab_bar.tabs[0].frames.len(), 0);
    assert_eq!(app.tab_bar.tabs[1].frames.len(), 1);
}

#[test]
fn test_route_frame_done_to_correct_tab() {
    let mut app = AppState::new("main-sid".into(), "m".into(), "".into(), String::new());
    app.tab_bar.insert_sub_agent("sub-1", "agentA", false);
    let frame = make_done_frame("sub-1");
    app.route_frame(frame);
    assert_eq!(app.tab_bar.tabs[0].frames.len(), 0);
    assert_eq!(app.tab_bar.tabs[1].frames.len(), 1);
}

#[test]
fn test_route_frame_unknown_session_uses_agent_name_as_title() {
    let mut app = AppState::new("main-sid".into(), "m".into(), "".into(), String::new());
    let frame = make_text_delta_frame("new-sid", "X", "hi");
    app.route_frame(frame);
    assert_eq!(app.tab_bar.tabs.len(), 1);
    assert_eq!(app.tab_bar.hidden_tabs.len(), 1);
    assert_eq!(app.tab_bar.hidden_tabs[0].session_id, "new-sid");
    assert_eq!(app.tab_bar.hidden_tabs[0].agent_name, "X");

    let frame2 = make_text_delta_frame("other-sid", "", "there");
    app.route_frame(frame2);
    assert_eq!(app.tab_bar.hidden_tabs.len(), 2);
    assert_eq!(app.tab_bar.hidden_tabs[1].session_id, "other-sid");
    assert_eq!(app.tab_bar.hidden_tabs[1].agent_name, "agent");
}

#[test]
fn test_route_frame_upgrades_fallback_agent_name() {
    // 首帧 agent_name 为空 → hidden tab 创建为 fallback "agent"
    let mut app = AppState::new("main-sid".into(), "m".into(), "".into(), String::new());
    let f1 = make_text_delta_frame("sub-sid", "", "first");
    app.route_frame(f1);
    assert_eq!(app.tab_bar.hidden_tabs[0].agent_name, "agent");

    // 后续帧带真实 agent_name → 在 hidden_tab 内原地升级 fallback "agent" 为真名，
    // 不恢复到活跃 tabs
    let f2 = make_text_delta_frame("sub-sid", "explorer", "second");
    app.route_frame(f2);
    assert_eq!(app.tab_bar.hidden_tabs.len(), 1);
    assert_eq!(app.tab_bar.hidden_tabs[0].agent_name, "explorer");
    assert_eq!(app.tab_bar.hidden_tabs[0].frames.len(), 2);
    assert_eq!(app.tab_bar.tabs.len(), 1);
}

#[test]
fn test_route_frame_active_tab_renders_immediately() {
    let mut app = AppState::new("main-sid".into(), "m".into(), "".into(), String::new());
    app.tab_bar.insert_sub_agent("sub-1", "agentA", false);
    app.tab_bar.active = 1;
    let frame = make_tool_call_frame("sub-1", "agentA", "c1", "bash", "{}");
    app.route_frame(frame);
    assert_eq!(app.tab_bar.tabs[1].messages.len(), 1);
    assert_eq!(
        app.tab_bar.tabs[1].messages[0].line_type,
        LineType::ToolCall {
            name: "bash".into()
        }
    );
}

#[test]
fn test_route_frame_inactive_tab_accumulates_only() {
    let mut app = AppState::new("main-sid".into(), "m".into(), "".into(), String::new());
    app.tab_bar.insert_sub_agent("sub-1", "agentA", false);
    app.tab_bar.active = 1;
    let frame = make_text_delta_frame("main-sid", "", "hello");
    app.route_frame(frame);
    assert_eq!(app.tab_bar.tabs[0].frames.len(), 1);
    assert!(app.tab_bar.tabs[0].streaming_text.is_empty());
}

#[test]
fn test_route_frame_done_updates_inactive_sub_tab_status_immediately() {
    // 子 tab 收到 Done，即使它不是 active，status 也应立刻变 Done（图标实时刷新）
    let mut app = AppState::new("main-sid".into(), "m".into(), "".into(), String::new());
    app.tab_bar.insert_sub_agent("sub-1", "agentA", false);
    // active 仍是 0 (default)
    assert_eq!(app.tab_bar.tabs[1].status, AgentStatus::Running);
    app.route_frame(make_done_frame("sub-1"));
    assert_eq!(app.tab_bar.tabs[1].status, AgentStatus::Done);
}

#[test]
fn test_route_frame_error_updates_inactive_sub_tab_status_immediately() {
    let mut app = AppState::new("main-sid".into(), "m".into(), "".into(), String::new());
    app.tab_bar.insert_sub_agent("sub-1", "agentA", false);
    assert_eq!(app.tab_bar.tabs[1].status, AgentStatus::Running);
    app.route_frame(make_error_frame("sub-1", "agentA", "X", "boom"));
    assert_eq!(app.tab_bar.tabs[1].status, AgentStatus::Error);
}

#[test]
fn test_route_frame_status_update_routes_by_session_id() {
    let mut app = AppState::new("main-sid".into(), "m".into(), "".into(), String::new());
    app.tab_bar.insert_sub_agent("sub-1", "agentA", false);
    let frame = make_status_update_frame("sub-1", "agentA", "working...");
    app.route_frame(frame);
    assert_eq!(app.tab_bar.tabs[0].frames.len(), 0);
    assert_eq!(app.tab_bar.tabs[1].frames.len(), 1);
}

#[test]
fn test_route_frame_empty_session_id_falls_back_to_default() {
    let mut app = AppState::new("main-sid".into(), "m".into(), "".into(), String::new());
    let frame = make_text_delta_frame("", "", "hello");
    app.route_frame(frame);
    assert_eq!(app.tab_bar.tabs[0].frames.len(), 1);
    assert_eq!(app.tab_bar.tabs[0].streaming_text, "hello");
}

// ════════════════════════════════════════════════════════════
// Step 6a: route_frame view_only tab creation
// ════════════════════════════════════════════════════════════

#[test]
fn route_frame_status_update_view_only_creates_view_only_tab() {
    let mut app = AppState::new("main-sid".into(), "m".into(), "".into(), String::new());
    let frame = make_status_update_frame_with_view_only("sub-1", "agentA", "hi", true);
    app.route_frame(frame);
    // ViewOnly 帧路由到 hidden_tabs，不创建活跃子 tab
    assert_eq!(app.tab_bar.tabs.len(), 1);
    assert_eq!(app.tab_bar.hidden_tabs.len(), 1);
    let hidden = app
        .tab_bar
        .hidden_tabs
        .iter()
        .find(|t| t.session_id == "sub-1")
        .unwrap();
    assert_eq!(hidden.agent_name, "agentA");
    assert_eq!(hidden.status, AgentStatus::ViewOnly);
    // 恢复后成为活跃 tab，status 保留
    assert_eq!(app.tab_bar.find_or_restore_tab("sub-1"), Some(1));
    assert_eq!(app.tab_bar.tabs[1].status, AgentStatus::ViewOnly);
}

#[test]
fn route_frame_status_update_view_false_creates_running_tab() {
    let mut app = AppState::new("main-sid".into(), "m".into(), "".into(), String::new());
    let frame = make_status_update_frame_with_view_only("sub-1", "agentA", "hi", false);
    app.route_frame(frame);
    // 非 ViewOnly 状态更新路由到 hidden_tabs（status 仍为 Running）
    assert_eq!(app.tab_bar.tabs.len(), 1);
    assert_eq!(app.tab_bar.hidden_tabs.len(), 1);
    assert_eq!(app.tab_bar.hidden_tabs[0].status, AgentStatus::Running);
}

#[test]
fn route_frame_existing_view_only_tab_not_recreated() {
    let mut app = AppState::new("main-sid".into(), "m".into(), "".into(), String::new());
    // Create sub-1 tab first
    app.tab_bar.insert_sub_agent("sub-1", "agentA", true);
    let len_before = app.tab_bar.tabs.len();
    // Send another StatusUpdate for same session
    let frame = make_status_update_frame_with_view_only("sub-1", "agentA", "updated", true);
    app.route_frame(frame);
    // Tab count unchanged (no duplicate)
    assert_eq!(app.tab_bar.tabs.len(), len_before);
    // Frame was still added to existing tab
    assert_eq!(app.tab_bar.tabs[1].frames.len(), 1);
}

#[test]
fn route_frame_user_inputs_populated_for_view_only_tab() {
    let mut app = AppState::new("main-sid".into(), "m".into(), "".into(), String::new());
    // Simulate Phase 1: input_history already populated (as handle_grpc_message would)
    app.input_history.push("my original task prompt".into());
    app.input_history.push("follow-up question".into());
    // Send StatusUpdate with view_only=true
    let frame = make_status_update_frame_with_view_only("sub-1", "agentA", "restored", true);
    app.route_frame(frame);
    // ViewOnly 帧路由到 hidden_tabs，task_prompt 已填充
    assert_eq!(app.tab_bar.hidden_tabs.len(), 1);
    let hidden = app
        .tab_bar
        .hidden_tabs
        .iter()
        .find(|t| t.session_id == "sub-1")
        .unwrap();
    assert_eq!(hidden.status, AgentStatus::ViewOnly);
    assert_eq!(
        hidden.task_prompt.as_deref(),
        Some("my original task prompt")
    );
    // 恢复后 task_prompt 保留在活跃 tab
    assert_eq!(app.tab_bar.find_or_restore_tab("sub-1"), Some(1));
    assert_eq!(
        app.tab_bar.tabs[1].task_prompt.as_deref(),
        Some("my original task prompt")
    );
}

// ════════════════════════════════════════════════════════════
// Step 6c: SessionNotActive Error frame rendering
// ════════════════════════════════════════════════════════════

#[test]
fn route_frame_error_session_not_active_renders_hint() {
    let mut app = AppState::new("main-sid".into(), "m".into(), "".into(), String::new());
    // 子 agent 错误帧默认路由到 hidden_tabs：status 置为 Error，但不渲染提示
    let frame = make_error_frame("sub-1", "agentA", "SessionNotActive", "session expired");
    app.route_frame(frame);
    assert_eq!(app.tab_bar.tabs.len(), 1);
    assert_eq!(app.tab_bar.hidden_tabs.len(), 1);
    let tab = &app.tab_bar.hidden_tabs[0];
    assert_eq!(tab.status, AgentStatus::Error);
    assert!(tab.messages.is_empty());

    // 恢复到活跃 tabs 后再发 Error -> 渲染友好提示
    app.tab_bar.find_or_restore_tab("sub-1");
    let frame2 = make_error_frame("sub-1", "agentA", "SessionNotActive", "session expired");
    app.route_frame(frame2);
    let tab = &app.tab_bar.tabs[1];
    assert_eq!(tab.status, AgentStatus::Error);
    // Should have friendly hint message (restore 时渲染的 Error 行之后)
    assert!(
        tab.messages
            .iter()
            .any(|m| m.content.contains("该会话已结束"))
    );
}

#[test]
fn route_frame_error_other_codes_unchanged() {
    let mut app = AppState::new("main-sid".into(), "m".into(), "".into(), String::new());
    app.tab_bar.insert_sub_agent("sub-1", "agentA", false);
    let frame = make_error_frame("sub-1", "agentA", "SomeOtherError", "details");
    app.route_frame(frame);
    let tab = &app.tab_bar.tabs[1];
    assert_eq!(tab.status, AgentStatus::Error);
    // No friendly hint for non-SessionNotActive
    for msg in &tab.messages {
        assert!(!msg.content.contains("该会话已结束"));
    }
}

#[test]
fn route_frame_error_routes_by_session_id() {
    let mut app = AppState::new("main-sid".into(), "m".into(), "".into(), String::new());
    app.tab_bar.insert_sub_agent("sub-1", "agentA", false);
    app.tab_bar.insert_sub_agent("sub-2", "agentB", false);
    // Error for sub-1 (index 2, newest inserts at 1)
    let frame = make_error_frame("sub-1", "agentA", "SessionNotActive", "expired");
    app.route_frame(frame);
    // Only sub-1 tab gets the error
    assert_eq!(app.tab_bar.tabs[1].session_id, "sub-2");
    assert_eq!(app.tab_bar.tabs[2].session_id, "sub-1");
    assert_eq!(app.tab_bar.tabs[1].status, AgentStatus::Running);
    assert_eq!(app.tab_bar.tabs[2].status, AgentStatus::Error);
}

// ════════════════════════════════════════════════════════════
// Step 6b: ViewOnly tab UI behavior tests
// ════════════════════════════════════════════════════════════

#[test]
fn view_only_tab_input_submission_disabled() {
    let mut app = AppState::new("sid".into(), "m".into(), "".into(), String::new());
    app.tab_bar.insert_sub_agent("sub-1", "agentA", true);
    app.tab_bar.active = 1;
    // Active tab is a ViewOnly sub-tab
    assert_eq!(app.active_tab().status, AgentStatus::ViewOnly);
    assert_ne!(app.tab_bar.active, 0);
    // The condition in handle_key_event that blocks Enter
    // (active != 0) is true, so input is blocked
}

#[test]
fn view_only_tab_shows_task_prompt_marker() {
    let mut app = AppState::new("sid".into(), "m".into(), "".into(), String::new());
    app.input_history.push("task prompt text".into());
    let frame = make_status_update_frame_with_view_only("sub-1", "agentA", "hi", true);
    app.route_frame(frame);
    // ViewOnly 帧路由到 hidden_tabs，task_prompt 已填充
    assert_eq!(app.tab_bar.hidden_tabs.len(), 1);
    let hidden = app
        .tab_bar
        .hidden_tabs
        .iter()
        .find(|t| t.session_id == "sub-1")
        .unwrap();
    assert_eq!(hidden.task_prompt.as_deref(), Some("task prompt text"));
    // 恢复后 task_prompt 保留在活跃 tab
    assert_eq!(app.tab_bar.find_or_restore_tab("sub-1"), Some(1));
    assert_eq!(
        app.tab_bar.tabs[1].task_prompt.as_deref(),
        Some("task prompt text")
    );
}

#[test]
fn view_only_tab_arrow_keys_browse_input_history() {
    let mut app = AppState::new("sid".into(), "m".into(), "".into(), String::new());
    app.input_history.push("first".into());
    app.input_history.push("second".into());
    app.input_history.push("third".into());

    // Simulate ↑ pressed while no history_index (go to last)
    let idx = app
        .history_index
        .map_or(app.input_history.len().saturating_sub(1), |i| {
            i.saturating_sub(1)
        });
    assert_eq!(idx, 2);
    app.history_index = Some(idx);
    app.textarea = AppState::new_textarea();
    app.textarea.insert_str(&app.input_history[idx]);
    assert_eq!(app.textarea.lines()[0], "third");

    // ↑ again
    let idx = app.history_index.unwrap().saturating_sub(1);
    assert_eq!(idx, 1);
    app.history_index = Some(idx);
    app.textarea = AppState::new_textarea();
    app.textarea.insert_str(&app.input_history[idx]);
    assert_eq!(app.textarea.lines()[0], "second");

    // ↓ (go forward)
    let ni = app.history_index.unwrap() + 1;
    assert_eq!(ni, 2);
    app.history_index = Some(ni);
    app.textarea = AppState::new_textarea();
    app.textarea.insert_str(&app.input_history[ni]);
    assert_eq!(app.textarea.lines()[0], "third");

    // ↓ again (past end → clear)
    let ni = app.history_index.unwrap() + 1;
    assert_eq!(ni, 3);
    app.history_index = None;
    app.textarea = AppState::new_textarea();
    assert!(app.textarea.lines()[0].is_empty());
}

// ════════════════════════════════════════════════════════════
// Step 7: Tab navigation (Alt+←/→)
// ════════════════════════════════════════════════════════════

#[test]
fn test_tabbar_activate_next_advances() {
    let mut bar = TabBar::new("main".into());
    bar.insert_sub_agent("sub1", "A", false);
    bar.insert_sub_agent("sub2", "B", false);
    assert_eq!(bar.active, 0);
    bar.activate_next();
    assert_eq!(bar.active, 1);
}

#[test]
fn test_tabbar_activate_next_at_last_wraps_to_zero() {
    let mut bar = TabBar::new("main".into());
    bar.insert_sub_agent("sub1", "A", false);
    bar.insert_sub_agent("sub2", "B", false);
    bar.active = 2;
    bar.activate_next();
    assert_eq!(bar.active, 0);
}

#[test]
fn test_tabbar_activate_prev_at_zero_wraps_to_last() {
    let mut bar = TabBar::new("main".into());
    bar.insert_sub_agent("sub1", "A", false);
    bar.insert_sub_agent("sub2", "B", false);
    assert_eq!(bar.active, 0);
    bar.activate_prev();
    assert_eq!(bar.active, 2);
}

#[test]
fn test_tabbar_activate_prev_decrements() {
    let mut bar = TabBar::new("main".into());
    bar.insert_sub_agent("sub1", "A", false);
    bar.insert_sub_agent("sub2", "B", false);
    bar.active = 2;
    bar.activate_prev();
    assert_eq!(bar.active, 1);
}

#[test]
fn test_tabbar_activate_calls_render_pending_on_target() {
    let mut bar = TabBar::new("main".into());
    bar.insert_sub_agent("sub1", "A", false);
    bar.insert_sub_agent("sub2", "B", false);
    // Push a ToolCall frame to tabs[1] — TextDelta only fills streaming_text,
    // so we use ToolCall to verify render_pending was indeed called (messages != empty)
    bar.tabs[1].frames.push(tool_call("bash", "c1", r#"{}"#));
    bar.activate(1);
    // After activate, render_pending was called, so messages should have content
    assert!(!bar.tabs[1].messages.is_empty());
    assert_eq!(
        bar.tabs[1].messages[0].line_type,
        LineType::ToolCall {
            name: "bash".into()
        }
    );
}

// ════════════════════════════════════════════════════════════
// Step 8: Token three-layer routing (L1 / L2 / L3)
// ════════════════════════════════════════════════════════════

fn make_usage_info_frame(
    sid: &str,
    input: u32,
    output: u32,
    tool_calls: u32,
    cache_create: u32,
    cache_read: u32,
) -> ServerMessage {
    ServerMessage {
        payload: Some(server_message::Payload::UsageInfo(
            visp_proto::visp::UsageInfo {
                input_tokens: input,
                output_tokens: output,
                tool_calls,
                session_id: sid.into(),
                cache_creation_input_tokens: cache_create,
                cache_read_input_tokens: cache_read,
            },
        )),
    }
}

#[test]
fn test_usage_routed_to_tab_pending_usage() {
    let mut app = AppState::new("main-sid".into(), "m".into(), "".into(), String::new());
    app.tab_bar.insert_sub_agent("sub-1", "agentA", false);
    app.apply_usage_info("sub-1", 100, 20, 3, 10, 5);
    // L1: sub tab pending_usage is set
    assert_eq!(app.tab_bar.tabs[1].pending_usage, Some((100, 20, 3, 10, 5)));
    // Default tab unchanged
    assert!(app.tab_bar.tabs[0].pending_usage.is_none());
}

#[test]
fn test_usage_accumulates_to_current_request_usage() {
    let mut app = AppState::new("main-sid".into(), "m".into(), "".into(), String::new());
    app.apply_usage_info("main-sid", 50, 10, 0, 5, 2);
    app.apply_usage_info("main-sid", 30, 8, 0, 3, 1);
    // L2 = cumulative sum
    assert_eq!(app.current_request_usage, (80, 18, 8, 3));
}

#[test]
fn test_usage_now_directly_updates_total_tokens() {
    let mut app = AppState::new("main-sid".into(), "m".into(), "".into(), String::new());
    app.apply_usage_info("main-sid", 50, 10, 0, 5, 2);
    // L3 updated directly from apply_usage_info for status bar
    assert_eq!(app.total_input_tokens, 50);
    assert_eq!(app.total_output_tokens, 10);
    assert_eq!(app.total_cache_creation_input_tokens, 5);
    assert_eq!(app.total_cache_read_input_tokens, 2);
}

#[test]
fn test_done_default_displays_l2_and_clears() {
    let mut app = AppState::new("main-sid".into(), "m".into(), "".into(), String::new());
    app.current_request_usage = (100, 50, 20, 10);
    app.apply_done_token_settlement("main-sid");
    // L2 is cleared
    assert_eq!(app.current_request_usage, (0, 0, 0, 0));
    // No Usage message added (token footer is appended in render_pending)
    assert!(app.tab_bar.tabs[0].messages.is_empty());
}

#[test]
fn test_done_default_clears_l2_only() {
    let mut app = AppState::new("main-sid".into(), "m".into(), "".into(), String::new());
    // L2 was accumulated from UsageInfo (which also updated L3)
    app.current_request_usage = (100, 50, 20, 10);
    // L3 was already set by apply_usage_info
    app.total_input_tokens = 100;
    app.total_output_tokens = 50;
    app.total_cache_creation_input_tokens = 20;
    app.total_cache_read_input_tokens = 10;
    // Done clears L2, does NOT touch L3 (already done by apply_usage_info)
    app.apply_done_token_settlement("main-sid");
    assert_eq!(app.current_request_usage, (0, 0, 0, 0));
    assert_eq!(app.total_input_tokens, 100);
    assert_eq!(app.total_output_tokens, 50);
    assert_eq!(app.total_cache_creation_input_tokens, 20);
    assert_eq!(app.total_cache_read_input_tokens, 10);
}

#[test]
fn test_done_sub_does_not_consume_pending_usage() {
    let mut app = AppState::new("main-sid".into(), "m".into(), "".into(), String::new());
    app.tab_bar.insert_sub_agent("sub-1", "agentA", false);
    app.tab_bar.tabs[1].pending_usage = Some((200, 30, 5, 15, 8));
    app.current_request_usage = (999, 999, 999, 999); // arbitrary L2
    app.apply_done_token_settlement("sub-1");
    // Sub tab: pending_usage is NOT consumed here (render_pending handles it)
    assert_eq!(app.tab_bar.tabs[1].pending_usage, Some((200, 30, 5, 15, 8)));
    // No Usage message added
    assert!(app.tab_bar.tabs[1].messages.is_empty());
    // L2 unchanged
    assert_eq!(app.current_request_usage, (999, 999, 999, 999));
    // L3 unchanged
    assert_eq!(app.total_input_tokens, 0);
}

#[test]
fn test_done_sub_does_not_clear_l2() {
    let mut app = AppState::new("main-sid".into(), "m".into(), "".into(), String::new());
    app.tab_bar.insert_sub_agent("sub-1", "agentA", false);
    app.tab_bar.tabs[1].pending_usage = Some((10, 5, 1, 2, 3));
    app.current_request_usage = (50, 25, 10, 5);
    app.apply_done_token_settlement("sub-1");
    // L2 preserved
    assert_eq!(app.current_request_usage, (50, 25, 10, 5));
}

#[test]
fn test_user_input_clears_current_request_usage() {
    let mut app = AppState::new("main-sid".into(), "m".into(), "".into(), String::new());
    app.apply_usage_info("main-sid", 100, 50, 0, 20, 10);
    assert_eq!(app.current_request_usage, (100, 50, 20, 10));
    // Simulate user input clearing L2
    app.current_request_usage = (0, 0, 0, 0);
    assert_eq!(app.current_request_usage, (0, 0, 0, 0));
}

#[test]
fn test_done_status_guard_blocks_token_settlement() {
    let mut app = AppState::new("main-sid".into(), "m".into(), "".into(), String::new());
    app.tab_bar.tabs[0].status = AgentStatus::Error;
    app.current_request_usage = (100, 50, 20, 10);
    app.apply_done_token_settlement("main-sid");
    // No token line added
    assert!(app.tab_bar.tabs[0].messages.is_empty());
    // L3 unchanged
    assert_eq!(app.total_input_tokens, 0);
    // L2 unchanged
    assert_eq!(app.current_request_usage, (100, 50, 20, 10));
}

// ── TabBar pagination tests ─────────────────────────────────────

fn make_tab_bar_with_subs(n: usize) -> TabBar {
    let mut tb = TabBar::new("main".into());
    for i in 0..n {
        tb.insert_sub_agent(format!("sub-{}", i), format!("agent{}", i), false);
    }
    tb
}

#[test]
fn test_layout_pages_default_always_first() {
    let tb = make_tab_bar_with_subs(10);
    let pages = tb.layout_pages(80);
    for range in &pages {
        assert!(
            !range.contains(&0),
            "Page {:?} contains default tab 0",
            range
        );
    }
}

#[test]
fn test_layout_pages_single_page_when_fits() {
    let tb = make_tab_bar_with_subs(2);
    let pages = tb.layout_pages(80);
    assert_eq!(pages.len(), 1);
    assert_eq!(pages[0], 1..3); // subs at indices 1,2
}

#[test]
fn test_layout_pages_multi_page_when_overflow() {
    let tb = make_tab_bar_with_subs(10);
    let pages = tb.layout_pages(80);
    assert_eq!(pages.len(), 3); // 4+4+2
    assert_eq!(pages[0], 1..5);
    assert_eq!(pages[1], 5..9);
    assert_eq!(pages[2], 9..11);
}

#[test]
fn test_alt_shift_right_advances_page() {
    let mut tb = make_tab_bar_with_subs(10);
    assert_eq!(tb.page_start, 0);
    let result = tb.next_page(80);
    assert!(result);
    assert_eq!(tb.page_start, 1);
}

#[test]
fn test_alt_shift_left_at_zero_stops() {
    let mut tb = make_tab_bar_with_subs(10);
    tb.page_start = 0;
    let result = tb.prev_page();
    assert!(!result);
    assert_eq!(tb.page_start, 0);
}

#[test]
fn test_alt_shift_right_at_last_stops() {
    let mut tb = make_tab_bar_with_subs(10);
    tb.page_start = 2; // last page (0-indexed, 3 pages total)
    let result = tb.next_page(80);
    assert!(!result);
    assert_eq!(tb.page_start, 2);
}

#[test]
fn test_select_idx_in_visible_when_active_in_page() {
    let mut tb = make_tab_bar_with_subs(8);
    tb.page_start = 0; // subs 1-4 visible
    tb.active = 2; // "sub-6" at index 2
    let idx = tb.select_idx_for_current_page(80);
    // visible = [default, sub[1], sub[2]] → idx = 1 + (2-1) = 2
    assert_eq!(idx, Some(2));
}

#[test]
fn test_active_tab_change_auto_scrolls_to_visible_page() {
    let mut tb = make_tab_bar_with_subs(8);
    tb.active = 6; // on page 1 (subs 5-8)
    tb.page_start = 0; // wrong page
    tb.ensure_active_visible(80);
    assert_eq!(tb.page_start, 1); // page containing index 6
}

// ════════════════════════════════════════════════════════════
// Step 11: Ctrl+W close sub-agent tab
// ════════════════════════════════════════════════════════════

fn make_tab_bar_with_done_subs(n: usize) -> TabBar {
    let mut tb = TabBar::new("main".into());
    for i in 0..n {
        tb.insert_sub_agent(format!("sub-{}", i), format!("agent{}", i), false);
    }
    for tab in tb.tabs.iter_mut().skip(1) {
        tab.status = AgentStatus::Done;
    }
    tb
}

#[test]
fn test_ctrl_w_on_default_is_noop() {
    let mut tb = TabBar::new("main".into());
    tb.insert_sub_agent("sub-1", "agentA", false);
    // active is 0 (default)
    assert!(!tb.close_active());
    assert_eq!(tb.tabs.len(), 2);
    assert_eq!(tb.active, 0);
}

#[test]
fn test_ctrl_w_on_running_sub_closes_tab() {
    let mut tb = TabBar::new("main".into());
    tb.insert_sub_agent("sub-1", "agentA", false);
    tb.active = 1;
    // status defaults to Running, but closing is now allowed
    assert_eq!(tb.tabs[1].status, AgentStatus::Running);
    assert!(tb.close_active());
    assert_eq!(tb.tabs.len(), 1);
}

#[test]
fn test_ctrl_w_on_done_sub_removes_tab() {
    let mut tb = make_tab_bar_with_done_subs(1);
    tb.active = 1;
    assert!(tb.close_active());
    assert_eq!(tb.tabs.len(), 1); // only default remains
    assert_eq!(tb.active, 0);
}

#[test]
fn test_ctrl_w_on_error_sub_removes_tab() {
    let mut tb = TabBar::new("main".into());
    tb.insert_sub_agent("sub-1", "agentA", false);
    tb.tabs[1].status = AgentStatus::Error;
    tb.active = 1;
    assert!(tb.close_active());
    assert_eq!(tb.tabs.len(), 1);
}

#[test]
fn test_ctrl_w_activates_previous_tab() {
    let mut tb = make_tab_bar_with_done_subs(3);
    // tabs: [default, sub-2(Done), sub-1(Done), sub-0(Done)]
    tb.active = 2; // sub-1 (index 2)
    assert!(tb.close_active());
    // After remove: [default, sub-2, sub-0]; active decrements to 1 → sub-2
    assert_eq!(tb.active, 1);
    assert_eq!(tb.tabs[tb.active].session_id, "sub-2");
}

#[test]
fn test_ctrl_w_at_last_sub_falls_back_to_default() {
    let mut tb = make_tab_bar_with_done_subs(1);
    tb.active = 1;
    assert!(tb.close_active());
    assert_eq!(tb.tabs.len(), 1);
    assert_eq!(tb.active, 0);
}

#[test]
fn test_ctrl_w_renders_pending_for_new_active() {
    let mut tb = make_tab_bar_with_done_subs(2);
    // tabs: [default, sub-1(Done), sub-0(Done)]
    tb.active = 2; // sub-0
    // Push a frame to sub-1 (index 1) — will become active after close
    tb.tabs[1].frames.push(tool_call("bash", "c1", r#"{}"#));
    assert!(tb.close_active());
    // After close: active=1 → sub-1, render_pending was called
    assert!(!tb.tabs[1].messages.is_empty());
    assert_eq!(
        tb.tabs[1].messages[0].line_type,
        LineType::ToolCall {
            name: "bash".into()
        }
    );
}

#[test]
fn test_ctrl_w_adjusts_tab_page() {
    // 5 subs → 2 pages (PER_PAGE=4): page 0 = indices 1-4, page 1 = index 5
    let mut tb = make_tab_bar_with_done_subs(5);
    tb.active = 5; // last sub, on page 1
    tb.page_start = 1;
    tb.last_term_width = 80;
    assert!(tb.close_active());
    // After close: active=4, which falls in page 0 (indices 1-4)
    assert_eq!(tb.page_start, 0);
    assert_eq!(tb.active, 4);
    assert_eq!(tb.tabs.len(), 5); // default + 4 subs
}

#[test]
fn test_ctrl_w_closed_session_can_reopen_on_new_event() {
    let mut app = AppState::new("main-sid".into(), "m".into(), "".into(), String::new());
    // 首帧路由到 hidden_tabs，不自动创建活跃 tab
    let frame = make_text_delta_frame("sub-1", "agentA", "hello");
    app.route_frame(frame);
    assert_eq!(app.tab_bar.hidden_tabs.len(), 1);

    // 恢复到活跃 tabs
    app.tab_bar.find_or_restore_tab("sub-1");
    assert_eq!(app.tab_bar.tabs.len(), 2);

    // Set sub to Done and close it（移入 closed_tabs）
    app.tab_bar.tabs[1].status = AgentStatus::Done;
    app.tab_bar.active = 1;
    assert!(app.tab_bar.close_active());
    assert_eq!(app.tab_bar.tabs.len(), 1);
    assert_eq!(app.tab_bar.closed_tabs.len(), 1);

    // 关闭后再来帧：不自动恢复到活跃 tabs，帧路由到 hidden_tabs 原地更新
    let frame2 = make_text_delta_frame("sub-1", "agentA", "world");
    app.route_frame(frame2);
    assert_eq!(app.tab_bar.tabs.len(), 1);
    assert_eq!(app.tab_bar.closed_tabs.len(), 1);
    assert_eq!(app.tab_bar.hidden_tabs.len(), 1);
    assert_eq!(app.tab_bar.hidden_tabs[0].session_id, "sub-1");
    assert_eq!(app.tab_bar.hidden_tabs[0].frames.len(), 1);

    // 手动 find_or_restore_tab 恢复（从 closed_tabs），重新打开成功
    let idx = app
        .tab_bar
        .find_or_restore_tab("sub-1")
        .expect("restore failed");
    assert_eq!(idx, 1);
    assert_eq!(app.tab_bar.tabs.len(), 2);
    assert_eq!(app.tab_bar.tabs[1].session_id, "sub-1");
    assert_eq!(app.tab_bar.tabs[1].frames.len(), 1);
}

// ════════════════════════════════════════════════════════════
// Step 12: end-to-end integration tests
// ════════════════════════════════════════════════════════════

fn make_error_frame(sid: &str, agent_name: &str, code: &str, message: &str) -> ServerMessage {
    ServerMessage {
        payload: Some(server_message::Payload::Error(visp_proto::visp::Error {
            code: code.into(),
            message: message.into(),
            session_id: sid.into(),
            agent_name: agent_name.into(),
        })),
    }
}

#[test]
fn test_e2e_spawn_subagent_creates_tab() {
    let mut app = AppState::new("main".into(), "m".into(), "".into(), String::new());
    let frame = make_text_delta_frame("sub1", "explorer", "hello");
    app.route_frame(frame);
    // 帧暂存于 hidden_tabs，不自动创建活跃 tab
    assert_eq!(app.tab_bar.tabs.len(), 1);
    assert_eq!(app.tab_bar.hidden_tabs.len(), 1);
    assert_eq!(app.tab_bar.hidden_tabs[0].session_id, "sub1");
    assert_eq!(app.tab_bar.hidden_tabs[0].agent_name, "explorer");
    assert_eq!(app.tab_bar.active, 0);

    // find_or_restore_tab 将其打开为活跃 tab (index 1)
    let idx = app.tab_bar.find_or_restore_tab("sub1").unwrap();
    assert_eq!(idx, 1);
    assert_eq!(app.tab_bar.tabs[1].session_id, "sub1");
    assert_eq!(app.tab_bar.tabs[1].agent_name, "explorer");
}

#[test]
fn test_e2e_subagent_done_changes_status_color() {
    let mut app = AppState::new("main".into(), "m".into(), "".into(), String::new());
    app.tab_bar.insert_sub_agent("sub1", "agentA", false);
    assert_eq!(app.tab_bar.tabs[1].status, AgentStatus::Running);
    // Tab must be active for route_frame to auto-render the Done frame
    app.tab_bar.active = 1;
    let frame = make_done_frame("sub1");
    app.route_frame(frame);
    assert_eq!(app.tab_bar.tabs[1].status, AgentStatus::Done);
}

#[test]
fn test_e2e_subagent_error_status_guards_done() {
    let mut app = AppState::new("main".into(), "m".into(), "".into(), String::new());
    app.tab_bar.insert_sub_agent("sub1", "agentA", false);
    app.tab_bar.active = 1;
    // Error frame changes status to Error
    let err_frame = make_error_frame("sub1", "agentA", "ERR", "oops");
    app.route_frame(err_frame);
    assert_eq!(app.tab_bar.tabs[1].status, AgentStatus::Error);
    // Done frame should NOT override Error status (guard: only Running → Done)
    let done_frame = make_done_frame("sub1");
    app.route_frame(done_frame);
    assert_eq!(app.tab_bar.tabs[1].status, AgentStatus::Error);
}

#[test]
fn test_e2e_subagent_inactive_does_not_pollute_default() {
    let mut app = AppState::new("main".into(), "m".into(), "".into(), String::new());
    // active defaults to 0; sub frames should go to hidden_tabs, not default
    let f1 = make_text_delta_frame("sub1", "agentA", "hello");
    let f2 = make_tool_call_frame("sub1", "agentA", "c1", "bash", "{}");
    app.route_frame(f1);
    app.route_frame(f2);
    // Default tab untouched
    assert_eq!(app.tab_bar.tabs.len(), 1);
    assert_eq!(app.tab_bar.tabs[0].frames.len(), 0);
    // Sub frames accumulate in hidden_tabs
    let sub = app
        .tab_bar
        .hidden_tabs
        .iter()
        .find(|t| t.session_id == "sub1")
        .expect("sub1 hidden tab not found");
    assert_eq!(sub.frames.len(), 2);
}

#[test]
fn test_e2e_switch_to_sub_renders_accumulated() {
    let mut app = AppState::new("main".into(), "m".into(), "".into(), String::new());
    // Accumulate 4 TextDelta + 1 ToolCall (ToolCall creates a message)
    for i in 0..4 {
        let frame = make_text_delta_frame("sub1", "agentA", &format!("delta{}", i));
        app.route_frame(frame);
    }
    let tc = make_tool_call_frame("sub1", "agentA", "c1", "bash", "{}");
    app.route_frame(tc);
    // Frames accumulate in hidden_tabs（不自动创建活跃 tab）
    assert_eq!(app.tab_bar.tabs.len(), 1);
    let sub = app
        .tab_bar
        .hidden_tabs
        .iter()
        .find(|t| t.session_id == "sub1")
        .expect("sub1 hidden tab not found");
    assert_eq!(sub.frames.len(), 5);
    // 手动恢复并激活 → 渲染所有累积帧
    let idx = app
        .tab_bar
        .find_or_restore_tab("sub1")
        .expect("restore failed");
    app.tab_bar.activate(idx);
    // After rendering: messages populated, all frames processed
    assert!(!app.tab_bar.tabs[idx].messages.is_empty());
    assert_eq!(app.tab_bar.tabs[idx].rendered_up_to, 5);
}

#[test]
fn test_e2e_sub_tab_shows_own_data_not_main() {
    let mut app = AppState::new("main-sid".into(), "m".into(), "".into(), String::new());
    // 主 tab 收到 TextDelta
    let main_frame = make_text_delta_frame("main-sid", "", "main-delta");
    app.route_frame(main_frame);
    assert!(app.tab_bar.tabs[0].streaming_text.contains("main-delta"));

    // 子 agent 收到 TextDelta（帧暂存于 hidden_tabs）
    let sub_frame = make_text_delta_frame("sub-1", "agentA", "sub-delta");
    app.route_frame(sub_frame);
    assert_eq!(app.tab_bar.hidden_tabs.len(), 1);
    assert!(app.tab_bar.hidden_tabs[0].streaming_text.is_empty()); // 隐藏未渲染
    assert_eq!(app.tab_bar.hidden_tabs[0].frames.len(), 1);

    // 子 agent 更多帧
    let sub_frame2 = make_text_delta_frame("sub-1", "agentA", " more");
    app.route_frame(sub_frame2);

    // 恢复到活跃 tabs 并切换到子 tab
    app.tab_bar.find_or_restore_tab("sub-1");
    app.tab_bar.activate(1);

    // 子 tab 应显示子 agent 数据，而非主 agent 数据
    assert!(
        !app.tab_bar.tabs[1].streaming_text.contains("main-delta"),
        "sub tab 不应包含主 agent 数据"
    );
    assert!(
        app.tab_bar.tabs[1].streaming_text.contains("sub-delta"),
        "sub tab 应包含子 agent 数据"
    );
    assert_eq!(app.tab_bar.tabs[1].streaming_text, "sub-delta more");

    // 主 tab 的 streaming_text 应保持不变
    assert_eq!(app.tab_bar.tabs[0].streaming_text, "main-delta");
}

#[test]
fn test_e2e_sub_tab_tool_call_messages_independent() {
    let mut app = AppState::new("main-sid".into(), "m".into(), "".into(), String::new());
    // 主 tab 收到 ToolCall（活跃，立即渲染）
    let main_tc = make_tool_call_frame("main-sid", "", "main-c1", "read", "{}");
    app.route_frame(main_tc);
    assert_eq!(app.tab_bar.tabs[0].messages.len(), 1);

    // 子 agent 收到 ToolCall（路由到 hidden_tabs，只累积帧）
    let sub_tc = make_tool_call_frame("sub-1", "agentA", "sub-c1", "bash", "{}");
    app.route_frame(sub_tc);
    assert_eq!(app.tab_bar.hidden_tabs[0].messages.len(), 0); // 隐藏未渲染
    assert_eq!(app.tab_bar.hidden_tabs[0].frames.len(), 1);

    // 恢复到活跃 tabs 并切换到子 tab
    app.tab_bar.find_or_restore_tab("sub-1");
    app.tab_bar.activate(1);

    // 子 tab 的 messages 应该是子 agent 的 ToolCall
    assert_eq!(app.tab_bar.tabs[1].messages.len(), 1);
    assert_eq!(
        app.tab_bar.tabs[1].messages[0].line_type,
        LineType::ToolCall {
            name: "bash".into()
        }
    );
    assert_eq!(
        app.tab_bar.tabs[1].messages[0].call_id,
        Some("sub-c1".into())
    );

    // 主 tab 的 messages 应保持不变
    assert_eq!(app.tab_bar.tabs[0].messages.len(), 1);
    assert_eq!(
        app.tab_bar.tabs[0].messages[0].call_id,
        Some("main-c1".into())
    );
}

#[test]
fn test_e2e_sub_agent_generating_flag() {
    let mut app = AppState::new("main-sid".into(), "m".into(), "".into(), String::new());
    let frame = make_text_delta_frame("sub-1", "agentA", "hello");
    app.route_frame(frame);
    // 帧暂存于 hidden_tabs（status Running）
    assert_eq!(app.tab_bar.hidden_tabs[0].status, AgentStatus::Running);
    // 恢复到活跃 tabs 后，子 agent 正在运行，generating 应为 true
    app.tab_bar.find_or_restore_tab("sub-1");
    assert!(
        app.tab_bar.tabs[1].generating,
        "子 agent tab 的 generating 应为 true"
    );
}

#[test]
fn test_e2e_token_l1_preserved_for_render_pending() {
    let mut app = AppState::new("main".into(), "m".into(), "".into(), String::new());
    app.tab_bar.insert_sub_agent("sub1", "agentA", false);
    // Route UsageInfo for sub (L1)
    app.apply_usage_info("sub1", 100, 200, 5, 10, 20);
    assert_eq!(
        app.tab_bar.tabs[1].pending_usage,
        Some((100, 200, 5, 10, 20))
    );
    // Route Done for sub → pending_usage preserved (render_pending handles it)
    app.apply_done_token_settlement("sub1");
    // L1 preserved for render_pending
    assert_eq!(
        app.tab_bar.tabs[1].pending_usage,
        Some((100, 200, 5, 10, 20))
    );
    // No Usage message added (render_pending appends to assistant text)
    assert!(app.tab_bar.tabs[1].messages.is_empty());
    // Sub-agent tokens NOT accumulated to L2 (orchestrator forwards them separately)
    assert_eq!(app.current_request_usage, (0, 0, 0, 0));
    // L3 NOT updated for sub-agent (orchestrator forwards via parent session_id)
    assert_eq!(app.total_input_tokens, 0);
    assert_eq!(app.total_output_tokens, 0);
    assert_eq!(app.total_cache_creation_input_tokens, 0);
    assert_eq!(app.total_cache_read_input_tokens, 0);
}

#[test]
fn test_e2e_token_l2_l3_only_on_default_done() {
    let mut app = AppState::new("main".into(), "m".into(), "".into(), String::new());
    // Two UsageInfo frames for main session
    app.apply_usage_info("main", 50, 80, 2, 5, 10);
    app.apply_usage_info("main", 30, 40, 1, 3, 5);
    // L2 accumulated (input, output, cache_create, cache_read)
    assert_eq!(app.current_request_usage, (80, 120, 8, 15));
    // Done for main → L2 cleared, L3 updated
    app.apply_done_token_settlement("main");
    // L2 cleared
    assert_eq!(app.current_request_usage, (0, 0, 0, 0));
    // L3 accumulated
    assert_eq!(app.total_input_tokens, 80);
    assert_eq!(app.total_output_tokens, 120);
    assert_eq!(app.total_cache_creation_input_tokens, 8);
    assert_eq!(app.total_cache_read_input_tokens, 15);
    // No Usage message added (token footer is appended in render_pending)
    assert!(app.tab_bar.tabs[0].messages.is_empty());
}

#[test]
fn test_e2e_no_sub_prefix_in_messages() {
    let mut app = AppState::new("main".into(), "m".into(), "".into(), String::new());
    let frame = make_text_delta_frame("sub1", "agentA", "hello world");
    app.route_frame(frame);
    // 恢复到活跃 tabs 并切换到子 tab 以渲染
    app.tab_bar.find_or_restore_tab("sub1");
    app.tab_bar.activate(1);
    // No message should contain the "[sub:" prefix (removed in Step 3)
    for msg in &app.tab_bar.tabs[1].messages {
        assert!(
            !msg.content.contains("[sub:"),
            "Message content should not contain [sub: prefix, got: {}",
            msg.content
        );
    }
}
// ════════════════════════════════════════════════════════════
// route_frame: 子 agent 首帧将主 tab 的 ToolCall 转为 AgentCall
// ════════════════════════════════════════════════════════════

#[test]
fn test_route_frame_sub_agent_first_frame_converts_toolcall_to_agentcall() {
    let mut app = AppState::new("main".into(), "m".into(), "".into(), String::new());
    // 主 tab 收到 ToolCall (explorer)
    app.route_frame(tool_call(
        "explorer",
        "call-1",
        r#"{"prompt":"Find TODOs"}"#,
    ));
    // 确认是 ToolCall
    assert_eq!(
        app.tab_bar.tabs[0].messages[0].line_type,
        LineType::ToolCall {
            name: "explorer".into()
        }
    );
    assert!(app.tab_bar.tabs[0].messages[0].sub_session_id.is_none());

    // 子 agent 首帧到达，agent_name = "explorer"
    app.route_frame(make_text_delta_frame("sub-sess-abc", "explorer", "hello"));

    // 主 tab 的 ToolCall 应被转为 AgentCall，且 sub_session_id 被设置
    assert_eq!(
        app.tab_bar.tabs[0].messages[0].line_type,
        LineType::AgentCall {
            name: "explorer".into()
        }
    );
    assert_eq!(
        app.tab_bar.tabs[0].messages[0].sub_session_id.as_deref(),
        Some("sub-sess-abc")
    );
}

#[test]
fn test_route_frame_sub_agent_does_not_convert_completed_toolcall() {
    let mut app = AppState::new("main".into(), "m".into(), "".into(), String::new());
    // ToolCall + ToolResult (已完成，无子 agent 帧)
    app.route_frame(tool_call("explorer", "call-1", r#"{"prompt":"test"}"#));
    app.route_frame(tool_result("call-1", "explorer", "done", false));

    // 无子 agent 帧到达 -> 不应被转换
    assert_eq!(
        app.tab_bar.tabs[0].messages[0].line_type,
        LineType::ToolCall {
            name: "explorer".into()
        }
    );
    assert!(app.tab_bar.tabs[0].messages[0].sub_session_id.is_none());

    // 子 agent 帧到达 -> 即使 ToolResult 已完成也应升级为 AgentCall
    app.route_frame(make_text_delta_frame("sub-sess-abc", "explorer", "hello"));
    assert_eq!(
        app.tab_bar.tabs[0].messages[0].line_type,
        LineType::AgentCall {
            name: "explorer".into()
        }
    );
    assert_eq!(
        app.tab_bar.tabs[0].messages[0].sub_session_id.as_deref(),
        Some("sub-sess-abc")
    );
}

#[test]
fn test_route_frame_thinking_first_then_text_upgrades_toolcall() {
    // 子 agent 首帧是 ThinkingBlock（无 agent_name），
    // 第二帧 TextDelta 带 agent_name，应延迟升级 ToolCall -> AgentCall
    let mut app = AppState::new("main".into(), "m".into(), "".into(), String::new());
    app.route_frame(tool_call(
        "explorer",
        "call-1",
        r#"{"prompt":"Find TODOs"}"#,
    ));

    // 首帧：ThinkingBlock（agent_name 为空）
    let thinking = visp_proto::visp::ServerMessage {
        payload: Some(visp_proto::visp::server_message::Payload::ThinkingBlock(
            visp_proto::visp::ThinkingBlock {
                thinking: "thinking...".into(),
                signature: String::new(),
                session_id: "sub-sess-xyz".into(),
            },
        )),
    };
    app.route_frame(thinking);

    // 首帧后：tab 已创建但 ToolCall 尚未升级
    assert_eq!(
        app.tab_bar.tabs[0].messages[0].line_type,
        LineType::ToolCall {
            name: "explorer".into()
        }
    );
    assert!(app.tab_bar.tabs[0].messages[0].sub_session_id.is_none());

    // 第二帧：TextDelta 带 agent_name = "explorer"
    app.route_frame(make_text_delta_frame("sub-sess-xyz", "explorer", "hello"));

    // 延迟升级应已发生
    assert_eq!(
        app.tab_bar.tabs[0].messages[0].line_type,
        LineType::AgentCall {
            name: "explorer".into()
        }
    );
    assert_eq!(
        app.tab_bar.tabs[0].messages[0].sub_session_id.as_deref(),
        Some("sub-sess-xyz")
    );
}

#[test]
fn test_route_frame_sets_task_prompt_on_sub_tab() {
    let mut app = AppState::new("main".into(), "m".into(), "".into(), String::new());
    app.route_frame(tool_call(
        "explorer",
        "call-1",
        r#"{"prompt":"Find all TODOs in the codebase"}"#,
    ));
    app.route_frame(make_text_delta_frame("sub-sess-abc", "explorer", "hello"));

    // 子 tab 创建时 task_prompt 应已设置（帧路由到 hidden_tabs）
    let sub_tab = app
        .tab_bar
        .hidden_tabs
        .iter()
        .find(|t| t.session_id == "sub-sess-abc")
        .unwrap();
    assert_eq!(
        sub_tab.task_prompt.as_deref(),
        Some("Find all TODOs in the codebase")
    );
}

#[test]
fn test_route_frame_task_prompt_set_via_delayed_upgrade() {
    // ThinkingBlock 首帧 -> 延迟升级 -> task_prompt 应被设置
    let mut app = AppState::new("main".into(), "m".into(), "".into(), String::new());
    app.route_frame(tool_call(
        "explorer",
        "call-1",
        r#"{"prompt":"Delayed prompt"}"#,
    ));

    let thinking = visp_proto::visp::ServerMessage {
        payload: Some(visp_proto::visp::server_message::Payload::ThinkingBlock(
            visp_proto::visp::ThinkingBlock {
                thinking: "thinking...".into(),
                signature: String::new(),
                session_id: "sub-delayed".into(),
            },
        )),
    };
    app.route_frame(thinking);

    // 首帧后：hidden tab 已创建但 task_prompt 尚未设置
    assert_eq!(app.tab_bar.hidden_tabs.len(), 1);
    assert!(app.tab_bar.hidden_tabs[0].task_prompt.is_none());

    // 第二帧带 agent_name -> 在 hidden_tab 上延迟设置 task_prompt，不恢复活跃 tabs
    app.route_frame(make_text_delta_frame("sub-delayed", "explorer", "hello"));
    assert_eq!(app.tab_bar.tabs.len(), 1);
    let hidden = app
        .tab_bar
        .hidden_tabs
        .iter()
        .find(|t| t.session_id == "sub-delayed")
        .expect("sub-delayed hidden tab not found");
    assert_eq!(hidden.task_prompt.as_deref(), Some("Delayed prompt"));
}

#[test]
fn test_render_pending_tool_result_merges_into_agentcall() {
    let mut tab = TabEntry::new("main", "agent");
    // 模拟 ToolCall 已被转为 AgentCall（由 route_frame 完成）
    tab.push_chat_line(
        LineType::AgentCall {
            name: "explorer".into(),
        },
        r#"{"prompt":"test"}"#.into(),
        Some("call-1".into()),
    );
    tab.messages[0].sub_session_id = Some("sub-abc".into());
    // ToolResult 到达
    tab.frames
        .push(tool_result("call-1", "explorer", "found 3 TODOs", false));
    tab.render_pending();

    assert_eq!(tab.messages.len(), 1);
    assert_eq!(
        tab.messages[0].line_type,
        LineType::AgentCall {
            name: "explorer".into()
        }
    );
    assert_eq!(tab.messages[0].sub_session_id.as_deref(), Some("sub-abc"));
    assert_eq!(
        tab.messages[0].tool_result.as_deref(),
        Some("found 3 TODOs")
    );
    assert!(!tab.messages[0].tool_error);
}

#[test]
fn test_button_hit_after_completion() {
    let mut app = AppState::new("main".into(), "m".into(), "".into(), String::new());
    app.route_frame(tool_call("explorer", "call-1", r#"{"prompt":"test"}"#));
    app.route_frame(make_text_delta_frame("sub-1", "explorer", "hello"));
    app.route_frame(make_done_frame("sub-1"));
    app.route_frame(tool_result("call-1", "explorer", "done", false));

    // 主 tab 的 ToolCall 应已升级为 AgentCall，sub_session_id 已设置
    let main_msg = &app.tab_bar.tabs[0].messages[0];
    assert_eq!(
        main_msg.line_type,
        LineType::AgentCall {
            name: "explorer".into()
        }
    );
    assert_eq!(main_msg.sub_session_id.as_deref(), Some("sub-1"));

    let render_w = 80u16;
    crate::ui::ensure_all_caches(&mut app, render_w);

    // 点击按钮区域应命中 (AGENT_CALL_STYLE: top_margin=1, margin_vertical=1 -> header at y+2)
    let result = crate::tool_ui::agent_open_tab_hit_test(
        &app.messages(),
        &app.message_caches,
        2,  // virtual_row = header 行 (y + top_margin + margin_vertical)
        70, // column 在按钮区域内
        render_w,
    );
    assert_eq!(result.as_deref(), Some("sub-1"));

    // find_or_restore_tab 应从 hidden_tabs 恢复
    assert_eq!(app.tab_bar.hidden_tabs.len(), 1);
    let idx = app.tab_bar.find_or_restore_tab("sub-1").unwrap();
    assert_eq!(idx, 1);
    assert_eq!(app.tab_bar.tabs.len(), 2);
    assert_eq!(app.tab_bar.hidden_tabs.len(), 0);
}
