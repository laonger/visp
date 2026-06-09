#![allow(dead_code)]

use crate::app::{AppState, ConfirmState, LineType};
use crate::client::ChatHandle;
use crate::ui::render;
use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers, MouseEventKind};
use std::io::{self, Write};
use vbw_proto::vibewisp::{LlmConfig, server_message};

/// Drop guard: 离开作用域时保证恢复终端状态
struct TerminalGuard;

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = crossterm::terminal::disable_raw_mode();
        // 仅关闭 mouse mode 1000 + 1006（和启用时一致）
        let _ = write!(io::stdout(), "\x1b[?1000l\x1b[?1006l");
        let _ = io::stdout().flush();
    }
}

pub async fn run(session_id: String, mut chat_handle: ChatHandle, model: String) -> io::Result<()> {
    crossterm::terminal::enable_raw_mode()?;
    // 只启用 mouse mode 1000（按钮点击事件），保留拖拽给终端做原生选择复制
    // 不启用 1002/1003，这样 drag 不会拦截终端选择
    write!(io::stdout(), "\x1b[?1000h\x1b[?1006h")?;
    io::stdout().flush()?;
    let _guard = TerminalGuard;
    let mut terminal = ratatui::init();
    let mut app = AppState::new(session_id.clone(), model);

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
        }
        if app.should_quit {
            break;
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
    app.needs_render = true;
    match event {
        Event::Key(key) => {
            if app.confirm.is_some() {
                // Ctrl+C 在确认模式下也能取消
                if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
                    if app.generating {
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
                                app.textarea = ratatui_textarea::TextArea::default();
                                app.textarea.set_placeholder_text("Type your message...");
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
                                app.textarea = ratatui_textarea::TextArea::default();
                                app.textarea.set_placeholder_text("Type your message...");
                            } else {
                                let q = app.confirm.take().unwrap();
                                chat_handle.send_response(&q.query_id, 1, "");
                                if app.generating {
                                    app.streaming_text.clear();
                                    app.pending_usage = None;
                                    app.current_request_id = None;
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
                app.textarea = ratatui_textarea::TextArea::default();
                app.textarea.set_placeholder_text("Type your message...");
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
                        let cmds = ["/clear", "/help", "/temp ", "/model ", "/init", "/mouse"];
                        let completion = if current.len() > 1 {
                            cmds.iter()
                                .find(|c| c.starts_with(&current))
                                .map(|c| c.to_string())
                        } else {
                            None
                        };
                        if let Some(cmd) = completion {
                            app.textarea = ratatui_textarea::TextArea::default();
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
                        app.textarea = ratatui_textarea::TextArea::default();
                        app.textarea.insert_str(&app.input_history[idx]);
                    }
                }
                KeyCode::Down => {
                    if let Some(idx) = app.history_index {
                        let ni = idx + 1;
                        if ni >= app.input_history.len() {
                            app.history_index = None;
                            app.textarea = ratatui_textarea::TextArea::default();
                            app.textarea.set_placeholder_text("Type your message...");
                        } else {
                            app.history_index = Some(ni);
                            app.textarea = ratatui_textarea::TextArea::default();
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
                    app.textarea.input(build_input_from_key(key));
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
        _ => {}
    }
    false
}

fn handle_grpc_message(
    msg: vbw_proto::vibewisp::ServerMessage,
    app: &mut AppState,
    chat_handle: &ChatHandle,
) {
    app.needs_render = true;
    match msg.payload {
        Some(server_message::Payload::TextDelta(delta)) => app.append_streaming(&delta.delta),
        Some(server_message::Payload::ToolCall(tc)) => {
            app.flush_streaming();
            let args_display = tc_display(&tc);
            app.add_tool_line(LineType::ToolCall, args_display, &tc.call_id);
        }
        Some(server_message::Payload::ToolResult(tr)) => app.insert_tool_result(
            &tr.call_id,
            format!(
                "{}: {}",
                if tr.is_error { "Error" } else { "Output" },
                tr.content
            ),
        ),
        Some(server_message::Payload::ThinkingBlock(tb)) => {
            app.flush_streaming();
            let text = format!("[Thinking] {}", tb.thinking);
            app.add_message(LineType::Thinking, text)
        }
        Some(server_message::Payload::UsageInfo(ui)) => {
            app.pending_usage = Some((ui.input_tokens, ui.output_tokens, ui.tool_calls));
        }
        Some(server_message::Payload::StatusUpdate(su)) => {
            app.add_message(LineType::Status, su.message)
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
            app.add_message(LineType::Error, format!("{}: {}", err.code, err.message));
            app.generating = false;
            app.current_request_id = None;
        }
        Some(server_message::Payload::Done(_)) => {
            if let Some((it, ot, tc)) = app.pending_usage.take() {
                let time = chrono::Local::now().format("%H:%M:%S");
                let usage = format!(
                    "\n\n[{} | Tokens: {} in / {} out | Tools: {}]",
                    time, it, ot, tc
                );
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
fn tc_display(tc: &vbw_proto::vibewisp::ToolCall) -> String {
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
            app.add_message(
                LineType::Status,
                "/clear /temp <val> /model <name> /mouse /init /help".into(),
            );
        }
        "/init" => {
            app.add_message(LineType::User, text.to_string());
            app.generating = true;
            app.scroll_following = true;
            chat_handle.send_input(text);
        }
        "/mouse" => {
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
        "/temp" if parts.len() >= 2 => {
            if let Ok(temp) = parts[1].parse::<f64>() {
                chat_handle.send_config_update(LlmConfig {
                    model: None,
                    temperature: Some(temp),
                    max_tokens: None,
                    extra: Default::default(),
                });
                app.add_message(LineType::Status, format!("Temperature set to {}", temp));
            }
        }
        "/model" if parts.len() >= 2 => {
            chat_handle.send_config_update(LlmConfig {
                model: Some(parts[1].to_string()),
                temperature: None,
                max_tokens: None,
                extra: Default::default(),
            });
            app.add_message(LineType::Status, format!("Model set to {}", parts[1]));
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
