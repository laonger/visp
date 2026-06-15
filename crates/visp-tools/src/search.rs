use crate::path::validate_path;
use crate::truncate::{DEFAULT_MAX_OUTPUT_BYTES, truncate_output};
use async_trait::async_trait;
use std::path::PathBuf;
use std::time::Duration;
use tokio::process::Command;
use tokio::time::timeout;
use visp_core::tool::{Tool, ToolContext, ToolResult};

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
fn build_grep_args(
    pattern: &str,
    path: &str,
    use_rg: bool,
    exclude_dirs: &[String],
    include: Option<&str>,
    context_lines: usize,
    max_matches: usize,
) -> (&'static str, Vec<String>) {
    if use_rg {
        let mut args: Vec<String> = vec!["-n".into()];
        for dir in EXCLUDE_DIRS {
            args.push("-g".into());
            args.push(format!("!{}", dir));
        }
        for dir in exclude_dirs {
            args.push("-g".into());
            args.push(format!("!{}", dir));
        }
        if let Some(inc) = include {
            args.push("-g".into());
            args.push(inc.into());
        }
        if context_lines > 0 {
            args.push("-C".into());
            args.push(context_lines.to_string());
        }
        if max_matches > 0 {
            args.push("-m".into());
            args.push(max_matches.to_string());
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
        if context_lines > 0 {
            args.push("-C".into());
            args.push(context_lines.to_string());
        }
        if max_matches > 0 {
            args.push("-m".into());
            args.push(max_matches.to_string());
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
async fn run_command(
    program: &str,
    args: &[String],
    timeout_secs: u64,
) -> Result<(String, String), ToolResult> {
    let cmd_output = match timeout(
        Duration::from_secs(timeout_secs),
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
                program, timeout_secs
            )));
        }
    };

    let stdout = String::from_utf8_lossy(&cmd_output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&cmd_output.stderr).to_string();
    Ok((stdout, stderr))
}

/// Grep 内容搜索，支持 per-tool 配置
pub struct Grep {
    timeout_secs: u64,
    exclude_dirs: Vec<String>,
}

impl Default for Grep {
    fn default() -> Self {
        Self {
            timeout_secs: GREP_TIMEOUT_SECS,
            exclude_dirs: Vec::new(),
        }
    }
}

impl Grep {
    /// 从 daemon 配置的 raw toml 值构造
    /// raw = config.tool.get("grep") → Option<&toml::Value>
    pub fn from_toml(raw: Option<&toml::Value>) -> Self {
        let mut timeout = GREP_TIMEOUT_SECS;
        let mut exclude: Vec<String> = Vec::new();

        if let Some(config) = raw.and_then(|v| v.as_table()) {
            if let Some(t) = config.get("timeout_secs").and_then(|v| v.as_integer())
                && t > 0
            {
                timeout = t as u64;
            }
            if let Some(dirs) = config.get("exclude_dirs").and_then(|v| v.as_array()) {
                exclude = dirs
                    .iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect();
            }
        }

        Self {
            timeout_secs: timeout,
            exclude_dirs: exclude,
        }
    }
}

#[async_trait]
impl Tool for Grep {
    fn name(&self) -> &str {
        "grep"
    }

    fn category(&self) -> &str {
        "common"
    }

    fn description(&self) -> &str {
        "Search file contents using regular expressions. \
         Use this to find code patterns, error messages, or any text across the project. \
         Uses ripgrep for fast recursive search. \
         Binary files and hidden directories are excluded by default. \
         Supports full regex syntax (e.g., 'fn\\\\s+\\\\w+' for function definitions)."
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "pattern": {
                    "type": "string",
                    "description": "Regular expression pattern to search for."
                },
                "path": {
                    "type": "string",
                    "description": "Optional search path (directory), relative to project root. Defaults to the working directory."
                },
                "include": {
                    "type": "string",
                    "description": "Optional glob pattern to filter files (e.g., '*.rs'). Passed to rg's -g option."
                },
                "context": {
                    "type": "integer",
                    "description": "Number of context lines to show around each match (0-50, clamped if exceeded). Default: 0."
                },
                "max_matches": {
                    "type": "integer",
                    "description": "Maximum number of matches to return (1-500, clamped if exceeded). Default: 50."
                }
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

        let include = arguments
            .get("include")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty());

        let context_lines = match arguments.get("context").and_then(|v| v.as_i64()) {
            Some(c) if c > 50 => 50_usize,
            Some(c) if c > 0 => c as usize,
            _ => 0,
        };

        let max_matches = match arguments.get("max_matches").and_then(|v| v.as_i64()) {
            Some(m) if m > 500 => 500_usize,
            Some(m) if m <= 0 => 1_usize,
            Some(m) => m as usize,
            None => 50_usize,
        };

        let use_rg = has_rg();
        let (program, args) = build_grep_args(
            pattern,
            path_str,
            use_rg,
            &self.exclude_dirs,
            include,
            context_lines,
            max_matches,
        );

        let (stdout, stderr) = match run_command(program, &args, self.timeout_secs).await {
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

/// Glob 文件名搜索，支持 per-tool 配置
pub struct Glob {
    timeout_secs: u64,
}

impl Default for Glob {
    fn default() -> Self {
        Self {
            timeout_secs: GREP_TIMEOUT_SECS,
        }
    }
}

impl Glob {
    /// 从 daemon 配置的 raw toml 值构造
    /// raw = config.tool.get("glob") → Option<&toml::Value>
    pub fn from_toml(raw: Option<&toml::Value>) -> Self {
        let mut timeout = GREP_TIMEOUT_SECS;

        if let Some(config) = raw.and_then(|v| v.as_table())
            && let Some(t) = config.get("timeout_secs").and_then(|v| v.as_integer())
            && t > 0
        {
            timeout = t as u64;
        }

        Self {
            timeout_secs: timeout,
        }
    }
}

#[async_trait]
impl Tool for Glob {
    fn name(&self) -> &str {
        "glob"
    }

    fn category(&self) -> &str {
        "common"
    }

    fn description(&self) -> &str {
        "Find files by filename pattern using glob/wildcard matching. \
         Use this to locate files when you know the name pattern but not the path. \
         Supports patterns like '**/*.rs', 'src/**/test*.ts', '*.toml'. \
         Results are sorted by modification time (newest first). \
         Uses ripgrep for fast recursive search."
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "pattern": {
                    "type": "string",
                    "description": "Glob pattern to match filenames (e.g., '**/*.rs', '*.toml')."
                },
                "path": {
                    "type": "string",
                    "description": "Optional search directory, relative to project root. Defaults to working directory."
                }
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

        let (stdout, stderr) = match run_command(program, &args, self.timeout_secs).await {
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

        let grep = Grep::default();
        let ctx = ToolContext {
            working_dir: dir.path().to_path_buf(),
            session_id: None,
            permission_rules: None,
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
        let grep = Grep::default();
        let ctx = ToolContext {
            working_dir: dir.path().to_path_buf(),
            session_id: None,
            permission_rules: None,
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

        let glob = Glob::default();
        let ctx = ToolContext {
            working_dir: dir.path().to_path_buf(),
            session_id: None,
            permission_rules: None,
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

        let glob = Glob::default();
        let ctx = ToolContext {
            working_dir: dir.path().to_path_buf(),
            session_id: None,
            permission_rules: None,
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
        let glob = Glob::default();
        let ctx = ToolContext {
            working_dir: dir.path().to_path_buf(),
            session_id: None,
            permission_rules: None,
        };
        let result = glob.execute(serde_json::json!({}), &ctx).await;
        assert!(result.is_error);
        assert!(result.content.contains("pattern"));
    }

    // ── from_toml 测试 ──────────────────────────────────────────────────────

    #[test]
    fn test_grep_from_toml_default() {
        let grep = Grep::from_toml(None);
        assert_eq!(grep.timeout_secs, GREP_TIMEOUT_SECS);
        assert!(grep.exclude_dirs.is_empty());
    }

    #[test]
    fn test_grep_from_toml_exclude_dirs() {
        let value: toml::Value = toml::from_str(
            r#"
timeout_secs = 60
exclude_dirs = ["vendor", ".build"]
"#,
        )
        .unwrap();
        let grep = Grep::from_toml(Some(&value));
        assert_eq!(grep.timeout_secs, 60);
        assert_eq!(grep.exclude_dirs, vec!["vendor", ".build"]);
    }

    #[test]
    fn test_grep_from_toml_zero_timeout_ignored() {
        let value: toml::Value = toml::from_str(
            r#"
timeout_secs = 0
"#,
        )
        .unwrap();
        let grep = Grep::from_toml(Some(&value));
        assert_eq!(grep.timeout_secs, GREP_TIMEOUT_SECS);
    }

    #[test]
    fn test_glob_from_toml_default() {
        let glob = Glob::from_toml(None);
        assert_eq!(glob.timeout_secs, GREP_TIMEOUT_SECS);
    }

    #[test]
    fn test_glob_from_toml_timeout() {
        let value: toml::Value = toml::from_str(
            r#"
timeout_secs = 45
"#,
        )
        .unwrap();
        let glob = Glob::from_toml(Some(&value));
        assert_eq!(glob.timeout_secs, 45);
    }

    #[test]
    fn test_glob_from_toml_zero_timeout_ignored() {
        let value: toml::Value = toml::from_str(
            r#"
timeout_secs = 0
"#,
        )
        .unwrap();
        let glob = Glob::from_toml(Some(&value));
        assert_eq!(glob.timeout_secs, GREP_TIMEOUT_SECS);
    }

    // ── grep 新参数测试 ─────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_grep_with_context() {
        let dir = tempdir().unwrap();
        // Create a file with context around the match
        fs::write(
            dir.path().join("main.rs"),
            "line1\nline2\nline3\nMATCH\nline4\nline5\nline6\n",
        )
        .unwrap();

        let grep = Grep::default();
        let ctx = ToolContext {
            working_dir: dir.path().to_path_buf(),
            session_id: None,
            permission_rules: None,
        };
        let result = grep
            .execute(serde_json::json!({"pattern": "MATCH", "context": 2}), &ctx)
            .await;

        assert!(!result.is_error, "unexpected error: {}", result.content);
        // With context=2, we should see 2 lines before and 2 lines after
        // File layout: line1(3-before), line2(2-before), line3(1-before), MATCH, line4(1-after), line5(2-after), line6(3-after)
        assert!(
            result.content.contains("line2"),
            "should contain line2 (2 lines before MATCH):\n{}",
            result.content
        );
        assert!(
            result.content.contains("line3"),
            "should contain line3 (1 line before MATCH):\n{}",
            result.content
        );
        assert!(
            result.content.contains("line4"),
            "should contain line4 (1 line after MATCH):\n{}",
            result.content
        );
        assert!(
            result.content.contains("line5"),
            "should contain line5 (2 lines after MATCH):\n{}",
            result.content
        );
    }

    #[tokio::test]
    async fn test_grep_with_include() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("a.rs"), "hello\n").unwrap();
        fs::write(dir.path().join("b.txt"), "hello\n").unwrap();

        let grep = Grep::default();
        let ctx = ToolContext {
            working_dir: dir.path().to_path_buf(),
            session_id: None,
            permission_rules: None,
        };
        let result = grep
            .execute(
                serde_json::json!({"pattern": "hello", "include": "*.rs"}),
                &ctx,
            )
            .await;

        assert!(!result.is_error, "unexpected error: {}", result.content);
        if has_rg() {
            // With rg, include restricts to *.rs files
            assert!(
                result.content.contains("a.rs"),
                "should find a.rs:\n{}",
                result.content
            );
            assert!(
                !result.content.contains("b.txt"),
                "should NOT find b.txt:\n{}",
                result.content
            );
        } else {
            // Without rg, include is ignored; grep finds both
            assert!(
                result.content.contains("hello"),
                "should find matches:\n{}",
                result.content
            );
        }
    }

    #[tokio::test]
    async fn test_grep_with_max_matches() {
        let dir = tempdir().unwrap();
        // Create a file with 10 matching lines
        let content: String = (0..10).map(|i| format!("match line {}\n", i)).collect();
        fs::write(dir.path().join("data.txt"), content).unwrap();

        let grep = Grep::default();
        let ctx = ToolContext {
            working_dir: dir.path().to_path_buf(),
            session_id: None,
            permission_rules: None,
        };
        let result = grep
            .execute(
                serde_json::json!({"pattern": "match", "max_matches": 3}),
                &ctx,
            )
            .await;

        assert!(!result.is_error, "unexpected error: {}", result.content);
        // Should find at most 3 matches
        let match_count = result.content.matches("match line").count();
        assert!(
            match_count <= 3,
            "expected ≤3 matches, got {}:\n{}",
            match_count,
            result.content
        );
    }

    #[tokio::test]
    async fn test_grep_context_clamped() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("main.rs"),
            "a\nb\nc\nd\ne\nMATCH\nf\ng\nh\ni\nj\n",
        )
        .unwrap();

        let grep = Grep::default();
        let ctx = ToolContext {
            working_dir: dir.path().to_path_buf(),
            session_id: None,
            permission_rules: None,
        };
        // context=100 should be clamped to 50; at minimum no crash
        let result = grep
            .execute(
                serde_json::json!({"pattern": "MATCH", "context": 100}),
                &ctx,
            )
            .await;

        assert!(!result.is_error, "unexpected error: {}", result.content);
        // Should still find the match
        assert!(
            result.content.contains("MATCH"),
            "should find MATCH:\n{}",
            result.content
        );
    }

    #[tokio::test]
    async fn test_grep_max_matches_clamped() {
        let dir = tempdir().unwrap();
        let content: String = (0..5).map(|i| format!("match line {}\n", i)).collect();
        fs::write(dir.path().join("data.txt"), content).unwrap();

        let grep = Grep::default();
        let ctx = ToolContext {
            working_dir: dir.path().to_path_buf(),
            session_id: None,
            permission_rules: None,
        };
        // max_matches=0 should be clamped to 1
        let result = grep
            .execute(
                serde_json::json!({"pattern": "match", "max_matches": 0}),
                &ctx,
            )
            .await;

        assert!(!result.is_error, "unexpected error: {}", result.content);
        // Should find at least 1 match
        assert!(
            result.content.contains("match line"),
            "should find at least one match:\n{}",
            result.content
        );
    }
}
