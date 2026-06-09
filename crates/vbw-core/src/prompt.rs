use crate::message::Message;

pub struct PromptBuilder;

const USER_QUERY_INSTRUCTION: &str = "\n\nWhen you need the user to make a choice, append the following at the end of your response:\n[USER_QUERY]\nYour question here\n- Option A description\n- Option B description\n[/USER_QUERY]\n\nUse [USER_QUERY allow_other=true] to allow custom input.";

impl PromptBuilder {
    pub fn build(system_template: &str, rules: &str, history: &[Message]) -> Vec<Message> {
        let system_content = match (system_template.is_empty(), rules.is_empty()) {
            (true, true) => USER_QUERY_INSTRUCTION.trim_start().to_string(),
            (true, false) => format!("{rules}{USER_QUERY_INSTRUCTION}"),
            (false, true) => format!("{system_template}{USER_QUERY_INSTRUCTION}"),
            (false, false) => {
                format!("{system_template}\n\n{rules}{USER_QUERY_INSTRUCTION}")
            }
        };

        let mut messages = vec![Message::system(system_content)];
        messages.extend(history.iter().filter(|m| !m.skip_context).cloned());
        messages
    }
}

pub fn user_query_instruction() -> &'static str {
    USER_QUERY_INSTRUCTION
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::Role;

    #[test]
    fn test_system_message() {
        let messages = PromptBuilder::build("You are helpful", "Be concise", &[]);
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].role, Role::System);
        assert!(
            messages[0]
                .content
                .starts_with("You are helpful\n\nBe concise")
        );
        assert!(messages[0].content.contains("[USER_QUERY]"));
        assert!(messages[0].content.contains("allow_other=true"));
    }

    #[test]
    fn test_history_order() {
        let history = vec![
            Message::user("Hello"),
            Message::assistant("Hi!"),
            Message::user("How are you?"),
        ];
        let messages = PromptBuilder::build("You are helpful", "Be concise", &history);
        assert_eq!(messages.len(), 4);
        assert_eq!(messages[0].role, Role::System);
        assert!(
            messages[0]
                .content
                .starts_with("You are helpful\n\nBe concise")
        );
        assert_eq!(messages[1], Message::user("Hello"));
        assert_eq!(messages[2], Message::assistant("Hi!"));
        assert_eq!(messages[3], Message::user("How are you?"));
    }

    #[test]
    fn test_empty_rules() {
        let messages = PromptBuilder::build("You are helpful", "", &[]);
        assert_eq!(messages.len(), 1);
        assert!(messages[0].content.starts_with("You are helpful"));
        assert!(messages[0].content.contains("[USER_QUERY]"));
    }

    #[test]
    fn test_empty_template() {
        let messages = PromptBuilder::build("", "Be concise", &[]);
        assert_eq!(messages.len(), 1);
        assert!(messages[0].content.starts_with("Be concise"));
        assert!(messages[0].content.contains("[USER_QUERY]"));
    }

    #[test]
    fn test_empty_both() {
        let messages = PromptBuilder::build("", "", &[]);
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
        let messages = PromptBuilder::build("system", "", &history);
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
    }
}
