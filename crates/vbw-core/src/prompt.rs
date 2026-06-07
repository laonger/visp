use crate::message::Message;

pub struct PromptBuilder;

impl PromptBuilder {
    pub fn build(system_template: &str, rules: &str, history: &[Message]) -> Vec<Message> {
        let system_content = match (system_template.is_empty(), rules.is_empty()) {
            (true, true) => String::new(),
            (true, false) => rules.to_string(),
            (false, true) => system_template.to_string(),
            (false, false) => format!("{system_template}\n\n{rules}"),
        };

        let mut messages = vec![Message::system(system_content)];
        messages.extend(history.iter().filter(|m| !m.skip_context).cloned());
        messages
    }
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
        assert_eq!(messages[0].content, "You are helpful\n\nBe concise");
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
        assert_eq!(messages[1], Message::user("Hello"));
        assert_eq!(messages[2], Message::assistant("Hi!"));
        assert_eq!(messages[3], Message::user("How are you?"));
    }

    #[test]
    fn test_empty_rules() {
        let messages = PromptBuilder::build("You are helpful", "", &[]);
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].content, "You are helpful");
    }

    #[test]
    fn test_empty_template() {
        let messages = PromptBuilder::build("", "Be concise", &[]);
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].content, "Be concise");
    }

    #[test]
    fn test_skip_context_messages_filtered() {
        let history = vec![
            Message::user("Hello"),
            Message { skip_context: true, ..Message::user("skip me") },
            Message::assistant("Hi!"),
        ];
        let messages = PromptBuilder::build("system", "", &history);
        assert_eq!(messages.len(), 3); // system + 2 non-skipped
        assert_eq!(messages[1], Message::user("Hello"));
        assert_eq!(messages[2], Message::assistant("Hi!"));
    }
}
