//! reload 核心测试（步骤 3a/3b/3c）。
//!
//! 隔离策略沿用 visp-config 既有做法：显式把 `VISP_CONFIG_DIR` 指向临时空目录，
//! 避免读取真实 `~/.config/visp`；测试统一标 `#[serial]`，防止 env 全局态互踩。

use super::*;
use std::fs;

use serial_test::serial;
use tempfile::TempDir;

/// 隔离全局配置的测试环境：`VISP_CONFIG_DIR` → 临时空目录，并在 drop 时还原。
struct IsolatedEnv {
    /// 仅用于保活临时目录（drop 时删除）。
    _config: TempDir,
    project: TempDir,
    prev_config_dir: Option<String>,
}

impl IsolatedEnv {
    fn new() -> Self {
        let config = TempDir::new().unwrap();
        let project = TempDir::new().unwrap();
        let prev_config_dir = std::env::var("VISP_CONFIG_DIR").ok();
        unsafe { std::env::set_var("VISP_CONFIG_DIR", config.path()) };
        Self {
            _config: config,
            project,
            prev_config_dir,
        }
    }

    fn project(&self) -> &Path {
        self.project.path()
    }
}

impl Drop for IsolatedEnv {
    fn drop(&mut self) {
        match &self.prev_config_dir {
            Some(value) => unsafe { std::env::set_var("VISP_CONFIG_DIR", value) },
            None => unsafe { std::env::remove_var("VISP_CONFIG_DIR") },
        }
    }
}

/// 测试用核心句柄：核心 + 可观察的两处全局资产。
struct TestHandles {
    core: Arc<ReloadCore>,
    tool_registry: Arc<ToolRegistry>,
    agent_registry: Arc<ArcSwap<AgentRegistry>>,
    /// 与核心共享的事件总线（A2a：通知额外发布到此）。
    bus: Arc<EventBus>,
}

fn make_core(env: &IsolatedEnv, multi_agent: bool) -> TestHandles {
    make_core_with_downlink(env, multi_agent, None)
}

/// 可注入 Chat 下行通道的核心构造（步骤 6c 通知测试用）。
fn make_core_with_downlink(
    env: &IsolatedEnv,
    multi_agent: bool,
    downlink: Option<mpsc::Sender<AgentEventFrame>>,
) -> TestHandles {
    let project = env.project();
    let rule_engine = Arc::new(RuleEngine::new(project).unwrap());

    let tool_registry = Arc::new(ToolRegistry::new());
    tool_registry
        .register(Arc::new(SkillTool::new(project)))
        .unwrap();

    let overrides: Vec<BuiltinAgentOverride> = Vec::new();
    let initial = visp_agent::agent_loader::load_agents(&[], &overrides);

    // 复刻 daemon 启动时的装配（main.rs 8.7.5）：多 agent 模式下把初始
    // subagent 集合注册为工具，使对账基线一致。
    if multi_agent {
        for agent in initial.list_subagents() {
            tool_registry
                .register(Arc::new(AgentTool::new(
                    agent.name.clone(),
                    agent.description.clone(),
                )))
                .unwrap();
        }
    }

    let agent_registry = Arc::new(ArcSwap::from_pointee(initial));

    let (tx, _rx) = mpsc::channel(16);
    let global_tx = if multi_agent { Some(tx) } else { None };

    let bus = Arc::new(EventBus::new());
    let mut core = ReloadCore::new(
        rule_engine,
        tool_registry.clone(),
        agent_registry.clone(),
        overrides,
        global_tx,
        project.to_path_buf(),
    );
    if let Some(downlink) = downlink {
        core = core.with_downlink(downlink);
    }
    core = core.with_bus(bus.clone());

    TestHandles {
        core: Arc::new(core),
        tool_registry,
        agent_registry,
        bus,
    }
}

fn write_valid_agent(dir: &Path, file: &str, name: &str, mode: &str) {
    fs::create_dir_all(dir).unwrap();
    fs::write(
        dir.join(file),
        format!("---\nname: {name}\ndescription: {name} 描述\nmode: {mode}\n---\n系统提示词\n"),
    )
    .unwrap();
}

fn write_agent_content(dir: &Path, file: &str, content: &str) {
    fs::create_dir_all(dir).unwrap();
    fs::write(dir.join(file), content).unwrap();
}

#[tokio::test]
#[serial]
async fn rules_item_reports_success_and_stats() {
    let env = IsolatedEnv::new();
    let handles = make_core(&env, false);

    // 核心构造后新增 AGENTS.md，重载应判为有变化并给出统计。
    fs::write(env.project().join("AGENTS.md"), "<Role>reload rules</Role>").unwrap();

    let items = handles.core.reload_domains(&[ReloadDomain::Rules]).await;

    assert_eq!(items.len(), 1);
    assert!(items[0].success, "rules 条目应成功：{:?}", items[0]);
    assert_eq!(items[0].changes, 1, "应统计到 1 个规则文件");
    assert!(items[0].message.contains("1 个规则文件"));
    assert!(
        handles
            .core
            .rule_engine
            .get_active_rules()
            .contains("reload rules")
    );
}

#[tokio::test]
#[serial]
async fn skills_item_rebuilds_tool_description() {
    let env = IsolatedEnv::new();
    let handles = make_core(&env, false);

    // 核心构造后新增 skill，重载应拾取并反映到 `skill` 工具 description。
    let skill_dir = env.project().join(".visp/skills/my-skill");
    fs::create_dir_all(&skill_dir).unwrap();
    fs::write(
        skill_dir.join("SKILL.md"),
        "---\nname: my-skill\ndescription: A fresh skill\n---\n正文\n",
    )
    .unwrap();

    let items = handles.core.reload_domains(&[ReloadDomain::Skills]).await;

    assert_eq!(items.len(), 1);
    assert!(items[0].success, "skills 条目应成功：{:?}", items[0]);
    let description = handles
        .tool_registry
        .get("skill")
        .unwrap()
        .description()
        .to_string();
    assert!(
        description.contains("my-skill"),
        "description: {description}"
    );
    assert!(
        description.contains("A fresh skill"),
        "description: {description}"
    );
}

#[tokio::test]
#[serial]
async fn agents_item_replaces_registry() {
    let env = IsolatedEnv::new();
    let handles = make_core(&env, true);
    write_valid_agent(
        &env.project().join(".visp/agents"),
        "reviewer.md",
        "reviewer",
        "subagent",
    );

    let items = handles.core.reload_domains(&[ReloadDomain::Agents]).await;

    assert_eq!(items.len(), 1);
    assert!(items[0].success, "agents 条目应成功：{:?}", items[0]);
    assert!(
        handles.agent_registry.load().get("reviewer").is_some(),
        "registry 应已替换并包含新 agent"
    );
}

#[tokio::test]
#[serial]
async fn explicit_entry_returns_system_prompt_receipt() {
    let env = IsolatedEnv::new();
    fs::write(env.project().join("AGENTS.md"), "rules").unwrap();
    let handles = make_core(&env, false);

    let items = handles.core.reload_all().await;

    assert_eq!(items.len(), 4, "显式入口应返回四类条目");
    assert_eq!(items[3].domain, ReloadDomain::SystemPrompt);
    assert!(items[3].success, "system_prompt 应恒成功");
    assert!(items[3].message.contains("新 session"));
}

#[tokio::test]
#[serial]
async fn partial_failure_is_isolated_to_agents_skip() {
    let env = IsolatedEnv::new();
    fs::write(env.project().join("AGENTS.md"), "rules").unwrap();
    let handles = make_core(&env, false);

    let agents_dir = env.project().join(".visp/agents");
    write_valid_agent(&agents_dir, "reviewer.md", "reviewer", "subagent");
    // 非法文件：无 frontmatter → load_agents 跳过并计入 skipped
    fs::write(agents_dir.join("broken.md"), "这不是合法的 agent 文件").unwrap();

    let items = handles.core.reload_all().await;

    assert_eq!(items.len(), 4);
    assert!(items.iter().all(|item| item.success), "items: {items:?}");
    let agents = &items[2];
    assert!(
        agents.message.contains("跳过"),
        "message: {}",
        agents.message
    );
    assert!(agents.message.contains('1'), "message: {}", agents.message);
    // 合法 agent 与内置 agent 均保留，旧集合不丢。
    let snapshot = handles.agent_registry.load();
    assert!(snapshot.get("reviewer").is_some());
    assert!(snapshot.get("default").is_some());
}

#[tokio::test]
#[serial]
async fn concurrent_reloads_serialize_and_return_full_results() {
    let env = IsolatedEnv::new();
    fs::write(env.project().join("AGENTS.md"), "rules").unwrap();
    let handles = make_core(&env, false);

    let first = handles.core.clone();
    let second = handles.core.clone();
    let task_a = tokio::spawn(async move { first.reload_all().await });
    let task_b = tokio::spawn(async move { second.reload_all().await });
    let (a, b) = tokio::join!(task_a, task_b);
    let (a, b) = (a.unwrap(), b.unwrap());

    assert_eq!(a.len(), 4);
    assert_eq!(b.len(), 4);
    assert!(a.iter().all(|item| item.success));
    assert!(b.iter().all(|item| item.success));
}

// ── 步骤 3b：三领域变化守卫 ─────────────────────────────────────────────

#[tokio::test]
#[serial]
async fn rules_no_change_guard_reports_unchanged() {
    let env = IsolatedEnv::new();
    fs::write(env.project().join("AGENTS.md"), "rules").unwrap();
    let handles = make_core(&env, false);

    let items = handles.core.reload_domains(&[ReloadDomain::Rules]).await;

    assert!(items[0].success);
    assert!(
        items[0].message.contains("无变更"),
        "message: {}",
        items[0].message
    );
    assert_eq!(items[0].changes, 0);
}

#[tokio::test]
#[serial]
async fn skills_no_change_guard_reports_unchanged() {
    let env = IsolatedEnv::new();
    let handles = make_core(&env, false);

    let items = handles.core.reload_domains(&[ReloadDomain::Skills]).await;

    assert!(items[0].success);
    assert!(
        items[0].message.contains("无变更"),
        "message: {}",
        items[0].message
    );
}

#[tokio::test]
#[serial]
async fn agents_no_change_guard_reports_unchanged() {
    let env = IsolatedEnv::new();
    let handles = make_core(&env, true);

    let items = handles.core.reload_domains(&[ReloadDomain::Agents]).await;

    assert!(items[0].success);
    assert!(
        items[0].message.contains("无变更"),
        "message: {}",
        items[0].message
    );
}

#[tokio::test]
#[serial]
async fn agents_permission_only_change_is_detected() {
    let env = IsolatedEnv::new();
    let handles = make_core(&env, true);
    let agents_dir = env.project().join(".visp/agents");

    write_agent_content(
        &agents_dir,
        "reviewer.md",
        "---\nname: reviewer\nmode: subagent\npermission: allow read_file *\n---\nbody\n",
    );
    // 首次重载建立基线（新增 reviewer）。
    let baseline = handles.core.reload_domains(&[ReloadDomain::Agents]).await;
    assert!(!baseline[0].message.contains("无变更"));

    // 仅改 permission（不动 name/description）。
    write_agent_content(
        &agents_dir,
        "reviewer.md",
        "---\nname: reviewer\nmode: subagent\npermission: deny edit_file *\n---\nbody\n",
    );
    let items = handles.core.reload_domains(&[ReloadDomain::Agents]).await;

    assert!(
        !items[0].message.contains("无变更"),
        "仅 permission 变化必须判为有变化：{}",
        items[0].message
    );
    let snapshot = handles.agent_registry.load();
    let reviewer = snapshot.get("reviewer").unwrap();
    assert_eq!(reviewer.permission.len(), 1);
    assert_eq!(
        reviewer.permission[0].action,
        visp_core::agent_definition::PermissionAction::Deny
    );
}

#[tokio::test]
#[serial]
async fn agents_system_prompt_only_change_is_detected() {
    let env = IsolatedEnv::new();
    let handles = make_core(&env, true);
    let agents_dir = env.project().join(".visp/agents");

    write_agent_content(
        &agents_dir,
        "reviewer.md",
        "---\nname: reviewer\nmode: subagent\n---\n旧系统提示词\n",
    );
    let baseline = handles.core.reload_domains(&[ReloadDomain::Agents]).await;
    assert!(!baseline[0].message.contains("无变更"));

    // 仅改 system_prompt 正文。
    write_agent_content(
        &agents_dir,
        "reviewer.md",
        "---\nname: reviewer\nmode: subagent\n---\n新系统提示词\n",
    );
    let items = handles.core.reload_domains(&[ReloadDomain::Agents]).await;

    assert!(
        !items[0].message.contains("无变更"),
        "仅 system_prompt 变化必须判为有变化：{}",
        items[0].message
    );
    let snapshot = handles.agent_registry.load();
    assert!(
        snapshot
            .get("reviewer")
            .unwrap()
            .system_prompt
            .contains("新系统提示词")
    );
}

#[tokio::test]
#[serial]
async fn no_change_avoids_swap_side_effects() {
    let env = IsolatedEnv::new();
    fs::write(env.project().join("AGENTS.md"), "rules").unwrap();
    let handles = make_core(&env, false);

    let agent_before = handles.agent_registry.load_full();
    let skill_before = handles.tool_registry.get("skill").unwrap();
    let rules_before = handles.core.rule_engine.get_active_rules();

    let items = handles.core.reload_all().await;

    // rules/skills/agents 三领域均无变化（system_prompt 恒成功回执）。
    for item in items.iter().take(3) {
        assert!(
            item.message.contains("无变更"),
            "领域 {:?} 应判无变更：{}",
            item.domain,
            item.message
        );
    }

    let agent_after = handles.agent_registry.load_full();
    let skill_after = handles.tool_registry.get("skill").unwrap();
    assert!(
        Arc::ptr_eq(&agent_before, &agent_after),
        "无变化不得 store ArcSwap（守卫须在替换之前）"
    );
    assert!(
        Arc::ptr_eq(&skill_before, &skill_after),
        "无变化不得替换 skill 工具"
    );
    assert_eq!(rules_before, handles.core.rule_engine.get_active_rules());
}

// ── 步骤 3c：agent 工具对账 ─────────────────────────────────────────────

#[tokio::test]
#[serial]
async fn removed_agent_removes_tool() {
    let env = IsolatedEnv::new();
    let handles = make_core(&env, true);
    let agents_dir = env.project().join(".visp/agents");

    write_valid_agent(&agents_dir, "reviewer.md", "reviewer", "subagent");
    handles.core.reload_domains(&[ReloadDomain::Agents]).await;
    assert!(handles.tool_registry.get("reviewer").is_some());

    fs::remove_file(agents_dir.join("reviewer.md")).unwrap();
    let items = handles.core.reload_domains(&[ReloadDomain::Agents]).await;

    assert!(items[0].success, "items: {:?}", items[0]);
    assert!(
        handles.tool_registry.get("reviewer").is_none(),
        "仅旧集合有的 agent 工具应被移除"
    );
}

#[tokio::test]
#[serial]
async fn added_agent_registers_tool() {
    let env = IsolatedEnv::new();
    let handles = make_core(&env, true);

    write_valid_agent(
        &env.project().join(".visp/agents"),
        "reviewer.md",
        "reviewer",
        "subagent",
    );
    let items = handles.core.reload_domains(&[ReloadDomain::Agents]).await;

    assert!(items[0].success, "items: {:?}", items[0]);
    let tool = handles
        .tool_registry
        .get("reviewer")
        .expect("新增 agent 应注册为工具");
    assert_eq!(tool.name(), "reviewer");
}

#[tokio::test]
#[serial]
async fn name_collision_fails_isolated() {
    let env = IsolatedEnv::new();
    let handles = make_core(&env, true);
    let agents_dir = env.project().join(".visp/agents");

    // 与核心工具 "skill" 大小写不敏感撞名。
    write_valid_agent(&agents_dir, "skill-agent.md", "Skill", "subagent");
    // 另一个正常 agent，验证对账不被撞名阻断。
    write_valid_agent(&agents_dir, "reviewer.md", "reviewer", "subagent");

    let items = handles.core.reload_domains(&[ReloadDomain::Agents]).await;

    assert!(
        !items[0].success,
        "撞名应使 agents 条目失败：{:?}",
        items[0]
    );
    assert!(
        items[0].message.contains("注册 agent 工具 'Skill' 失败"),
        "message: {}",
        items[0].message
    );
    assert!(
        handles.tool_registry.get("reviewer").is_some(),
        "其余对账应继续完成"
    );
}

#[tokio::test]
#[serial]
async fn description_change_replaces_tool() {
    let env = IsolatedEnv::new();
    let handles = make_core(&env, true);
    let agents_dir = env.project().join(".visp/agents");

    write_agent_content(
        &agents_dir,
        "reviewer.md",
        "---\nname: reviewer\ndescription: 旧描述\nmode: subagent\n---\nbody\n",
    );
    handles.core.reload_domains(&[ReloadDomain::Agents]).await;
    assert_eq!(
        handles.tool_registry.get("reviewer").unwrap().description(),
        "旧描述"
    );

    write_agent_content(
        &agents_dir,
        "reviewer.md",
        "---\nname: reviewer\ndescription: 新描述\nmode: subagent\n---\nbody\n",
    );
    let items = handles.core.reload_domains(&[ReloadDomain::Agents]).await;

    assert!(items[0].success, "items: {:?}", items[0]);
    assert_eq!(
        handles.tool_registry.get("reviewer").unwrap().description(),
        "新描述",
        "description 变化应对同名工具做替换"
    );
}

#[tokio::test]
#[serial]
async fn mode_switch_to_primary_removes_tool() {
    let env = IsolatedEnv::new();
    let handles = make_core(&env, true);
    let agents_dir = env.project().join(".visp/agents");

    write_valid_agent(&agents_dir, "reviewer.md", "reviewer", "subagent");
    handles.core.reload_domains(&[ReloadDomain::Agents]).await;
    assert!(handles.tool_registry.get("reviewer").is_some());

    // Subagent → Primary：表现为「旧有新无」。
    write_valid_agent(&agents_dir, "reviewer.md", "reviewer", "primary");
    let items = handles.core.reload_domains(&[ReloadDomain::Agents]).await;

    assert!(items[0].success, "items: {:?}", items[0]);
    assert!(
        handles.tool_registry.get("reviewer").is_none(),
        "mode 改为 Primary 后不应再保留 agent 工具"
    );
    assert!(handles.agent_registry.load().get("reviewer").is_some());
}

#[tokio::test]
#[serial]
async fn tool_set_matches_subagent_set_after_reconcile() {
    let env = IsolatedEnv::new();
    let handles = make_core(&env, true);
    let agents_dir = env.project().join(".visp/agents");

    write_valid_agent(&agents_dir, "alpha.md", "alpha", "subagent");
    write_agent_content(
        &agents_dir,
        "beta.md",
        "---\nname: beta\ndescription: beta 原描述\nmode: subagent\n---\nbody\n",
    );
    handles.core.reload_domains(&[ReloadDomain::Agents]).await;

    // 一次重载内完成：删除 alpha、改 beta 描述、新增 gamma。
    fs::remove_file(agents_dir.join("alpha.md")).unwrap();
    write_agent_content(
        &agents_dir,
        "beta.md",
        "---\nname: beta\ndescription: beta 新描述\nmode: subagent\n---\nbody\n",
    );
    write_valid_agent(&agents_dir, "gamma.md", "gamma", "all");
    let items = handles.core.reload_domains(&[ReloadDomain::Agents]).await;

    assert!(items[0].success, "items: {:?}", items[0]);
    let snapshot = handles.agent_registry.load();
    for agent in snapshot.list_subagents() {
        assert!(
            handles.tool_registry.get(&agent.name).is_some(),
            "subagent '{}' 应有对应工具",
            agent.name
        );
    }
    assert!(
        handles.tool_registry.get("alpha").is_none(),
        "已删除的 agent 工具必须移除"
    );
}

// ── 步骤 6c：自动重载通知推送 ───────────────────────────────────────────

#[tokio::test]
#[serial]
async fn auto_change_pushes_single_summary_with_empty_session_id() {
    let env = IsolatedEnv::new();
    let (tx, mut rx) = mpsc::channel::<AgentEventFrame>(8);
    let handles = make_core_with_downlink(&env, false, Some(tx));

    fs::write(env.project().join("AGENTS.md"), "auto reload").unwrap();
    let items = handles.core.reload_domains(&[ReloadDomain::Rules]).await;
    assert!(items[0].changes > 0, "应有变更：{:?}", items[0]);

    let frame = rx.try_recv().expect("有变更应推送一条汇总通知");
    assert_eq!(frame.session_id, "", "session_id 必须为空以路由主 tab");
    match frame.event {
        AgentEvent::StatusUpdate(message) => {
            assert!(message.contains("已热重载"), "message: {message}");
            assert!(message.contains("rules"), "message: {message}");
        }
        _ => panic!("应为 StatusUpdate 帧"),
    }
    assert!(rx.try_recv().is_err(), "一次自动重载至多推送一条通知");
}

/// A2a：自动重载通知在既有下行之外**额外**发布到事件总线（纯加法）。
#[tokio::test]
#[serial]
async fn auto_change_also_publishes_frame_to_bus() {
    let env = IsolatedEnv::new();
    let (tx, mut down_rx) = mpsc::channel::<AgentEventFrame>(8);
    let handles = make_core_with_downlink(&env, false, Some(tx));

    // 先订阅，再触发（总线只投递订阅之后发布的事件）。
    let mut bus_rx = handles.bus.subscribe();

    fs::write(env.project().join("AGENTS.md"), "auto reload").unwrap();
    let items = handles.core.reload_domains(&[ReloadDomain::Rules]).await;
    assert!(items[0].changes > 0, "应有变更：{:?}", items[0]);

    // 既有下行行为未变：仍收到一条汇总通知。
    let down_frame = down_rx.try_recv().expect("既有下行仍应收到通知");
    assert!(matches!(down_frame.event, AgentEvent::StatusUpdate(_)));

    // 总线额外收到同一帧。
    let envelope = bus_rx.try_recv().expect("有变更应额外发布到总线");
    assert_eq!(envelope.frame.session_id, "");
    match envelope.frame.event {
        AgentEvent::StatusUpdate(message) => {
            assert!(message.contains("已热重载"), "message: {message}");
        }
        _ => panic!("应为 StatusUpdate 帧"),
    }
}

#[tokio::test]
#[serial]
async fn no_change_is_silent() {
    let env = IsolatedEnv::new();
    let (tx, mut rx) = mpsc::channel::<AgentEventFrame>(8);
    let handles = make_core_with_downlink(&env, false, Some(tx));

    let items = handles.core.reload_domains(&[ReloadDomain::Rules]).await;
    assert_eq!(items[0].changes, 0, "无 AGENTS.md 应判无变更");
    assert!(rx.try_recv().is_err(), "全部无变化应静默");
}

#[tokio::test]
#[serial]
async fn failure_pushes_error_status() {
    let env = IsolatedEnv::new();
    let (tx, mut rx) = mpsc::channel::<AgentEventFrame>(8);
    let handles = make_core_with_downlink(&env, true, Some(tx));

    // 与核心工具 "skill" 大小写不敏感撞名 → agents 领域整体失败。
    write_valid_agent(
        &env.project().join(".visp/agents"),
        "skill-agent.md",
        "Skill",
        "subagent",
    );
    let items = handles.core.reload_domains(&[ReloadDomain::Agents]).await;
    assert!(!items[0].success, "撞名应使 agents 失败：{:?}", items[0]);

    let frame = rx.try_recv().expect("失败应推送一条错误性质通知");
    assert_eq!(frame.session_id, "");
    match frame.event {
        AgentEvent::StatusUpdate(message) => {
            assert!(message.contains("失败"), "message: {message}");
        }
        _ => panic!("应为 StatusUpdate 帧"),
    }
}

#[tokio::test]
#[serial]
async fn downlink_unavailable_does_not_panic() {
    let env = IsolatedEnv::new();
    let (tx, rx) = mpsc::channel::<AgentEventFrame>(8);
    // 接收端已关闭：best-effort 发送失败应被静默容忍。
    drop(rx);
    let handles = make_core_with_downlink(&env, false, Some(tx));

    fs::write(env.project().join("AGENTS.md"), "auto reload").unwrap();
    let items = handles.core.reload_domains(&[ReloadDomain::Rules]).await;

    assert_eq!(items.len(), 1);
    assert!(items[0].success);
    assert_eq!(items[0].changes, 1, "reload 结果不受通道不可用影响");
}
