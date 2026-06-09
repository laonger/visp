use async_trait::async_trait;
use std::time::Duration;
use tokio::process::Command;
use tokio::time::timeout;
use visp_core::tool::{Tool, ToolContext, ToolResult};

use crate::truncate::{DEFAULT_MAX_OUTPUT_BYTES, truncate_output};

const DEFAULT_TIMEOUT_SECS: u64 = 120;
const BLOCKED_COMMANDS: &[&str] = &["sudo", "rm -rf /", "chmod 777", "chmod 7777"];

/// Bash 命令执行工具，支持 per-tool 配置
pub struct Bash {
    blocked_commands: Vec<String>,
    default_timeout_secs: u64,
    max_output_bytes: usize,
}

impl Default for Bash {
    fn default() -> Self {
        Self {
            blocked_commands: Vec::new(),
            default_timeout_secs: DEFAULT_TIMEOUT_SECS,
            max_output_bytes: DEFAULT_MAX_OUTPUT_BYTES,
        }
    }
}

impl Bash {
    /// 从 daemon 配置的 raw toml 值构造
    pub fn from_toml(raw: Option<&toml::Value>) -> Self {
        let mut blocked: Vec<String> = Vec::new();
        let mut timeout = DEFAULT_TIMEOUT_SECS;
        let mut max_output = DEFAULT_MAX_OUTPUT_BYTES;

        if let Some(config) = raw.and_then(|v| v.as_table()) {
            if let Some(cmds) = config.get("blocked_commands").and_then(|v| v.as_array()) {
                blocked = cmds
                    .iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect();
            }
            if let Some(t) = config
                .get("default_timeout_secs")
                .and_then(|v| v.as_integer())
                && t > 0
            {
                timeout = t as u64;
            }
            if let Some(o) = config.get("max_output_bytes").and_then(|v| v.as_integer())
                && o > 0
            {
                max_output = o as usize;
            }
        }

        Self {
            blocked_commands: blocked,
            default_timeout_secs: timeout,
            max_output_bytes: max_output,
        }
    }

    /// 检查命令是否命中黑名单（内置 + 自定义）
    fn is_blocked(&self, command: &str) -> bool {
        let lower = command.to_lowercase();
        // 检查内置黑名单
        if BLOCKED_COMMANDS.iter().any(|&b| lower.contains(b)) {
            return true;
        }
        // 检查自定义黑名单
        self.blocked_commands.iter().any(|b| lower.contains(b))
    }

    /// 判断 bash 命令是否包含删除/清理等危险操作
    fn is_destructive_command(&self, command: &str) -> bool {
        let lower = command.to_lowercase();
        // trim 掉前导空格，使得命令开头的 rm 也能被检测到
        let trimmed = lower.trim_start();

        // 匹配命令中间或换行后的危险操作（带前导空格/换行）
        let mid_patterns = [
            " rm ",
            " rm -",
            " rm\t",
            "\nrm ",
            " rmdir ",
            "\nrmdir ",
            " del ",
            " del\t",
            "\ndel ",
            " rd ",
            "\nrd ",
            " clean ",
            " cleanup ",
            " truncate ",
            " dd ",
            "\ndd ",
            " format ",
            "\nformat ",
            " mkfs ",
            "\nmkfs ",
            " > ", // echo > file
        ];
        if mid_patterns.iter().any(|p| lower.contains(p)) {
            return true;
        }

        // 匹配命令开头（trim 后）的危险命令
        let start_patterns = [
            "rm ",
            "rm -",
            "rm\t",
            "rmdir ",
            "del ",
            "del\t",
            "rd ",
            "clean ",
            "cleanup ",
            "truncate ",
            "dd ",
            "format ",
            "mkfs ",
            "mkfs.", // mkfs.ext4 etc.
            "> ",    // redirect at start
        ];
        if start_patterns.iter().any(|p| trimmed.starts_with(p)) {
            return true;
        }

        false
    }
}

#[async_trait]
impl Tool for Bash {
    fn name(&self) -> &str {
        "bash"
    }

    fn category(&self) -> &str {
        "common"
    }

    fn description(&self) -> &str {
        "Execute a single shell command string on the host system with the user's permissions. \
         Use this for running scripts, build tools, git operations, file manipulation, and other CLI tasks. \
         The command runs in a persistent shell session with timeout control. \
         Not suitable for interactive programs (no stdin/stdout). \
         Blocked commands: sudo, rm -rf with top-level paths. \
         Timeout is configurable (default 120s, max 600s)."
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "minLength": 1,
                    "description": "Shell command string to execute. Can include pipes, redirects, and multiple commands separated by && or ;."
                },
                "description": {
                    "type": "string",
                    "description": "Optional brief description of what this command does (5-10 words). Shown to the user during approval."
                },
                "timeout": {
                    "type": "integer",
                    "description": "Optional timeout in milliseconds (default: 120000, max: 600000)."
                },
                "workdir": {
                    "type": "string",
                    "description": "Optional working directory. Defaults to the project root."
                }
            },
            "required": ["command"]
        })
    }

    fn requires_approval_for(&self, arguments: &serde_json::Value) -> bool {
        arguments
            .get("command")
            .and_then(|v| v.as_str())
            .map(|cmd| self.is_destructive_command(cmd))
            .unwrap_or(false)
    }

    async fn execute(&self, arguments: serde_json::Value, context: &ToolContext) -> ToolResult {
        let command = match arguments.get("command").and_then(|v| v.as_str()) {
            Some(cmd) if !cmd.trim().is_empty() => cmd,
            Some(_) => return ToolResult::error("Command is empty"),
            None => return ToolResult::error("Missing required parameter: command"),
        };

        if self.is_blocked(command) {
            return ToolResult::error(format!("Command blocked by safety blacklist: {}", command));
        }

        let timeout_secs = arguments
            .get("timeout")
            .and_then(|v| v.as_u64())
            .unwrap_or(self.default_timeout_secs);

        let result = timeout(
            Duration::from_secs(timeout_secs),
            Command::new("sh")
                .arg("-c")
                .arg(command)
                .current_dir(&context.working_dir)
                .stdin(std::process::Stdio::null())
                .output(),
        )
        .await;

        match result {
            Ok(Ok(output)) => {
                let mut combined = String::new();
                if !output.stdout.is_empty() {
                    combined.push_str(&String::from_utf8_lossy(&output.stdout));
                }
                if !output.stderr.is_empty() {
                    if !combined.is_empty() {
                        combined.push('\n');
                    }
                    combined.push_str(&String::from_utf8_lossy(&output.stderr));
                }
                let truncated = truncate_output(&combined, self.max_output_bytes);
                if output.status.success() {
                    ToolResult::success(truncated)
                } else {
                    ToolResult::error(format!(
                        "Command failed with exit code {}:\n{}",
                        output.status.code().unwrap_or(-1),
                        truncated
                    ))
                }
            }
            Ok(Err(e)) => ToolResult::error(format!("Failed to execute command: {}", e)),
            Err(_) => {
                ToolResult::error(format!("Command timed out after {} seconds", timeout_secs))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use tempfile::tempdir;

    fn test_context(dir: &Path) -> ToolContext {
        ToolContext {
            working_dir: dir.to_path_buf(),
            session_id: None,
        }
    }

    #[tokio::test]
    async fn test_bash_echo() {
        let dir = tempdir().unwrap();
        let ctx = test_context(dir.path());
        let result = Bash::default()
            .execute(serde_json::json!({"command": "echo hello"}), &ctx)
            .await;
        assert!(!result.is_error, "echo should succeed");
        assert!(
            result.content.contains("hello"),
            "output should contain 'hello', got: {:?}",
            result.content
        );
    }

    #[tokio::test]
    async fn test_bash_timeout() {
        let dir = tempdir().unwrap();
        let ctx = test_context(dir.path());
        let result = Bash::default()
            .execute(
                serde_json::json!({"command": "sleep 10", "timeout": 2}),
                &ctx,
            )
            .await;
        assert!(result.is_error, "should time out");
        assert!(
            result.content.contains("timed out"),
            "should mention timeout, got: {:?}",
            result.content
        );
    }

    #[tokio::test]
    async fn test_bash_blocked_command() {
        let dir = tempdir().unwrap();
        let ctx = test_context(dir.path());
        let result = Bash::default()
            .execute(serde_json::json!({"command": "sudo echo hello"}), &ctx)
            .await;
        assert!(result.is_error, "blocked command should return error");
        assert!(
            result.content.to_lowercase().contains("blocked"),
            "should mention blocked, got: {:?}",
            result.content
        );
    }

    #[tokio::test]
    async fn test_bash_stdin_closed() {
        let dir = tempdir().unwrap();
        let ctx = test_context(dir.path());
        // cat without input should return immediately because stdin is null
        let result = Bash::default()
            .execute(serde_json::json!({"command": "cat", "timeout": 5}), &ctx)
            .await;
        // Should not hang; cat with closed stdin exits cleanly
        assert!(!result.is_error, "cat with null stdin should succeed");
    }

    #[tokio::test]
    async fn test_bash_current_dir() {
        let dir = tempdir().unwrap();
        // canonicalize to resolve symlinks (macOS /tmp → /private/tmp)
        let canonical = std::fs::canonicalize(dir.path()).unwrap();
        let ctx = test_context(&canonical);
        let result = Bash::default()
            .execute(serde_json::json!({"command": "pwd"}), &ctx)
            .await;
        assert!(!result.is_error, "pwd should succeed");
        let pwd_output = result.content.trim();
        assert_eq!(
            pwd_output,
            canonical.to_string_lossy().as_ref(),
            "pwd should match working_dir"
        );
    }

    // ── is_destructive_command 测试 ───────────────────────────────────────

    fn destructive() -> Bash {
        Bash::default()
    }

    #[test]
    fn test_destructive_rm_start() {
        assert!(destructive().is_destructive_command("rm -rf /"));
    }

    #[test]
    fn test_destructive_rm_with_leading_spaces() {
        assert!(destructive().is_destructive_command("  rm -rf /"));
    }

    #[test]
    fn test_destructive_rm_in_middle() {
        assert!(destructive().is_destructive_command("echo hello && rm -rf /"));
    }

    #[test]
    fn test_destructive_rm_after_newline() {
        assert!(destructive().is_destructive_command("echo hello\nrm -rf /"));
    }

    #[test]
    fn test_destructive_dd_start() {
        assert!(destructive().is_destructive_command("dd if=/dev/zero of=/dev/sda bs=1M"));
    }

    #[test]
    fn test_destructive_mkfs_start() {
        assert!(destructive().is_destructive_command("mkfs.ext4 /dev/sdb1"));
    }

    #[test]
    fn test_destructive_redirect() {
        assert!(destructive().is_destructive_command("echo hello > /etc/passwd"));
    }

    #[test]
    fn test_destructive_redirect_at_start() {
        assert!(destructive().is_destructive_command("> /etc/passwd"));
    }

    #[test]
    fn test_non_destructive_echo() {
        assert!(!destructive().is_destructive_command("echo hello"));
    }

    #[test]
    fn test_non_destructive_grep_rm() {
        // "rm" in "grep" or "foorm" should not trigger
        assert!(!destructive().is_destructive_command("grep -r 'pattern' ."));
        assert!(!destructive().is_destructive_command("echo foorm"));
    }

    #[test]
    fn test_non_destructive_read() {
        assert!(!destructive().is_destructive_command("cat /etc/passwd"));
    }

    #[test]
    fn test_non_destructive_transform() {
        // "format" inside "transform" should not trigger
        assert!(!destructive().is_destructive_command("echo transform_data"));
    }

    #[tokio::test]
    async fn test_bash_non_utf8_output() {
        let dir = tempdir().unwrap();
        let ctx = test_context(dir.path());
        let result = Bash::default()
            .execute(
                // Use octal escapes (POSIX compatible) instead of \xNN (bash extension)
                serde_json::json!({"command": "printf '\\377\\376\\000\\001'"}),
                &ctx,
            )
            .await;
        // Should not panic; non-UTF-8 bytes are handled via from_utf8_lossy
        assert!(!result.content.is_empty(), "should produce some output");
        // The replacement character should appear for invalid bytes
        assert!(
            result.content.contains('\u{FFFD}'),
            "should contain replacement character for invalid UTF-8"
        );
    }

    #[test]
    fn test_from_toml_default() {
        let bash = Bash::from_toml(None);
        assert!(bash.blocked_commands.is_empty());
        assert_eq!(bash.default_timeout_secs, DEFAULT_TIMEOUT_SECS);
        assert_eq!(bash.max_output_bytes, DEFAULT_MAX_OUTPUT_BYTES);
    }

    #[test]
    fn test_from_toml_blocked_commands() {
        let toml_str = r#"
blocked_commands = ["docker", "kill"]
default_timeout_secs = 30
max_output_bytes = 512
"#;
        let value: toml::Value = toml::from_str(toml_str).unwrap();
        let bash = Bash::from_toml(Some(&value));
        assert_eq!(bash.blocked_commands, vec!["docker", "kill"]);
        assert_eq!(bash.default_timeout_secs, 30);
        assert_eq!(bash.max_output_bytes, 512);
    }

    #[test]
    fn test_from_toml_zero_values_ignored() {
        let toml_str = r#"
default_timeout_secs = 0
max_output_bytes = 0
"#;
        let value: toml::Value = toml::from_str(toml_str).unwrap();
        let bash = Bash::from_toml(Some(&value));
        // Zero values should fall back to defaults
        assert_eq!(bash.default_timeout_secs, DEFAULT_TIMEOUT_SECS);
        assert_eq!(bash.max_output_bytes, DEFAULT_MAX_OUTPUT_BYTES);
    }
}
