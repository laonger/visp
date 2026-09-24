//! daemon 共享 reload 核心（设计 §5.5）。
//!
//! 显式 `/reload` 与自动文件监听两条触发路径共用**一把异步互斥量**与同一套
//! 重载流程：`rules → skills → agents`；显式入口额外产出 `system_prompt`
//! 成功回执（L1 语义确认）。
//!
//! 本模块只产出核心内部的逐项结果，不实现 gRPC handler（步骤 4a）与通知
//! 推送（步骤 6c），也不触碰任何 session 状态。

use std::path::{Path, PathBuf};
use std::sync::Arc;

use arc_swap::ArcSwap;
use tokio::sync::mpsc;

use visp_agent::agent_loader::{BuiltinAgentOverride, load_agents_with_stats};
use visp_core::agent::Envelope;
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
        Self {
            rule_engine,
            tool_registry,
            agent_registry,
            builtin_overrides,
            global_tx,
            project_root,
            lock: tokio::sync::Mutex::new(()),
        }
    }

    /// 显式入口：四类全跑（system_prompt 仅确认回执）。
    pub async fn reload_all(&self) -> Vec<ReloadItem> {
        self.reload_domains(&[
            ReloadDomain::Rules,
            ReloadDomain::Skills,
            ReloadDomain::Agents,
            ReloadDomain::SystemPrompt,
        ])
        .await
    }

    /// 自动入口雏形：只重载入参命中的领域子集。
    pub async fn reload_domains(&self, domains: &[ReloadDomain]) -> Vec<ReloadItem> {
        let _guard = self.lock.lock().await;
        canonical_order(domains)
            .into_iter()
            .map(|domain| self.reload_one(domain))
            .collect()
    }

    fn reload_one(&self, domain: ReloadDomain) -> ReloadItem {
        match domain {
            ReloadDomain::Rules => self.reload_rules(),
            ReloadDomain::Skills => self.reload_skills(),
            ReloadDomain::Agents => self.reload_agents(),
            ReloadDomain::SystemPrompt => system_prompt_receipt(),
        }
    }

    /// rules：调用 `RuleEngine::reload()`（构建-替换语义已在内部实现）。
    fn reload_rules(&self) -> ReloadItem {
        match self.rule_engine.reload() {
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

    /// skills：重建 `SkillTool` 实例，经 `ToolRegistry::update` 同名替换。
    fn reload_skills(&self) -> ReloadItem {
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

    /// agents：重算 agent 目录 → `load_agents` → 整体 `store` 替换。
    fn reload_agents(&self) -> ReloadItem {
        let agent_dirs = collect_agent_dirs(&self.project_root);
        let dir_refs: Vec<&Path> = agent_dirs.iter().map(PathBuf::as_path).collect();
        let (registry, stats) = load_agents_with_stats(&dir_refs, &self.builtin_overrides);
        let count = registry.list().len();

        self.agent_registry.store(Arc::new(registry));

        let mut message = format!("{count} 个 agent");
        if stats.skipped > 0 {
            message.push_str(&format!("，跳过 {} 个非法文件", stats.skipped));
        }
        if self.global_tx.is_none() {
            message.push_str("（单 agent 模式，不注册 agent 工具）");
        }

        ReloadItem {
            domain: ReloadDomain::Agents,
            success: true,
            message,
            changes: count,
        }
    }
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
