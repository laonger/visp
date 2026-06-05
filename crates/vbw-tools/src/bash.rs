use async_trait::async_trait;
use std::time::Duration;
use tokio::process::Command;
use tokio::time::timeout;
use vbw_core::tool::{Tool, ToolContext, ToolResult};

use crate::truncate::{DEFAULT_MAX_OUTPUT_BYTES, truncate_output};

const DEFAULT_TIMEOUT_SECS: u64 = 120;
const BLOCKED_COMMANDS: &[&str] = &["sudo", "rm -rf /", "chmod 777", "chmod 7777"];

/// 检查命令是否命中黑名单
fn is_blocked(command: &str) -> bool {
    let lower = command.to_lowercase();
    BLOCKED_COMMANDS.iter().any(|&b| lower.contains(b))
}

/// Bash 命令执行工具
pub struct Bash;

#[async_trait]
impl Tool for Bash {
    fn name(&self) -> &str {
        "bash"
    }

    fn description(&self) -> &str {
        "执行 shell 命令。必须提供 command 参数指定要执行的命令。"
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "description": "要执行的 shell 命令"
                }
            },
            "required": ["command"]
        })
    }

    async fn execute(&self, arguments: serde_json::Value, context: &ToolContext) -> ToolResult {
        let command = match arguments.get("command").and_then(|v| v.as_str()) {
            Some(cmd) => cmd,
            None => return ToolResult::error("Missing required parameter: command"),
        };

        if is_blocked(command) {
            return ToolResult::error(format!("Command blocked by safety blacklist: {}", command));
        }

        let timeout_secs = arguments
            .get("timeout")
            .and_then(|v| v.as_u64())
            .unwrap_or(DEFAULT_TIMEOUT_SECS);

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
                let truncated = truncate_output(&combined, DEFAULT_MAX_OUTPUT_BYTES);
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
        let result = Bash
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
        let result = Bash
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
        let result = Bash
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
        let result = Bash
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
        let result = Bash
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

    #[tokio::test]
    async fn test_bash_non_utf8_output() {
        let dir = tempdir().unwrap();
        let ctx = test_context(dir.path());
        let result = Bash
            .execute(
                serde_json::json!({"command": "printf '\\xff\\xfe\\x00\\x01'"}),
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
}
