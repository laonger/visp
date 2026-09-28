//! R-2：随包 `assets/hooks/herdr.hook.sh` 参考脚本集成测试。
//!
//! 依据 herdr 集成设计 §11.1（外置脚本形态）与任务 R-2 规格。测试以假
//! `HERDR_BIN_PATH`（记录 argv 的临时脚本）断言护栏、三态映射与未映射事件静默。
//! 脚本统一以 `sh <path>` 调用，不依赖仓库保存的可执行位。

#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use tempfile::TempDir;

/// 参考脚本路径（仓库根 `assets/hooks/herdr.hook.sh`）。
fn script_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("assets")
        .join("hooks")
        .join("herdr.hook.sh")
}

/// 测试沙盒：假 herdr CLI（记录 argv）+ 记录文件。
struct Sandbox {
    /// 仅用于持有临时目录生命周期（含假 CLI 与记录文件）。
    _dir: TempDir,
    fake_bin: PathBuf,
    out: PathBuf,
}

impl Sandbox {
    fn new() -> Self {
        let dir = TempDir::new().unwrap();
        let fake_bin = dir.path().join("fake-herdr");
        // 假 CLI：把收到的 argv 逐行追加写入 $HERDR_TEST_OUT，并以标记行分隔每次调用
        // （`/bin/sh` 绝对解释器，不依赖 PATH）。追加而非覆盖，以便记录多次调用。
        fs::write(
            &fake_bin,
            "#!/bin/sh\nprintf '%s\\n' \"$@\" >> \"$HERDR_TEST_OUT\"\nprintf '%s\\n' '=== VISP CALL ===' >> \"$HERDR_TEST_OUT\"\n",
        )
        .unwrap();
        let mut perms = fs::metadata(&fake_bin).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&fake_bin, perms).unwrap();

        let out = dir.path().join("argv.txt");
        Self {
            _dir: dir,
            fake_bin,
            out,
        }
    }

    /// 以给定环境运行脚本；返回假 CLI 记录的 argv（未被调用则 `None`）。
    ///
    /// 仅返回**第一次**调用的 argv；需要断言多次调用时用 [`Self::run_calls`]。
    /// 每次调用先删除记录文件，使「文件缺失」可靠等价于「假 CLI 未被触发」。
    fn run(
        &self,
        event: Option<&str>,
        herdr_env: Option<&str>,
        pane: Option<&str>,
    ) -> Option<Vec<String>> {
        self.run_calls(event, herdr_env, pane).into_iter().next()
    }

    /// 以给定环境运行脚本；返回假 CLI 的**每次**调用 argv（按调用顺序）。
    ///
    /// 未触发假 CLI 时返回空 `Vec`。不设置 `VISP_HERDR_NOTIFY`。
    fn run_calls(
        &self,
        event: Option<&str>,
        herdr_env: Option<&str>,
        pane: Option<&str>,
    ) -> Vec<Vec<String>> {
        self.run_calls_env(event, herdr_env, pane, None)
    }

    /// 同 [`Self::run_calls`]，并额外设置 `VISP_HERDR_NOTIFY`（`None` 表示不设置）。
    fn run_calls_env(
        &self,
        event: Option<&str>,
        herdr_env: Option<&str>,
        pane: Option<&str>,
        notify: Option<&str>,
    ) -> Vec<Vec<String>> {
        let script = script_path();
        assert!(script.exists(), "参考脚本缺失: {}", script.display());
        let _ = fs::remove_file(&self.out);

        let mut cmd = Command::new("sh");
        cmd.arg(&script)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .env("HERDR_TEST_OUT", &self.out)
            .env("HERDR_BIN_PATH", &self.fake_bin)
            // 显式清除，避免测试运行环境自身已处于 herdr pane 内造成污染。
            .env_remove("HERDR_ENV")
            .env_remove("HERDR_PANE_ID")
            .env_remove("VISP_HOOK_EVENT")
            .env_remove("VISP_HERDR_NOTIFY");
        if let Some(env) = herdr_env {
            cmd.env("HERDR_ENV", env);
        }
        if let Some(pane) = pane {
            cmd.env("HERDR_PANE_ID", pane);
        }
        if let Some(event) = event {
            cmd.env("VISP_HOOK_EVENT", event);
        }
        if let Some(notify) = notify {
            cmd.env("VISP_HERDR_NOTIFY", notify);
        }

        let status = cmd.status().expect("运行 hook 脚本失败");
        assert!(status.success(), "hook 脚本非零退出: {status:?}");

        let raw = fs::read_to_string(&self.out).unwrap_or_default();
        let mut calls: Vec<Vec<String>> = Vec::new();
        let mut current: Vec<String> = Vec::new();
        for line in raw.lines() {
            if line == "=== VISP CALL ===" {
                calls.push(std::mem::take(&mut current));
            } else {
                current.push(line.to_string());
            }
        }
        calls
    }
}

const PANE: &str = "pane-42";

/// 断言 argv 中出现 `--state <expected>`。
fn assert_state(argv: &[String], expected: &str) {
    let idx = argv
        .iter()
        .position(|a| a == "--state")
        .expect("argv 缺少 --state");
    assert_eq!(argv.get(idx + 1).map(String::as_str), Some(expected));
}

/// 断言 argv 不含 `--message`。
fn assert_no_message(argv: &[String]) {
    assert!(
        !argv.iter().any(|a| a == "--message"),
        "不应携带 --message: {argv:?}"
    );
}

// 1. PermissionRequest → blocked + message，argv 形状精确。
#[test]
fn permission_request_reports_blocked_with_message() {
    let sandbox = Sandbox::new();
    let argv = sandbox
        .run(Some("PermissionRequest"), Some("1"), Some(PANE))
        .expect("PermissionRequest 未触发假 CLI");

    let expected: Vec<String> = [
        "pane",
        "report-agent",
        PANE,
        "--source",
        "custom:visp",
        "--agent",
        "visp",
        "--state",
        "blocked",
        "--message",
        "visp: 等待用户处理",
    ]
    .into_iter()
    .map(String::from)
    .collect();
    assert_eq!(argv, expected);
}

// 2. UserPromptSubmit → working；五个 idle 事件 → idle（均无 message）。
#[test]
fn prompt_submit_reports_working_and_terminal_events_report_idle() {
    let sandbox = Sandbox::new();

    let working = sandbox
        .run(Some("UserPromptSubmit"), Some("1"), Some(PANE))
        .expect("UserPromptSubmit 未触发假 CLI");
    assert_state(&working, "working");
    assert_no_message(&working);

    for event in [
        "Stop",
        "StopFailure",
        "AgentRunEnd",
        "SubagentStop",
        "SessionStart",
    ] {
        let argv = sandbox
            .run(Some(event), Some("1"), Some(PANE))
            .unwrap_or_else(|| panic!("{event} 未触发假 CLI"));
        assert_state(&argv, "idle");
        assert_no_message(&argv);
    }
}

// 3. 护栏：HERDR_ENV 非 1 / HERDR_PANE_ID 缺失或为空 → 不调用假 CLI。
#[test]
fn guard_skips_outside_herdr_or_without_pane() {
    let sandbox = Sandbox::new();

    assert!(
        sandbox.run(Some("Stop"), None, Some(PANE)).is_none(),
        "HERDR_ENV 未设仍调用假 CLI"
    );
    assert!(
        sandbox.run(Some("Stop"), Some("0"), Some(PANE)).is_none(),
        "HERDR_ENV=0 仍调用假 CLI"
    );
    assert!(
        sandbox.run(Some("Stop"), Some("1"), None).is_none(),
        "HERDR_PANE_ID 未设仍调用假 CLI"
    );
    assert!(
        sandbox.run(Some("Stop"), Some("1"), Some("")).is_none(),
        "HERDR_PANE_ID 为空仍调用假 CLI"
    );
}

// 4. 未映射事件（PostToolUse）/ 无事件 → 不调用假 CLI。
#[test]
fn unmapped_event_is_silent() {
    let sandbox = Sandbox::new();

    assert!(
        sandbox
            .run(Some("PostToolUse"), Some("1"), Some(PANE))
            .is_none(),
        "未映射事件仍调用假 CLI"
    );
    assert!(
        sandbox.run(None, Some("1"), Some(PANE)).is_none(),
        "无事件仍调用假 CLI"
    );
}

// 5. Stop + 通知开启（默认）→ 两次调用：先 report-agent（idle），再显式 notification。
#[test]
fn stop_sends_explicit_notification_after_report() {
    let sandbox = Sandbox::new();
    let calls = sandbox.run_calls(Some("Stop"), Some("1"), Some(PANE));
    assert_eq!(calls.len(), 2, "Stop 应触发两次调用: {calls:?}");

    // 第一次：状态上报仍为 report-agent --state idle。
    let expected_report: Vec<String> = [
        "pane",
        "report-agent",
        PANE,
        "--source",
        "custom:visp",
        "--agent",
        "visp",
        "--state",
        "idle",
    ]
    .into_iter()
    .map(String::from)
    .collect();
    assert_eq!(calls[0], expected_report, "第一次调用应为状态上报");

    // 第二次：显式完成通知。
    let expected_notify: Vec<String> = [
        "notification",
        "show",
        "visp",
        "--body",
        "回合完成",
        "--sound",
        "done",
    ]
    .into_iter()
    .map(String::from)
    .collect();
    assert_eq!(calls[1], expected_notify, "第二次调用应为显式通知");
}

// 6. VISP_HERDR_NOTIFY 关闭值（0/false/off，大小写不敏感）→ Stop 只上报、不发通知。
#[test]
fn notify_disabled_suppresses_notification() {
    let sandbox = Sandbox::new();

    for value in ["0", "false", "off", "FALSE", "Off"] {
        let calls = sandbox.run_calls_env(Some("Stop"), Some("1"), Some(PANE), Some(value));
        assert_eq!(
            calls.len(),
            1,
            "VISP_HERDR_NOTIFY={value} 仍发通知: {calls:?}"
        );
        assert_eq!(calls[0][0], "pane", "关闭通知后应只剩状态上报");
    }
}

// 7. 非 Stop 事件（UserPromptSubmit/PermissionRequest 及 Agent* 终态）→ 从不发通知。
#[test]
fn non_stop_events_never_notify() {
    let sandbox = Sandbox::new();

    for event in [
        "UserPromptSubmit",
        "PermissionRequest",
        "AgentRunEnd",
        "SubagentStop",
        "SessionStart",
    ] {
        let calls = sandbox.run_calls(Some(event), Some("1"), Some(PANE));
        assert_eq!(calls.len(), 1, "{event} 不应发通知: {calls:?}");
        assert!(
            !calls[0].iter().any(|a| a == "notification"),
            "{event} 产生了 notification 调用: {calls:?}"
        );
    }
}
