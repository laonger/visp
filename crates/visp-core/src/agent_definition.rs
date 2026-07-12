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
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum PermissionAction {
    Allow,
    Deny,
}

/// 权限规则三元组
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
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
    /// 允许调用的子 Agent 名称列表（空列表表示不限制，仅指业务逻辑层面）
    pub allowed_sub_agents: Vec<String>,
    /// 系统提示词
    pub system_prompt: String,
}

/// 权限检查结果
#[derive(Debug, Clone, PartialEq)]
pub enum PermissionDecision {
    Allowed,
    Denied(String),
}

/// 合并三组权限规则：父 session deny → 父 agent deny → 子 agent 规则 → 兜底 `*: deny`
pub fn merge_permissions(
    parent_session_permission: &[PermissionRule],
    parent_agent_permission: &[PermissionRule],
    subagent_permission: &[PermissionRule],
) -> Vec<PermissionRule> {
    let mut result = Vec::new();

    // 1. 父 session 的 deny 规则
    for rule in parent_session_permission {
        if rule.action == PermissionAction::Deny {
            result.push(rule.clone());
        }
    }

    // 2. 父 agent 的 deny 规则
    for rule in parent_agent_permission {
        if rule.action == PermissionAction::Deny {
            result.push(rule.clone());
        }
    }

    // 3. 子 agent 的所有规则
    result.extend(subagent_permission.iter().cloned());

    // 4. 兜底 *: deny（如果没有显式 *: deny）
    if !result
        .iter()
        .any(|r| r.permission == "*" && r.pattern == "*" && r.action == PermissionAction::Deny)
    {
        result.push(PermissionRule {
            permission: "*".to_string(),
            pattern: "*".to_string(),
            action: PermissionAction::Deny,
        });
    }

    result
}

/// 检查工具调用是否被允许
/// 两轮匹配：先精确匹配 permission，再通配匹配 "*"
/// 无匹配时默认 Allow
pub fn check_permission(
    name: &str,
    args: &serde_json::Value,
    rules: &[PermissionRule],
) -> PermissionDecision {
    let args_str = args.to_string();

    // 第一轮：精确匹配 permission
    for rule in rules {
        if rule.permission == name && (rule.pattern == "*" || args_str.contains(&rule.pattern)) {
            match rule.action {
                PermissionAction::Allow => return PermissionDecision::Allowed,
                PermissionAction::Deny => {
                    return PermissionDecision::Denied(format!(
                        "permission denied: tool '{}' is blocked by rule '{}'",
                        name, rule.permission
                    ));
                }
            }
        }
    }

    // 第二轮：通配匹配 permission="*"
    for rule in rules {
        if rule.permission == "*" && (rule.pattern == "*" || args_str.contains(&rule.pattern)) {
            match rule.action {
                PermissionAction::Allow => return PermissionDecision::Allowed,
                PermissionAction::Deny => {
                    return PermissionDecision::Denied(format!(
                        "permission denied: tool '{}' blocked by wildcard rule",
                        name
                    ));
                }
            }
        }
    }

    // 无匹配 → 默认 Allow
    PermissionDecision::Allowed
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── merge_permissions ────────────────────────────────────────────────

    #[test]
    fn test_merge_both_empty_adds_deny_all() {
        let result = merge_permissions(&[], &[], &[]);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].permission, "*");
        assert_eq!(result[0].pattern, "*");
        assert_eq!(result[0].action, PermissionAction::Deny);
    }

    #[test]
    fn test_merge_inherits_parent_session_deny() {
        let session_rules = vec![PermissionRule {
            permission: "edit".into(),
            pattern: "*".into(),
            action: PermissionAction::Deny,
        }];
        let result = merge_permissions(&session_rules, &[], &[]);
        assert!(
            result
                .iter()
                .any(|r| r.permission == "edit" && r.action == PermissionAction::Deny)
        );
    }

    #[test]
    fn test_merge_inherits_parent_agent_deny() {
        let agent_rules = vec![PermissionRule {
            permission: "bash".into(),
            pattern: "*".into(),
            action: PermissionAction::Deny,
        }];
        let result = merge_permissions(&[], &agent_rules, &[]);
        assert!(
            result
                .iter()
                .any(|r| r.permission == "bash" && r.action == PermissionAction::Deny)
        );
    }

    #[test]
    fn test_merge_subagent_allow_overrides_deny_all() {
        let sub_rules = vec![PermissionRule {
            permission: "read".into(),
            pattern: "*".into(),
            action: PermissionAction::Allow,
        }];
        let result = merge_permissions(&[], &[], &sub_rules);
        // Should have both the allow rule and the fallback deny-all
        assert!(
            result
                .iter()
                .any(|r| r.permission == "read" && r.action == PermissionAction::Allow)
        );
        assert!(
            result
                .iter()
                .any(|r| r.permission == "*" && r.action == PermissionAction::Deny)
        );
    }

    #[test]
    fn test_merge_existing_deny_all_not_duplicated() {
        let sub_rules = vec![PermissionRule {
            permission: "*".into(),
            pattern: "*".into(),
            action: PermissionAction::Deny,
        }];
        let result = merge_permissions(&[], &[], &sub_rules);
        let deny_all_count = result
            .iter()
            .filter(|r| {
                r.permission == "*" && r.pattern == "*" && r.action == PermissionAction::Deny
            })
            .count();
        assert_eq!(deny_all_count, 1);
    }

    #[test]
    fn test_merge_explicit_deny_all_preserved() {
        let sub_rules = vec![PermissionRule {
            permission: "*".into(),
            pattern: "*".into(),
            action: PermissionAction::Deny,
        }];
        let result = merge_permissions(&[], &[], &sub_rules);
        assert!(result.iter().any(|r| r.permission == "*"
            && r.pattern == "*"
            && r.action == PermissionAction::Deny));
    }

    // ── check_permission ─────────────────────────────────────────────────

    #[test]
    fn test_check_exact_allow() {
        let rules = vec![PermissionRule {
            permission: "edit".into(),
            pattern: "*".into(),
            action: PermissionAction::Allow,
        }];
        assert_eq!(
            check_permission("edit", &serde_json::json!({}), &rules),
            PermissionDecision::Allowed
        );
    }

    #[test]
    fn test_check_exact_deny() {
        let rules = vec![PermissionRule {
            permission: "bash".into(),
            pattern: "*".into(),
            action: PermissionAction::Deny,
        }];
        assert!(matches!(
            check_permission("bash", &serde_json::json!({}), &rules),
            PermissionDecision::Denied(_)
        ));
    }

    #[test]
    fn test_check_wildcard_permission_match() {
        let rules = vec![PermissionRule {
            permission: "*".into(),
            pattern: "*".into(),
            action: PermissionAction::Allow,
        }];
        assert_eq!(
            check_permission("any_tool", &serde_json::json!({}), &rules),
            PermissionDecision::Allowed
        );
    }

    #[test]
    fn test_check_exact_match_overrides_wildcard() {
        let rules = vec![
            PermissionRule {
                permission: "*".into(),
                pattern: "*".into(),
                action: PermissionAction::Allow,
            },
            PermissionRule {
                permission: "danger".into(),
                pattern: "*".into(),
                action: PermissionAction::Deny,
            },
        ];
        assert!(matches!(
            check_permission("danger", &serde_json::json!({}), &rules),
            PermissionDecision::Denied(_)
        ));
        assert_eq!(
            check_permission("safe", &serde_json::json!({}), &rules),
            PermissionDecision::Allowed
        );
    }

    // ── allowed_sub_agents ──────────────────────────────────────────────

    #[test]
    fn test_agent_definition_allowed_sub_agents_default_empty() {
        let def = AgentDefinition {
            name: "test".to_string(),
            description: String::new(),
            mode: AgentMode::Primary,
            model: None,
            temperature: None,
            steps: None,
            permission: vec![],
            system_prompt: String::new(),
            allowed_sub_agents: Vec::new(),
        };
        assert!(def.allowed_sub_agents.is_empty());
    }

    #[test]
    fn test_agent_definition_with_allowed_sub_agents() {
        let def = AgentDefinition {
            name: "test".to_string(),
            description: String::new(),
            mode: AgentMode::Primary,
            model: None,
            temperature: None,
            steps: None,
            permission: vec![],
            system_prompt: String::new(),
            allowed_sub_agents: vec!["fixer".to_string(), "explorer".to_string()],
        };
        assert_eq!(def.allowed_sub_agents.len(), 2);
        assert!(def.allowed_sub_agents.contains(&"fixer".to_string()));
        assert!(def.allowed_sub_agents.contains(&"explorer".to_string()));
    }

    #[test]
    fn test_check_no_match_default_allow() {
        let rules = vec![PermissionRule {
            permission: "edit".into(),
            pattern: "*".into(),
            action: PermissionAction::Deny,
        }];
        assert_eq!(
            check_permission("unknown_tool", &serde_json::json!({}), &rules),
            PermissionDecision::Allowed
        );
    }
}
