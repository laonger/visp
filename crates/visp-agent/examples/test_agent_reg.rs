// 模拟 daemon 的 agent 工具注册流程（诊断用）
use std::sync::Arc;

use tokio::sync::mpsc;
use visp_agent::agent_loader::load_agents;
use visp_core::agent::{AgentTool, Envelope};
use visp_core::tool_registry::ToolRegistry;

#[tokio::main]
async fn main() {
    // 空 agent 目录（模拟 desktop 场景：cwd=crates/visp-desktop 无 .visp/agents）
    let dirs: Vec<&std::path::Path> = vec![];
    let registry = Arc::new(load_agents(&dirs, &[]));
    println!(
        "注册的 agents: {:?}",
        registry
            .list_subagents()
            .iter()
            .map(|a| a.name.clone())
            .collect::<Vec<_>>()
    );

    let tool_registry = ToolRegistry::new();
    let (global_tx, _rx) = mpsc::channel::<Envelope>(256);
    for agent_def in registry.list_subagents() {
        let tool = Arc::new(AgentTool::new(
            agent_def.name.clone(),
            agent_def.description.clone(),
        ));
        tool_registry
            .register(tool)
            .map_err(|e| format!("register agent tool: {e}"))
            .unwrap();
    }
    println!("注册的工具: {:?}", tool_registry.names());
    let _ = global_tx;
}
