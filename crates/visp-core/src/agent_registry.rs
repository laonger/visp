use std::collections::HashMap;

use crate::agent_definition::{AgentDefinition, AgentMode};
use crate::error::CoreError;

/// Agent 注册中心
pub struct AgentRegistry {
    agents: HashMap<String, AgentDefinition>,
}

impl AgentRegistry {
    pub fn new() -> Self {
        Self {
            agents: HashMap::new(),
        }
    }

    /// 注册 agent，同名返回 Err
    pub fn register(&mut self, agent: AgentDefinition) -> Result<(), CoreError> {
        let name = agent.name.clone();
        if self.agents.contains_key(&name) {
            return Err(CoreError::Tool(format!("duplicate agent name: {name}")));
        }
        self.agents.insert(name, agent);
        Ok(())
    }

    /// 注册或替换 agent（同名时覆盖）
    pub fn register_or_replace(&mut self, agent: AgentDefinition) {
        self.agents.insert(agent.name.clone(), agent);
    }

    /// 按名称查找 agent
    pub fn get(&self, name: &str) -> Option<&AgentDefinition> {
        self.agents.get(name)
    }

    /// 获取默认 agent
    /// 优先取 mode != Subagent 且名称含 "default" 的 agent；
    /// 无则取第一个 mode != Subagent 的 agent；
    /// 全 Subagent 时返回 None
    pub fn default(&self) -> Option<&AgentDefinition> {
        // 优先名称为 default 的
        for agent in self.agents.values() {
            if agent.mode != AgentMode::Subagent && agent.name == "default" {
                return Some(agent);
            }
        }
        // 备选第一个 primary / all
        self.agents
            .values()
            .find(|&agent| agent.mode != AgentMode::Subagent)
    }

    /// 列出所有 agent
    pub fn list(&self) -> Vec<&AgentDefinition> {
        self.agents.values().collect()
    }

    /// 列出所有 subagent（mode == Subagent 或 All）
    pub fn list_subagents(&self) -> Vec<&AgentDefinition> {
        self.agents
            .values()
            .filter(|a| a.mode == AgentMode::Subagent || a.mode == AgentMode::All)
            .collect()
    }
}

impl Default for AgentRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_agent(name: &str, mode: AgentMode) -> AgentDefinition {
        AgentDefinition {
            name: name.to_string(),
            description: String::new(),
            mode,
            model: None,
            temperature: None,
            steps: None,
            permission: vec![],
            system_prompt: String::new(),
        }
    }

    #[test]
    fn test_register_and_get() {
        let mut reg = AgentRegistry::new();
        reg.register(make_agent("code-review", AgentMode::Subagent))
            .unwrap();
        let agent = reg.get("code-review");
        assert!(agent.is_some());
        assert_eq!(agent.unwrap().name, "code-review");
    }

    #[test]
    fn test_register_duplicate_returns_err() {
        let mut reg = AgentRegistry::new();
        reg.register(make_agent("test", AgentMode::Primary))
            .unwrap();
        let result = reg.register(make_agent("test", AgentMode::Primary));
        assert!(result.is_err());
    }

    #[test]
    fn test_get_unknown_returns_none() {
        let reg = AgentRegistry::new();
        assert!(reg.get("nonexistent").is_none());
    }

    #[test]
    fn test_default_prefers_named_default() {
        let mut reg = AgentRegistry::new();
        reg.register(make_agent("other", AgentMode::Primary))
            .unwrap();
        reg.register(make_agent("default", AgentMode::Primary))
            .unwrap();
        let def = reg.default().unwrap();
        assert_eq!(def.name, "default");
    }

    #[test]
    fn test_default_falls_back_to_first_primary() {
        let mut reg = AgentRegistry::new();
        reg.register(make_agent("alpha", AgentMode::Primary))
            .unwrap();
        reg.register(make_agent("beta", AgentMode::Subagent))
            .unwrap();
        let def = reg.default().unwrap();
        assert_eq!(def.name, "alpha");
    }

    #[test]
    fn test_default_returns_none_when_all_subagent() {
        let mut reg = AgentRegistry::new();
        reg.register(make_agent("s1", AgentMode::Subagent)).unwrap();
        reg.register(make_agent("s2", AgentMode::Subagent)).unwrap();
        assert!(reg.default().is_none());
    }

    #[test]
    fn test_list_returns_all() {
        let mut reg = AgentRegistry::new();
        reg.register(make_agent("a", AgentMode::Primary)).unwrap();
        reg.register(make_agent("b", AgentMode::Subagent)).unwrap();
        assert_eq!(reg.list().len(), 2);
    }

    #[test]
    fn test_list_subagents_only() {
        let mut reg = AgentRegistry::new();
        reg.register(make_agent("general", AgentMode::Primary))
            .unwrap();
        reg.register(make_agent("reviewer", AgentMode::Subagent))
            .unwrap();
        reg.register(make_agent("both", AgentMode::All)).unwrap();
        let subs = reg.list_subagents();
        assert_eq!(subs.len(), 2);
        assert!(
            subs.iter()
                .all(|a| a.name == "reviewer" || a.name == "both")
        );
    }
}
