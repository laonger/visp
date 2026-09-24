//! daemon 共享 reload 核心（设计 §5.5）。
//!
//! 显式 `/reload` 与自动文件监听两条触发路径共用**一把异步互斥量**与同一套
//! 重载流程：`rules → skills → agents`；显式入口额外产出 `system_prompt`
//! 成功回执（L1 语义确认）。
//!
//! 本模块只产出核心内部的逐项结果，不实现 gRPC handler（步骤 4a）与通知
//! 推送（步骤 6c），也不触碰任何 session 状态。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use arc_swap::ArcSwap;
use tokio::sync::mpsc;

use visp_agent::agent_loader::{BuiltinAgentOverride, load_agents_with_stats};
use visp_core::agent::{AgentEvent, AgentEventFrame, AgentTool, Envelope};
use visp_core::agent_registry::AgentRegistry;
use visp_core::rules::RuleEngine;
use visp_core::tool_registry::ToolRegistry;
use visp_tools::skill::SkillTool;

/// 待重载领域。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ReloadDomain {
    /// AGENTS.md / rules。
    Rules,
    /// skills 列表（烧入 `skill` 工具 description）。
    Skills,
    /// AgentRegistry（内置 / `[[agent.builtin]]` overrides / 全局 / 项目定义）。
    Agents,
    /// system-prompt.md base 模板（仅显式路径的确认回执，L1）。
    SystemPrompt,
}

/// 单个领域的重载结果（核心内部唯一形态；gRPC 单向映射见步骤 4a）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReloadItem {
    pub domain: ReloadDomain,
    /// 该领域整体是否成功。
    pub success: bool,
    /// 成功时为统计摘要，失败时为原因。
    pub message: String,
    /// 变更条目数；0 表示无变更或本领域不适用。
    pub changes: usize,
}

/// 显式与自动路径共享的 reload 核心。
pub struct ReloadCore {
    rule_engine: Arc<RuleEngine>,
    tool_registry: Arc<ToolRegistry>,
    agent_registry: Arc<ArcSwap<AgentRegistry>>,
    /// daemon 启动时构建一次的 `[[agent.builtin]]` overrides
    /// （daemon.toml 不热重载，故其内容在进程生命周期内静态）。
    builtin_overrides: Vec<BuiltinAgentOverride>,
    /// 事件通道（对账注册 agent 工具所需）；`None` 表示单 agent 模式。
    global_tx: Option<mpsc::Sender<Envelope>>,
    /// daemon 启动时的 project root（skills 重扫复用，不临时取 cwd）。
    project_root: PathBuf,
    /// 最近一次生效的 skills listing（变化守卫素材，设计 §5.2/§7 决策 13）。
    /// 重载执行受 [`ReloadCore::lock`] 串行化，此锁仅作内部可变状态。
    skills_listing: std::sync::Mutex<String>,
    /// Chat 下行通道（`AgentEventFrame`，Chat 流下行帧的唯一来源）。
    /// 自动入口完成后 best-effort 推送汇总通知（设计 §5.5/§7 决策 11）。
    downlink: Option<mpsc::Sender<AgentEventFrame>>,
    /// 显式与自动路径共用的异步互斥量（设计 §7 决策 9）。
    lock: tokio::sync::Mutex<()>,
}

impl ReloadCore {
    pub fn new(
        rule_engine: Arc<RuleEngine>,
        tool_registry: Arc<ToolRegistry>,
        agent_registry: Arc<ArcSwap<AgentRegistry>>,
        builtin_overrides: Vec<BuiltinAgentOverride>,
        global_tx: Option<mpsc::Sender<Envelope>>,
        project_root: PathBuf,
    ) -> Self {
        let skills_listing = visp_core::session::load_skills(&project_root);
        Self {
            rule_engine,
            tool_registry,
            agent_registry,
            builtin_overrides,
            global_tx,
            project_root,
            skills_listing: std::sync::Mutex::new(skills_listing),
            downlink: None,
            lock: tokio::sync::Mutex::new(()),
        }
    }

    /// 注入 Chat 下行通道，供自动入口 best-effort 推送热重载通知。
    ///
    /// 通道身份：`AgentEventFrame` 下行帧的唯一来源（daemon 装配处
    /// `orchestrator_grpc_tx`），与 `global_tx`（agent 工具事件通道）不同。
    pub fn with_downlink(mut self, downlink: mpsc::Sender<AgentEventFrame>) -> Self {
        self.downlink = Some(downlink);
        self
    }

    /// 显式入口：四类全跑（system_prompt 仅确认回执）。
    ///
    /// 显式路径把逐项结果直接返回给 gRPC handler 渲染，**不**推送自动通知。
    pub async fn reload_all(&self) -> Vec<ReloadItem> {
        self.execute_domains(&[
            ReloadDomain::Rules,
            ReloadDomain::Skills,
            ReloadDomain::Agents,
            ReloadDomain::SystemPrompt,
        ])
        .await
    }

    /// 自动入口：只重载入参命中的领域子集，完成后推送至多一条汇总通知。
    pub async fn reload_domains(&self, domains: &[ReloadDomain]) -> Vec<ReloadItem> {
        let items = self.execute_domains(domains).await;
        self.notify(&items);
        items
    }

    /// 两入口共享的重载执行：取互斥量、按规范化顺序逐领域执行。
    async fn execute_domains(&self, domains: &[ReloadDomain]) -> Vec<ReloadItem> {
        let _guard = self.lock.lock().await;
        canonical_order(domains)
            .into_iter()
            .map(|domain| self.reload_one(domain))
            .collect()
    }

    /// 自动入口完成后的 best-effort 通知（设计 §7 决策 11）。
    ///
    /// 至多一条：有变更→汇总；失败→错误性质提示；全部无变化→静默。
    /// 通道未注入 / 已关闭 / 已满均静默丢弃，不影响 reload 结果。
    fn notify(&self, items: &[ReloadItem]) {
        let Some(message) = build_notification(items) else {
            return;
        };
        let Some(downlink) = &self.downlink else {
            return;
        };
        let frame = AgentEventFrame {
            event: AgentEvent::StatusUpdate(message),
            session_id: String::new(),
            agent_name: String::new(),
            parent_session_id: None,
            parent_session_name: None,
        };
        let _ = downlink.try_send(frame);
    }

    fn reload_one(&self, domain: ReloadDomain) -> ReloadItem {
        match domain {
            ReloadDomain::Rules => self.reload_rules(),
            ReloadDomain::Skills => self.reload_skills(),
            ReloadDomain::Agents => self.reload_agents(),
            ReloadDomain::SystemPrompt => system_prompt_receipt(),
        }
    }

    /// rules：调用 `RuleEngine::reload()`，并消费其返回的 `changed` 作为守卫
    /// （不重复比较拼接串，见计划备注 12）。
    fn reload_rules(&self) -> ReloadItem {
        match self.rule_engine.reload() {
            Ok(result) if !result.changed => no_change_item(ReloadDomain::Rules),
            Ok(result) => {
                let files = result.ruleset.files.len();
                ReloadItem {
                    domain: ReloadDomain::Rules,
                    success: true,
                    message: format!("{files} 个规则文件"),
                    changes: files,
                }
            }
            Err(error) => ReloadItem {
                domain: ReloadDomain::Rules,
                success: false,
                message: format!("重载失败：{error}"),
                changes: 0,
            },
        }
    }

    /// skills：重扫 listing，与上次生效的 listing 等值则跳过；
    /// 有变化才重建 `SkillTool` 实例并 `ToolRegistry::update` 同名替换。
    fn reload_skills(&self) -> ReloadItem {
        let listing = visp_core::session::load_skills(&self.project_root);
        {
            let mut previous = self.skills_listing.lock().unwrap();
            if *previous == listing {
                return no_change_item(ReloadDomain::Skills);
            }
            *previous = listing;
        }

        let tool: Arc<dyn visp_core::tool::Tool> = Arc::new(SkillTool::new(&self.project_root));
        match self.tool_registry.update("skill", tool) {
            Ok(()) => ReloadItem {
                domain: ReloadDomain::Skills,
                success: true,
                message: "skills 列表已更新".to_string(),
                changes: 1,
            },
            Err(error) => ReloadItem {
                domain: ReloadDomain::Skills,
                success: false,
                message: format!("重载失败：{error}"),
                changes: 0,
            },
        }
    }

    /// agents：重算 agent 目录 → `load_agents` → 与旧 registry 全 9 字段等值比较；
    /// 有变化才整体 `store` 替换，并在替换后对账 agent 工具。
    fn reload_agents(&self) -> ReloadItem {
        let previous = self.agent_registry.load_full();
        let agent_dirs = collect_agent_dirs(&self.project_root);
        let dir_refs: Vec<&Path> = agent_dirs.iter().map(PathBuf::as_path).collect();
        let (registry, stats) = load_agents_with_stats(&dir_refs, &self.builtin_overrides);

        if registries_equal(&previous, &registry) {
            return no_change_item(ReloadDomain::Agents);
        }

        let changes = agent_change_count(&previous, &registry);
        let old_tools = subagent_tool_map(&previous);
        let new_tools = subagent_tool_map(&registry);

        self.agent_registry.store(Arc::new(registry));

        let failures = if self.global_tx.is_some() {
            self.reconcile_agent_tools(&old_tools, &new_tools)
        } else {
            Vec::new()
        };

        let mut message = format!("{changes} 个 agent 变更");
        if stats.skipped > 0 {
            message.push_str(&format!("，跳过 {} 个非法文件", stats.skipped));
        }
        if self.global_tx.is_none() {
            message.push_str("（单 agent 模式，不注册 agent 工具）");
        } else if !failures.is_empty() {
            message.push_str(&format!(
                "，{} 个 agent 工具对账失败：{}",
                failures.len(),
                failures.join("；")
            ));
        }

        ReloadItem {
            domain: ReloadDomain::Agents,
            success: failures.is_empty(),
            message,
            changes,
        }
    }

    /// agent 工具对账（设计 §5.3 五分支），返回失败明细。
    ///
    /// ① 仅旧集合有 → 移除；② 仅新集合有 → 注册；③ 与现存工具重名
    /// （`ToolRegistry` 内部大小写不敏感）→ 由 `register` 报错并计入失败，
    /// 不阻断其余对账；④ 两边都有但 description 变化 → 同名替换；
    /// ⑤ Subagent/All → Primary 表现为「旧有新无」→ 走 ① 移除。
    fn reconcile_agent_tools(
        &self,
        old: &HashMap<String, String>,
        new: &HashMap<String, String>,
    ) -> Vec<String> {
        let mut failures = Vec::new();

        for name in old.keys() {
            if !new.contains_key(name)
                && let Err(error) = self.tool_registry.remove(name)
            {
                failures.push(format!("移除 agent 工具 '{name}' 失败：{error}"));
            }
        }

        for (name, description) in new {
            match old.get(name) {
                Some(old_description) if old_description != description => {
                    let tool = arc_agent_tool(name, description);
                    if let Err(error) = self.tool_registry.update(name, tool) {
                        failures.push(format!("替换 agent 工具 '{name}' 失败：{error}"));
                    }
                }
                Some(_) => {}
                None => {
                    let tool = arc_agent_tool(name, description);
                    if let Err(error) = self.tool_registry.register(tool) {
                        failures.push(format!("注册 agent 工具 '{name}' 失败：{error}"));
                    }
                }
            }
        }

        failures
    }
}

/// 构造 AgentTool（对账注册 / 替换共用）。
fn arc_agent_tool(name: &str, description: &str) -> Arc<dyn visp_core::tool::Tool> {
    Arc::new(AgentTool::new(name.to_string(), description.to_string()))
}

/// subagent 集合的 name → description 快照（对账输入）。
fn subagent_tool_map(registry: &AgentRegistry) -> HashMap<String, String> {
    registry
        .list_subagents()
        .iter()
        .map(|agent| (agent.name.clone(), agent.description.clone()))
        .collect()
}

/// 无变化领域的统一结果（守卫拦下，未发生替换）。
fn no_change_item(domain: ReloadDomain) -> ReloadItem {
    ReloadItem {
        domain,
        success: true,
        message: "无变更".to_string(),
        changes: 0,
    }
}

/// 构造自动入口的至多一条汇总通知文案（设计 §7 决策 11）。
///
/// - 存在失败领域 → 错误性质汇总（含失败原因）；
/// - 否则存在变更领域 → 「已热重载」汇总（含各领域变更数）；
/// - 全部无变化 → `None`（静默）。
fn build_notification(items: &[ReloadItem]) -> Option<String> {
    let changed: Vec<&ReloadItem> = items
        .iter()
        .filter(|item| item.success && item.changes > 0)
        .collect();
    let failed: Vec<&ReloadItem> = items.iter().filter(|item| !item.success).collect();
    if changed.is_empty() && failed.is_empty() {
        return None;
    }

    let mut parts = Vec::new();
    if !changed.is_empty() {
        let detail = changed
            .iter()
            .map(|item| format!("{}（{} 变更）", domain_label(item.domain), item.changes))
            .collect::<Vec<_>>()
            .join("、");
        parts.push(format!("已热重载：{detail}"));
    }
    if !failed.is_empty() {
        let detail = failed
            .iter()
            .map(|item| format!("{}：{}", domain_label(item.domain), item.message))
            .collect::<Vec<_>>()
            .join("；");
        parts.push(format!("失败：{detail}"));
    }
    Some(parts.join("；"))
}

/// 领域在通知文案中的稳定短名。
fn domain_label(domain: ReloadDomain) -> &'static str {
    match domain {
        ReloadDomain::Rules => "rules",
        ReloadDomain::Skills => "skills",
        ReloadDomain::Agents => "agents",
        ReloadDomain::SystemPrompt => "system_prompt",
    }
}

/// 两个 registry 的「定义集合」是否相等：name → 完整 `AgentDefinition` 全字段等值。
///
/// 复用 1b 补充的 `PartialEq`，覆盖全部 9 个字段（含 permission / system_prompt）。
fn registries_equal(a: &AgentRegistry, b: &AgentRegistry) -> bool {
    let a_agents = a.list();
    if a_agents.len() != b.list().len() {
        return false;
    }
    a_agents
        .iter()
        .all(|agent| b.get(&agent.name) == Some(*agent))
}

/// 统计定义集合的实际变更条目数（新增 + 修改 + 删除）。
fn agent_change_count(old: &AgentRegistry, new: &AgentRegistry) -> usize {
    let mut count = 0;
    for agent in old.list() {
        match new.get(&agent.name) {
            Some(other) if other == agent => {}
            _ => count += 1, // 修改或删除
        }
    }
    for agent in new.list() {
        if old.get(&agent.name).is_none() {
            count += 1; // 新增
        }
    }
    count
}

/// system-prompt.md 的确认回执：无操作，仅确认「仅对新 session 生效」的 L1 语义。
fn system_prompt_receipt() -> ReloadItem {
    ReloadItem {
        domain: ReloadDomain::SystemPrompt,
        success: true,
        message: "system-prompt.md 仅对新 session 生效".to_string(),
        changes: 0,
    }
}

/// 每次重载时重新收集 agent 目录。
///
/// 存在性检查**不可缓存启动时结果**——否则「运行中创建 agents 目录」的场景
/// （文件监听补挂链所覆盖）不会被拾取（设计 §5.5）。顺序与 daemon 启动一致：
/// 全局（低优先级）→ 项目（高优先级）。
fn collect_agent_dirs(project_root: &Path) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(global_agents_dir) = visp_config::path::agents_dir_global()
        && global_agents_dir.exists()
    {
        dirs.push(global_agents_dir);
    }
    let project_agents_dir = visp_config::path::agents_dir_project(project_root);
    if project_agents_dir.exists() {
        dirs.push(project_agents_dir);
    }
    dirs
}

/// 规范化领域顺序并去重：rules → skills → agents → system_prompt。
///
/// 保证自动入口传入的任意子集产出稳定顺序，便于调用方与测试断言。
fn canonical_order(domains: &[ReloadDomain]) -> Vec<ReloadDomain> {
    const ORDER: [ReloadDomain; 4] = [
        ReloadDomain::Rules,
        ReloadDomain::Skills,
        ReloadDomain::Agents,
        ReloadDomain::SystemPrompt,
    ];
    ORDER
        .into_iter()
        .filter(|domain| domains.contains(domain))
        .collect()
}

#[cfg(test)]
#[path = "reload_tests.rs"]
mod tests;
