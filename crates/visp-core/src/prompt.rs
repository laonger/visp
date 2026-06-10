use std::path::Path;

use crate::message::Message;
use crate::message::Role;

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

impl PromptBuilder {
    pub fn build(
        system_template: &str,
        rules: &str,
        history: &[Message],
        working_dir: &Path,
        date_str: &str,
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

        let mut messages = vec![Message::system(system_content)];
        messages.extend(history.iter().filter(|m| !m.skip_context).cloned());
        messages
    }
}

pub fn user_query_instruction() -> &'static str {
    USER_QUERY_INSTRUCTION
}

pub const TOOL_OUTPUT_MAX_CHARS: usize = 2_000;

/// 估算消息列表中每条消息在 prompt 中的实际 token 数
/// - 非 Tool 消息：直接返回 msg.estimated_tokens
/// - Tool 消息超过 TOOL_OUTPUT_MAX_CHARS：按截断后长度估算
pub fn estimate_message_tokens_for_prompt(msg: &Message) -> u32 {
    if msg.role == Role::Tool && msg.content.chars().count() > TOOL_OUTPUT_MAX_CHARS {
        ((TOOL_OUTPUT_MAX_CHARS as f64 / 4.0).ceil() as u32) + 1
    } else {
        msg.estimated_tokens
    }
}

/// 批量版本
pub fn estimate_messages_tokens_for_prompt(messages: &[Message]) -> u32 {
    messages
        .iter()
        .map(estimate_message_tokens_for_prompt)
        .sum()
}

/// max_context_tokens 是 effective limit（含预留）
/// 只减去 max(output_tokens, 4000) 作为输出保留空间
pub fn calculate_available(max_context_tokens: u32, output_tokens: u32) -> u32 {
    max_context_tokens.saturating_sub(output_tokens.max(4_000))
}

/// 截断工具输出到 TOOL_OUTPUT_MAX_CHARS 字符（按字符计数，不是字节）
/// 使用 chars().take() 保证 Unicode 安全
pub fn truncate_tool_output(content: &str) -> String {
    if content.chars().count() <= TOOL_OUTPUT_MAX_CHARS {
        content.to_string()
    } else {
        let truncated: String = content.chars().take(TOOL_OUTPUT_MAX_CHARS).collect();
        format!("{}... (truncated)", truncated)
    }
}

/// 返回第 n+1 轮起点索引（轮次由 User 消息位置识别）
/// 如果不足 n 轮 User 消息，返回 history.len()
pub fn find_head_end(history: &[Message], n: usize) -> usize {
    let user_positions: Vec<usize> = history
        .iter()
        .enumerate()
        .filter(|(_, m)| m.role == Role::User)
        .map(|(i, _)| i)
        .collect();

    if user_positions.len() <= n {
        return history.len();
    }
    user_positions[n]
}

/// 返回倒数第 n 轮起点索引（轮次由 User 消息位置识别）
/// 如果不足 n 轮，返回 0
pub fn find_tail_start(history: &[Message], n: usize) -> usize {
    let user_positions: Vec<usize> = history
        .iter()
        .enumerate()
        .filter(|(_, m)| m.role == Role::User)
        .map(|(i, _)| i)
        .collect();

    let len = user_positions.len();
    if len < n {
        return 0;
    }
    user_positions[len - n]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::Role;
    use std::path::Path;

    #[test]
    fn test_system_message() {
        let messages = PromptBuilder::build(
            "You are helpful",
            "Be concise",
            &[],
            Path::new("/tmp"),
            "2026-06-09",
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
        let messages =
            PromptBuilder::build("You are helpful", "", &[], Path::new("/tmp"), "2026-06-09");
        assert_eq!(messages.len(), 1);
        assert!(messages[0].content.starts_with("You are helpful"));
        assert!(messages[0].content.contains("[USER_QUERY]"));
    }

    #[test]
    fn test_empty_template() {
        let messages = PromptBuilder::build("", "Be concise", &[], Path::new("/tmp"), "2026-06-09");
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
        let messages = PromptBuilder::build("", "", &[], Path::new("/tmp"), "2026-06-09");
        assert_eq!(messages.len(), 1);
        assert!(messages[0].content.contains("[USER_QUERY]"));
    }

    #[test]
    fn test_skip_context_messages_filtered() {
        let history = vec![
            Message::user("Hello"),
            Message {
                skip_context: true,
                ..Message::user("skip me")
            },
            Message::assistant("Hi!"),
        ];
        let messages =
            PromptBuilder::build("system", "", &history, Path::new("/tmp"), "2026-06-09");
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

    // ---- 3a: Token 估算 + 预算 + 截断 ----

    #[test]
    fn test_estimate_msg_tokens_for_prompt_user() {
        let msg = Message::user("hello world");
        assert_eq!(
            estimate_message_tokens_for_prompt(&msg),
            msg.estimated_tokens
        );
    }

    #[test]
    fn test_estimate_msg_tokens_for_prompt_short_tool() {
        let msg = Message::tool("short output", "call_1");
        assert_eq!(
            estimate_message_tokens_for_prompt(&msg),
            msg.estimated_tokens
        );
    }

    #[test]
    fn test_estimate_msg_tokens_for_prompt_long_tool() {
        let long = "a".repeat(2500);
        let msg = Message::tool(&long, "id");
        assert_eq!(estimate_message_tokens_for_prompt(&msg), 501);
    }

    #[test]
    fn test_estimate_messages_tokens_for_prompt_empty() {
        assert_eq!(estimate_messages_tokens_for_prompt(&[]), 0);
    }

    #[test]
    fn test_calculate_available_standard() {
        assert_eq!(calculate_available(128_000, 4_000), 124_000);
    }

    #[test]
    fn test_calculate_available_high_output() {
        assert_eq!(calculate_available(128_000, 8_000), 120_000);
    }

    #[test]
    fn test_truncate_tool_output_short() {
        assert_eq!(truncate_tool_output("short"), "short");
    }

    #[test]
    fn test_truncate_tool_output_long() {
        let long = "x".repeat(3000);
        let result = truncate_tool_output(&long);
        let expected_prefix: String = "x".repeat(2000);
        assert!(result.starts_with(&expected_prefix));
        assert!(result.len() > 2000);
        assert!(result.contains("truncated"));
    }

    #[test]
    fn test_truncate_tool_output_unicode() {
        let chinese = "你好".repeat(1500); // 3000 chars
        let result = truncate_tool_output(&chinese);
        let expected_prefix: String = "你好".repeat(1000); // 2000 chars
        assert!(result.starts_with(&expected_prefix));
        assert!(result.contains("truncated"));
    }

    // ---- 3b: 边界函数 ----

    #[test]
    fn test_find_head_end_two_turns_n1() {
        let history = vec![
            Message::user("q1"),
            Message::assistant("a1"),
            Message::user("q2"),
            Message::assistant("a2"),
        ];
        assert_eq!(find_head_end(&history, 1), 2);
    }

    #[test]
    fn test_find_head_end_not_enough_turns() {
        let history = vec![Message::user("q1"), Message::assistant("a1")];
        assert_eq!(find_head_end(&history, 5), 2);
    }

    #[test]
    fn test_find_head_end_empty() {
        let history: Vec<Message> = vec![];
        assert_eq!(find_head_end(&history, 2), 0);
    }

    #[test]
    fn test_find_tail_start_two_turns_n2() {
        let history = vec![
            Message::user("q1"),
            Message::assistant("a1"),
            Message::user("q2"),
            Message::assistant("a2"),
        ];
        // 总共 2 轮，倒数第 2 轮即第 1 轮起点
        assert_eq!(find_tail_start(&history, 2), 0);
    }

    #[test]
    fn test_find_tail_start_empty() {
        let history: Vec<Message> = vec![];
        assert_eq!(find_tail_start(&history, 2), 0);
    }
}
