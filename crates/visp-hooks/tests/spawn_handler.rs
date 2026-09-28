//! 1b-2b 进程执行 Handler 测试（实施计划步骤 1b-2 测试表第 7–12 条）。
//!
//! 覆盖：超时杀整棵进程树、白名单 env 隔离、stdin 完整 JSON 写完即关、
//! `cwd = project_path`、`VISP_IN_HOOK=1`、失败隔离（仅计数不 panic）。

use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::time::{Duration, Instant};

use tempfile::TempDir;
use visp_hooks::*;

/// 进程规格快捷方式（`timeout_ms`/`cwd` 取默认）。
fn spec(command: &str, args: &[&str]) -> ProcessSpec {
    ProcessSpec {
        command: command.to_string(),
        args: args.iter().map(|a| a.to_string()).collect(),
        ..Default::default()
    }
}

/// 规则快捷方式（事件名不影响 Handler 直接调用）。
fn rule(id: &str) -> DispatchRule {
    DispatchRule {
        id: id.to_string(),
        event: vec![HookEventName::Stop],
        ..Default::default()
    }
}

/// 单规则 Handler。
fn make_handler(dir: &Path, id: &str, spec: ProcessSpec) -> SpawnHandler {
    SpawnHandler::new(dir.to_path_buf(), HashMap::from([(id.to_string(), spec)]))
}

/// 携带完整 JSON 载荷的事件视图。
fn event() -> DispatchInput {
    DispatchInput::new(HookEventName::Stop)
        .with_payload(r#"{"hook_event_name":"Stop","status":"completed"}"#)
}

/// 直接调用 Handler（不经决策内核）。
async fn run_one(handler: &SpawnHandler, id: &str) {
    handler.run(&rule(id), &event()).await;
}

/// 读取脚本写出的 pid（短重试，容忍调度延迟）。
#[cfg(unix)]
fn read_pid(path: &Path) -> i32 {
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        if let Ok(text) = fs::read_to_string(path)
            && let Ok(pid) = text.trim().parse::<i32>()
        {
            return pid;
        }
        assert!(Instant::now() < deadline, "未读取到子进程 pid");
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// 进程是否仍存在（`kill(pid, 0)`）。
#[cfg(unix)]
fn pid_alive(pid: i32) -> bool {
    // SAFETY: 信号 0 仅做存在性探测，无副作用。
    unsafe { libc::kill(pid, 0) == 0 }
}

/// 轮询等待进程消失；返回是否在超时内消失。
#[cfg(unix)]
fn wait_until_dead(pid: i32, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if !pid_alive(pid) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    !pid_alive(pid)
}

// 7. 超时：超 timeout_ms 杀进程，含孙进程（进程组终止验证）。
#[cfg(unix)]
#[tokio::test]
async fn timeout_kills_whole_process_tree_including_grandchild() {
    let dir = TempDir::new().unwrap();
    let pid_file = dir.path().join("grandchild.pid");

    // `sh` 为直接子进程，后台 `sleep` 为孙进程；非交互 sh 不做 job control，
    // 二者同属一个进程组。
    let mut proc = spec(
        "sh",
        &["-c", "sleep 30 & echo $! > \"$TREE_PID_FILE\"; wait"],
    );
    proc.timeout_ms = 700;
    proc.env
        .push(("TREE_PID_FILE".into(), pid_file.display().to_string()));
    let handler = make_handler(dir.path(), "tree", proc);

    let started = Instant::now();
    run_one(&handler, "tree").await;

    assert!(started.elapsed() < Duration::from_secs(5), "超时未及时返回");
    assert_eq!(handler.stats().timed_out(), 1);
    assert_eq!(handler.stats().failed(), 0);

    let grandchild = read_pid(&pid_file);
    assert!(
        wait_until_dead(grandchild, Duration::from_secs(3)),
        "孙进程 {grandchild} 仍存活，进程树未被终止"
    );
}

// 8. 白名单 env：子进程仅见白名单 + 规则 env；父进程密钥不可见。
#[tokio::test]
async fn child_env_is_allowlisted_and_hides_parent_secrets() {
    let dir = TempDir::new().unwrap();
    let out = dir.path().join("env.txt");

    let mut proc = spec("sh", &["-c", "env > \"$ENV_OUT\""]);
    proc.env.push(("ENV_OUT".into(), out.display().to_string()));
    proc.env.push(("RULE_CUSTOM".into(), "rule-value".into()));

    let parent_env = vec![
        ("PATH".to_string(), "/usr/bin:/bin".to_string()),
        ("LANG".to_string(), "en_US.UTF-8".to_string()),
        ("SUPER_SECRET_TOKEN".to_string(), "top-secret".to_string()),
        ("HERDR_SOCKET".to_string(), "herdr-value".to_string()),
    ];
    let handler = SpawnHandler::with_parent_env(
        dir.path().to_path_buf(),
        HashMap::from([("env".to_string(), proc)]),
        parent_env,
    );

    run_one(&handler, "env").await;
    assert_eq!(handler.stats().executed(), 1);

    let body = fs::read_to_string(&out).unwrap();
    // 白名单 + 规则 env + 投递标量可见。
    assert!(body.contains("PATH=/usr/bin:/bin"), "{body}");
    assert!(body.contains("LANG=en_US.UTF-8"), "{body}");
    assert!(body.contains("RULE_CUSTOM=rule-value"), "{body}");
    assert!(body.contains("HERDR_SOCKET=herdr-value"), "{body}");
    assert!(body.contains("VISP_IN_HOOK=1"), "{body}");
    assert!(body.contains("VISP_HOOK_EVENT=Stop"), "{body}");
    assert!(body.contains("VISP_HOOK_RULE_ID=env"), "{body}");
    // 父进程密钥不可见。
    assert!(
        !body.contains("SUPER_SECRET_TOKEN"),
        "父进程密钥泄露: {body}"
    );
    assert!(!body.contains("top-secret"), "父进程密钥泄露: {body}");
}

// 9. stdin：完整 JSON 写入后关闭；大载荷边界不截断。
#[tokio::test]
async fn stdin_receives_complete_json_then_closes() {
    let dir = TempDir::new().unwrap();
    let out = dir.path().join("stdin.json");

    // `cat` 只有读到 EOF（stdin 关闭）才会退出并写完文件。
    let mut proc = spec("sh", &["-c", "cat > \"$STDIN_OUT\""]);
    proc.timeout_ms = 5_000;
    proc.env
        .push(("STDIN_OUT".into(), out.display().to_string()));
    let handler = make_handler(dir.path(), "stdin", proc);

    let big = format!(
        r#"{{"hook_event_name":"Stop","blob":"{}"}}"#,
        "x".repeat(200_000)
    );
    handler
        .run(
            &rule("stdin"),
            &DispatchInput::new(HookEventName::Stop).with_payload(big.clone()),
        )
        .await;

    assert_eq!(handler.stats().executed(), 1, "stdin 未关闭或 cat 异常退出");
    let written = fs::read_to_string(&out).unwrap();
    assert_eq!(written.len(), big.len(), "大载荷被截断");
    assert_eq!(written, big, "stdin 载荷与写入不一致");
}

// 10. cwd = canonical project_path（规则 cwd 可覆盖）。
#[tokio::test]
async fn cwd_defaults_to_project_path() {
    let dir = TempDir::new().unwrap();
    let out = dir.path().join("pwd.txt");

    let mut proc = spec("sh", &["-c", "pwd -P > \"$PWD_OUT\""]);
    proc.env.push(("PWD_OUT".into(), out.display().to_string()));
    let handler = make_handler(dir.path(), "cwd", proc);

    run_one(&handler, "cwd").await;
    assert_eq!(handler.stats().executed(), 1);
    let printed = fs::read_to_string(&out).unwrap();
    assert_eq!(
        fs::canonicalize(printed.trim()).unwrap(),
        fs::canonicalize(dir.path()).unwrap()
    );

    // 规则 cwd 覆盖 project_path。
    let sub = dir.path().join("sub");
    fs::create_dir_all(&sub).unwrap();
    let mut proc = spec("sh", &["-c", "pwd -P > \"$PWD_OUT\""]);
    proc.env.push(("PWD_OUT".into(), out.display().to_string()));
    proc.cwd = Some(sub.clone());
    let handler = make_handler(dir.path(), "cwd-override", proc);

    run_one(&handler, "cwd-override").await;
    let printed = fs::read_to_string(&out).unwrap();
    assert_eq!(
        fs::canonicalize(printed.trim()).unwrap(),
        fs::canonicalize(&sub).unwrap()
    );
}

// 11. VISP_IN_HOOK=1 防递归标记注入。
#[tokio::test]
async fn injects_visp_in_hook_marker() {
    let dir = TempDir::new().unwrap();
    let out = dir.path().join("in_hook.txt");

    let mut proc = spec("sh", &["-c", "printf %s \"$VISP_IN_HOOK\" > \"$OUT\""]);
    proc.env.push(("OUT".into(), out.display().to_string()));
    let handler = make_handler(dir.path(), "marker", proc);

    run_one(&handler, "marker").await;
    assert_eq!(handler.stats().executed(), 1);
    assert_eq!(fs::read_to_string(&out).unwrap(), "1");
}

// 12. 失败隔离：崩溃 / 非零退出 / 无法启动 → 仅计数 + 日志，不 panic、不上抛。
#[tokio::test]
async fn failures_are_counted_and_isolated() {
    let dir = TempDir::new().unwrap();

    // 无法启动：命令不存在。
    let handler = make_handler(
        dir.path(),
        "missing",
        spec("/nonexistent/visp-hook-cmd", &[]),
    );
    run_one(&handler, "missing").await;
    assert_eq!(handler.stats().spawn_failed(), 1);
    assert_eq!(handler.stats().executed(), 0);

    // 非零退出。
    let handler = make_handler(dir.path(), "exit", spec("sh", &["-c", "exit 3"]));
    run_one(&handler, "exit").await;
    assert_eq!(handler.stats().failed(), 1);
    assert_eq!(handler.stats().executed(), 0);

    // 崩溃（被信号终止）。
    let handler = make_handler(dir.path(), "crash", spec("sh", &["-c", "kill -9 $$"]));
    run_one(&handler, "crash").await;
    assert_eq!(handler.stats().failed(), 1);
    assert_eq!(handler.stats().executed(), 0);

    // 缺少执行规格：跳过并计数，不 panic。
    let handler = SpawnHandler::new(dir.path().to_path_buf(), HashMap::new());
    run_one(&handler, "unknown").await;
    assert_eq!(handler.stats().failed(), 1);
}
