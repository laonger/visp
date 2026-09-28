//! 真实进程 [`Handler`]：argv 直执 + 白名单 env + stdin JSON + 超时杀进程树
//! （设计 D4/D5/§8.3；实施计划步骤 1b-2 测试第 7–12 条）。
//!
//! 与 [`crate::dispatcher`] 的决策内核分离：内核只决定「何时、以何顺序调用」，
//! 本模块负责真正的副作用，失败**绝不 panic 或上抛**（仅结构化日志 + 计数）。
//!
//! ## 执行语义
//!
//! - **argv 直执**：`command` + `args` 直接 `exec`，不经 shell（[`ProcessSpec`]）。
//! - **环境白名单**：子进程环境被清空后仅注入父进程的 `PATH`/`HOME`/`TERM`/`LANG`
//!   与 `VISP_*`/`HERDR_*`，再叠加规则显式 `env` 与投递标量；父进程其余变量
//!   （尤其密钥）不可见。
//! - **stdin**：完整事件 JSON 写入后立即关闭写端（避免 `read` 型脚本挂起）。
//! - **超时**：超过 `timeout_ms` 杀**整个进程组**（子进程经 `process_group(0)`
//!   独立成组，孙进程一并终止）。
//! - **cwd**：规则 `cwd`，缺省为 `project_path`。
//! - **防递归**：注入 `VISP_IN_HOOK=1`。
//!
//! Windows 一期不保证（设计 §14），进程组终止仅在 unix 生效；非 unix 退化为
//! 终止直接子进程。

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{Map, Value};
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

use crate::dispatcher::{DispatchInput, DispatchRule, Handler};
use crate::env::{
    ENV_VISP_HOOK_EVENT, ENV_VISP_HOOK_RULE_ID, ENV_VISP_HOOK_SCHEMA, ENV_VISP_IN_HOOK,
    ENV_VISP_PROJECT_PATH, VISP_HOOK_SCHEMA,
};

/// `timeout_ms` 缺省值（设计 §7.2：Claude 族惯例 60000）。
pub const DEFAULT_TIMEOUT_MS: u64 = 60_000;

/// 父进程环境白名单（精确名，设计 D5/§8.3）。
const ENV_ALLOWLIST: [&str; 4] = ["PATH", "HOME", "TERM", "LANG"];

/// 父进程环境白名单（前缀）。
const ENV_PREFIX_ALLOWLIST: [&str; 2] = ["VISP_", "HERDR_"];

/// 是否允许从父进程继承该环境变量。
fn env_allowed(key: &str) -> bool {
    ENV_ALLOWLIST.contains(&key)
        || ENV_PREFIX_ALLOWLIST
            .iter()
            .any(|prefix| key.starts_with(prefix))
}

/// 单条进程规则的执行规格（由接线层从 `visp_config::hooks::HookRule` 适配）。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ProcessSpec {
    /// argv 首元素（**无 shell** 直执）。
    pub command: String,
    /// argv 其余参数。
    pub args: Vec<String>,
    /// 规则显式环境变量（叠加在白名单之上，可覆盖同名白名单项）。
    pub env: Vec<(String, String)>,
    /// 单次超时（毫秒）；`0` 视为 [`DEFAULT_TIMEOUT_MS`]。
    pub timeout_ms: u64,
    /// 工作目录；`None` 时用 [`SpawnHandler`] 的 `project_path`。
    pub cwd: Option<PathBuf>,
}

/// 进程执行计数（设计 §15：`executed`/`failed`/`timed_out`）。
///
/// 另计 `spawn_failed`（无法启动）以便与「启动后异常」区分。
#[derive(Debug, Default)]
pub struct HookStats {
    executed: AtomicU64,
    failed: AtomicU64,
    timed_out: AtomicU64,
    spawn_failed: AtomicU64,
}

impl HookStats {
    /// 以零计数构造。
    pub fn new() -> Self {
        Self::default()
    }

    /// 正常退出（exit code 0）次数。
    pub fn executed(&self) -> u64 {
        self.executed.load(Ordering::SeqCst)
    }

    /// 启动后失败次数（非零退出 / 被信号终止 / 等待出错）。
    pub fn failed(&self) -> u64 {
        self.failed.load(Ordering::SeqCst)
    }

    /// 超时被杀次数。
    pub fn timed_out(&self) -> u64 {
        self.timed_out.load(Ordering::SeqCst)
    }

    /// 无法启动次数（命令不存在 / 权限不足等）。
    pub fn spawn_failed(&self) -> u64 {
        self.spawn_failed.load(Ordering::SeqCst)
    }
}

/// 进程 Handler：按规则 `id` 查 [`ProcessSpec`] 并 spawn 子进程执行。
///
/// 决策内核的 [`Handler::run`] 保证同规则串行（除非 `parallel`）、跨规则按排序；
/// 本类型只负责单次执行的正确性与失败隔离。
pub struct SpawnHandler {
    project_path: PathBuf,
    specs: HashMap<String, ProcessSpec>,
    /// 父进程环境快照（构造时采集；测试可注入，避免依赖真实进程环境）。
    parent_env: Vec<(String, String)>,
    stats: HookStats,
}

impl SpawnHandler {
    /// 采集当前进程环境并构造。
    pub fn new(project_path: impl Into<PathBuf>, specs: HashMap<String, ProcessSpec>) -> Self {
        Self::with_parent_env(project_path, specs, std::env::vars().collect())
    }

    /// 以显式父进程环境构造（测试用，保证白名单过滤可确定性断言）。
    pub fn with_parent_env(
        project_path: impl Into<PathBuf>,
        specs: HashMap<String, ProcessSpec>,
        parent_env: Vec<(String, String)>,
    ) -> Self {
        Self {
            project_path: project_path.into(),
            specs,
            parent_env,
            stats: HookStats::new(),
        }
    }

    /// 计数快照。
    pub fn stats(&self) -> &HookStats {
        &self.stats
    }

    /// 构造子进程环境：白名单父环境 → 规则显式 `env` → 投递标量。
    ///
    /// 顺序即优先级，后者覆盖前者。
    fn child_env(
        &self,
        spec: &ProcessSpec,
        rule: &DispatchRule,
        event: &DispatchInput,
    ) -> Vec<(String, String)> {
        let mut env: HashMap<String, String> = HashMap::new();
        // 1. 父进程白名单（不继承其余变量，尤其不泄露密钥）。
        for (key, value) in &self.parent_env {
            if env_allowed(key) {
                env.insert(key.clone(), value.clone());
            }
        }
        // 2. 规则显式 env。
        for (key, value) in &spec.env {
            env.insert(key.clone(), value.clone());
        }
        // 3. 投递标量（设计 §6.3）与防递归标记。
        env.insert(
            ENV_VISP_HOOK_EVENT.to_string(),
            event.event.as_str().to_string(),
        );
        env.insert(ENV_VISP_HOOK_RULE_ID.to_string(), rule.id.clone());
        env.insert(
            ENV_VISP_HOOK_SCHEMA.to_string(),
            VISP_HOOK_SCHEMA.to_string(),
        );
        env.insert(
            ENV_VISP_PROJECT_PATH.to_string(),
            self.project_path.to_string_lossy().into_owned(),
        );
        env.insert(ENV_VISP_IN_HOOK.to_string(), "1".to_string());

        env.into_iter().collect()
    }

    /// 执行一次规则（失败仅日志 + 计数，绝不 panic）。
    async fn execute(&self, rule: &DispatchRule, event: &DispatchInput) {
        let Some(spec) = self.specs.get(&rule.id) else {
            tracing::warn!(rule_id = %rule.id, "hook 进程规则缺少执行规格，已跳过");
            self.stats.failed.fetch_add(1, Ordering::SeqCst);
            return;
        };

        let payload = event
            .payload
            .clone()
            .unwrap_or_else(|| fallback_payload(event));

        let cwd = spec
            .cwd
            .clone()
            .unwrap_or_else(|| self.project_path.clone());

        let mut command = Command::new(&spec.command);
        command
            .args(&spec.args)
            .current_dir(&cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        command.env_clear();
        for (key, value) in self.child_env(spec, rule, event) {
            command.env(key, value);
        }
        #[cfg(unix)]
        command.process_group(0);

        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(error) => {
                self.stats.spawn_failed.fetch_add(1, Ordering::SeqCst);
                tracing::warn!(
                    rule_id = %rule.id,
                    command = %spec.command,
                    %error,
                    "hook 进程启动失败"
                );
                return;
            }
        };

        // stdin：写入完整载荷后关闭写端，避免 `read` 型脚本等待 EOF 挂起。
        if let Some(mut stdin) = child.stdin.take() {
            let bytes = payload.into_bytes();
            tokio::spawn(async move {
                let _ = stdin.write_all(&bytes).await;
                let _ = stdin.shutdown().await;
            });
        }

        let timeout_ms = if spec.timeout_ms == 0 {
            DEFAULT_TIMEOUT_MS
        } else {
            spec.timeout_ms
        };

        match tokio::time::timeout(Duration::from_millis(timeout_ms), child.wait()).await {
            Ok(Ok(status)) if status.success() => {
                self.stats.executed.fetch_add(1, Ordering::SeqCst);
            }
            Ok(Ok(status)) => {
                self.stats.failed.fetch_add(1, Ordering::SeqCst);
                tracing::warn!(rule_id = %rule.id, code = ?status.code(), "hook 进程非零退出");
            }
            Ok(Err(error)) => {
                self.stats.failed.fetch_add(1, Ordering::SeqCst);
                tracing::warn!(rule_id = %rule.id, %error, "hook 进程等待失败");
            }
            Err(_) => {
                // 杀整棵进程树（进程组），再回收直接子进程。
                #[cfg(unix)]
                if let Some(pid) = child.id() {
                    kill_process_group(pid);
                }
                #[cfg(not(unix))]
                let _ = child.start_kill();
                let _ = child.wait().await;
                self.stats.timed_out.fetch_add(1, Ordering::SeqCst);
                tracing::warn!(rule_id = %rule.id, timeout_ms, "hook 进程超时，已终止进程树");
            }
        }
    }
}

#[async_trait]
impl Handler for SpawnHandler {
    async fn run(&self, rule: &DispatchRule, event: &DispatchInput) {
        self.execute(rule, event).await;
    }
}

/// 杀死以 `pid` 为组长的整个进程组（含孙进程）。
#[cfg(unix)]
fn kill_process_group(pid: u32) {
    // SAFETY: `kill(2)` 仅发送信号，无内存安全影响；负 pid 表示进程组。
    unsafe {
        let _ = libc::kill(-(pid as i32), libc::SIGKILL);
    }
}

/// 事件视图的精简 JSON（接线层未提供完整载荷时的兜底）。
fn fallback_payload(event: &DispatchInput) -> String {
    let mut object = Map::new();
    object.insert(
        "hook_event_name".to_string(),
        Value::String(event.event.as_str().to_string()),
    );
    if let Some(tool_name) = &event.tool_name {
        object.insert("tool_name".to_string(), Value::String(tool_name.clone()));
    }
    if let Some(source) = &event.source {
        object.insert("source".to_string(), Value::String(source.clone()));
    }
    if let Some(kind) = &event.kind {
        object.insert("kind".to_string(), Value::String(kind.clone()));
    }
    Value::Object(object).to_string()
}
