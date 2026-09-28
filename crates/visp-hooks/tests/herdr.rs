//! 1b-4a 内置 herdr 绑定测试（设计 D8；herdr 集成设计决策 3–4）。
//!
//! 全程以假 `HERDR_BIN_PATH` 脚本记录 argv，不依赖真实 herdr 与真实环境变量
//! （护栏经 `from_env` 显式注入，避免污染进程环境）。

#![cfg(unix)]

use std::fs;
use std::path::Path;
use std::time::{Duration, Instant};

use tempfile::TempDir;
use visp_hooks::*;

/// 环境快照快捷方式。
fn env(entries: &[(&str, &str)]) -> Vec<(String, String)> {
    entries
        .iter()
        .map(|(key, value)| (key.to_string(), value.to_string()))
        .collect()
}

/// 造一个可执行的假 herdr：把收到的 argv 逐行写入 `out`。
fn recording_herdr(dir: &Path, out: &Path) -> String {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.join("herdr-fake.sh");
    let body = format!("#!/bin/sh\nprintf '%s\\n' \"$@\" > '{}'\n", out.display());
    fs::write(&path, body).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    path.to_string_lossy().into_owned()
}

/// 造一个行为可控（退出码 / 阻塞）的假 herdr。
fn behavior_herdr(dir: &Path, name: &str, body: &str) -> String {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.join(name);
    fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    path.to_string_lossy().into_owned()
}

fn read_args(path: &Path) -> Vec<String> {
    fs::read_to_string(path)
        .unwrap()
        .lines()
        .map(str::to_string)
        .collect()
}

/// 断言 `--flag value` 成对出现。
fn assert_flag_value(args: &[String], flag: &str, value: &str) {
    let found = args
        .windows(2)
        .any(|pair| pair[0] == flag && pair[1] == value);
    assert!(found, "argv 缺少 `{flag} {value}`：{args:?}");
}

/// 启用护栏的绑定。
fn enabled_binding(bin: &str) -> HerdrBinding {
    HerdrBinding::from_env(&env(&[
        (HERDR_ENV, "1"),
        (HERDR_BIN_PATH, bin),
        (HERDR_PANE_ID, "pane-7"),
    ]))
}

// 命令构造：argv 形状（纯函数，可测）。
#[test]
fn report_args_shape() {
    let args = report_args("pane-7", HerdrState::Blocked, Some("visp 待审批"));
    assert_eq!(args[0], "pane");
    assert_eq!(args[1], "report-agent");
    assert_eq!(args[2], "pane-7");
    assert_flag_value(&args, "--source", "custom:visp");
    assert_flag_value(&args, "--agent", "visp");
    assert_flag_value(&args, "--state", "blocked");
    assert_flag_value(&args, "--message", "visp 待审批");

    // 无 message 时不追加 `--message`。
    let args = report_args("p", HerdrState::Idle, None);
    assert!(!args.iter().any(|arg| arg == "--message"));
}

// 状态映射：覆盖全部约定事件；未约定事件不映射。
#[test]
fn state_mapping_covers_hook_events() {
    assert_eq!(
        state_for_event(HookEventName::UserPromptSubmit),
        Some(HerdrState::Working)
    );
    assert_eq!(
        state_for_event(HookEventName::PermissionRequest),
        Some(HerdrState::Blocked)
    );
    for event in [
        HookEventName::Stop,
        HookEventName::AgentRunEnd,
        HookEventName::SubagentStop,
        HookEventName::SessionStart,
        HookEventName::StopFailure,
        HookEventName::PostToolUseFailure,
    ] {
        assert_eq!(state_for_event(event), Some(HerdrState::Idle), "{event:?}");
    }
    assert_eq!(state_for_event(HookEventName::ToolCallRequested), None);
    assert_eq!(state_for_event(HookEventName::PostToolUse), None);
    assert_eq!(state_for_event(HookEventName::SessionEnd), None);
}

// 护栏：HERDR_ENV=1 且 BIN/PANE 均存在才启用。
#[test]
fn guard_requires_env_bin_and_pane() {
    assert!(
        HerdrBinding::from_env(&env(&[
            (HERDR_ENV, "1"),
            (HERDR_BIN_PATH, "/bin/true"),
            (HERDR_PANE_ID, "p1"),
        ]))
        .enabled()
    );
    for missing in [
        env(&[(HERDR_BIN_PATH, "/bin/true"), (HERDR_PANE_ID, "p1")]),
        env(&[
            (HERDR_ENV, "0"),
            (HERDR_BIN_PATH, "/bin/true"),
            (HERDR_PANE_ID, "p1"),
        ]),
        env(&[(HERDR_ENV, "1"), (HERDR_PANE_ID, "p1")]),
        env(&[(HERDR_ENV, "1"), (HERDR_BIN_PATH, "/bin/true")]),
        env(&[
            (HERDR_ENV, "1"),
            (HERDR_BIN_PATH, ""),
            (HERDR_PANE_ID, "p1"),
        ]),
    ] {
        assert!(!HerdrBinding::from_env(&missing).enabled(), "{missing:?}");
    }
}

// 护栏不满足：静默 no-op，不调用 CLI、不计数。
#[tokio::test]
async fn disabled_binding_is_silent_noop() {
    let dir = TempDir::new().unwrap();
    let out = dir.path().join("args.txt");
    let bin = recording_herdr(dir.path(), &out);

    let binding = HerdrBinding::from_env(&env(&[(HERDR_BIN_PATH, &bin), (HERDR_PANE_ID, "p1")]));
    assert!(!binding.enabled());

    let event = DispatchInput::new(HookEventName::PermissionRequest).with_kind("approval");
    binding.run(&builtin_rule(), &event).await;

    assert!(!out.exists(), "护栏不满足时不应调用 CLI");
    assert_eq!(binding.stats().reported(), 0);
}

// 未约定事件：即便护栏满足也 no-op。
#[tokio::test]
async fn unmapped_event_is_noop() {
    let dir = TempDir::new().unwrap();
    let out = dir.path().join("args.txt");
    let bin = recording_herdr(dir.path(), &out);
    let binding = enabled_binding(&bin);

    let event = DispatchInput::new(HookEventName::PostToolUse).with_tool_name("Bash");
    binding.run(&builtin_rule(), &event).await;

    assert!(!out.exists(), "未映射事件不应调用 CLI");
    assert_eq!(binding.stats().reported(), 0);
}

// PermissionRequest → blocked，携带脱敏摘要 message。
#[tokio::test]
async fn permission_request_reports_blocked() {
    let dir = TempDir::new().unwrap();
    let out = dir.path().join("args.txt");
    let bin = recording_herdr(dir.path(), &out);
    let binding = enabled_binding(&bin);

    let event = DispatchInput::new(HookEventName::PermissionRequest).with_kind("approval");
    binding.run(&builtin_rule(), &event).await;

    assert_eq!(binding.stats().reported(), 1);
    let args = read_args(&out);
    assert_eq!(args[0], "pane");
    assert_eq!(args[1], "report-agent");
    assert_eq!(args[2], "pane-7");
    assert_flag_value(&args, "--source", "custom:visp");
    assert_flag_value(&args, "--agent", "visp");
    assert_flag_value(&args, "--state", "blocked");
    assert!(args.iter().any(|arg| arg == "--message"), "{args:?}");
}

// UserPromptSubmit → working。
#[tokio::test]
async fn user_prompt_submit_reports_working() {
    let dir = TempDir::new().unwrap();
    let out = dir.path().join("args.txt");
    let bin = recording_herdr(dir.path(), &out);
    let binding = enabled_binding(&bin);

    binding
        .run(
            &builtin_rule(),
            &DispatchInput::new(HookEventName::UserPromptSubmit),
        )
        .await;

    assert_eq!(binding.stats().reported(), 1);
    let args = read_args(&out);
    assert_flag_value(&args, "--state", "working");
    assert!(!args.iter().any(|arg| arg == "--message"));
}

// Stop → idle。
#[tokio::test]
async fn stop_reports_idle() {
    let dir = TempDir::new().unwrap();
    let out = dir.path().join("args.txt");
    let bin = recording_herdr(dir.path(), &out);
    let binding = enabled_binding(&bin);

    binding
        .run(&builtin_rule(), &DispatchInput::new(HookEventName::Stop))
        .await;

    assert_eq!(binding.stats().reported(), 1);
    assert_flag_value(&read_args(&out), "--state", "idle");
}

// 失败隔离：CLI 不存在 → 计数 spawn_failed，不 panic。
#[tokio::test]
async fn missing_cli_is_counted_not_panicked() {
    let binding = enabled_binding("/nonexistent/visp-herdr-cli");
    binding
        .run(&builtin_rule(), &DispatchInput::new(HookEventName::Stop))
        .await;
    assert_eq!(binding.stats().spawn_failed(), 1);
    assert_eq!(binding.stats().reported(), 0);
}

// 失败隔离：非零退出 → 计数 failed。
#[tokio::test]
async fn nonzero_exit_is_counted() {
    let dir = TempDir::new().unwrap();
    let bin = behavior_herdr(dir.path(), "herdr-exit.sh", "exit 3");
    let binding = enabled_binding(&bin);

    binding
        .run(
            &builtin_rule(),
            &DispatchInput::new(HookEventName::UserPromptSubmit),
        )
        .await;

    assert_eq!(binding.stats().failed(), 1);
    assert_eq!(binding.stats().reported(), 0);
}

// 失败隔离：超时 → 短超时内返回并计数 timed_out，进程被终止。
#[tokio::test]
async fn timeout_is_counted_and_killed() {
    let dir = TempDir::new().unwrap();
    let bin = behavior_herdr(dir.path(), "herdr-slow.sh", "sleep 30");
    let binding = HerdrBinding::from_env_with_timeout(
        &env(&[
            (HERDR_ENV, "1"),
            (HERDR_BIN_PATH, &bin),
            (HERDR_PANE_ID, "p1"),
        ]),
        Duration::from_millis(200),
    );

    let started = Instant::now();
    binding
        .run(&builtin_rule(), &DispatchInput::new(HookEventName::Stop))
        .await;

    assert!(started.elapsed() < Duration::from_secs(5), "超时未及时返回");
    assert_eq!(binding.stats().timed_out(), 1);
    assert_eq!(binding.stats().reported(), 0);
}

// 内置规则身份：id、合并策略与命中事件。
#[test]
fn builtin_rule_identity_and_coalesce() {
    let rule = builtin_rule();
    assert_eq!(rule.id, BUILTIN_HERDR_RULE_ID);
    assert_eq!(rule.id, "builtin:herdr");
    assert_eq!(rule.on_full, QueuePolicy::CoalesceLatest);
    for event in [
        HookEventName::PermissionRequest,
        HookEventName::UserPromptSubmit,
        HookEventName::Stop,
        HookEventName::SessionStart,
    ] {
        assert!(rule.event.contains(&event), "{event:?}");
    }
}
