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
- The selection UI lets the user navigate with arrow keys and confirm with Enter\n\
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
}
