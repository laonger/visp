use std::path::Path;

use crate::message::Message;

pub struct PromptBuilder;

const USER_QUERY_INSTRUCTION: &str = "\n\n## IMPORTANT: How to ask the user to choose\n\
\n\
When you need the user to make a decision, you MUST use the [USER_QUERY] marker at the very end of your response. \
Do NOT ask the user to choose using plain text — it will not trigger the selection UI.\n\
\n\
### Use regular options when the user should pick from predefined choices:\n\
\n\
[USER_QUERY]\n\
Which approach do you prefer?\n\
- SQLite\n\
- PostgreSQL\n\
[/USER_QUERY]\n\
\n\
### Use allow_other=true when you want the user to speak freely or provide custom input:\n\
\n\
[USER_QUERY allow_other=true]\n\
What's on your mind?\n\
[/USER_QUERY]\n\
\n\
Or with some suggestions:\n\
[USER_QUERY allow_other=true]\n\
How should we fix this bug?\n\
- Refactor the module\n\
- Add a quick patch\n\
[/USER_QUERY]\n\
\n\
### Rules:\n\
- Marker MUST be at the very end of your response (not in the middle)\n\
- Each option MUST be on its own line starting with `- `\n\
- Options should be REAL choices, not invitations to talk\n\
- Do NOT create \"listening\" options like \"你来说说看\" or \"I'll listen\" — use allow_other=true instead\n\
- Only use [USER_QUERY] when you genuinely need user input\n\
- `allow_other=true` adds an \"Other\" button that opens a text input field";

#[allow(clippy::too_many_arguments)]
impl PromptBuilder {
    pub fn build(
        system_template: &str,
        rules: &str,
        history: &[Message],
        working_dir: &Path,
        date_str: &str,
        max_context_tokens: Option<u32>,
        output_tokens: u32,
        trimmer: &dyn crate::context::ContextTrimmer,
    ) -> Vec<Message> {
        let mut system_content = match (system_template.is_empty(), rules.is_empty()) {
            (true, true) => USER_QUERY_INSTRUCTION.trim_start().to_string(),
            (true, false) => format!("## Instructions\n\n{rules}{USER_QUERY_INSTRUCTION}"),
            (false, true) => format!("{system_template}{USER_QUERY_INSTRUCTION}"),
            (false, false) => {
                format!("{system_template}\n\n## Instructions\n\n{rules}{USER_QUERY_INSTRUCTION}")
            }
        };

        let env_context = format!(
            "\n\n## Current Context\n\nDate: {date_str}\nWorking Directory: {}",
            working_dir.display()
        );
        system_content.push_str(&env_context);

        let system_msg = Message::system(system_content);
        let system_tokens = system_msg.estimated_tokens;

        // 过滤 skip_context 消息
        let mut filtered: Vec<Message> = history
            .iter()
            .filter(|m| !m.skip_context)
            .cloned()
            .collect();

        // 上下文裁剪（内部完成 Tool 输出截断）
        if let Some(max_ctx) = max_context_tokens {
            filtered = trimmer.trim(&filtered, max_ctx, system_tokens, output_tokens);
        }

        let mut messages = vec![system_msg];
        messages.extend(filtered);
        messages
    }
}

pub const DEFAULT_SYSTEM_PROMPT: &str = concat!(
    "You are visp, a lightweight AI coding assistant.\n",
    "\n",
    "Optimize for quality, speed, cost, and reliability by dispatching the right specialist lanes,\n",
    "tracking background task state, and integrating results into one coherent outcome.\n",
    "\n",
    "## Interaction Rules\n",
    "- Always wait for tool results; do not assume outcomes\n",
    "- Multiple tools can be called in parallel within a single reply\n",
    "- When a tool requires approval, a confirmation bar will appear (Approve / Deny / Always Allow)\n",
    "- Wait for each tool to complete before proceeding, unless tools can run in parallel\n",
    "- When you need the user to make a choice, use the [USER_QUERY] marker (see detailed instructions at end of prompt)\n",
    "\n",
    "## Task Delegation Philosophy\n",
    "\n",
    "Sub-agents are the default mechanism for code work.\n",
    "\n",
    "For any non-trivial coding request, prefer delegation over direct reasoning.\n",
    "\n",
    "Use the main agent primarily for:\n",
    "- task decomposition\n",
    "- coordination\n",
    "- result integration\n",
    "- user communication\n",
    "\n",
    "Use specialists for:\n",
    "- code discovery\n",
    "- code understanding\n",
    "- implementation\n",
    "- verification\n",
    "\n",
    "Only skip delegation when:\n",
    "- answering a purely conversational question\n",
    "- performing a tiny edit that affects a single obvious location\n",
    "- the user explicitly requests reasoning without code exploration\n",
    "\n",
    "Available sub-agents are listed in the Delegation Guidelines section.\n",
    "Provide clear, bounded task specifications.\n",
    "Sub-agents have access to the tools they need and return their results.\n",
    "\n",
    "## Workflow\n",
    "\n",
    "### 1. Understand\n",
    "Parse request: explicit requirements + implicit needs.\n",
    "\n",
    "### 2. Path Selection\n",
    "\n",
    "Coding:\n",
    "1. Single specialist\n",
    "2. Multi-specialist workflow\n",
    "3. Direct execution\n",
    "\n",
    "Delegate when:\n",
    "- code search is needed\n",
    "- code understanding is needed\n",
    "- debugging is needed\n",
    "- multiple files may change\n",
    "\n",
    "Non-coding:\n",
    "1. Direct answer\n",
    "\n",
    "### Exploration Stop Rule\n",
    "\n",
    "Gather only enough information to complete the task.\n",
    "Stop searching when:\n",
    "\n",
    "- the implementation location is identified\n",
    "- the relevant code is understood\n",
    "- a change plan can be produced\n",
    "\n",
    "Do not perform exhaustive codebase exploration.\n",
    "\n",
    "### 3. Delegation Check\n",
    "Review available sub-agents and their lane rules.\n",
    "\n",
    "**Dispatch efficiency:**\n",
    "- Reference paths/lines, don't paste files (`src/main.rs:42` not full contents)\n",
    "- Brief user on delegation goal before each call\n",
    "- Direct execution is allowed only when:\n",
    "  - the change affects a single obvious location\n",
    "  - no code discovery is required\n",
    "  - no code understanding is required\n",
    "  - delegation overhead clearly exceeds implementation effort\n",
    "- Record task IDs and state\n",
    "- Do not immediately wait after spawning independent background tasks unless the next step\n",
    "  truly depends on their result\n",
    "- Reconcile results, resolve conflicts, and gate dependent lanes\n",
    "\n",
    "### 4. Plan and Parallelize\n",
    "Build a short work graph before dispatching:\n",
    "- Independent lanes that can run now\n",
    "- Dependency-ordered lanes that must wait\n",
    "- Verification/review lanes that run after implementation\n",
    "\n",
    "### 5. Execute\n",
    "Dispatch tasks to sub-agents or execute directly.\n",
    "Track each task's specialist, objective, and file/topic ownership.\n",
    "Continue orchestration only on non-overlapping work.\n",
    "\n",
    "### 6. Verify\n",
    "- Run relevant checks/diagnostics for the change\n",
    "- Confirm specialists completed successfully\n",
    "- Verify solution meets requirements\n",
    "\n",
    "After code changes:\n",
    "\n",
    "1. Run the smallest relevant validation first\n",
    "2. Then broader validation if needed\n",
    "\n",
    "Examples:\n",
    "\n",
    "- Single function fix → targeted test\n",
    "- Module change → crate test\n",
    "- API change → affected integration tests\n",
    "\n",
    "Avoid expensive validation when a smaller check is sufficient.\n",
    "\n",
    "### Failure Handling\n",
    "\n",
    "When a delegated task fails:\n",
    "\n",
    "1. Determine whether retrying is useful\n",
    "2. Try an alternative lane if available\n",
    "3. Narrow the task scope\n",
    "4. Ask the user only if additional information is required\n",
    "\n",
    "Do not abandon the task after a single failure.\n",
    "\n",
    "## Communication\n",
    "\n",
    "### Clarity Over Assumptions\n",
    "- If request is vague or has multiple valid interpretations, ask a targeted question\n",
    "  before proceeding\n",
    "- Don't guess at critical details (file paths, API choices, architectural decisions)\n",
    "- Do make reasonable assumptions for minor details and state them briefly\n",
    "\n",
    "### Concise Execution\n",
    "- Answer directly, no preamble\n",
    "- Don't summarize what you did unless asked\n",
    "- Don't explain code unless asked\n",
    "- Brief delegation notices: \"Checking via explorer...\" not verbose explanations\n",
    "\n",
    "### No Flattery\n",
    "Never: \"Great question!\" \"Excellent idea!\" or any praise of user input.\n",
    "\n",
    "### Honest Pushback\n",
    "When user's approach seems problematic:\n",
    "- State concern + alternative concisely\n",
    "- Ask if they want to proceed anyway\n",
    "- Don't lecture, don't blindly implement\n",
    "\n",
    "## Context Budget\n",
    "\n",
    "Prefer using:\n",
    "- symbol references\n",
    "- file paths\n",
    "- summaries\n",
    "\n",
    "Avoid:\n",
    "- pasting entire files\n",
    "- repeating previous findings\n",
    "\n",
    "Keep delegated task descriptions under 300 tokens whenever possible.\n",
    "\n",
    "## Result Contract\n",
    "\n",
    "Every delegated task should return:\n",
    "\n",
    "- conclusion\n",
    "- evidence\n",
    "- confidence\n",
    "- next action\n",
);

pub fn user_query_instruction() -> &'static str {
    USER_QUERY_INSTRUCTION
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::ContextTrimmer;
    use crate::message::Role;
    use std::path::Path;
    use std::sync::atomic::AtomicU32;
    use std::sync::atomic::Ordering;

    struct MockTrimmer {
        call_count: AtomicU32,
    }

    impl MockTrimmer {
        fn new() -> Self {
            Self {
                call_count: AtomicU32::new(0),
            }
        }
    }

    impl ContextTrimmer for MockTrimmer {
        fn trim(
            &self,
            history: &[Message],
            _max_ctx: u32,
            _overhead: u32,
            _output: u32,
        ) -> Vec<Message> {
            self.call_count.fetch_add(1, Ordering::SeqCst);
            history.to_vec()
        }
    }

    #[test]
    fn test_system_message() {
        let trimmer = MockTrimmer::new();
        let messages = PromptBuilder::build(
            "You are helpful",
            "Be concise",
            &[],
            Path::new("/tmp"),
            "2026-06-09",
            None,
            4096,
            &trimmer,
        );
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].role, Role::System);
        assert!(
            messages[0]
                .content
                .starts_with("You are helpful\n\n## Instructions\n\nBe concise")
        );
        assert!(messages[0].content.contains("[USER_QUERY]"));
        assert!(messages[0].content.contains("allow_other=true"));
        assert!(messages[0].content.contains("Current Context"));
        assert!(messages[0].content.contains("/tmp"));
        assert!(messages[0].content.contains("2026-06-09"));
    }

    #[test]
    fn test_history_order() {
        let trimmer = MockTrimmer::new();
        let history = vec![
            Message::user("Hello"),
            Message::assistant("Hi!"),
            Message::user("How are you?"),
        ];
        let messages = PromptBuilder::build(
            "You are helpful",
            "Be concise",
            &history,
            Path::new("/tmp"),
            "2026-06-09",
            None,
            4096,
            &trimmer,
        );
        assert_eq!(messages.len(), 4);
        assert_eq!(messages[0].role, Role::System);
        assert!(
            messages[0]
                .content
                .starts_with("You are helpful\n\n## Instructions\n\nBe concise")
        );
        assert_eq!(messages[1], Message::user("Hello"));
        assert_eq!(messages[2], Message::assistant("Hi!"));
        assert_eq!(messages[3], Message::user("How are you?"));
    }

    #[test]
    fn test_empty_rules() {
        let trimmer = MockTrimmer::new();
        let messages = PromptBuilder::build(
            "You are helpful",
            "",
            &[],
            Path::new("/tmp"),
            "2026-06-09",
            None,
            4096,
            &trimmer,
        );
        assert_eq!(messages.len(), 1);
        assert!(messages[0].content.starts_with("You are helpful"));
        assert!(messages[0].content.contains("[USER_QUERY]"));
    }

    #[test]
    fn test_empty_template() {
        let trimmer = MockTrimmer::new();
        let messages = PromptBuilder::build(
            "",
            "Be concise",
            &[],
            Path::new("/tmp"),
            "2026-06-09",
            None,
            4096,
            &trimmer,
        );
        assert_eq!(messages.len(), 1);
        assert!(
            messages[0]
                .content
                .starts_with("## Instructions\n\nBe concise")
        );
        assert!(messages[0].content.contains("[USER_QUERY]"));
    }

    #[test]
    fn test_empty_both() {
        let trimmer = MockTrimmer::new();
        let messages = PromptBuilder::build(
            "",
            "",
            &[],
            Path::new("/tmp"),
            "2026-06-09",
            None,
            4096,
            &trimmer,
        );
        assert_eq!(messages.len(), 1);
        assert!(messages[0].content.contains("[USER_QUERY]"));
    }

    #[test]
    fn test_skip_context_messages_filtered() {
        let trimmer = MockTrimmer::new();
        let history = vec![
            Message::user("Hello"),
            Message {
                skip_context: true,
                ..Message::user("skip me")
            },
            Message::assistant("Hi!"),
        ];
        let messages = PromptBuilder::build(
            "system",
            "",
            &history,
            Path::new("/tmp"),
            "2026-06-09",
            None,
            4096,
            &trimmer,
        );
        assert_eq!(messages.len(), 3); // system + 2 non-skipped
        assert_eq!(messages[1], Message::user("Hello"));
        assert_eq!(messages[2], Message::assistant("Hi!"));
    }

    #[test]
    fn test_user_query_instruction_present() {
        let instruction = user_query_instruction();
        assert!(instruction.contains("[USER_QUERY]"));
        assert!(instruction.contains("[/USER_QUERY]"));
        assert!(instruction.contains("allow_other=true"));
        // 语气必须强硬，不能是 suggestion
        assert!(instruction.contains("MUST"));
        assert!(!instruction.contains("you can"));
    }

    #[test]
    fn test_build_no_context_trimming() {
        let trimmer = MockTrimmer::new();
        let history = vec![
            Message::user("u1"),
            Message::assistant("a1"),
            Message::user("u2"),
        ];
        let messages = PromptBuilder::build(
            "system",
            "",
            &history,
            Path::new("/tmp"),
            "2026-06-09",
            None,
            4096,
            &trimmer,
        );
        // None → no trimming: system + all 3 messages, trimmer not called
        assert_eq!(messages.len(), 4);
        assert_eq!(trimmer.call_count.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn test_build_with_context_trimming() {
        let trimmer = MockTrimmer::new();
        let history = vec![
            Message::user("u1"),
            Message::assistant("a1"),
            Message::user("u2"),
            Message::assistant("a2"),
        ];
        let messages = PromptBuilder::build(
            "system",
            "",
            &history,
            Path::new("/tmp"),
            "2026-06-09",
            Some(3000),
            4096,
            &trimmer,
        );
        // trimmer is called
        assert!(!messages.is_empty());
        assert_eq!(messages[0].role, Role::System);
        assert!(messages.len() >= 2);
        assert!(trimmer.call_count.load(Ordering::SeqCst) > 0);
    }

    #[test]
    fn test_build_passes_through_tool_output_when_no_trimming() {
        let trimmer = MockTrimmer::new();
        let long_output = "x".repeat(3000);
        let tool_msg = Message::tool(&long_output, "call_1");
        let history = vec![Message::user("u1"), tool_msg];
        let messages = PromptBuilder::build(
            "system",
            "",
            &history,
            Path::new("/tmp"),
            "2026-06-09",
            None,
            4096,
            &trimmer,
        );
        // No trimming means no truncation — tool output passes through as-is
        let tool_result = &messages[2]; // system + user + tool
        assert_eq!(tool_result.role, Role::Tool);
        assert_eq!(tool_result.content.len(), 3000);
        assert!(!tool_result.content.contains("truncated"));
        assert_eq!(trimmer.call_count.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn test_build_skip_context_excluded() {
        let trimmer = MockTrimmer::new();
        let skip_msg = Message {
            skip_context: true,
            ..Message::user("skip me")
        };
        let history = vec![Message::user("keep me"), skip_msg];
        let messages = PromptBuilder::build(
            "system",
            "",
            &history,
            Path::new("/tmp"),
            "2026-06-09",
            None,
            4096,
            &trimmer,
        );
        assert_eq!(messages.len(), 2); // system + 1 non-skipped
        assert_eq!(messages[1], Message::user("keep me"));
        assert_eq!(trimmer.call_count.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn test_default_prompt_contains_role() {
        assert!(DEFAULT_SYSTEM_PROMPT.contains("visp"));
    }

    #[test]
    fn test_default_prompt_no_project_specific_content() {
        // Coding conventions 已移到 AGENTS.md，不应出现在 DEFAULT 中
        assert!(!DEFAULT_SYSTEM_PROMPT.contains("Conventional Commits"));
        assert!(!DEFAULT_SYSTEM_PROMPT.contains("简洁优先"));
        assert!(!DEFAULT_SYSTEM_PROMPT.contains("TDD"));
    }

    #[test]
    fn test_default_prompt_contains_interaction_rules() {
        assert!(DEFAULT_SYSTEM_PROMPT.contains("[USER_QUERY]"));
        // 引用详细说明，但不包含完整格式
        assert!(DEFAULT_SYSTEM_PROMPT.contains("see detailed instructions"));
        // 通用工具规则
        assert!(DEFAULT_SYSTEM_PROMPT.contains("tool results"));
    }

    #[test]
    fn test_default_prompt_no_hardcoded_tools() {
        // 不应硬编码工具名，工具由动态指南渲染
        assert!(!DEFAULT_SYSTEM_PROMPT.contains("ReadFile"));
        assert!(!DEFAULT_SYSTEM_PROMPT.contains("Bash"));
    }
}
