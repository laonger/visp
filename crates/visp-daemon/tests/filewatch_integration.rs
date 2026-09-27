//! 文件监听运行时集成测试（步骤 6b / 验收 22）。
//!
//! 断言一律以「最终收敛」为准（有限时间轮询等待），不对事件序列或精确次数
//! 做脆弱断言——本机 macOS 固定走 kqueue 后端，事件时序与 inotify 不同
//! （计划备注 3）。debounce 时长注入为 50ms，不睡真实 200ms 的整数倍。

use std::fs;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tempfile::TempDir;

use visp_daemon::reload::{ReloadDomain, ReloadItem};
use visp_daemon::watch::{FileWatcher, ReloadExecutor, WatchPlan};

/// 测试注入的较短 debounce 窗口。
const TEST_DEBOUNCE: Duration = Duration::from_millis(50);

/// 记录调用领域的假执行器。
#[derive(Default)]
struct FakeExecutor {
    calls: Mutex<Vec<Vec<ReloadDomain>>>,
}

impl FakeExecutor {
    fn call_count(&self) -> usize {
        self.calls.lock().unwrap().len()
    }

    fn calls(&self) -> Vec<Vec<ReloadDomain>> {
        self.calls.lock().unwrap().clone()
    }
}

#[async_trait::async_trait]
impl ReloadExecutor for FakeExecutor {
    async fn reload_domains(&self, domains: &[ReloadDomain]) -> Vec<ReloadItem> {
        self.calls.lock().unwrap().push(domains.to_vec());
        Vec::new()
    }
}

/// 有限时间轮询等待，任何断言都建立在「最终收敛」上。
async fn wait_until(predicate: impl Fn() -> bool, timeout: Duration) -> bool {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if predicate() {
            return true;
        }
        if tokio::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// 构造监听：`WatchPlan` 经 [`FileWatcher::start`] 翻译为 `visp-fs` 目标后启动，
/// 与生产路径完全一致。
async fn start(project: &Path, global: Option<&Path>, executor: &Arc<FakeExecutor>) -> FileWatcher {
    let plan = Arc::new(WatchPlan::build(project, global));
    FileWatcher::start(plan, executor.clone(), TEST_DEBOUNCE, None, None)
        .await
        .expect("watcher 应能创建")
}

/// 给 kqueue 挂载留出注册时间。
async fn settle_backend() {
    tokio::time::sleep(Duration::from_millis(80)).await;
}

#[tokio::test]
async fn writing_agents_md_triggers_rules_reload() {
    let project = TempDir::new().unwrap();
    let executor = Arc::new(FakeExecutor::default());
    let watcher = start(project.path(), None, &executor).await;
    settle_backend().await;

    fs::write(project.path().join("AGENTS.md"), "hello").unwrap();

    assert!(
        wait_until(|| executor.call_count() >= 1, Duration::from_secs(5)).await,
        "写 AGENTS.md 应触发领域重载：{:?}",
        executor.calls()
    );
    assert!(
        executor
            .calls()
            .iter()
            .any(|domains| domains.contains(&ReloadDomain::Rules)),
        "应包含 rules 领域：{:?}",
        executor.calls()
    );
    watcher.stop();
}

#[tokio::test]
async fn rapid_writes_are_debounced_into_one_reload() {
    let project = TempDir::new().unwrap();
    // 文件在监听启动前已存在（计划备注 3）：否则 kqueue 会对新建文件自动补挂，
    // 「原地覆写」盲区被掩盖。
    let agents_md = project.path().join("AGENTS.md");
    fs::write(&agents_md, "initial").unwrap();
    let executor = Arc::new(FakeExecutor::default());
    let watcher = start(project.path(), None, &executor).await;
    settle_backend().await;

    for index in 0..5 {
        fs::write(&agents_md, format!("content {index}")).unwrap();
    }

    assert!(
        wait_until(|| executor.call_count() >= 1, Duration::from_secs(5)).await,
        "应至少触发一次重载"
    );
    // 静默期后仍应只有一次（窗口内多次写入被合并）。
    tokio::time::sleep(TEST_DEBOUNCE * 4).await;
    assert_eq!(
        executor.call_count(),
        1,
        "窗口内多写应合并为单次重载：{:?}",
        executor.calls()
    );
    watcher.stop();
}

/// 计划 3a 测试 3：**启动前已存在**文件的原地覆写必须产生事件。
///
/// 修正既有 `rapid_writes_are_debounced_into_one_reload` 掩蔽的盲区——kqueue 非递归
/// 目录监听不上报已存在文件的内容修改，必须依赖 `visp-fs` 的文件级监听。
#[tokio::test]
async fn in_place_overwrite_of_preexisting_file_produces_event() {
    let project = TempDir::new().unwrap();
    let agents_md = project.path().join("AGENTS.md");
    fs::write(&agents_md, "initial").unwrap();

    let executor = Arc::new(FakeExecutor::default());
    let watcher = start(project.path(), None, &executor).await;
    settle_backend().await;

    // 原地覆写（非原子替换）：文件路径未变、inode 未变。
    fs::write(&agents_md, "overwritten").unwrap();

    assert!(
        wait_until(
            || executor
                .calls()
                .iter()
                .any(|domains| domains.contains(&ReloadDomain::Rules)),
            Duration::from_secs(5)
        )
        .await,
        "启动前已存在文件的原地覆写应触发 rules 重载：{:?}",
        executor.calls()
    );
    watcher.stop();
}

#[tokio::test]
async fn atomic_write_converges_to_rules_reload() {
    let project = TempDir::new().unwrap();
    let executor = Arc::new(FakeExecutor::default());
    let watcher = start(project.path(), None, &executor).await;
    settle_backend().await;

    // 编辑器原子写：临时文件 + rename 覆盖目标。
    let tmp = project.path().join("AGENTS.md.tmp");
    fs::write(&tmp, "atomic content").unwrap();
    fs::rename(&tmp, project.path().join("AGENTS.md")).unwrap();

    assert!(
        wait_until(
            || executor
                .calls()
                .iter()
                .any(|domains| domains.contains(&ReloadDomain::Rules)),
            Duration::from_secs(5)
        )
        .await,
        "原子写应最终收敛到 rules 重载：{:?}",
        executor.calls()
    );
    watcher.stop();
}

#[tokio::test]
async fn cross_domain_events_are_aggregated() {
    let project = TempDir::new().unwrap();
    fs::create_dir_all(project.path().join(".visp/agents")).unwrap();
    let executor = Arc::new(FakeExecutor::default());
    let watcher = start(project.path(), None, &executor).await;
    settle_backend().await;

    fs::write(project.path().join("AGENTS.md"), "rules").unwrap();
    fs::write(
        project.path().join(".visp/agents/reviewer.md"),
        "---\nname: reviewer\nmode: subagent\n---\nbody\n",
    )
    .unwrap();

    assert!(
        wait_until(
            || executor.calls().iter().any(|domains| {
                domains.contains(&ReloadDomain::Rules) && domains.contains(&ReloadDomain::Agents)
            }),
            Duration::from_secs(5)
        )
        .await,
        "同一窗口的跨领域事件应聚合为一次调用：{:?}",
        executor.calls()
    );
    watcher.stop();
}

#[tokio::test]
async fn excluded_files_produce_no_reload() {
    let project = TempDir::new().unwrap();
    let visp = project.path().join(".visp");
    fs::create_dir_all(visp.join("logs")).unwrap();
    let executor = Arc::new(FakeExecutor::default());
    let watcher = start(project.path(), None, &executor).await;
    settle_backend().await;

    for name in [
        "system-prompt.md",
        "daemon.toml",
        "webfetch.toml",
        "codegraph.db",
        "logs/daemon.log",
    ] {
        fs::write(visp.join(name), "ignored").unwrap();
    }

    tokio::time::sleep(TEST_DEBOUNCE * 4).await;
    assert_eq!(
        executor.call_count(),
        0,
        "排除清单文件不应触发任何领域重载：{:?}",
        executor.calls()
    );
    watcher.stop();
}

#[tokio::test]
async fn missing_dir_is_remounted_and_rescanned() {
    let project = TempDir::new().unwrap();
    assert!(!project.path().join(".visp").exists());
    let executor = Arc::new(FakeExecutor::default());
    let watcher = start(project.path(), None, &executor).await;
    settle_backend().await;

    // 运行中首次创建 `.visp/agents` 并立即写入：降级链补挂 + 补挂后重扫。
    let agents = project.path().join(".visp/agents");
    fs::create_dir_all(&agents).unwrap();
    fs::write(
        agents.join("reviewer.md"),
        "---\nname: reviewer\nmode: subagent\n---\nbody\n",
    )
    .unwrap();

    assert!(
        wait_until(
            || executor
                .calls()
                .iter()
                .any(|domains| domains.contains(&ReloadDomain::Agents)),
            Duration::from_secs(5)
        )
        .await,
        "缺目录补挂后应重扫拾取窗口内写入：{:?}",
        executor.calls()
    );
    watcher.stop();
}

#[tokio::test]
async fn repeated_mount_storm_does_not_break_watching() {
    let project = TempDir::new().unwrap();
    fs::create_dir_all(project.path().join(".visp")).unwrap();
    let executor = Arc::new(FakeExecutor::default());
    let watcher = start(project.path(), None, &executor).await;
    settle_backend().await;

    // 目录创建/删除风暴：已监听路径集合去重，不应干扰后续监听。
    let agents = project.path().join(".visp/agents");
    for _ in 0..20 {
        fs::create_dir_all(&agents).unwrap();
        fs::remove_dir_all(&agents).unwrap();
    }

    // 风暴结束后恢复正常写入，仍应触发 agents 重载。
    fs::create_dir_all(&agents).unwrap();
    fs::write(
        agents.join("reviewer.md"),
        "---\nname: reviewer\nmode: subagent\n---\nbody\n",
    )
    .unwrap();

    assert!(
        wait_until(
            || executor
                .calls()
                .iter()
                .any(|domains| domains.contains(&ReloadDomain::Agents)),
            Duration::from_secs(5)
        )
        .await,
        "去重后监听仍应正常工作：{:?}",
        executor.calls()
    );
    watcher.stop();
}

#[tokio::test]
async fn stop_ceases_further_reloads() {
    let project = TempDir::new().unwrap();
    let executor = Arc::new(FakeExecutor::default());
    let watcher = start(project.path(), None, &executor).await;
    settle_backend().await;

    watcher.stop();
    tokio::time::sleep(Duration::from_millis(100)).await;
    let before = executor.call_count();

    fs::write(project.path().join("AGENTS.md"), "after stop").unwrap();
    tokio::time::sleep(TEST_DEBOUNCE * 6).await;

    assert_eq!(
        executor.call_count(),
        before,
        "停止后不应再触发重载：{:?}",
        executor.calls()
    );
}
