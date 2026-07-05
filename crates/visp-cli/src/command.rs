//! CLI-side command handling.
//!
//! Command routing is driven by [`visp_command::parse`], which recognises
//! all built-in slash commands.  Three execution paths exist:
//!
//! 1. **Local commands** — handled entirely in the CLI without contacting
//!    the daemon: [`/clear`], [`/help`], [`/mouse`].
//!
//! 2. **Daemon passthrough commands** — sent as `UserInput` text to the
//!    daemon, which intercepts and processes them: [`/init`],
//!    [`/init-agent`], [`/init-skill`].  The CLI just forwards the raw text.
//!
//! 3. **Daemon RPC commands** — trigger gRPC calls or `ConfigUpdate`
//!    messages via [`ChatHandle`]: [`/new`], [`/list`], [`/sessions`],
//!    [`/temp`], [`/model`].

use crate::app::{AppState, LineType};
use crate::client::ChatHandle;
use visp_proto::visp::LlmConfig;

/// Handle a slash command in the CLI.
///
/// Dispatches to the appropriate handler based on the command type.
pub fn handle(text: &str, app: &mut AppState, chat_handle: &mut ChatHandle) {
    // ── CLI-local commands (pure UI, not in visp-command) ──────
    //
    // These are matched by simple string prefix because they are trivial
    // and will never involve daemon logic.
    let cmd_base = text.split_whitespace().next().unwrap_or("");
    match cmd_base {
        "/clear" => {
            app.clear_messages();
            return;
        }
        "/help" => {
            app.show_help = !app.show_help;
            return;
        }
        "/mouse" => {
            crate::event::toggle_mouse_mode(app);
            return;
        }
        _ => {}
    }

    // ── All other commands: use visp-command parsing ───────────
    match visp_command::parse(text) {
        // -- Daemon passthrough (send as UserInput) --------------
        visp_command::Command::Init { .. }
        | visp_command::Command::InitAgent { .. }
        | visp_command::Command::InitSkill { .. } => {
            app.current_request_usage = (0, 0, 0, 0);
            app.add_message(LineType::User, text.to_string());
            app.set_generating(true);
            app.scroll_following = true;
            chat_handle.send_input(text);
        }

        // -- Session management (gRPC RPC) ----------------------
        visp_command::Command::NewSession => {
            app.clear_streaming();
            app.set_generating(false);
            app.stale_done_expected = false;
            app.current_request_id = None;
            app.confirm = None;
            app.pending_new_session = true;
            app.add_message(LineType::Status, "Creating new session...".into());
        }

        visp_command::Command::ListSessions => {
            app.add_message(LineType::Status, "Fetching sessions...".into());
            app.pending_list_sessions = true;
        }

        visp_command::Command::SwitchSession { target } => {
            app.add_message(
                LineType::Status,
                format!("Switching to session {target}..."),
            );
            app.clear_streaming();
            app.set_generating(false);
            app.stale_done_expected = false;
            app.current_request_id = None;
            app.confirm = None;
            app.pending_switch_session = Some(target);
        }

        // -- Config commands (ConfigUpdate / UI picker) ----------
        visp_command::Command::SetTemperature { raw } => {
            match visp_command::resolve(
                &visp_command::Command::SetTemperature { raw: raw.clone() },
                std::path::Path::new(""),
            ) {
                Ok(_) => {
                    if let Ok(temp) = raw.parse::<f64>() {
                        chat_handle.send_config_update(LlmConfig {
                            model: None,
                            model_key: None,
                            temperature: Some(temp),
                            max_tokens: None,
                            max_context_tokens: None,
                            extra: Default::default(),
                        });
                        app.add_message(LineType::Status, format!("Temperature set to {temp}"));
                    }
                }
                Err(e) => {
                    app.add_message(LineType::Status, format!("Error: {e}"));
                }
            }
        }

        visp_command::Command::SetModel { name } => {
            match name {
                Some(model) => {
                    // Direct model switch via ConfigUpdate
                    chat_handle.send_config_update(LlmConfig {
                        model: None,
                        model_key: Some(model.clone()),
                        temperature: None,
                        max_tokens: None,
                        max_context_tokens: None,
                        extra: Default::default(),
                    });
                    app.add_message(LineType::Status, format!("Switched to model {model}"));
                }
                None => {
                    // Show interactive model picker
                    if app.available_models.is_empty() {
                        app.add_message(LineType::Status, "No alternate models configured".into());
                    } else {
                        app.add_message(LineType::Status, "Select a model:".into());
                        app.pending_model_select = true;
                    }
                }
            }
        }

        // Unknown / not a slash command — do nothing
        visp_command::Command::None => {}
    }
}
