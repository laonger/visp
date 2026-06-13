#![allow(dead_code)]

/// 写入 /tmp/visp-cli-debug.log，用于诊断 textarea 折行/粘贴等问题。
/// 不在生产环境启用，无性能影响（为空时直接返回）。
#[macro_export]
macro_rules! debug_log {
    ($($arg:tt)*) => {{
        #[cfg(debug_assertions)]
        {
            use std::io::Write;
            let _ = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open("/tmp/visp-cli-debug.log")
                .and_then(|mut f| {
                    let ts = chrono::Local::now().format("%H:%M:%S%.3f");
                    writeln!(f, "[{ts}] {}", format!($($arg)*))
                });
        }
    }};
}

use crate::app::{AppState, ConfirmState, LineType};
use crate::client::{ChatHandle, VbwClient};
use crate::ui::render;
use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers, MouseEventKind};
use std::io::{self, Write};
use visp_proto::visp::{LlmConfig, server_message};

/// 将一段文本插入到 textarea 中（模拟逐字输入）
/// 注意：`\n` 必须映射为 `Key::Enter`，否则 ratatui_textarea 会丢弃换行符前的内容
fn paste_text(textarea: &mut ratatui_textarea::TextArea<'static>, text: &str) {
    let has_cr = text.contains('\r');
    let _newline_count = text.chars().filter(|&c| c == '\n' || c == '\r').count();
    debug_log!(
        "paste: len={}, newlines={}, contains_cr={has_cr}",
        text.len(),
        _newline_count
    );
    // 统一换行符：\r\n → \n, \r → \n
    // 终端粘贴可能携带 \r\n(Windows) 或 \r(旧 Mac)，不处理会导致多余空行或光标回行首覆盖
    let text = if has_cr {
        text.replace("\r\n", "\n").replace('\r', "\n")
    } else {
        text.to_string()
    };
    for c in text.chars() {
        if c == '\n' {
            textarea.input(ratatui_textarea::Input {
                key: ratatui_textarea::Key::Enter,
                ctrl: false,
                alt: false,
                shift: false,
            });
        } else {
            textarea.input(ratatui_textarea::Input {
                key: ratatui_textarea::Key::Char(c),
                ctrl: false,
                alt: false,
                shift: false,
            });
        }
    }
}

/// Drop guard: 离开作用域时保证恢复终端状态
struct TerminalGuard;

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = crossterm::terminal::disable_raw_mode();
        // 关闭 mouse mode 1000 + 1006 和 bracketed paste mode 2004
        let _ = write!(io::stdout(), "\x1b[?1000l\x1b[?1006l\x1b[?2004l");
        let _ = io::stdout().flush();
    }
}

pub async fn run(
    session_id: String,
    mut chat_handle: ChatHandle,
    model: String,
    client: &mut VbwClient,
    project_path: &str,
    available_models: Vec<String>,
    model_names: Vec<String>,
) -> io::Result<()> {
    if let Ok((_w, _h)) = crossterm::terminal::size() {
        debug_log!("session start: {_w}x{_h}, model={model}");
    }
    crossterm::terminal::enable_raw_mode()?;
    // 只启用 mouse mode 1000（按钮点击事件），保留拖拽给终端做原生选择复制
    // 不启用 1002/1003，这样 drag 不会拦截终端选择
    write!(io::stdout(), "\x1b[?1000h\x1b[?1006h\x1b[?2004h")?;
    io::stdout().flush()?;
    let _guard = TerminalGuard;
    let mut terminal = ratatui::init();
    let mut app = AppState::new(session_id.clone(), model);
    app.available_models = available_models;
    app.model_names = model_names;

    // exit 信号：键盘线程检测到 Ctrl+D 时通知主循环无条件退出
    let (exit_tx, mut exit_rx) = tokio::sync::watch::channel(false);
    let thread_exit_tx = exit_tx.clone();

    // 键盘事件采集线程，同时负责 Ctrl+D 退出检测
    let (key_tx, mut key_rx) = tokio::sync::mpsc::unbounded_channel::<Event>();
    std::thread::spawn(move || {
        loop {
            if key_tx.is_closed() {
                break;
            }
            if crossterm::event::poll(std::time::Duration::from_millis(100)).unwrap_or(false)
                && let Ok(event) = crossterm::event::read()
            {
                // 独立检测 Ctrl+D：无论任何状态，无条件触发退出
                if matches!(event, Event::Key(KeyEvent { code: KeyCode::Char('d'), modifiers, .. })
                    if modifiers.contains(KeyModifiers::CONTROL))
                {
                    let _ = thread_exit_tx.send(true);
                    break;
                }
                // MouseMoved 在源头过滤
                if matches!(event, Event::Mouse(ref m) if m.kind == MouseEventKind::Moved) {
                    continue;
                }
                if key_tx.send(event).is_err() {
                    break;
                }
            }
        }
    });
    // 主线程的 exit_tx 后续不再使用，drop 后 thread_exit_tx 是唯一 sender
    drop(exit_tx);

    let _ = terminal.draw(|f| render(&mut app, f));

    // Points spinner 动画 tick：generating 期间每 140ms 推进一帧
    let mut spinner_tick = tokio::time::interval(std::time::Duration::from_millis(140));

    loop {
        tokio::select! {
            event = key_rx.recv() => {
                match event {
                    Some(e) => { if handle_key_event(e, &mut app, &mut chat_handle) { break; } }
                    None => break,
                }
            }
            msg = chat_handle.recv() => {
                match msg {
                    Some(msg) => handle_grpc_message(msg, &mut app, &chat_handle),
                    None => { app.should_quit = true; }
                }
            }
            _ = exit_rx.changed() => {
                // Ctrl+D: 先发 Cancel 让 daemon 优雅停止，再断开连接
                chat_handle.send_cancel();
                break;
            }
            _ = spinner_tick.tick() => {
                // 仅 generating 期间推进 spinner 帧并请求重绘
                if app.generating {
                    app.spinner_frame = app.spinner_frame.wrapping_add(1);
                    app.needs_render = true;
                }
            }
        }
        if app.should_quit {
            break;
        }

        // 处理 /new 命令：创建新 session 并替换 chat_handle 的 session_id
        if app.pending_new_session {
            match client.create_session(project_path, None).await {
                Ok(session) => {
                    chat_handle.send_cancel();
                    chat_handle.session_id = session.session_id.clone();
                    let model = session.model.clone();
                    app.reset_for_new_session(session.session_id, model);
                    app.add_message(
                        LineType::Status,
                        "New session started. Use /help for available commands.".into(),
                    );
                }
                Err(e) => {
                    app.add_message(
                        LineType::Error,
                        format!("Failed to create new session: {e}"),
                    );
                    app.pending_new_session = false;
                }
            }
        }

        // 处理 /list 命令：创建交互式 session 选择器
        if app.pending_list_sessions {
            match client.list_sessions().await {
                Ok(sessions) => {
                    if sessions.is_empty() {
                        app.add_message(LineType::Status, "No sessions found.".into());
                    } else {
                        let labels: Vec<String> = sessions
                            .iter()
                            .map(|s| {
                                let short_id: String = s.session_id.chars().take(8).collect();
                                let status_str = match s.status {
                                    0 => "IDLE",
                                    1 => "RUNNING",
                                    2 => "COMPLETED",
                                    3 => "ERROR",
                                    _ => "UNKNOWN",
                                };
                                let last_msg = s.last_user_message.as_str();
                                format!("  {short_id}  {status_str:>9}  {last_msg}")
                            })
                            .collect();
                        let session_ids: Vec<String> =
                            sessions.iter().map(|s| s.session_id.clone()).collect();
                        let mut state = ratatui::widgets::ListState::default();
                        state.select(Some(0));
                        app.session_select = Some(crate::app::SessionSelectState {
                            labels,
                            state,
                            session_ids,
                        });
                        app.needs_render = true;
                    }
                }
                Err(e) => {
                    app.add_message(LineType::Error, format!("Failed to list sessions: {e}"));
                }
            }
            app.pending_list_sessions = false;
        }

        // 处理 /model 命令：创建交互式模型选择器
        if app.pending_model_select {
            if !app.available_models.is_empty() {
                let display_labels = app.available_models.clone();
                let model_names = if app.model_names.is_empty() {
                    app.available_models.clone()
                } else {
                    app.model_names.clone()
                };
                let mut state = ratatui::widgets::ListState::default();
                state.select(Some(0));
                app.model_select = Some(crate::app::ModelSelectState {
                    display_labels,
                    model_names,
                    state,
                });
                app.needs_render = true;
            }
            app.pending_model_select = false;
        }

        // 处理 /sessions <id> 命令：切换到指定 session
        if let Some(ref target_id) = app.pending_switch_session.clone() {
            match client.get_session(target_id).await {
                Ok(session) => {
                    chat_handle.send_cancel();
                    let full_id = session.session_id.clone();
                    let model = session.model.clone();
                    let short: String = full_id.chars().take(8).collect();
                    chat_handle.session_id = full_id.clone();
                    app.reset_for_new_session(full_id, model);
                    chat_handle.send_join();
                    app.add_message(LineType::Status, format!("Switched to session {short}"));
                }
                Err(e) => {
                    app.add_message(LineType::Error, format!("Session not found: {e}"));
                    app.pending_switch_session = None;
                }
            }
        }

        if app.needs_render {
            // 确认状态始终需要渲染，不受流节流影响
            if app.generating && app.confirm.is_none() && !app.try_begin_stream_render() {
                app.needs_render = false;
            }
            if app.needs_render {
                let _ = terminal.draw(|f| render(&mut app, f));
                app.needs_render = false;
            }
        }
    }

    ratatui::restore();
    Ok(())
}

fn handle_key_event(event: Event, app: &mut AppState, chat_handle: &mut ChatHandle) -> bool {
    // 选择模式下：↑↓ 导航，Enter 确认，Esc/q 退出
    if let Some(ref mut ss) = app.session_select {
        if let Event::Key(key) = event {
            match key.code {
                KeyCode::Up | KeyCode::Char('k') => {
                    ss.state.select_previous();
                    app.needs_render = true;
                    return false;
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    ss.state.select_next();
                    app.needs_render = true;
                    return false;
                }
                KeyCode::Enter => {
                    if let Some(idx) = ss.state.selected()
                        && idx < ss.session_ids.len()
                    {
                        let target_id = ss.session_ids[idx].clone();
                        app.session_select = None;
                        app.streaming_text.clear();
                        app.generating = false;
                        app.stale_done_expected = false;
                        app.current_request_id = None;
                        app.confirm = None;
                        app.pending_switch_session = Some(target_id);
                        app.add_message(LineType::Status, "Switching session...".into());
                    }
                    app.needs_render = true;
                    return false;
                }
                KeyCode::Esc | KeyCode::Char('q') => {
                    app.session_select = None;
                    app.needs_render = true;
                    return false;
                }
                _ => {}
            }
        }
        // 选择模式下拦截所有其他按键
        return false;
    }

    // 模型选择模式下：↑↓ 导航，Enter 确认，Esc/q 退出
    if let Some(ref mut ms) = app.model_select {
        if let Event::Key(key) = event {
            match key.code {
                KeyCode::Up | KeyCode::Char('k') => {
                    ms.state.select_previous();
                    app.needs_render = true;
                    return false;
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    ms.state.select_next();
                    app.needs_render = true;
                    return false;
                }
                KeyCode::Enter => {
                    if let Some(ms) = app.model_select.take()
                        && let Some(idx) = ms.state.selected()
                        && idx < ms.model_names.len()
                    {
                        let model_key = ms.model_names[idx].clone();
                        app.model = ms.display_labels[idx].clone();
                        chat_handle.send_config_update(LlmConfig {
                            model: Some(model_key),
                            temperature: None,
                            max_tokens: None,
                            max_context_tokens: None,
                            extra: Default::default(),
                        });
                        app.add_message(LineType::Status, format!("Model set to {}", app.model));
                    }
                    app.needs_render = true;
                    return false;
                }
                KeyCode::Esc | KeyCode::Char('q') => {
                    app.model_select = None;
                    app.needs_render = true;
                    return false;
                }
                _ => {}
            }
        }
        // 模型选择模式下拦截所有其他按键
        return false;
    }

    app.needs_render = true;
    match event {
        Event::Key(key) => {
            // Alt+M 或 Ctrl+M: 切换鼠标捕获模式，任何时候都生效
            // Ctrl+M 在许多终端等价于 Enter，同时检查两种可能
            if (key.code == KeyCode::Char('m')
                && (key.modifiers.contains(KeyModifiers::ALT)
                    || key.modifiers.contains(KeyModifiers::CONTROL)))
                || (key.code == KeyCode::Enter && key.modifiers.contains(KeyModifiers::CONTROL))
            {
                toggle_mouse_mode(app);
                return false;
            }

            // F1: 切换帮助弹窗
            if key.code == KeyCode::F(1) {
                app.show_help = !app.show_help;
                return false;
            }

            // 帮助弹窗打开时，按任意键关闭（F1 在上面已经处理了切换）
            if app.show_help {
                app.show_help = false;
                return false;
            }

            if app.confirm.is_some() {
                // Ctrl+C 在确认模式下也能取消
                if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
                    if app.generating {
                        if let Some(q) = app.confirm.take() {
                            chat_handle.send_response(&q.query_id, 1, "");
                        }
                        app.stale_done_expected = true;
                        // 保留 assistant 已输出的消息，移除末尾的 [USER_QUERY] 标记
                        if let Some(close_pos) = app.streaming_text.rfind("[/USER_QUERY]")
                            && let Some(open_pos) =
                                app.streaming_text[..close_pos].rfind("[USER_QUERY")
                        {
                            app.streaming_text.truncate(open_pos);
                        }
                        app.flush_streaming();
                        app.pending_usage = None;
                        app.current_request_id = None;
                        app.generating = false;
                        chat_handle.send_cancel();
                    }
                    return false;
                }
                match key.code {
                    KeyCode::Left => {
                        if let Some(ref mut confirm) = app.confirm {
                            let total = if confirm.options.is_empty() {
                                3 // Approve, Deny, Always Allow
                            } else {
                                confirm.options.len() + if confirm.allow_other { 1 } else { 0 }
                            };
                            if confirm.other_active {
                                confirm.other_active = false;
                            } else if confirm.selected_index == 0 {
                                confirm.selected_index = total.saturating_sub(1);
                            } else {
                                confirm.selected_index -= 1;
                            }
                        }
                    }
                    KeyCode::Right => {
                        if let Some(ref mut confirm) = app.confirm {
                            let total = if confirm.options.is_empty() {
                                3 // Approve, Deny, Always Allow
                            } else {
                                confirm.options.len() + if confirm.allow_other { 1 } else { 0 }
                            };
                            if confirm.other_active {
                                confirm.other_active = false;
                            } else {
                                let next = confirm.selected_index + 1;
                                if next >= total {
                                    confirm.selected_index = 0;
                                } else {
                                    confirm.selected_index = next;
                                }
                            }
                        }
                    }
                    KeyCode::Enter => {
                        if let Some(ref mut confirm) = app.confirm {
                            if confirm.other_active {
                                let q = app.confirm.take().unwrap();
                                let text = app.textarea.lines().join("\n");
                                app.textarea = AppState::new_textarea();
                                chat_handle.send_response(&q.query_id, -1, &text);
                            } else if confirm.allow_other {
                                let opts_len = if confirm.options.is_empty() {
                                    3 // Approve, Deny, Always Allow
                                } else {
                                    confirm.options.len()
                                };
                                if confirm.selected_index == opts_len {
                                    confirm.other_active = true;
                                    return false;
                                }
                                let q = app.confirm.take().unwrap();
                                chat_handle.send_response(&q.query_id, q.selected_index as i32, "");
                            } else {
                                let q = app.confirm.take().unwrap();
                                chat_handle.send_response(&q.query_id, q.selected_index as i32, "");
                            }
                        }
                    }
                    KeyCode::Esc => {
                        if let Some(ref mut confirm) = app.confirm {
                            if confirm.other_active {
                                confirm.other_active = false;
                                app.textarea = AppState::new_textarea();
                            } else {
                                let q = app.confirm.take().unwrap();
                                chat_handle.send_response(&q.query_id, 1, "");
                                if app.generating {
                                    app.stale_done_expected = true;
                                    app.streaming_text.clear();
                                    app.pending_usage = None;
                                    app.current_request_id = None;
                                    app.generating = false;
                                    chat_handle.send_cancel();
                                }
                            }
                        }
                    }
                    _ => {
                        if let Some(ref confirm) = app.confirm {
                            if confirm.other_active {
                                app.textarea.input(build_input_from_key(key));
                            } else {
                                app.needs_render = false;
                            }
                        }
                    }
                }
                return false;
            }
            // Ctrl+C: 取消正在生成的请求
            if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
                if app.generating {
                    app.stale_done_expected = true;
                    app.streaming_text.clear();
                    app.pending_usage = None;
                    app.current_request_id = None;
                    app.generating = false;
                    chat_handle.send_cancel();
                }
                return false;
            }
            // F2 已在键盘线程处理，此处不再需要
            if app.generating {
                return false;
            }
            if key.code == KeyCode::Enter {
                if key.modifiers.contains(KeyModifiers::ALT) {
                    app.textarea.insert_newline();
                    return false;
                }
                let text: String = app.textarea.lines().join("\n");
                app.textarea = AppState::new_textarea();
                if text.trim().is_empty() {
                    return false;
                }
                if text.starts_with('/') {
                    handle_command(&text, app, chat_handle);
                } else {
                    app.add_message(LineType::User, text.clone());
                    app.input_history.push(text.clone());
                    app.history_index = None;
                    app.generating = true;
                    app.scroll_following = true;
                    let rid = chat_handle.send_input(&text);
                    app.current_request_id = Some(rid);
                }
                return false;
            }
            match key.code {
                KeyCode::Tab => {
                    let current = app
                        .textarea
                        .lines()
                        .first()
                        .map(|s| s.to_string())
                        .unwrap_or_default();
                    if current.starts_with('/') {
                        let cmds = [
                            "/clear",
                            "/help",
                            "/list",
                            "/sessions ",
                            "/temp ",
                            "/model ",
                            "/init",
                            "/mouse",
                        ];
                        let completion = if current.len() > 1 {
                            cmds.iter()
                                .find(|c| c.starts_with(&current))
                                .map(|c| c.to_string())
                        } else {
                            None
                        };
                        if let Some(cmd) = completion {
                            app.textarea = AppState::new_textarea();
                            app.textarea.insert_str(&cmd);
                        }
                    }
                    return false;
                }
                KeyCode::Up => {
                    if !app.input_history.is_empty() {
                        let idx = app
                            .history_index
                            .map_or(app.input_history.len().saturating_sub(1), |i| {
                                i.saturating_sub(1)
                            });
                        app.history_index = Some(idx);
                        app.textarea = AppState::new_textarea();
                        app.textarea.insert_str(&app.input_history[idx]);
                    }
                }
                KeyCode::Down => {
                    if let Some(idx) = app.history_index {
                        let ni = idx + 1;
                        if ni >= app.input_history.len() {
                            app.history_index = None;
                            app.textarea = AppState::new_textarea();
                        } else {
                            app.history_index = Some(ni);
                            app.textarea = AppState::new_textarea();
                            app.textarea.insert_str(&app.input_history[ni]);
                        }
                    }
                }
                KeyCode::PageUp => {
                    let y = app.scroll_state.offset().y;
                    app.scroll_state
                        .set_offset(ratatui::layout::Position::new(0, y.saturating_sub(10)));
                    app.scroll_following = false;
                }
                KeyCode::PageDown => {
                    let y = app.scroll_state.offset().y;
                    app.scroll_state
                        .set_offset(ratatui::layout::Position::new(0, y.saturating_add(10)));
                }
                _ => {
                    let input = build_input_from_key(key);
                    debug_log!(
                        "key -> textarea: {:?} (lines={})",
                        input.key,
                        app.textarea.lines().len()
                    );
                    app.textarea.input(input);
                }
            }
        }
        // 帮助弹窗打开时，鼠标点击关闭
        Event::Mouse(m)
            if m.kind == MouseEventKind::Down(crossterm::event::MouseButton::Left)
                && app.show_help =>
        {
            app.show_help = false;
            return false;
        }
        // 状态栏左键点击切换鼠标模式（底部区域，右半侧）
        Event::Mouse(m) if m.kind == MouseEventKind::Down(crossterm::event::MouseButton::Left) => {
            if let Ok((term_cols, term_rows)) = crossterm::terminal::size() {
                // 状态栏在最底部 (0-indexed: term_rows - 1)
                // 放宽到最后 3 行 + 右侧 1/3，兼容各种布局偏移
                let bottom_start = term_rows.saturating_sub(3);
                let right_start = (term_cols * 2 / 3).max(term_cols.saturating_sub(20));
                if m.row >= bottom_start && m.column >= right_start {
                    toggle_mouse_mode(app);
                    return false;
                }
            }
        }
        Event::Mouse(m) => match m.kind {
            MouseEventKind::ScrollUp => {
                if app.try_begin_scroll() {
                    app.scroll_state.scroll_up();
                    app.scroll_state.scroll_up();
                    app.scroll_state.scroll_up();
                    app.scroll_following = false;
                } else {
                    app.needs_render = false;
                }
            }
            MouseEventKind::ScrollDown => {
                if app.try_begin_scroll() {
                    app.scroll_state.scroll_down();
                    app.scroll_state.scroll_down();
                    app.scroll_state.scroll_down();
                } else {
                    app.needs_render = false;
                }
            }
            _ => {}
        },
        // 处理终端粘贴事件（bracketed paste）
        Event::Paste(text) if app.confirm.as_ref().is_none_or(|c| c.other_active) => {
            debug_log!("paste event received: len={}", text.len());
            paste_text(&mut app.textarea, &text);
        }
        _ => {}
    }
    false
}

fn handle_grpc_message(
    msg: visp_proto::visp::ServerMessage,
    app: &mut AppState,
    chat_handle: &ChatHandle,
) {
    app.needs_render = true;
    match msg.payload {
        Some(server_message::Payload::TextDelta(delta)) => app.append_streaming(&delta.delta),
        Some(server_message::Payload::ToolCall(tc)) => {
            app.flush_streaming();
            let args_display = tc_display(&tc);
            app.add_tool_line(
                LineType::ToolCall {
                    name: tc.tool_name.clone(),
                },
                args_display,
                &tc.call_id,
            );
        }
        Some(server_message::Payload::ToolResult(tr)) => {
            app.flush_streaming();
            // 查找 tool_name：优先从 proto 取，fallback 找匹配的 ToolCall
            let tool_name = if !tr.tool_name.is_empty() {
                tr.tool_name.clone()
            } else {
                app.messages
                    .iter()
                    .find(|m| {
                        matches!(&m.line_type, LineType::ToolCall { .. })
                            && m.call_id.as_deref() == Some(&tr.call_id)
                    })
                    .and_then(|m| {
                        if let LineType::ToolCall { name } = &m.line_type {
                            Some(name.clone())
                        } else {
                            None
                        }
                    })
                    .unwrap_or_default()
            };
            app.add_tool_line(
                if tr.is_error {
                    LineType::ToolError { name: tool_name }
                } else {
                    LineType::ToolResult { name: tool_name }
                },
                tr.content,
                &tr.call_id,
            );
        }
        Some(server_message::Payload::ThinkingBlock(tb)) => {
            let text = format!("[Thinking] {}", tb.thinking);
            app.update_thinking(text)
        }
        Some(server_message::Payload::UsageInfo(ui)) => {
            app.pending_usage = Some((
                ui.input_tokens,
                ui.output_tokens,
                ui.tool_calls,
                ui.cache_creation_input_tokens,
                ui.cache_read_input_tokens,
            ));
            app.total_input_tokens += ui.input_tokens;
            app.total_output_tokens += ui.output_tokens;
            app.total_cache_creation_input_tokens += ui.cache_creation_input_tokens;
            app.total_cache_read_input_tokens += ui.cache_read_input_tokens;
        }
        Some(server_message::Payload::StatusUpdate(su)) => {
            app.add_message(LineType::Status, su.message);
            // 加载 session 历史中的用户输入到 input_history（↑↓ 翻找历史提问）
            if !su.user_inputs.is_empty() {
                for input in &su.user_inputs {
                    if !app.input_history.contains(input) {
                        app.input_history.push(input.clone());
                    }
                }
            }
        }
        Some(server_message::Payload::UserQuery(uq)) => {
            app.confirm = Some(ConfirmState {
                query_id: uq.query_id,
                message: uq.message,
                options: uq.options,
                allow_other: uq.allow_other,
                selected_index: 0,
                other_active: false,
            });
        }
        Some(server_message::Payload::Error(err)) => {
            // Skip stale Error from cancelled request (cancel sends Error, not Done)
            if app.stale_done_expected {
                app.stale_done_expected = false;
                return;
            }
            app.add_message(LineType::Error, format!("{}: {}", err.code, err.message));
            app.generating = false;
            app.current_request_id = None;
        }
        Some(server_message::Payload::Done(_)) => {
            if app.stale_done_expected {
                app.stale_done_expected = false;
                return;
            }
            if let Some((it, ot, tc, ccit, crit)) = app.pending_usage.take() {
                let time = chrono::Local::now().format("%H:%M:%S");
                let usage = if ccit > 0 || crit > 0 {
                    format!(
                        "\n\n[{} | Tokens: {} in / {} out | Cache: {} create / {} read | Tools: {}]",
                        time, it, ot, ccit, crit, tc
                    )
                } else {
                    format!(
                        "\n\n[{} | Tokens: {} in / {} out | Tools: {}]",
                        time, it, ot, tc
                    )
                };
                app.streaming_text.push_str(&usage);
            }
            app.flush_streaming();
            app.generating = false;
            if let Some(rid) = app.current_request_id.take() {
                chat_handle.send_ack(rid);
            }
        }
        None => {}
    }
}

/// 格式化工具调用参数显示
fn tc_display(tc: &visp_proto::visp::ToolCall) -> String {
    match serde_json::from_str::<serde_json::Value>(&tc.arguments) {
        Ok(serde_json::Value::Object(obj)) => {
            let vals: Vec<String> = obj
                .values()
                .filter_map(|v| v.as_str().map(|s| format!("\"{}\"", s)))
                .collect();
            if vals.is_empty() {
                format!("{} {}", tc.tool_name, tc.arguments)
            } else {
                format!("{}: {}", tc.tool_name, vals.join(" "))
            }
        }
        _ => format!("{}: {}", tc.tool_name, tc.arguments),
    }
}

fn handle_command(text: &str, app: &mut AppState, chat_handle: &mut ChatHandle) {
    let parts: Vec<&str> = text.splitn(2, ' ').collect();
    match parts[0] {
        "/clear" => app.clear_messages(),
        "/help" => {
            app.show_help = !app.show_help;
        }
        "/new" => {
            // 标记需要创建新 session，主循环中处理（需要 async 调用 client.create_session）
            app.streaming_text.clear();
            app.generating = false;
            app.stale_done_expected = false;
            app.current_request_id = None;
            app.confirm = None;
            app.pending_new_session = true;
            app.add_message(LineType::Status, "Creating new session...".into());
        }
        "/init" => {
            app.add_message(LineType::User, text.to_string());
            app.generating = true;
            app.scroll_following = true;
            chat_handle.send_input(text);
        }
        "/mouse" => {
            toggle_mouse_mode(app);
        }
        "/list" => {
            app.add_message(LineType::Status, "Fetching sessions...".into());
            app.pending_list_sessions = true;
        }
        "/sessions" => {
            if parts.len() >= 2 && !parts[1].is_empty() {
                let target = parts[1].to_string();
                app.add_message(
                    LineType::Status,
                    format!("Switching to session {target}..."),
                );
                app.streaming_text.clear();
                app.generating = false;
                app.stale_done_expected = false;
                app.current_request_id = None;
                app.confirm = None;
                app.pending_switch_session = Some(target);
            } else {
                // 无参数：同 /list
                app.add_message(LineType::Status, "Fetching sessions...".into());
                app.pending_list_sessions = true;
            }
        }
        "/temp" if parts.len() >= 2 => {
            if let Ok(temp) = parts[1].parse::<f64>() {
                chat_handle.send_config_update(LlmConfig {
                    model: None,
                    temperature: Some(temp),
                    max_tokens: None,
                    max_context_tokens: None,
                    extra: Default::default(),
                });
                app.add_message(LineType::Status, format!("Temperature set to {}", temp));
            }
        }
        "/model" => {
            if app.available_models.is_empty() {
                app.add_message(LineType::Status, "No alternate models configured".into());
            } else {
                app.add_message(LineType::Status, "Select a model:".into());
                app.pending_model_select = true;
            }
        }
        _ => {}
    }
}

fn build_input_from_key(key: KeyEvent) -> ratatui_textarea::Input {
    ratatui_textarea::Input {
        key: match key.code {
            KeyCode::Char(c) => ratatui_textarea::Key::Char(c),
            KeyCode::Enter => ratatui_textarea::Key::Enter,
            KeyCode::Backspace => ratatui_textarea::Key::Backspace,
            KeyCode::Delete => ratatui_textarea::Key::Delete,
            KeyCode::Left => ratatui_textarea::Key::Left,
            KeyCode::Right => ratatui_textarea::Key::Right,
            KeyCode::Up => ratatui_textarea::Key::Up,
            KeyCode::Down => ratatui_textarea::Key::Down,
            KeyCode::Tab => ratatui_textarea::Key::Tab,
            KeyCode::Esc => ratatui_textarea::Key::Esc,
            KeyCode::Home => ratatui_textarea::Key::Home,
            KeyCode::End => ratatui_textarea::Key::End,
            _ => ratatui_textarea::Key::Char(' '),
        },
        ctrl: key.modifiers.contains(KeyModifiers::CONTROL),
        alt: key.modifiers.contains(KeyModifiers::ALT),
        shift: key.modifiers.contains(KeyModifiers::SHIFT),
    }
}

fn toggle_mouse_mode(app: &mut AppState) {
    app.mouse_captured = !app.mouse_captured;
    let _ = if app.mouse_captured {
        use std::io::Write;
        write!(io::stdout(), "\x1b[?1000h\x1b[?1006h")
    } else {
        use std::io::Write;
        write!(io::stdout(), "\x1b[?1000l\x1b[?1006l")
    };
    let _ = io::stdout().flush();
    let mode = if app.mouse_captured {
        "Mouse"
    } else {
        "Select"
    };
    app.add_message(LineType::Status, format!("Mouse mode: {mode}"));
}
