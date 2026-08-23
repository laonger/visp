use super::*;
use std::path::Path;
use tempfile::{TempDir, tempdir};

fn test_context(dir: &Path) -> ToolContext {
    ToolContext {
        working_dir: dir.to_path_buf(),
        session_id: None,
        permission_rules: None,
        global_tx: None,
        visp_trace_id: None,
        iter_span_w3c_id: None,
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

#[tokio::test]
async fn test_bash_workdir_param_executes_in_subdir() {
    let dir = tempdir().unwrap();
    let ctx = test_context(dir.path());
    // Create a subdirectory with a unique file
    let subdir = dir.path().join("sub");
    std::fs::create_dir(&subdir).unwrap();
    std::fs::write(subdir.join("marker.txt"), "found").unwrap();
    // Use workdir param to run `cat marker.txt` from the subdir
    let result = Bash::default()
        .execute(
            serde_json::json!({"command": "cat marker.txt", "workdir": "sub"}),
            &ctx,
        )
        .await;
    assert!(!result.is_error, "should succeed in subdir");
    assert!(
        result.content.contains("found"),
        "should read file from workdir, got: {:?}",
        result.content
    );
}

#[tokio::test]
async fn test_bash_workdir_param_absolute_path() {
    let dir = tempdir().unwrap();
    let ctx = test_context(dir.path());
    let subdir = dir.path().join("abs_sub");
    std::fs::create_dir(&subdir).unwrap();
    std::fs::write(subdir.join("abs_marker.txt"), "abs_found").unwrap();
    // Use absolute path as workdir
    let abs_path = subdir.to_string_lossy().to_string();
    let result = Bash::default()
        .execute(
            serde_json::json!({"command": "cat abs_marker.txt", "workdir": abs_path}),
            &ctx,
        )
        .await;
    assert!(!result.is_error, "should succeed with absolute workdir");
    assert!(
        result.content.contains("abs_found"),
        "should read file from absolute workdir, got: {:?}",
        result.content
    );
}

#[tokio::test]
async fn test_bash_workdir_outside_project_rejected() {
    let dir = tempdir().unwrap();
    let ctx = test_context(dir.path());
    // Try to escape via ../
    let result = Bash::default()
        .execute(
            serde_json::json!({"command": "echo hello", "workdir": "../outside"}),
            &ctx,
        )
        .await;
    assert!(result.is_error, "should reject workdir outside project");
    assert!(
        result.content.to_lowercase().contains("invalid workdir"),
        "should mention invalid workdir, got: {:?}",
        result.content
    );
}

#[tokio::test]
async fn test_bash_workdir_absolute_path_outside_project_rejected() {
    let dir = tempdir().unwrap();
    let ctx = test_context(dir.path());
    // Absolute path outside working_dir - must be rejected even though
    // Path::join replaces the base when given an absolute path.
    let outside = TempDir::new().unwrap();
    let outside_path = outside.path().to_string_lossy().to_string();
    let result = Bash::default()
        .execute(
            serde_json::json!({"command": "echo hello", "workdir": outside_path}),
            &ctx,
        )
        .await;
    assert!(
        result.is_error,
        "should reject absolute workdir outside project"
    );
    assert!(
        result.content.to_lowercase().contains("invalid workdir"),
        "should mention invalid workdir, got: {:?}",
        result.content
    );
}

#[tokio::test]
async fn test_bash_workdir_system_path_rejected() {
    let dir = tempdir().unwrap();
    let ctx = test_context(dir.path());
    // Well-known system path that definitely exists but is outside project
    let result = Bash::default()
        .execute(
            serde_json::json!({"command": "echo hello", "workdir": "/tmp"}),
            &ctx,
        )
        .await;
    assert!(result.is_error, "should reject /tmp as workdir");
    assert!(
        result.content.to_lowercase().contains("invalid workdir"),
        "should mention invalid workdir, got: {:?}",
        result.content
    );
}

#[tokio::test]
async fn test_bash_workdir_nonexistent_rejected() {
    let dir = tempdir().unwrap();
    let ctx = test_context(dir.path());
    let result = Bash::default()
        .execute(
            serde_json::json!({"command": "echo hello", "workdir": "no_such_dir"}),
            &ctx,
        )
        .await;
    assert!(result.is_error, "should reject nonexistent workdir");
    assert!(
        result.content.to_lowercase().contains("invalid workdir"),
        "should mention invalid workdir, got: {:?}",
        result.content
    );
}

#[tokio::test]
async fn test_bash_workdir_empty_falls_back_to_context() {
    let dir = tempdir().unwrap();
    let ctx = test_context(dir.path());
    std::fs::write(dir.path().join("root_marker.txt"), "root").unwrap();
    // Empty workdir should fall back to context.working_dir
    let result = Bash::default()
        .execute(
            serde_json::json!({"command": "cat root_marker.txt", "workdir": ""}),
            &ctx,
        )
        .await;
    assert!(!result.is_error, "should fall back to context working_dir");
    assert!(
        result.content.contains("root"),
        "should read file from context working_dir, got: {:?}",
        result.content
    );
}

#[tokio::test]
async fn test_bash_no_workdir_uses_context() {
    let dir = tempdir().unwrap();
    let ctx = test_context(dir.path());
    std::fs::write(dir.path().join("ctx_marker.txt"), "ctx").unwrap();
    // No workdir param at all
    let result = Bash::default()
        .execute(serde_json::json!({"command": "cat ctx_marker.txt"}), &ctx)
        .await;
    assert!(!result.is_error, "should use context working_dir");
    assert!(
        result.content.contains("ctx"),
        "should read file from context working_dir, got: {:?}",
        result.content
    );
}

#[tokio::test]
async fn test_bash_timeout_kills_child() {
    let dir = tempdir().unwrap();
    let ctx = test_context(dir.path());
    let start = std::time::Instant::now();
    let result = Bash::default()
        .execute(
            serde_json::json!({"command": "sleep 61", "timeout": 2}),
            &ctx,
        )
        .await;
    let elapsed = start.elapsed();
    assert!(result.is_error, "should time out");
    assert!(
        result.content.contains("timed out"),
        "should mention timeout, got: {:?}",
        result.content
    );
    assert!(
        elapsed.as_secs() < 10,
        "tool should return promptly after timeout, took {:?}",
        elapsed
    );
    // The child process must be killed, not left running as a zombie/orphan.
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    let pgrep = tokio::process::Command::new("pgrep")
        .arg("-f")
        .arg("sleep 61")
        .output()
        .await
        .expect("pgrep should run");
    assert!(
        !pgrep.status.success(),
        "child process should be killed after timeout, still running: {}",
        String::from_utf8_lossy(&pgrep.stdout)
    );
}

#[tokio::test]
async fn test_bash_large_output_no_deadlock() {
    let dir = tempdir().unwrap();
    let ctx = test_context(dir.path());
    // Produces 90KB of output: more than the OS pipe buffer (16-64KB), so the
    // child would block forever on a full pipe unless stdout is drained
    // concurrently while waiting for the process to exit.
    let result = Bash::default()
        .execute(
            serde_json::json!({"command": "head -c 90000 /dev/zero", "timeout": 10}),
            &ctx,
        )
        .await;
    assert!(
        !result.is_error,
        "should not spuriously time out on a chatty command, got: {:?}",
        result.content
    );
    assert!(
        result.content.len() >= 90000,
        "output should include all 90KB, got {} bytes",
        result.content.len()
    );
}

// ── is_blocked 词边界匹配测试 ─────────────────────────────────────────

fn bash_with_node_blacklist() -> Bash {
    Bash {
        blocked_commands: vec!["node".into(), "npm".into(), "pnpm".into()],
        default_timeout_secs: DEFAULT_TIMEOUT_SECS,
        max_output_bytes: DEFAULT_MAX_OUTPUT_BYTES,
    }
}

#[test]
fn test_blocked_word_at_start() {
    // 词首位置
    assert!(bash_with_node_blacklist().is_blocked("node test/foo.js"));
}

#[test]
fn test_blocked_word_before_and() {
    // 词边界后是 && 运算符
    assert!(bash_with_node_blacklist().is_blocked("node && echo hi"));
}

#[test]
fn test_blocked_word_before_semicolon() {
    // 词边界后是分号
    assert!(bash_with_node_blacklist().is_blocked("node; echo"));
}

#[test]
fn test_blocked_word_with_flags() {
    assert!(bash_with_node_blacklist().is_blocked("node -e \"x\""));
}

#[test]
fn test_not_blocked_node_modules() {
    // 不误伤 node_modules（_ 是词字符）
    assert!(!bash_with_node_blacklist().is_blocked("ls node_modules"));
}

#[test]
fn test_not_blocked_prefix_word() {
    // 不误伤以 node 为前缀的单词（nodemon）
    assert!(!bash_with_node_blacklist().is_blocked("nodemon server.js"));
}

#[test]
fn test_blocked_npm_and_pnpm() {
    let bash = bash_with_node_blacklist();
    assert!(bash.is_blocked("npm install"));
    assert!(bash.is_blocked("pnpm i"));
}

#[test]
fn test_blocked_builtin_sudo() {
    // 内置黑名单单词保持拦截
    assert!(Bash::default().is_blocked("sudo apt"));
}

#[test]
fn test_blocked_word_in_middle_of_pipeline() {
    // 命令链中间出现也要拦截
    assert!(bash_with_node_blacklist().is_blocked("echo hi && node test.js"));
}

#[test]
fn test_blocked_word_with_trailing_space_config() {
    // 兼容配置里带尾空格的黑名单项（daemon.toml 现状是 "node "）
    let bash = Bash {
        blocked_commands: vec!["node ".into()],
        default_timeout_secs: DEFAULT_TIMEOUT_SECS,
        max_output_bytes: DEFAULT_MAX_OUTPUT_BYTES,
    };
    assert!(bash.is_blocked("node && echo hi"));
    assert!(!bash.is_blocked("ls node_modules"));
}

#[test]
fn test_description_mentions_path_quoting() {
    // 回归：bash 工具必须提示"文件路径用引号包裹"（项目路径常含空格，如 "untitled folder 37"）
    use visp_core::tool::Tool;
    let bash = Bash::default();
    let desc = bash.description();
    assert!(
        desc.to_lowercase().contains("quotes") || desc.to_lowercase().contains("quote"),
        "description 应提示路径引号：{desc}"
    );
    assert!(
        desc.to_lowercase().contains("spaces") || desc.contains("空格"),
        "description 应提示路径含空格：{desc}"
    );
}

#[test]
fn test_parameters_mention_path_quoting() {
    use visp_core::tool::Tool;
    let bash = Bash::default();
    let params = bash.parameters();
    let cmd_desc = params["properties"]["command"]["description"]
        .as_str()
        .expect("command 参数应有描述");
    assert!(
        cmd_desc.to_lowercase().contains("quotes") || cmd_desc.to_lowercase().contains("quote"),
        "command 参数描述应提示路径引号：{cmd_desc}"
    );
}
