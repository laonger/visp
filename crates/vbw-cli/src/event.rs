#![allow(dead_code)]

use crate::app::{AppState, ConfirmState, LineType};
use crate::client::ChatHandle;
use crate::ui::render;
use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers, MouseEventKind};
use std::io;
use vbw_proto::vibewisp::{LlmConfig, server_message};

pub async fn run(session_id: String, mut chat_handle: ChatHandle, model: String) -> io::Result<()> {
    crossterm::terminal::enable_raw_mode()?;
    crossterm::execute!(io::stdout(), crossterm::event::EnableMouseCapture)?;
    let mut terminal = ratatui::init();
    let mut app = AppState::new(session_id.clone(), model);

    // 独立长驻键盘任务，不随 select! 取消（解决 spawn_blocking 僵尸问题）
    let (key_tx, mut key_rx) = tokio::sync::mpsc::unbounded_channel::<Event>();
    std::thread::spawn(move || {
        while let Ok(event) = crossterm::event::read() {
            // MouseMoved 在源头过滤，不进入 channel
            if matches!(event, Event::Mouse(ref m) if m.kind == MouseEventKind::Moved) {
                continue;
            }
            if key_tx.send(event).is_err() {
                break;
            }
        }
    });

    let _ = terminal.draw(|f| render(&mut app, f));

    loop {
        tokio::select! {
            event = key_rx.recv() => {
                match event {
                    Some(e) => { if handle_key_event(e, &mut app, &chat_handle) { break; } }
                    None => break,
                }
            }
            msg = chat_handle.recv() => {
                match msg {
                    Some(msg) => handle_grpc_message(msg, &mut app, &chat_handle),
                    None => { app.should_quit = true; }
                }
            }
        }
        if app.should_quit {
            break;
        }
        if app.needs_render {
            if app.generating && !app.try_begin_stream_render() {
                app.needs_render = false;
            }
            if app.needs_render {
                let _ = terminal.draw(|f| render(&mut app, f));
                app.needs_render = false;
            }
        }
    }

    ratatui::restore();
    let _ = crossterm::execute!(io::stdout(), crossterm::event::DisableMouseCapture);
    Ok(())
}

fn handle_key_event(event: Event, app: &mut AppState, chat_handle: &ChatHandle) -> bool {
    app.needs_render = true;
    match event {
        Event::Key(key) => {
            if app.confirm.is_some() {
                let q = app.confirm.take().unwrap();
                let approved = matches!(key.code, KeyCode::Char('y') | KeyCode::Char('Y'));
                chat_handle.send_response(&q.query_id, approved);
                return false;
            }
            if key.modifiers.contains(KeyModifiers::CONTROL) {
                match key.code {
                    KeyCode::Char('c') => {
                        if app.generating {
                            chat_handle.send_cancel();
                        }
                        return false;
                    }
                    KeyCode::Char('d') => return true,
                    _ => {}
                }
            }
            if app.generating {
                return false;
            }
            if key.code == KeyCode::Enter {
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
                    chat_handle.send_input(&text);
                }
                return false;
            }
            match key.code {
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
    _chat_handle: &ChatHandle,
) {
    app.needs_render = true;
    match msg.payload {
        Some(server_message::Payload::TextDelta(delta)) => app.append_streaming(&delta.delta),
        Some(server_message::Payload::ToolCall(tc)) => app.add_tool_line(
            LineType::ToolCall,
            format!("{} {}", tc.tool_name, tc.arguments),
            &tc.call_id,
        ),
        Some(server_message::Payload::ToolResult(tr)) => app.insert_tool_result(
            &tr.call_id,
            format!(
                "{}: {}",
                if tr.is_error { "Error" } else { "Output" },
                tr.content
            ),
        ),
        Some(server_message::Payload::StatusUpdate(su)) => {
            app.add_message(LineType::Status, su.message)
        }
        Some(server_message::Payload::UserQuery(uq)) => {
            app.confirm = Some(ConfirmState {
                query_id: uq.query_id,
                message: uq.message,
            });
        }
        Some(server_message::Payload::Error(err)) => {
            app.add_message(LineType::Error, format!("{}: {}", err.code, err.message));
            app.generating = false;
        }
        Some(server_message::Payload::Done(_)) => {
            app.flush_streaming();
            app.generating = false;
        }
        None => {}
    }
}

fn handle_command(text: &str, app: &mut AppState, chat_handle: &ChatHandle) {
    let parts: Vec<&str> = text.splitn(2, ' ').collect();
    match parts[0] {
        "/clear" => app.clear_messages(),
        "/help" => {
            app.add_message(
                LineType::Status,
                "/clear /temp <val> /model <name> /help".into(),
            );
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
