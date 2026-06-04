#![allow(dead_code)]

use std::io;
use crossterm::event::{self, Event, KeyCode, KeyModifiers};
use vbw_proto::vibewisp::{server_message, LlmConfig};
use crate::app::{AppState, LineType, ConfirmState};
use crate::client::ChatHandle;
use crate::ui::render;

pub async fn run(
    session_id: String,
    mut chat_handle: ChatHandle,
    model: String,
) -> io::Result<()> {
    crossterm::terminal::enable_raw_mode()?;
    let mut terminal = ratatui::init();
    let mut app = AppState::new(session_id.clone(), model);

    loop {
        tokio::select! {
            crossterm_event = tokio::task::spawn_blocking(|| {
                event::read()
            }) => {
                match crossterm_event {
                    Ok(Ok(event)) => {
                        if handle_key_event(event, &mut app, &chat_handle) {
                            break;
                        }
                    }
                    _ => break,
                }
            }

            msg = chat_handle.recv() => {
                match msg {
                    Some(msg) => {
                        handle_grpc_message(msg, &mut app, &chat_handle);
                    }
                    None => {
                        app.add_message(LineType::Status, "Daemon disconnected".into());
                        app.should_quit = true;
                    }
                }
            }
        }

        if app.should_quit {
            break;
        }

        let _ = terminal.draw(|f| render(&app, f));
    }

    ratatui::restore();
    Ok(())
}

fn handle_key_event(event: Event, app: &mut AppState, chat_handle: &ChatHandle) -> bool {
    match event {
        Event::Key(key) => {
            // Confirm 区优先
            if app.confirm.is_some() {
                match key.code {
                    KeyCode::Char('y') | KeyCode::Char('Y') => {
                        let q = app.confirm.take().unwrap();
                        chat_handle.send_response(&q.query_id, true);
                    }
                    _ => {
                        let q = app.confirm.take().unwrap();
                        chat_handle.send_response(&q.query_id, false);
                    }
                }
                return false;
            }

            // Ctrl+C / Ctrl+D
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

            // generating：忽略其他键
            if app.generating {
                return false;
            }

            // Enter：发送
            if key.code == KeyCode::Enter {
                let text = app.textarea.lines().join("\n");
                app.textarea = tui_textarea::TextArea::default();
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

            // ↑↓ 历史, PageUp/PageDown 滚动
            match key.code {
                KeyCode::Up => {
                    if !app.input_history.is_empty() {
                        let idx = app.history_index.map_or(
                            app.input_history.len().saturating_sub(1),
                            |i| i.saturating_sub(1),
                        );
                        app.history_index = Some(idx);
                        app.textarea = tui_textarea::TextArea::default();
                        app.textarea.insert_str(&app.input_history[idx]);
                    }
                }
                KeyCode::Down => {
                    if let Some(idx) = app.history_index {
                        let new_idx = idx + 1;
                        if new_idx >= app.input_history.len() {
                            app.history_index = None;
                            app.textarea = tui_textarea::TextArea::default();
                            app.textarea.set_placeholder_text("Type your message...");
                        } else {
                            app.history_index = Some(new_idx);
                            app.textarea = tui_textarea::TextArea::default();
                            app.textarea.insert_str(&app.input_history[new_idx]);
                        }
                    }
                }
                KeyCode::PageUp => {
                    app.scroll_offset = app.scroll_offset.saturating_add(5);
                    app.scroll_following = false;
                }
                KeyCode::PageDown => {
                    app.scroll_offset = app.scroll_offset.saturating_sub(5);
                    if app.scroll_offset == 0 {
                        app.scroll_following = true;
                    }
                }
                _ => {
                    app.textarea.input(key);
                }
            }
        }
        Event::Resize(_, _) => {}
        _ => {}
    }
    false
}

fn handle_grpc_message(
    msg: vbw_proto::vibewisp::ServerMessage,
    app: &mut AppState,
    _chat_handle: &ChatHandle,
) {
    match msg.payload {
        Some(server_message::Payload::TextDelta(delta)) => {
            app.append_streaming(&delta.delta);
        }
        Some(server_message::Payload::ToolCall(tc)) => {
            app.add_message(
                LineType::ToolCall,
                format!("🔧 {}({})", tc.tool_name, tc.arguments),
            );
        }
        Some(server_message::Payload::ToolResult(tr)) => {
            let prefix = if tr.is_error { "❌" } else { "📄" };
            app.add_message(LineType::ToolResult, format!("{} {}", prefix, tr.content));
        }
        Some(server_message::Payload::StatusUpdate(su)) => {
            app.add_message(LineType::Status, su.message);
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
        "/clear" => app.messages.clear(),
        "/help" => {
            app.add_message(LineType::Status, "/clear — clear screen".into());
            app.add_message(LineType::Status, "/temp <val> — set temperature".into());
            app.add_message(LineType::Status, "/model <name> — set model".into());
            app.add_message(LineType::Status, "/help — this message".into());
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
