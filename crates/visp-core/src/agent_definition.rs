/// Agent 模式
#[derive(Debug, Clone, PartialEq)]
pub enum AgentMode {
    /// 主 Agent，用户可直接选择
    Primary,
    /// 子 Agent，仅通过 task 工具调用
    Subagent,
    /// 两者皆可
    All,
}

/// 权限动作
#[derive(Debug, Clone, PartialEq)]
pub enum PermissionAction {
    Allow,
    Deny,
}

/// 权限规则三元组
#[derive(Debug, Clone, PartialEq)]
pub struct PermissionRule {
    /// 工具名，如 "edit"，"*" 表示所有工具
    pub permission: String,
    /// 参数路径 glob，如 "*" 表示所有路径
    pub pattern: String,
    /// 允许或拒绝
    pub action: PermissionAction,
}

/// Agent 静态定义
#[derive(Debug, Clone)]
pub struct AgentDefinition {
    pub name: String,
    pub description: String,
    pub mode: AgentMode,
    /// 使用的模型 key（可选，未指定则继承 session 默认模型）
    pub model: Option<String>,
    pub temperature: Option<f32>,
    /// 最大 agentic 迭代次数
    pub steps: Option<u32>,
    /// 权限规则集
    pub permission: Vec<PermissionRule>,
    /// 系统提示词
    pub system_prompt: String,
}
