//! 内置技能定义

/// 单个内置技能的数据结构
pub struct BuiltinSkill {
    pub name: &'static str,
    pub description: &'static str,
    pub content: &'static str,
}

/// 返回所有内置技能
pub fn builtin_skills() -> &'static [BuiltinSkill] {
    &[BuiltinSkill {
        name: "delegation-workflow",
        description: "Prefer this workflow for code exploration and implementation tasks.",
        content: concat!(
            "## When to Delegate (PREFERRED)\n",
            "\n",
            "**Prefer delegation over doing it yourself.** Delegate when the task involves:\n",
            "\n",
            "- Searching or exploring the codebase → `explorer`\n",
            "- Reading and understanding unfamiliar code → `code_reader`\n",
            "- Making code changes (edit, write, refactor, fix) → `fixer`\n",
            "\n",
            "## When NOT to Delegate\n",
            "\n",
            "- Reading a single file with a known path → use `read_file` directly\n",
            "- Running build/test/git commands → use `bash` directly\n",
            "- Quick symbol lookup → use `codegraph_*` tools directly\n",
            "\n",
            "## Delegation Workflow\n",
            "\n",
            "1. **Explore**: delegate to `explorer` to gather context\n",
            "2. **Implement**: delegate to `fixer` with the gathered context\n",
            "3. **Review**: verify sub-agent results before proceeding\n",
        ),
    }]
}

/// 按名称查找内置技能
pub fn find_builtin_skill(name: &str) -> Option<&'static BuiltinSkill> {
    builtin_skills().iter().find(|s| s.name == name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_builtin_skills_non_empty() {
        let skills = builtin_skills();
        assert!(!skills.is_empty());
    }

    #[test]
    fn test_builtin_skills_have_name_and_content() {
        for skill in builtin_skills() {
            assert!(!skill.name.is_empty());
            assert!(!skill.content.is_empty());
        }
    }

    #[test]
    fn test_find_builtin_skill_exists() {
        let skill = find_builtin_skill("delegation-workflow");
        assert!(skill.is_some());
        assert_eq!(skill.unwrap().name, "delegation-workflow");
    }

    #[test]
    fn test_find_builtin_skill_not_found() {
        assert!(find_builtin_skill("nonexistent-skill").is_none());
    }

    #[test]
    fn test_builtin_skill_content_contains_workflow() {
        let skill = find_builtin_skill("delegation-workflow").unwrap();
        assert!(skill.content.contains("PREFERRED"));
        assert!(skill.content.contains("Delegation Workflow"));
    }
}
