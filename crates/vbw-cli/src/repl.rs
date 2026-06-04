#![allow(dead_code)]

use std::io::{self, BufRead};
use std::time::{Duration, Instant};

use crate::client::ChatHandle;
use crate::display;
use vbw_proto::vibewisp::{LlmConfig, server_message};

enum InputMode {
    Normal,
    ConfirmQuery { query_id: String },
}

fn prompt(mode: &InputMode) -> &str {
    match mode {
        InputMode::Normal => "> ",
        InputMode::ConfirmQuery { .. } => "[y/N] ",
    }
}

/// Returns `true` to continue, `false` to exit the REPL.
fn handle_command(input: &str, _session_id: &str, chat_handle: &ChatHandle) -> bool {
    let parts: Vec<&str> = input.splitn(2, ' ').collect();
    match parts[0] {
        "/quit" | "/exit" => false,
        "/clear" => {
            print!("\x1b[2J\x1b[H");
            true
        }
        "/temp" => {
            if parts.len() < 2 {
                println!("Usage: /temp <value>");
            } else if let Ok(temp) = parts[1].parse::<f64>() {
                chat_handle.send_config_update(LlmConfig {
                    model: None,
                    temperature: Some(temp),
                    max_tokens: None,
                    extra: Default::default(),
                });
            } else {
                println!("Invalid temperature value");
            }
            true
        }
        "/model" => {
            if parts.len() < 2 {
                println!("Usage: /model <name>");
            } else {
                chat_handle.send_config_update(LlmConfig {
                    model: Some(parts[1].to_string()),
                    temperature: None,
                    max_tokens: None,
                    extra: Default::default(),
                });
            }
            true
        }
        "/help" => {
            println!("/quit, /exit  — quit REPL");
            println!("/clear        — clear screen");
            println!("/temp <val>   — set temperature");
            println!("/model <name> — set model");
            println!("/help         — show this message");
            true
        }
        _ => {
            println!("Unknown command: {}", parts[0]);
            true
        }
    }
}

pub async fn run(
    session_id: String,
    mut chat_handle: ChatHandle,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut last_ctrl_c: Option<Instant> = None;
    let (input_tx, mut input_rx) = tokio::sync::mpsc::unbounded_channel::<Option<String>>();

    // 独立 readline task（临时 stdin 实现），Ctrl+D 发 None 退出
    tokio::task::spawn_blocking(move || {
        let stdin = io::stdin();
        for line in stdin.lock().lines() {
            match line {
                Ok(l) => {
                    let _ = input_tx.send(Some(l));
                }
                Err(_) => {
                    let _ = input_tx.send(None);
                    break;
                }
            }
        }
        let _ = input_tx.send(None);
    });

    println!("vibewisp REPL — type /help for commands, /quit to exit");

    loop {
        tokio::select! {
            // 分支 1：键盘输入
            input = input_rx.recv() => {
                match input {
                    None | Some(None) => break,  // 通道关闭或 Ctrl+D: 退出
                    Some(Some(line)) => {
                        let trimmed = line.trim();
                        if trimmed.is_empty() { continue; }
                        if trimmed.starts_with('/') {
                            if !handle_command(trimmed, &session_id, &chat_handle) {
                                break;
                            }
                        } else {
                            eprintln!("[CLI] sending UserInput: {}", trimmed);
                            chat_handle.send_input(trimmed);
                        }
                    }
                }
            }

            // 分支 2：gRPC 响应
            msg = chat_handle.recv() => {
                match msg {
                    None => {
                        display::print_cli_error("daemon disconnected");
                        break;
                    }
                    Some(msg) => {
                        match msg.payload {
                            Some(server_message::Payload::TextDelta(delta)) => {
                                display::print_streaming(&delta.delta);
                            }
                            Some(server_message::Payload::ToolCall(tc)) => {
                                display::print_tool_call(&tc.tool_name, &tc.arguments);
                            }
                            Some(server_message::Payload::ToolResult(tr)) => {
                                display::print_tool_result(&tr.content, tr.is_error);
                            }
                            Some(server_message::Payload::StatusUpdate(su)) => {
                                display::print_status(&su.message);
                            }
                            Some(server_message::Payload::UserQuery(uq)) => {
                                display::print_query(&uq.message);
                                // 等待用户输入 y/n
                                eprintln!("[CLI] waiting for UserQuery response (y/n)");
                                if let Some(answer) = input_rx.recv().await {
                                    let approved = answer.map(|s| s.trim().to_lowercase() == "y").unwrap_or(false);
                                    chat_handle.send_response(&uq.query_id, approved);
                                }
                            }
                            Some(server_message::Payload::Error(err)) => {
                                display::print_daemon_error(&err.code, &err.message);
                            }
                            Some(server_message::Payload::Done(_)) => {
                                display::print_done();
                                println!(); // 空行分隔，确保下次 prompt 正常显示
                            }
                            None => {}
                        }
                    }
                }
            }

            // 分支 3：Ctrl+C
            _ = tokio::signal::ctrl_c() => {
                let now = Instant::now();
                if let Some(prev) = last_ctrl_c
                    && now.duration_since(prev) < Duration::from_secs(1)
                {
                    break;
                }
                last_ctrl_c = Some(now);
                chat_handle.send_cancel();
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mock_chat_handle() -> (
        ChatHandle,
        tokio::sync::mpsc::Receiver<vbw_proto::vibewisp::ClientMessage>,
    ) {
        let (tx, rx) = tokio::sync::mpsc::channel(16);
        let handle = ChatHandle {
            request_tx: tx,
            response_stream: Box::pin(futures::stream::empty()),
            session_id: "test-session".to_string(),
        };
        (handle, rx)
    }

    #[test]
    fn test_prompt_normal() {
        assert_eq!(prompt(&InputMode::Normal), "> ");
    }

    #[test]
    fn test_prompt_confirm() {
        assert_eq!(
            prompt(&InputMode::ConfirmQuery {
                query_id: "q1".into()
            }),
            "[y/N] "
        );
    }

    #[test]
    fn test_handle_quit() {
        let (handle, _rx) = mock_chat_handle();
        assert!(!handle_command("/quit", "s1", &handle));
        assert!(!handle_command("/exit", "s1", &handle));
    }

    #[test]
    fn test_handle_unknown() {
        let (handle, _rx) = mock_chat_handle();
        assert!(handle_command("/unknown", "s1", &handle));
    }

    #[test]
    fn test_handle_help() {
        let (handle, _rx) = mock_chat_handle();
        assert!(handle_command("/help", "s1", &handle));
    }

    #[test]
    fn test_handle_clear() {
        let (handle, _rx) = mock_chat_handle();
        assert!(handle_command("/clear", "s1", &handle));
    }

    #[tokio::test]
    async fn test_handle_temp_valid() {
        let (handle, mut rx) = mock_chat_handle();
        assert!(handle_command("/temp 0.7", "s1", &handle));
        let msg = rx.recv().await.expect("expected message");
        match msg.payload {
            Some(vbw_proto::vibewisp::client_message::Payload::ConfigUpdate(cu)) => {
                let config = cu.config.expect("config should be set");
                assert_eq!(config.temperature, Some(0.7));
            }
            _ => panic!("expected ConfigUpdate"),
        }
    }

    #[tokio::test]
    async fn test_handle_temp_invalid() {
        let (handle, mut rx) = mock_chat_handle();
        assert!(handle_command("/temp notanumber", "s1", &handle));
        // No message should be sent
        let result = tokio::time::timeout(Duration::from_millis(50), rx.recv()).await;
        assert!(result.is_err(), "expected no message to be sent");
    }

    #[tokio::test]
    async fn test_handle_model() {
        let (handle, mut rx) = mock_chat_handle();
        assert!(handle_command("/model gpt-4", "s1", &handle));
        let msg = rx.recv().await.expect("expected message");
        match msg.payload {
            Some(vbw_proto::vibewisp::client_message::Payload::ConfigUpdate(cu)) => {
                let config = cu.config.expect("config should be set");
                assert_eq!(config.model, Some("gpt-4".to_string()));
            }
            _ => panic!("expected ConfigUpdate"),
        }
    }

    #[tokio::test]
    async fn test_handle_temp_no_arg() {
        let (handle, mut rx) = mock_chat_handle();
        assert!(handle_command("/temp", "s1", &handle));
        let result = tokio::time::timeout(Duration::from_millis(50), rx.recv()).await;
        assert!(result.is_err(), "expected no message to be sent");
    }

    #[tokio::test]
    async fn test_handle_model_no_arg() {
        let (handle, mut rx) = mock_chat_handle();
        assert!(handle_command("/model", "s1", &handle));
        let result = tokio::time::timeout(Duration::from_millis(50), rx.recv()).await;
        assert!(result.is_err(), "expected no message to be sent");
    }
}
