/// ActiveAgent — 运行时活跃的 agent 实例
/// 由 ActiveAgentRegistry 管理，提供父子关系追踪
use std::time::Instant;

use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use visp_core::agent::OrchestratorMessage;

/// 运行时活跃的 Agent 实例
#[derive(Clone)]
pub struct ActiveAgent {
    /// Session ID（全局唯一）
    pub session_id: String,
    /// 父 Agent 的 Session ID（根 agent 为 None）
    pub parent_session_id: Option<String>,
    /// Agent 类型名称（对应 AgentDefinition.name）
    pub agent_name: String,
    /// 取消令牌
    pub cancel_token: CancellationToken,
    /// 收件箱发送端（Orchestrator → Agent）
    pub inbox: mpsc::Sender<OrchestratorMessage>,
    /// 当前正在等待的 tool_call_id（如果有）
    pub pending_call_id: Option<String>,
    /// 启动时间
    pub started_at: Instant,
}

/// 活跃 Agent 注册中心
pub struct ActiveAgentRegistry {
    agents: std::collections::HashMap<String, ActiveAgent>,
}

impl ActiveAgentRegistry {
    pub fn new() -> Self {
        Self {
            agents: std::collections::HashMap::new(),
        }
    }

    /// 注册一个活跃 agent（同 session_id 覆盖旧值）
    pub fn register(&mut self, agent: ActiveAgent) {
        self.agents.insert(agent.session_id.clone(), agent);
    }

    /// 移除并返回 agent
    pub fn remove(&mut self, session_id: &str) -> Option<ActiveAgent> {
        self.agents.remove(session_id)
    }

    /// 获取 agent 引用
    pub fn get(&self, session_id: &str) -> Option<&ActiveAgent> {
        self.agents.get(session_id)
    }

    /// 获取 agent 可变引用
    pub fn get_mut(&mut self, session_id: &str) -> Option<&mut ActiveAgent> {
        self.agents.get_mut(session_id)
    }

    /// 返回直接子 agent 列表
    pub fn children_of(&self, parent_id: &str) -> Vec<&ActiveAgent> {
        self.agents
            .values()
            .filter(|a| a.parent_session_id.as_deref() == Some(parent_id))
            .collect()
    }

    /// 递归查找所有子孙 agent（BFS）
    pub fn descendants_of(&self, parent_id: &str) -> Vec<&ActiveAgent> {
        let mut result = Vec::new();
        let mut queue: Vec<&str> = vec![parent_id];
        while let Some(current) = queue.pop() {
            for child in self.agents.values() {
                if child.parent_session_id.as_deref() == Some(current) {
                    result.push(child);
                    queue.push(&child.session_id);
                }
            }
        }
        result
    }

    /// 计算 agent 在树中的深度（根为 0）
    pub fn compute_depth(&self, session_id: &str) -> u32 {
        let mut depth = 0;
        let mut current = session_id;
        loop {
            if let Some(agent) = self.agents.get(current) {
                match agent.parent_session_id.as_deref() {
                    Some(parent) => {
                        depth += 1;
                        current = parent;
                    }
                    None => return depth,
                }
            } else {
                return 0;
            }
        }
    }

    /// 当前活跃 agent 数量
    pub fn len(&self) -> usize {
        self.agents.len()
    }

    /// 是否为空
    pub fn is_empty(&self) -> bool {
        self.agents.is_empty()
    }

    /// 返回所有 agent 的克隆列表（用于遍历时避免借用问题）
    pub fn agents_cloned(&self) -> Vec<ActiveAgent> {
        self.agents.values().cloned().collect()
    }
}

impl Default for ActiveAgentRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
#[path = "active_agent_tests.rs"]
mod tests;
