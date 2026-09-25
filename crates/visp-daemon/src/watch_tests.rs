//! 监听计划构建测试（步骤 6a）。
//!
//! 计划构建只读传入路径的存在性与祖先链边界（`.git` 标记 / `$HOME`），用 tempdir
//! 构造存在性组合即可，不触碰真实 `~/.config/visp`。涉及祖先链边界（尤其 `$HOME`）
//! 的用例需隔离环境变量，以 `#[serial]` 标注避免并发改写。

use super::*;
use std::fs;

use serial_test::serial;
use tempfile::TempDir;

/// 取出指定目录的监听条目。
fn entry<'a>(plan: &'a WatchPlan, dir: &Path) -> &'a WatchEntry {
    plan.entries
        .iter()
        .find(|entry| entry.dir == dir)
        .unwrap_or_else(|| panic!("缺少监听条目 {dir:?}"))
}

/// 分类结果领域集合。
fn domains(plan: &WatchPlan, path: &Path) -> Option<Vec<ReloadDomain>> {
    plan.classify(path)
        .map(|classification| classification.domains)
}

/// 构造一个「项目与全局目录素材齐备」的组合。
fn full_setup() -> (TempDir, TempDir) {
    let project = TempDir::new().unwrap();
    let global = TempDir::new().unwrap();
    for sub in ["rules", "agents", "skills"] {
        fs::create_dir_all(project.path().join(".visp").join(sub)).unwrap();
        fs::create_dir_all(global.path().join(sub)).unwrap();
    }
    (project, global)
}

#[test]
fn nine_category_paths_map_to_domains() {
    let (project, global) = full_setup();
    let plan = WatchPlan::build(project.path(), Some(global.path()));

    // #1 祖先 / project 根：非递归。
    assert!(!entry(&plan, project.path()).recursive);
    // #2 全局根：非递归。
    assert!(!entry(&plan, global.path()).recursive);
    // #6 项目 `.visp`：非递归。
    assert!(!entry(&plan, &project.path().join(".visp")).recursive);
    // #3/#4/#5 全局 rules/agents/skills：递归。
    for sub in ["rules", "agents", "skills"] {
        assert!(
            entry(&plan, &global.path().join(sub)).recursive,
            "全局 {sub} 应递归监听"
        );
        // #7/#8/#9 项目 rules/agents/skills：递归。
        assert!(
            entry(&plan, &project.path().join(".visp").join(sub)).recursive,
            "项目 {sub} 应递归监听"
        );
    }

    let project = project.path();
    let global = global.path();
    // rules 三类来源
    assert_eq!(
        domains(&plan, &project.join("AGENTS.md")),
        Some(vec![ReloadDomain::Rules])
    );
    assert_eq!(
        domains(&plan, &project.join(".visp/rules/a.md")),
        Some(vec![ReloadDomain::Rules])
    );
    assert_eq!(
        domains(&plan, &global.join("rules/a.md")),
        Some(vec![ReloadDomain::Rules])
    );
    // agents 两类来源
    assert_eq!(
        domains(&plan, &project.join(".visp/agents/a.md")),
        Some(vec![ReloadDomain::Agents])
    );
    assert_eq!(
        domains(&plan, &global.join("agents/a.md")),
        Some(vec![ReloadDomain::Agents])
    );
    // skills 两类来源（任意事件，含 SKILL.md）
    assert_eq!(
        domains(&plan, &project.join(".visp/skills/my-skill/SKILL.md")),
        Some(vec![ReloadDomain::Skills])
    );
    assert_eq!(
        domains(&plan, &global.join("skills/my-skill/SKILL.md")),
        Some(vec![ReloadDomain::Skills])
    );
    // 全局顶层 AGENTS.md
    assert_eq!(
        domains(&plan, &global.join("AGENTS.md")),
        Some(vec![ReloadDomain::Rules])
    );
}

#[test]
fn nine_category_targets_are_declared_for_visp_fs() {
    let (project, global) = full_setup();
    let plan = WatchPlan::build(project.path(), Some(global.path()));
    let targets = plan.fs_targets();

    let find = |root: &Path| {
        targets
            .iter()
            .find(|target| target.root.as_path() == root)
            .unwrap_or_else(|| panic!("缺少监听目标 {root:?}"))
    };

    // project 根合并了「AGENTS.md + `.visp` 前缀」两条包含（非递归）。
    let project_root = find(project.path());
    assert_eq!(project_root.mode, WatchMode::DirectChildren);
    assert!(
        project_root
            .filter
            .includes
            .contains(&Include::FileName("AGENTS.md".into()))
    );
    assert!(
        project_root
            .filter
            .includes
            .contains(&Include::Prefix(PathBuf::from(".visp")))
    );
    assert!(project_root.filter.excludes.is_empty());

    // 全局根与项目 `.visp` 根：按子目录名分流（非递归）。
    let global_root = find(global.path());
    assert_eq!(global_root.mode, WatchMode::DirectChildren);
    for name in ["AGENTS.md", "rules", "agents", "skills"] {
        assert!(
            global_root
                .filter
                .includes
                .contains(&Include::FileName(name.into()))
        );
    }
    let visp = find(&project.path().join(".visp"));
    assert_eq!(visp.mode, WatchMode::DirectChildren);
    for name in ["rules", "agents", "skills"] {
        assert!(
            visp.filter
                .includes
                .contains(&Include::FileName(name.into()))
        );
    }

    // 递归目标：rules/agents 按 `.md` 过滤，skills 全部包含（空包含集）。
    for root in [
        global.path().join("rules"),
        global.path().join("agents"),
        project.path().join(".visp/rules"),
        project.path().join(".visp/agents"),
    ] {
        let target = find(&root);
        assert_eq!(target.mode, WatchMode::Recursive);
        assert_eq!(
            target.filter.includes,
            vec![Include::Extension("md".into())]
        );
    }
    for root in [
        global.path().join("skills"),
        project.path().join(".visp/skills"),
    ] {
        let target = find(&root);
        assert_eq!(target.mode, WatchMode::Recursive);
        assert!(target.filter.includes.is_empty(), "skills 目标应全部包含");
    }
}

#[test]
#[serial]
fn ancestor_chain_is_non_recursive() {
    let base = TempDir::new().unwrap();
    let project = base.path().join("a/b/c");
    fs::create_dir_all(&project).unwrap();
    let plan = WatchPlan::build(&project, None);

    for ancestor in visp_config::agents_md_ancestors(&project) {
        assert!(
            !entry(&plan, &ancestor).recursive,
            "祖先 {ancestor:?} 应非递归监听"
        );
    }

    // project 根这一层含 `.visp` 子树规则：`.visp` 创建可触发补挂校准（空领域），
    // `.visp/rules` 命中 rules。
    assert_eq!(
        plan.classify(&project.join(".visp")),
        Some(Classification { domains: vec![] })
    );
    assert_eq!(
        domains(&plan, &project.join(".visp/rules/a.md")),
        Some(vec![ReloadDomain::Rules])
    );
}

/// 子步骤 5b：监听计划 #1（祖先链）必须与加载器共用同一套边界解析。
///
/// 一致性断言：计划中所有「project 的祖先或自身」条目 == 加载器解析出的祖先链；
/// 既不能「监听但不加载」，也不能「加载但不监听」。
#[test]
#[serial]
fn watch_plan_ancestor_chain_matches_loader() {
    let base = TempDir::new().unwrap();
    let project = base.path().join("a/b/c");
    fs::create_dir_all(&project).unwrap();

    let plan = WatchPlan::build(&project, None);
    let expected = visp_config::agents_md_ancestors(&project);

    let actual: Vec<PathBuf> = plan
        .entries
        .iter()
        .filter(|entry| project.starts_with(&entry.dir))
        .map(|entry| entry.dir.clone())
        .collect();

    assert_eq!(
        actual, expected,
        "监听计划 #1 必须与加载器解析出的祖先链一致"
    );
}

/// 子步骤 5b：祖先链止于 git 根（含），计划不含 git 根之上的目录。
///
/// `#[serial]` + 短时改写 `HOME`，以复现设计 §5.1 规则 2「项目位于 `$HOME` 之下」
/// 的上溯路径（否则天然未在 `$HOME` 下，链只含项目层，无法验证 git 边界）。
#[test]
#[serial]
fn watch_plan_stops_at_git_root() {
    let base = TempDir::new().unwrap();
    let git_root = base.path().join("repo");
    let project = git_root.join("pkg/sub");
    fs::create_dir_all(&project).unwrap();
    fs::create_dir(git_root.join(".git")).unwrap();

    let previous = std::env::var_os("HOME");
    // SAFETY: `#[serial]` 保证本测试独占执行；仅在本构建窗口内重定向 HOME。
    unsafe { std::env::set_var("HOME", base.path()) };
    let plan = WatchPlan::build(&project, None);
    let expected = visp_config::agents_md_ancestors(&project);
    match previous {
        Some(value) => unsafe { std::env::set_var("HOME", value) },
        None => unsafe { std::env::remove_var("HOME") },
    }

    let actual: Vec<PathBuf> = plan
        .entries
        .iter()
        .filter(|entry| project.starts_with(&entry.dir))
        .map(|entry| entry.dir.clone())
        .collect();

    assert_eq!(actual, expected, "监听计划 #1 必须与加载器共用祖先链");
    assert!(
        actual.contains(&git_root),
        "祖先链应包含 git 根层：{actual:?}"
    );
    assert!(
        !actual.contains(&base.path().to_path_buf()),
        "祖先链不得越过 git 根：{actual:?}"
    );
}

#[test]
fn fresh_project_without_visp_uses_root_fallback() {
    let base = TempDir::new().unwrap();
    let project = base.path().join("fresh");
    fs::create_dir_all(&project).unwrap();
    assert!(!project.join(".visp").exists());

    let plan = WatchPlan::build(&project, None);

    // 降级：project 根非递归监听，`.visp` 子树前缀规则命中补挂链。
    assert!(!entry(&plan, &project).recursive);
    assert_eq!(
        plan.classify(&project.join(".visp")),
        Some(Classification { domains: vec![] })
    );
    assert_eq!(
        domains(&plan, &project.join(".visp/agents/reviewer.md")),
        Some(vec![ReloadDomain::Agents])
    );
    assert_eq!(
        domains(&plan, &project.join(".visp/skills/x/SKILL.md")),
        Some(vec![ReloadDomain::Skills])
    );

    // `.visp` 尚不存在，但计划中已保留其递归目标（供运行中补挂 + 重扫）。
    assert!(entry(&plan, &project.join(".visp/agents")).recursive);
}

#[test]
fn missing_subdirs_fall_back_to_nearest_ancestor() {
    let project = TempDir::new().unwrap();
    let global = TempDir::new().unwrap();
    // 只建 `.visp` 与全局根，不建 rules/agents/skills。
    fs::create_dir_all(project.path().join(".visp")).unwrap();

    let plan = WatchPlan::build(project.path(), Some(global.path()));

    // `.visp` 存在 → 其子目录创建事件直接分流（无需 project 根前缀规则参与）。
    assert_eq!(
        domains(&plan, &project.path().join(".visp/rules")),
        Some(vec![ReloadDomain::Rules])
    );
    assert_eq!(
        domains(&plan, &project.path().join(".visp/agents")),
        Some(vec![ReloadDomain::Agents])
    );
    assert_eq!(
        domains(&plan, &project.path().join(".visp/skills")),
        Some(vec![ReloadDomain::Skills])
    );

    // 全局根存在 → 三个子目录创建事件分流。
    assert_eq!(
        domains(&plan, &global.path().join("rules")),
        Some(vec![ReloadDomain::Rules])
    );
    assert_eq!(
        domains(&plan, &global.path().join("agents")),
        Some(vec![ReloadDomain::Agents])
    );
    assert_eq!(
        domains(&plan, &global.path().join("skills")),
        Some(vec![ReloadDomain::Skills])
    );

    // 目标目录仍作为递归补挂目标保留。
    assert!(
        entry(&plan, &project.path().join(".visp/rules")).recursive,
        "缺失子目录应保留递归补挂目标"
    );
    assert!(
        entry(&plan, &global.path().join("rules")).recursive,
        "缺失全局子目录应保留递归补挂目标"
    );
}

#[test]
fn excluded_paths_produce_no_domain() {
    let (project, global) = full_setup();
    let plan = WatchPlan::build(project.path(), Some(global.path()));
    let project = project.path();
    let global = global.path();

    let excluded = [
        project.join(".visp/system-prompt.md"),
        project.join(".visp/daemon.toml"),
        project.join(".visp/webfetch.toml"),
        project.join(".visp/codegraph.db"),
        project.join(".visp/logs/daemon.log"),
        project.join(".visp/.startup-error"),
        global.join("system-prompt.md"),
        global.join("daemon.toml"),
        global.join("webfetch.toml"),
        global.join("logs/daemon.log"),
    ];
    for path in excluded {
        assert_eq!(plan.classify(&path), None, "{path:?} 不应命中任何领域");
    }

    // 对照：非排除文件正常命中。
    assert_eq!(
        domains(&plan, &project.join(".visp/rules/a.md")),
        Some(vec![ReloadDomain::Rules])
    );
}

#[test]
fn global_root_children_dispatch_by_subdir() {
    let global = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    let plan = WatchPlan::build(project.path(), Some(global.path()));
    let global = global.path();

    assert_eq!(
        domains(&plan, &global.join("AGENTS.md")),
        Some(vec![ReloadDomain::Rules])
    );
    assert_eq!(
        domains(&plan, &global.join("rules")),
        Some(vec![ReloadDomain::Rules])
    );
    assert_eq!(
        domains(&plan, &global.join("agents")),
        Some(vec![ReloadDomain::Agents])
    );
    assert_eq!(
        domains(&plan, &global.join("skills")),
        Some(vec![ReloadDomain::Skills])
    );
    // 顶层其他文件（daemon.toml 等）不命中。
    assert_eq!(plan.classify(&global.join("daemon.toml")), None);
}

/// 记录调用次数的假体执行器（工厂门控测试用，不触碰真实资产）。
#[derive(Default)]
struct CountingExecutor {
    calls: std::sync::atomic::AtomicUsize,
}

#[async_trait::async_trait]
impl ReloadExecutor for CountingExecutor {
    async fn reload_domains(&self, _domains: &[ReloadDomain]) -> Vec<ReloadItem> {
        self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Vec::new()
    }
}

/// 步骤 6d：开关关闭 → 工厂返回「未创建」，不构造 watcher、不起后台任务。
#[tokio::test]
async fn filewatcher_factory_disabled_returns_none() {
    let project = TempDir::new().unwrap();
    let executor = Arc::new(CountingExecutor::default());

    let watcher = start_file_watcher(false, project.path(), None, executor.clone(), None).await;

    assert!(watcher.is_none(), "开关关闭时不应创建 watcher");
    // 返回 None 即未构造 FileWatcher（`visp-fs` watcher 与后台任务唯一创建点在
    // FileWatcher::start），假体执行器在整个窗口内零调用。
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(
        executor.calls.load(std::sync::atomic::Ordering::SeqCst),
        0,
        "关闭时不应有任何自动重载调用"
    );
}

/// 步骤 6d：开关开启 → 工厂按监听计划创建 watcher（门控对照，证明关闭分支非恒真）。
#[tokio::test]
async fn filewatcher_factory_enabled_creates_watcher() {
    let project = TempDir::new().unwrap();
    fs::create_dir_all(project.path().join(".visp/agents")).unwrap();
    let executor = Arc::new(CountingExecutor::default());

    let watcher = start_file_watcher(true, project.path(), None, executor, None).await;

    assert!(watcher.is_some(), "开关开启时应创建 watcher");
    watcher.unwrap().stop();
}

/// 构造一个「不可挂载」降级原因（细则 5）。
fn unmountable() -> visp_fs::runtime::DegradeReason {
    visp_fs::runtime::DegradeReason::Unmountable {
        root: PathBuf::from("/nowhere/watched"),
    }
}

/// 修正 3：降级事件 → best-effort 推送一条 `session_id` 为空的汇总 Status 帧。
#[tokio::test]
async fn degraded_event_pushes_empty_session_status_frame() {
    let (msg_tx, msg_rx) = mpsc::unbounded_channel::<WatchMessage>();
    let (down_tx, mut down_rx) = mpsc::channel::<AgentEventFrame>(8);

    let project = TempDir::new().unwrap();
    let plan = Arc::new(WatchPlan::build(project.path(), None));
    let executor = Arc::new(CountingExecutor::default());

    let handle = tokio::spawn(run_loop(
        msg_rx,
        plan,
        executor,
        Duration::from_millis(20),
        Some(down_tx),
    ));

    msg_tx.send(WatchMessage::Degraded(unmountable())).unwrap();

    let frame = tokio::time::timeout(Duration::from_secs(2), down_rx.recv())
        .await
        .expect("应在时限内收到降级通知")
        .expect("下行通道应仍打开");
    assert_eq!(
        frame.session_id, "",
        "降级通知 session_id 必须为空（TUI 路由主 tab 状态行）"
    );
    assert!(
        matches!(frame.event, AgentEvent::StatusUpdate(_)),
        "降级通知应为 StatusUpdate"
    );

    drop(msg_tx);
    let _ = handle.await;
}

/// 修正 3：通道不可用（未注入）→ 不 panic、不影响消费循环。
#[tokio::test]
async fn degraded_without_downlink_does_not_panic_and_keeps_consuming() {
    let (msg_tx, msg_rx) = mpsc::unbounded_channel::<WatchMessage>();

    let project = TempDir::new().unwrap();
    let plan = Arc::new(WatchPlan::build(project.path(), None));
    let executor = Arc::new(CountingExecutor::default());

    let handle = tokio::spawn(run_loop(
        msg_rx,
        plan,
        executor.clone(),
        Duration::from_millis(20),
        None,
    ));

    msg_tx.send(WatchMessage::Degraded(unmountable())).unwrap();
    tokio::time::sleep(Duration::from_millis(80)).await;
    assert!(!handle.is_finished(), "通道不可用时降级不得终止消费循环");

    // 后续文件事件仍正常驱动重载，证明循环存活且行为不受影响。
    msg_tx
        .send(WatchMessage::Event(visp_fs::runtime::FileEvent {
            path: project.path().join(".visp/rules/a.md"),
            kind: visp_fs::normalize::EventType::Modified,
        }))
        .unwrap();
    let reloaded = tokio::time::timeout(Duration::from_secs(2), async {
        while executor.calls.load(std::sync::atomic::Ordering::SeqCst) == 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    assert!(reloaded.is_ok(), "降级后消费循环仍应处理文件事件");

    drop(msg_tx);
    let _ = handle.await;
}

/// 修正 3：已关闭通道 → `notify_degraded` 静默容忍（best-effort，不 panic）。
#[test]
fn notify_degraded_is_best_effort_when_channel_unavailable() {
    let reason = unmountable();
    notify_degraded(&None, &reason); // 未注入：静默
    let (down_tx, down_rx) = mpsc::channel(1);
    drop(down_rx); // 已关闭
    notify_degraded(&Some(down_tx), &reason);
}
