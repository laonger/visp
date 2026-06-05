use crate::path::validate_path;
use crate::truncate::{DEFAULT_MAX_OUTPUT_BYTES, truncate_output};
use async_trait::async_trait;
use std::path::PathBuf;
use std::time::Duration;
use tokio::process::Command;
use tokio::time::timeout;
use vbw_core::tool::{Tool, ToolContext, ToolResult};

const GREP_TIMEOUT_SECS: u64 = 30;

const EXCLUDE_DIRS: &[&str] = &[
    ".git",
    "node_modules",
    "target",
    ".venv",
    "__pycache__",
    "dist",
    "build",
];

/// 检查 rg (ripgrep) 是否可用
fn has_rg() -> bool {
    std::process::Command::new("which")
        .arg("rg")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// 构建 grep/rg 命令参数
fn build_grep_args(pattern: &str, path: &str, use_rg: bool) -> (&'static str, Vec<String>) {
    if use_rg {
        let mut args: Vec<String> = vec!["-n".into()];
        for dir in EXCLUDE_DIRS {
            args.push("-g".into());
            args.push(format!("!{}", dir));
        }
        args.push("--".into());
        args.push(pattern.into());
        args.push(path.into());
        ("rg", args)
    } else {
        let mut args: Vec<String> = vec!["-rnI".into()];
        if cfg!(target_os = "linux") {
            args.push("-P".into());
        }
        args.push(pattern.into());
        args.push(path.into());
        ("grep", args)
    }
}

/// 构建 glob 命令参数
fn build_glob_args(pattern: &str, path: &str, use_rg: bool) -> (&'static str, Vec<String>) {
    if use_rg {
        (
            "rg",
            vec!["--files".into(), "-g".into(), pattern.into(), path.into()],
        )
    } else {
        ("find", vec![path.into(), "-name".into(), pattern.into()])
    }
}

/// 执行命令并处理超时/错误，返回 (stdout, stderr)
async fn run_command(program: &str, args: &[String]) -> Result<(String, String), ToolResult> {
    let cmd_output = match timeout(
        Duration::from_secs(GREP_TIMEOUT_SECS),
        Command::new(program).args(args).output(),
    )
    .await
    {
        Ok(Ok(o)) => o,
        Ok(Err(e)) => {
            return Err(ToolResult::error(format!(
                "Failed to execute {}: {}",
                program, e
            )));
        }
        Err(_) => {
            return Err(ToolResult::error(format!(
                "{} timed out after {}s",
                program, GREP_TIMEOUT_SECS
            )));
        }
    };

    let stdout = String::from_utf8_lossy(&cmd_output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&cmd_output.stderr).to_string();
    Ok((stdout, stderr))
}

/// Grep 内容搜索
pub struct Grep;

#[async_trait]
impl Tool for Grep {
    fn name(&self) -> &str {
        "grep"
    }

    fn description(&self) -> &str {
        "内容搜索（正则）。必须提供 pattern 参数。"
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "pattern": { "type": "string", "description": "搜索模式" },
                "path": { "type": "string", "description": "搜索路径（可选，默认工作目录）" }
            },
            "required": ["pattern"]
        })
    }

    async fn execute(&self, arguments: serde_json::Value, context: &ToolContext) -> ToolResult {
        let pattern = match arguments.get("pattern").and_then(|v| v.as_str()) {
            Some(p) => p,
            None => return ToolResult::error("Missing required parameter: pattern"),
        };

        let search_path = match arguments
            .get("path")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
        {
            Some(p) => {
                let user_path = PathBuf::from(p);
                match validate_path(&user_path, &context.working_dir) {
                    Ok(p) => p,
                    Err(e) => return ToolResult::error(e),
                }
            }
            None => context.working_dir.clone(),
        };

        let path_str = match search_path.to_str() {
            Some(s) => s,
            None => return ToolResult::error("Path is not valid UTF-8"),
        };

        let use_rg = has_rg();
        let (program, args) = build_grep_args(pattern, path_str, use_rg);

        let (stdout, stderr) = match run_command(program, &args).await {
            Ok(v) => v,
            Err(e) => return e,
        };

        if !stdout.is_empty() {
            let warning = if !use_rg {
                "Note: ripgrep not found, falling back to grep. Install ripgrep for better performance and regex support.\n"
            } else {
                ""
            };
            let truncated = truncate_output(&stdout, DEFAULT_MAX_OUTPUT_BYTES);
            let output = if warning.is_empty() {
                truncated
            } else {
                format!("{warning}{truncated}")
            };
            ToolResult::success(output)
        } else {
            if !stderr.trim().is_empty() {
                ToolResult::error(format!("{} error: {}", program, stderr.trim()))
            } else {
                ToolResult::success(format!("No matches found for pattern: {}", pattern))
            }
        }
    }
}

/// Glob 文件名搜索
pub struct Glob;

#[async_trait]
impl Tool for Glob {
    fn name(&self) -> &str {
        "glob"
    }

    fn description(&self) -> &str {
        "文件名搜索（通配符）。必须提供 pattern 参数。"
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "pattern": { "type": "string", "description": "通配符模式" },
                "path": { "type": "string", "description": "搜索路径（可选）" }
            },
            "required": ["pattern"]
        })
    }

    async fn execute(&self, arguments: serde_json::Value, context: &ToolContext) -> ToolResult {
        let pattern = match arguments.get("pattern").and_then(|v| v.as_str()) {
            Some(p) => p,
            None => return ToolResult::error("Missing required parameter: pattern"),
        };

        let search_path = match arguments
            .get("path")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
        {
            Some(p) => {
                let user_path = PathBuf::from(p);
                match validate_path(&user_path, &context.working_dir) {
                    Ok(p) => p,
                    Err(e) => return ToolResult::error(e),
                }
            }
            None => context.working_dir.clone(),
        };

        let path_str = match search_path.to_str() {
            Some(s) => s,
            None => return ToolResult::error("Path is not valid UTF-8"),
        };

        let use_rg = has_rg();
        let (program, args) = build_glob_args(pattern, path_str, use_rg);

        let (stdout, stderr) = match run_command(program, &args).await {
            Ok(v) => v,
            Err(e) => return e,
        };

        if stdout.is_empty() {
            if stderr.trim().is_empty() {
                ToolResult::success(format!("No matches found for pattern: {}", pattern))
            } else {
                ToolResult::error(format!("{} error: {}", program, stderr.trim()))
            }
        } else {
            let warning = if !use_rg {
                "Note: ripgrep not found, falling back to find. Install ripgrep for better performance.\n"
            } else {
                ""
            };
            let truncated = truncate_output(&stdout, DEFAULT_MAX_OUTPUT_BYTES);
            let output = if warning.is_empty() {
                truncated
            } else {
                format!("{warning}{truncated}")
            };
            ToolResult::success(output)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[tokio::test]
    async fn test_grep_skips_binary() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("a.txt"), b"hello world\n").unwrap();
        fs::write(dir.path().join("b.bin"), b"hello\0binary\n").unwrap();

        let grep = Grep;
        let ctx = ToolContext {
            working_dir: dir.path().to_path_buf(),
            session_id: None,
        };
        let result = grep
            .execute(serde_json::json!({"pattern": "hello"}), &ctx)
            .await;

        assert!(!result.is_error, "unexpected error: {}", result.content);
        assert!(
            result.content.contains("a.txt"),
            "should find a.txt:\n{}",
            result.content
        );
        assert!(
            !result.content.contains("b.bin"),
            "should NOT find b.bin:\n{}",
            result.content
        );
    }

    #[tokio::test]
    async fn test_grep_missing_pattern() {
        let dir = tempdir().unwrap();
        let grep = Grep;
        let ctx = ToolContext {
            working_dir: dir.path().to_path_buf(),
            session_id: None,
        };
        let result = grep.execute(serde_json::json!({}), &ctx).await;
        assert!(result.is_error);
        assert!(result.content.contains("pattern"));
    }

    #[tokio::test]
    async fn test_glob_basic() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("a.rs"), "").unwrap();
        fs::write(dir.path().join("b.rs"), "").unwrap();
        fs::write(dir.path().join("c.txt"), "").unwrap();

        let glob = Glob;
        let ctx = ToolContext {
            working_dir: dir.path().to_path_buf(),
            session_id: None,
        };
        let result = glob
            .execute(serde_json::json!({"pattern": "*.rs"}), &ctx)
            .await;

        assert!(!result.is_error, "unexpected error: {}", result.content);
        assert!(
            result.content.contains("a.rs"),
            "should find a.rs:\n{}",
            result.content
        );
        assert!(
            result.content.contains("b.rs"),
            "should find b.rs:\n{}",
            result.content
        );
        assert!(
            !result.content.contains("c.txt"),
            "should not find c.txt:\n{}",
            result.content
        );
    }

    #[tokio::test]
    async fn test_glob_nested() {
        let dir = tempdir().unwrap();
        let sub = dir.path().join("sub");
        fs::create_dir(&sub).unwrap();
        fs::write(sub.join("nested.rs"), "").unwrap();
        fs::write(dir.path().join("root.rs"), "").unwrap();

        let glob = Glob;
        let ctx = ToolContext {
            working_dir: dir.path().to_path_buf(),
            session_id: None,
        };
        let result = glob
            .execute(serde_json::json!({"pattern": "*.rs"}), &ctx)
            .await;

        assert!(!result.is_error, "unexpected error: {}", result.content);
        assert!(
            result.content.contains("root.rs"),
            "should find root.rs:\n{}",
            result.content
        );
        assert!(
            result.content.contains("nested.rs"),
            "should find nested.rs:\n{}",
            result.content
        );
    }

    #[tokio::test]
    async fn test_glob_missing_pattern() {
        let dir = tempdir().unwrap();
        let glob = Glob;
        let ctx = ToolContext {
            working_dir: dir.path().to_path_buf(),
            session_id: None,
        };
        let result = glob.execute(serde_json::json!({}), &ctx).await;
        assert!(result.is_error);
        assert!(result.content.contains("pattern"));
    }
}
