//! Slash-command business logic.
//!
//! This crate owns the parsing and prompt generation for slash commands
//! that need daemon-side processing (e.g. `/init`, `/init-agent`, `/init-skill`).
//!
//! The daemon intercepts `UserInput` messages that begin with `/`, routes
//! them through [`parse`] / [`resolve`], and executes the resulting
//! [`CommandAction`].  The CLI remains a thin client — it sends raw text
//! and the daemon does the transformation.

use std::path::Path;

pub mod init;
pub mod init_agent;
pub mod init_skill;

/// A parsed slash command.
///
/// Every recognised slash command has a variant here.  The CLI uses
/// [`parse`] to route commands, and the daemon calls [`resolve`] on
/// commands that are intercepted via `UserInput` (`/init`, `/init-agent`,
/// `/init-skill`).  Other variants are actioned by the CLI via gRPC /
/// `ConfigUpdate` and never reach the daemon as text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    /// `/init [focus...]` — generate or update `AGENTS.md`.
    Init {
        /// User-provided focus or constraints (text after `/init `).
        args: String,
    },
    /// `/init-agent <name>` — create a template agent definition file.
    InitAgent {
        /// Agent name (alphanumeric, hyphens, underscores).
        name: String,
    },
    /// `/init-skill <name>` — create a template skill file.
    InitSkill {
        /// Skill name (alphanumeric, hyphens, underscores).
        name: String,
    },
    /// `/new` — create a new session.
    NewSession,
    /// `/list` — list all sessions.
    ListSessions,
    /// `/sessions <id>` — switch to a session by short-id.
    SwitchSession {
        /// Target session short-id.
        target: String,
    },
    /// `/temp <n>` — set temperature (0.0–1.0).
    SetTemperature {
        /// Raw text after `/temp ` (validated by [`resolve`]).
        raw: String,
    },
    /// `/model [name]` — switch model.
    ///
    /// `name = None` means the CLI should show an interactive picker.
    SetModel {
        /// Model name, or `None` to show a picker.
        name: Option<String>,
    },
    /// Not a recognised slash command — treat as a normal user message.
    None,
}

/// What the daemon should do with a parsed command.
#[derive(Debug, Clone)]
pub enum CommandAction {
    /// Replace the user input with this prompt text and forward to the LLM.
    Prompt(String),

    /// Write a file to the given path with the given content.
    /// The daemon must create parent directories before writing.
    WriteFile {
        /// Absolute path to the file to write.
        path: std::path::PathBuf,
        /// File content.
        content: String,
    },

    /// Not a daemon command — forward the original text to the LLM as-is.
    None,
}

/// Parse a raw user input string into a [`Command`].
///
/// Recognises all built-in slash commands.  CLI-local commands (`/clear`,
/// `/help`, `/mouse`) are **not** included — they are handled directly in
/// the CLI and never sent to the daemon.
///
/// ```
/// use visp_command::{parse, Command};
///
/// // /init variants
/// assert_eq!(parse("/init"), Command::Init { args: String::new() });
/// assert_eq!(parse("/init focus on testing"),
///            Command::Init { args: "focus on testing".into() });
///
/// // /init-agent / /init-skill
/// let agent = parse("/init-agent my-agent");
/// assert!(matches!(agent, Command::InitAgent { .. }));
/// let skill = parse("/init-skill my-skill");
/// assert!(matches!(skill, Command::InitSkill { .. }));
///
/// // Session management
/// assert_eq!(parse("/new"), Command::NewSession);
/// assert_eq!(parse("/list"), Command::ListSessions);
/// assert_eq!(parse("/sessions abc"),
///            Command::SwitchSession { target: "abc".into() });
///
/// // Config
/// assert_eq!(parse("/temp 0.5"),
///            Command::SetTemperature { raw: "0.5".into() });
/// assert_eq!(parse("/model"),
///            Command::SetModel { name: None });
/// assert_eq!(parse("/model gpt-4o"),
///            Command::SetModel { name: Some("gpt-4o".into()) });
///
/// // Not recognised
/// assert_eq!(parse("hello world"), Command::None);
/// assert_eq!(parse("/clear"), Command::None);
/// ```
pub fn parse(text: &str) -> Command {
    let text = text.trim();

    // `/init-agent <name>`
    if let Some(rest) = text.strip_prefix("/init-agent") {
        let name = if rest.is_empty() || !rest.starts_with(char::is_whitespace) {
            return Command::None;
        } else {
            rest.trim().to_string()
        };
        return Command::InitAgent { name };
    }

    // `/init-skill <name>`
    if let Some(rest) = text.strip_prefix("/init-skill") {
        let name = if rest.is_empty() || !rest.starts_with(char::is_whitespace) {
            return Command::None;
        } else {
            rest.trim().to_string()
        };
        return Command::InitSkill { name };
    }

    // Must check `/init-agent` / `/init-skill` BEFORE `/init`
    // so that `/init-agent foo` is not parsed as `/init` with args "agent foo".

    // `/init [focus...]`
    if let Some(rest) = text.strip_prefix("/init") {
        if rest.is_empty() {
            return Command::Init {
                args: String::new(),
            };
        } else if rest.starts_with(char::is_whitespace) {
            return Command::Init {
                args: rest.trim().to_string(),
            };
        } else {
            return Command::None; // e.g. `/initialize`
        }
    }

    // `/new`
    if text == "/new" {
        return Command::NewSession;
    }

    // `/list`
    if text == "/list" {
        return Command::ListSessions;
    }

    // `/sessions <id>`
    if let Some(rest) = text.strip_prefix("/sessions") {
        if rest.is_empty() {
            // bare `/sessions` — same as `/list` (handled in CLI)
            return Command::ListSessions;
        } else if rest.starts_with(char::is_whitespace) {
            return Command::SwitchSession {
                target: rest.trim().to_string(),
            };
        } else {
            return Command::None;
        }
    }

    // `/temp <n>`
    if let Some(rest) = text.strip_prefix("/temp") {
        if rest.is_empty() || !rest.starts_with(char::is_whitespace) {
            return Command::None;
        }
        return Command::SetTemperature {
            raw: rest.trim().to_string(),
        };
    }

    // `/model [name]`
    if let Some(rest) = text.strip_prefix("/model") {
        if rest.is_empty() {
            return Command::SetModel { name: None };
        } else if rest.starts_with(char::is_whitespace) {
            return Command::SetModel {
                name: Some(rest.trim().to_string()),
            };
        } else {
            return Command::None;
        }
    }

    Command::None
}

/// Resolve a parsed [`Command`] into an action for the daemon to execute.
///
/// `project_path` is the project root directory, used to compute file
/// paths for agent / skill templates.
pub fn resolve(cmd: &Command, project_path: &Path) -> Result<CommandAction, String> {
    match cmd {
        Command::Init { args } => Ok(CommandAction::Prompt(init::build_init_prompt(args))),

        Command::InitAgent { name } => {
            init_agent::validate_name(name)?;
            let path = init_agent::file_path(project_path, name);
            if path.exists() {
                return Err(format!("Agent file already exists at {}", path.display()));
            }
            let content = init_agent::template(name);
            Ok(CommandAction::WriteFile { path, content })
        }

        Command::InitSkill { name } => {
            init_skill::validate_name(name)?;
            let path = init_skill::file_path(project_path, name);
            if path.exists() {
                return Err(format!("Skill file already exists at {}", path.display()));
            }
            let content = init_skill::template(name);
            Ok(CommandAction::WriteFile { path, content })
        }

        // ── Session management / config commands ────────────────
        // These are handled by the CLI via gRPC / ConfigUpdate and
        // never reach the daemon as UserInput text.  `resolve()`
        // still validates arguments for consistency.
        Command::NewSession | Command::ListSessions => Ok(CommandAction::None),

        Command::SwitchSession { target } => {
            if target.is_empty() {
                return Err("Session ID cannot be empty".into());
            }
            Ok(CommandAction::None)
        }

        Command::SetTemperature { raw } => {
            let value: f64 = raw.parse().map_err(|_| {
                format!(
                    "Invalid temperature value: '{raw}' — expected a number between 0.0 and 1.0"
                )
            })?;
            if !(0.0..=1.0).contains(&value) {
                return Err(format!(
                    "Temperature must be between 0.0 and 1.0, got {value}"
                ));
            }
            Ok(CommandAction::None)
        }

        Command::SetModel { name } => {
            if let Some(n) = name
                && n.is_empty()
            {
                return Err("Model name cannot be empty".into());
            }
            // `name = None` means "show picker" — no validation needed.
            Ok(CommandAction::None)
        }

        Command::None => Ok(CommandAction::None),
    }
}

/// Resolve a parsed [`Command`] into the prompt text that should be sent
/// to the LLM.
///
/// Only meaningful for [`Command::Init`]; returns `None` for all other
/// variants.
#[deprecated(since = "0.5.0", note = "use resolve() instead")]
pub fn to_prompt(cmd: &Command) -> Option<String> {
    match cmd {
        Command::Init { args } => Some(init::build_init_prompt(args)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── parse ───────────────────────────────────────────────────

    #[test]
    fn test_parse_init_exact() {
        assert_eq!(
            parse("/init"),
            Command::Init {
                args: String::new()
            }
        );
    }

    #[test]
    fn test_parse_init_with_args() {
        assert_eq!(
            parse("/init focus on testing"),
            Command::Init {
                args: "focus on testing".into()
            }
        );
    }

    #[test]
    fn test_parse_init_with_extra_spaces() {
        assert_eq!(
            parse("  /init   focus   "),
            Command::Init {
                args: "focus".into()
            }
        );
    }

    #[test]
    fn test_parse_init_edge_cases() {
        assert_eq!(
            parse("/initialize"),
            Command::None,
            "/initialize should not match /init"
        );
        assert_eq!(
            parse("/initiation"),
            Command::None,
            "/initiation should not match /init"
        );
    }

    #[test]
    fn test_parse_init_agent() {
        let cmd = parse("/init-agent my-agent");
        assert_eq!(
            cmd,
            Command::InitAgent {
                name: "my-agent".into()
            }
        );
    }

    #[test]
    fn test_parse_init_agent_without_name_returns_none() {
        assert_eq!(
            parse("/init-agent"),
            Command::None,
            "/init-agent without a name is invalid"
        );
    }

    #[test]
    fn test_parse_init_agent_prefix_only() {
        assert_eq!(
            parse("/init-agenthink"),
            Command::None,
            "partial prefix should not match"
        );
    }

    #[test]
    fn test_parse_init_skill() {
        let cmd = parse("/init-skill my-skill");
        assert_eq!(
            cmd,
            Command::InitSkill {
                name: "my-skill".into()
            }
        );
    }

    #[test]
    fn test_parse_init_skill_without_name_returns_none() {
        assert_eq!(
            parse("/init-skill"),
            Command::None,
            "/init-skill without a name is invalid"
        );
    }

    #[test]
    fn test_parse_not_daemon_command() {
        // CLI-local commands — not in visp-command enum
        assert_eq!(parse("/clear"), Command::None);
        assert_eq!(parse("/help"), Command::None);
        assert_eq!(parse("/mouse"), Command::None);
        // Unknown commands
        assert_eq!(parse("hello"), Command::None);
        assert_eq!(parse("/xyz"), Command::None);
    }

    #[test]
    fn test_parse_new_session() {
        assert_eq!(parse("/new"), Command::NewSession);
        assert_eq!(parse("  /new  "), Command::NewSession);
    }

    #[test]
    fn test_parse_list_sessions() {
        assert_eq!(parse("/list"), Command::ListSessions);
        // Bare `/sessions` is treated as `/list`
        assert_eq!(parse("/sessions"), Command::ListSessions);
        assert_eq!(parse("  /list  "), Command::ListSessions);
    }

    #[test]
    fn test_parse_switch_session() {
        assert_eq!(
            parse("/sessions abc"),
            Command::SwitchSession {
                target: "abc".into()
            }
        );
        assert_eq!(
            parse("/sessions   short-id-123  "),
            Command::SwitchSession {
                target: "short-id-123".into()
            }
        );
    }

    #[test]
    fn test_parse_set_temperature() {
        assert_eq!(
            parse("/temp 0.5"),
            Command::SetTemperature { raw: "0.5".into() }
        );
        assert_eq!(
            parse("/temp 1"),
            Command::SetTemperature { raw: "1".into() }
        );
        // Bare `/temp` without value is invalid
        assert_eq!(parse("/temp"), Command::None);
    }

    #[test]
    fn test_parse_set_model() {
        assert_eq!(parse("/model"), Command::SetModel { name: None });
        assert_eq!(
            parse("/model gpt-4o"),
            Command::SetModel {
                name: Some("gpt-4o".into())
            }
        );
        assert_eq!(
            parse("/model claude-sonnet-4"),
            Command::SetModel {
                name: Some("claude-sonnet-4".into())
            }
        );
    }

    #[test]
    fn test_parse_empty_string() {
        assert_eq!(parse(""), Command::None);
    }

    #[test]
    fn test_parse_only_whitespace() {
        assert_eq!(parse("   "), Command::None);
    }

    // ── resolve ─────────────────────────────────────────────────

    #[test]
    fn test_resolve_init() {
        let cmd = parse("/init focus on CI");
        let result = resolve(&cmd, Path::new("/tmp")).unwrap();
        match result {
            CommandAction::Prompt(p) => {
                assert!(p.contains("Create or update `AGENTS.md`"));
                assert!(p.contains("focus on CI"));
            }
            _ => panic!("expected Prompt"),
        }
    }

    #[test]
    fn test_resolve_none() {
        let result = resolve(&Command::None, Path::new("/tmp")).unwrap();
        assert!(matches!(result, CommandAction::None));
    }

    #[test]
    fn test_resolve_init_agent_invalid_name() {
        let cmd = Command::InitAgent {
            name: "bad name!".into(),
        };
        let result = resolve(&cmd, Path::new("/tmp"));
        assert!(result.is_err());
    }

    // ── resolve — new commands ─────────────────────────────────

    #[test]
    fn test_resolve_new_session() {
        let result = resolve(&Command::NewSession, Path::new("/tmp")).unwrap();
        assert!(matches!(result, CommandAction::None));
    }

    #[test]
    fn test_resolve_list_sessions() {
        let result = resolve(&Command::ListSessions, Path::new("/tmp")).unwrap();
        assert!(matches!(result, CommandAction::None));
    }

    #[test]
    fn test_resolve_switch_session() {
        let result = resolve(
            &Command::SwitchSession {
                target: "abc".into(),
            },
            Path::new("/tmp"),
        )
        .unwrap();
        assert!(matches!(result, CommandAction::None));
    }

    #[test]
    fn test_resolve_switch_session_empty() {
        let result = resolve(
            &Command::SwitchSession {
                target: String::new(),
            },
            Path::new("/tmp"),
        );
        assert!(result.is_err(), "empty target should fail");
    }

    #[test]
    fn test_resolve_set_temperature() {
        let result = resolve(
            &Command::SetTemperature { raw: "0.5".into() },
            Path::new("/tmp"),
        )
        .unwrap();
        assert!(matches!(result, CommandAction::None));
    }

    #[test]
    fn test_resolve_set_temperature_invalid() {
        let result = resolve(
            &Command::SetTemperature { raw: "abc".into() },
            Path::new("/tmp"),
        );
        assert!(result.is_err(), "non-numeric should fail");
    }

    #[test]
    fn test_resolve_set_temperature_out_of_range() {
        let result = resolve(
            &Command::SetTemperature { raw: "2.0".into() },
            Path::new("/tmp"),
        );
        assert!(result.is_err(), "out of range should fail");
    }

    #[test]
    fn test_resolve_set_model_with_name() {
        let result = resolve(
            &Command::SetModel {
                name: Some("gpt-4o".into()),
            },
            Path::new("/tmp"),
        )
        .unwrap();
        assert!(matches!(result, CommandAction::None));
    }

    #[test]
    fn test_resolve_set_model_no_name() {
        let result = resolve(&Command::SetModel { name: None }, Path::new("/tmp")).unwrap();
        assert!(matches!(result, CommandAction::None));
    }

    #[test]
    fn test_resolve_set_model_empty_name() {
        let result = resolve(
            &Command::SetModel {
                name: Some(String::new()),
            },
            Path::new("/tmp"),
        );
        assert!(result.is_err());
    }

    // ── to_prompt compat ────────────────────────────────────────

    #[test]
    #[allow(deprecated)]
    fn test_to_prompt_init() {
        let cmd = parse("/init focus on CI");
        let prompt = to_prompt(&cmd).unwrap();
        assert!(prompt.contains("Create or update `AGENTS.md`"));
    }
}
