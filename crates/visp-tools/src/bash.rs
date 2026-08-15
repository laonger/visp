use async_trait::async_trait;
use std::time::Duration;
use tokio::process::Command;
use tokio::time::timeout;
use visp_core::tool::{Tool, ToolContext, ToolResult};

use crate::path::validate_path;
use crate::truncate::{DEFAULT_MAX_OUTPUT_BYTES, truncate_output};

const DEFAULT_TIMEOUT_SECS: u64 = 60;
const OUTPUT_DRAIN_TIMEOUT_SECS: u64 = 5;
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
    ///
    /// 单词黑名单（如 "sudo"、"node"）做词边界匹配，避免误伤 node_modules 等；
    /// 多词黑名单（如 "rm -rf /"）保持子串匹配（现状）。
    fn is_blocked(&self, command: &str) -> bool {
        let lower = command.to_lowercase();
        BLOCKED_COMMANDS
            .iter()
            .copied()
            .chain(self.blocked_commands.iter().map(|s| s.as_str()))
            .any(|b| match b.trim() {
                "" => false,
                word if word.contains(' ') => lower.contains(word),
                word => is_command_word(&lower, word),
            })
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

        // Resolve working directory: explicit `workdir` param (validated) or fallback to context.
        let workdir = match arguments.get("workdir").and_then(|v| v.as_str()) {
            Some(wd) if !wd.trim().is_empty() => {
                match validate_path(std::path::Path::new(wd), &context.working_dir) {
                    Ok(p) => p,
                    Err(e) => return ToolResult::error(format!("Invalid workdir: {}", e)),
                }
            }
            _ => context.working_dir.clone(),
        };

        tracing::info!(
            command = %truncate_for_log(command, 200),
            timeout_secs,
            workdir = %workdir.display(),
            "bash: executing command"
        );

        // Spawn the child instead of using `Command::output()` so we keep a
        // handle that can be explicitly killed when the timeout elapses.
        // `kill_on_drop(true)` guarantees the child is terminated even if the
        // surrounding future/task is dropped or cancelled.
        let mut child = match Command::new("sh")
            .arg("-c")
            .arg(command)
            .current_dir(&workdir)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()
        {
            Ok(c) => c,
            Err(e) => return ToolResult::error(format!("Failed to spawn command: {}", e)),
        };

        // Drain stdout/stderr concurrently with waiting; otherwise a chatty
        // child can block forever on a full pipe buffer and never exit.
        let stdout_pipe = child.stdout.take();
        let stderr_pipe = child.stderr.take();

        // Each read is individually bounded so an orphaned subprocess holding
        // the pipe open can't hang us indefinitely.
        let stdout_task = tokio::spawn(async move {
            let mut buf = Vec::new();
            if let Some(mut pipe) = stdout_pipe {
                let _ = timeout(
                    Duration::from_secs(OUTPUT_DRAIN_TIMEOUT_SECS),
                    tokio::io::AsyncReadExt::read_to_end(&mut pipe, &mut buf),
                )
                .await;
            }
            buf
        });
        let stderr_task = tokio::spawn(async move {
            let mut buf = Vec::new();
            if let Some(mut pipe) = stderr_pipe {
                let _ = timeout(
                    Duration::from_secs(OUTPUT_DRAIN_TIMEOUT_SECS),
                    tokio::io::AsyncReadExt::read_to_end(&mut pipe, &mut buf),
                )
                .await;
            }
            buf
        });

        let status = tokio::select! {
            _ = tokio::time::sleep(Duration::from_secs(timeout_secs)) => {
                // Timeout elapsed: explicitly kill the child and reap the zombie.
                let _ = child.start_kill();
                let _ = child.wait().await;
                None
            }
            status = child.wait() => {
                Some(status)
            }
        };

        // The reads complete once the pipe write ends are closed (child exited
        // or was killed); each read is individually bounded.
        let stdout_bytes = stdout_task.await.unwrap_or_default();
        let stderr_bytes = stderr_task.await.unwrap_or_default();
        let mut combined = String::new();
        if !stdout_bytes.is_empty() {
            combined.push_str(&String::from_utf8_lossy(&stdout_bytes));
        }
        if !stderr_bytes.is_empty() {
            if !combined.is_empty() {
                combined.push('\n');
            }
            combined.push_str(&String::from_utf8_lossy(&stderr_bytes));
        }
        let truncated = truncate_output(&combined, self.max_output_bytes);

        match status {
            Some(Ok(output_status)) => {
                tracing::info!(
                    exit_code = ?output_status.code(),
                    output_len = truncated.len(),
                    "bash: command completed"
                );
                if output_status.success() {
                    ToolResult::success(truncated)
                } else {
                    ToolResult::error(format!(
                        "Command failed with exit code {}:\n{}",
                        output_status.code().unwrap_or(-1),
                        truncated
                    ))
                }
            }
            Some(Err(e)) => {
                tracing::warn!(error = %e, "bash: command execution error");
                ToolResult::error(format!("Failed to execute command: {}", e))
            }
            None => {
                tracing::warn!(timeout_secs, "bash: command timed out, process killed");
                ToolResult::error(format!("Command timed out after {} seconds", timeout_secs))
            }
        }
    }
}

/// Truncate a string for log output while keeping it on a single line.
fn truncate_for_log(s: &str, max_len: usize) -> String {
    let mut end = max_len.min(s.len());
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    if end >= s.len() {
        s.to_string()
    } else {
        format!("{}... [truncated]", &s[..end])
    }
}

/// 词边界匹配：b 必须作为独立单词出现（前后是串首/串尾或非词字符），
/// 这样 "node" 能命中 `node && ...` / `node; ...`，但不误伤 node_modules、nodemon。
fn is_command_word(lower: &str, b: &str) -> bool {
    let bytes = lower.as_bytes();
    let b_bytes = b.as_bytes();
    let mut start = 0;
    while let Some(rel) = lower[start..].find(b) {
        let abs = start + rel;
        let before_ok = abs == 0 || !is_word_char(bytes[abs - 1]);
        let after_pos = abs + b_bytes.len();
        let after_ok = after_pos >= bytes.len() || !is_word_char(bytes[after_pos]);
        if before_ok && after_ok {
            return true;
        }
        start = abs + 1;
    }
    false
}

fn is_word_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

#[cfg(test)]
#[path = "bash_tests.rs"]
mod tests;
