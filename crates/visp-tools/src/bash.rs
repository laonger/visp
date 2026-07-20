use async_trait::async_trait;
use std::time::Duration;
use tokio::process::Command;
use tokio::time::timeout;
use visp_core::tool::{Tool, ToolContext, ToolResult};

use crate::path::validate_path;
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

        let result = timeout(
            Duration::from_secs(timeout_secs),
            Command::new("sh")
                .arg("-c")
                .arg(command)
                .current_dir(&workdir)
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
#[path = "bash_tests.rs"]
mod tests;
