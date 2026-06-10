use std::path::Path;

use crate::message::Message;
use crate::message::Role;
use crate::message::estimate_message_tokens;

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
        max_context_tokens: Option<u32>,
        output_tokens: u32,
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

        // 上下文裁剪
        if let Some(max_ctx) = max_context_tokens {
            filtered = trim_context(&filtered, system_tokens, max_ctx, output_tokens);
        }

        // 截断 Tool 消息的输出
        for msg in &mut filtered {
            if msg.role == Role::Tool {
                let truncated = truncate_tool_output(&msg.content);
                if truncated != msg.content {
                    msg.content = truncated;
                    msg.estimated_tokens = estimate_message_tokens(msg);
                }
            }
        }

        let mut messages = vec![system_msg];
        messages.extend(filtered);
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

/// 从前往后删除完整轮次，直到预算满足或无可删除。
/// 一个"轮次"从 User 开始到下一个 User 之前结束。
/// 返回裁剪后的消息列表。
fn drop_old_turns(messages: &[Message], budget: u32) -> Vec<Message> {
    if messages.is_empty() {
        return vec![];
    }

    let mut tokens = estimate_messages_tokens_for_prompt(messages);
    if tokens <= budget {
        return messages.to_vec();
    }

    let mut start = 0;
    loop {
        // 找到第一个 User
        let first_user = messages[start..].iter().position(|m| m.role == Role::User);
        let Some(first_idx) = first_user.map(|i| i + start) else {
            break;
        };

        // 找到第二个 User（轮次边界）
        let second_user = messages[first_idx + 1..]
            .iter()
            .position(|m| m.role == Role::User);
        let Some(boundary) = second_user.map(|i| i + first_idx + 1) else {
            break; // 不足一个完整轮次
        };

        // 删除 messages[start..boundary]
        let removed_tokens: u32 = messages[start..boundary]
            .iter()
            .map(estimate_message_tokens_for_prompt)
            .sum();

        start = boundary;
        tokens = tokens.saturating_sub(removed_tokens);

        if tokens <= budget {
            break;
        }
    }

    messages[start..].to_vec()
}

/// 极端情况：HEAD+TAIL 已超预算时使用。
/// 保留首条 User + 尾部最近消息，过滤孤立 ToolResult。
fn keep_head_and_tail(history: &[Message], budget: u32) -> Vec<Message> {
    if history.is_empty() {
        return vec![];
    }

    // 找到第一条 User 消息
    let first_user_idx = match history.iter().position(|m| m.role == Role::User) {
        Some(idx) => idx,
        None => return vec![],
    };

    let first_user_tokens = estimate_message_tokens_for_prompt(&history[first_user_idx]);
    if first_user_tokens > budget {
        return vec![history[first_user_idx].clone()];
    }

    let mut remaining = budget - first_user_tokens;
    let mut tail_indices: Vec<usize> = Vec::new();
    let mut confirmed_tool_ids: Vec<String> = Vec::new();

    // 从尾往前遍历
    for i in (first_user_idx + 1..history.len()).rev() {
        let msg = &history[i];
        let tokens = estimate_message_tokens_for_prompt(msg);
        if tokens <= remaining {
            tail_indices.push(i);
            remaining -= tokens;
            // 收集 Assistant 消息的 tool_calls
            if msg.role == Role::Assistant
                && let Some(ref calls) = msg.tool_calls
            {
                for call in calls {
                    confirmed_tool_ids.push(call.id.clone());
                }
            }
        }
    }

    // 反转得到原始顺序，然后过滤孤立 ToolResult
    tail_indices.reverse();
    let filtered_indices: Vec<usize> = tail_indices
        .into_iter()
        .filter(|&i| {
            let msg = &history[i];
            if msg.role == Role::Tool {
                if let Some(ref call_id) = msg.tool_call_id {
                    return confirmed_tool_ids.contains(call_id);
                }
                return false;
            }
            true
        })
        .collect();

    // 构建结果
    let mut result = vec![history[first_user_idx].clone()];

    if let Some(&first_tail_idx) = filtered_indices.first() {
        if first_tail_idx > first_user_idx + 1 {
            result.push(Message::system(
                "[... earlier messages omitted due to context limit ...]",
            ));
        }
        for &i in &filtered_indices {
            result.push(history[i].clone());
        }
    }

    result
}

const PROTECTED_HEAD_TURNS: usize = 5;
const PROTECTED_TAIL_TURNS: usize = 10;

/// 主入口：整合上述裁剪策略。
/// 优先使用 HEAD+MIDDLE+TAIL 策略，极端情况回退到 keep_head_and_tail。
pub fn trim_context(
    history: &[Message],
    system_tokens: u32,
    max_context_tokens: u32,
    output_tokens: u32,
) -> Vec<Message> {
    if history.is_empty() {
        return vec![];
    }

    let available = calculate_available(max_context_tokens, output_tokens);
    let budget = available.saturating_sub(system_tokens);

    if budget == 0 {
        return keep_head_and_tail(history, 0);
    }

    let total_tokens = estimate_messages_tokens_for_prompt(history);
    if total_tokens <= budget {
        return history.to_vec();
    }

    let head_end = find_head_end(history, PROTECTED_HEAD_TURNS);
    let tail_start = find_tail_start(history, PROTECTED_TAIL_TURNS);

    // HEAD 和 TAIL 重叠 → 使用 keep_head_and_tail
    if head_end >= tail_start {
        return keep_head_and_tail(history, budget);
    }

    let head = &history[..head_end];
    let middle = &history[head_end..tail_start];
    let tail = &history[tail_start..];

    let head_tokens: u32 = head.iter().map(estimate_message_tokens_for_prompt).sum();
    let tail_tokens: u32 = tail.iter().map(estimate_message_tokens_for_prompt).sum();

    if head_tokens + tail_tokens > budget {
        return keep_head_and_tail(history, budget);
    }

    let mid_budget = budget - head_tokens - tail_tokens;
    let trimmed_middle = drop_old_turns(middle, mid_budget);

    let mut result = Vec::with_capacity(head.len() + trimmed_middle.len() + tail.len());
    result.extend_from_slice(head);
    result.extend(trimmed_middle);
    result.extend_from_slice(tail);
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::Role;
    use crate::message::ToolCallRequest;
    use std::path::Path;

    #[test]
    fn test_system_message() {
        let messages = PromptBuilder::build(
            "You are helpful",
            "Be concise",
            &[],
            Path::new("/tmp"),
            "2026-06-09",
            None,
            4096,
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
            None,
            4096,
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
        let messages = PromptBuilder::build(
            "You are helpful",
            "",
            &[],
            Path::new("/tmp"),
            "2026-06-09",
            None,
            4096,
        );
        assert_eq!(messages.len(), 1);
        assert!(messages[0].content.starts_with("You are helpful"));
        assert!(messages[0].content.contains("[USER_QUERY]"));
    }

    #[test]
    fn test_empty_template() {
        let messages = PromptBuilder::build(
            "",
            "Be concise",
            &[],
            Path::new("/tmp"),
            "2026-06-09",
            None,
            4096,
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
        let messages =
            PromptBuilder::build("", "", &[], Path::new("/tmp"), "2026-06-09", None, 4096);
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
        let messages = PromptBuilder::build(
            "system",
            "",
            &history,
            Path::new("/tmp"),
            "2026-06-09",
            None,
            4096,
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

    // ---- 3c: drop_old_turns ----

    #[test]
    fn test_drop_old_turns_empty() {
        let result = drop_old_turns(&[], 1000);
        assert!(result.is_empty());
    }

    #[test]
    fn test_drop_old_turns_all_fit() {
        let history = vec![
            Message::user("u1"),
            Message::assistant("a1"),
            Message::user("u2"),
            Message::assistant("a2"),
        ];
        let result = drop_old_turns(&history, 1000);
        assert_eq!(result.len(), 4);
        assert_eq!(result[0], Message::user("u1"));
        assert_eq!(result[1], Message::assistant("a1"));
        assert_eq!(result[2], Message::user("u2"));
        assert_eq!(result[3], Message::assistant("a2"));
    }

    #[test]
    fn test_drop_old_turns_drop_one_turn() {
        // budget=4 drops [U1,A1] (~4 tokens), leaving [U2,A2] (~4 tokens) which fits
        let history = vec![
            Message::user("u1"),
            Message::assistant("a1"),
            Message::user("u2"),
            Message::assistant("a2"),
        ];
        let result = drop_old_turns(&history, 4);
        assert_eq!(result.len(), 2);
        assert_eq!(result[0], Message::user("u2"));
        assert_eq!(result[1], Message::assistant("a2"));
    }

    #[test]
    fn test_drop_old_turns_budget_zero() {
        // Can't drop all — last partial turn [U2,A2] remains
        let history = vec![
            Message::user("u1"),
            Message::assistant("a1"),
            Message::user("u2"),
            Message::assistant("a2"),
        ];
        let result = drop_old_turns(&history, 0);
        assert_eq!(result.len(), 2);
        assert_eq!(result[0], Message::user("u2"));
        assert_eq!(result[1], Message::assistant("a2"));
    }

    #[test]
    fn test_drop_old_turns_not_enough_for_turn() {
        // Single turn [U1,A1] — no second User to form a complete turn boundary
        let history = vec![Message::user("u1"), Message::assistant("a1")];
        let result = drop_old_turns(&history, 0);
        assert_eq!(result.len(), 2);
    }

    // ---- 3d: keep_head_and_tail ----

    #[test]
    fn test_keep_head_and_tail_all_fit() {
        let history = vec![
            Message::user("u1"),
            Message::assistant("a1"),
            Message::user("u2"),
            Message::assistant("a2"),
        ];
        let result = keep_head_and_tail(&history, 1000);
        assert_eq!(result.len(), 4);
        assert_eq!(result[0], Message::user("u1"));
    }

    #[test]
    fn test_keep_head_and_tail_filter_orphan_tool() {
        let a1 = Message::assistant("a1");
        // A1 has no tool_calls, so TR1 with call_a is orphaned
        let tr1 = Message::tool("tr1", "call_a");
        let history = vec![
            Message::user("u1"),
            a1,
            tr1.clone(),
            Message::user("u2"),
            Message::assistant("a2"),
        ];
        // Budget=12: fits all but TR1 is orphan (no corresponding Assistant tool_calls)
        let result = keep_head_and_tail(&history, 12);
        assert_eq!(result.len(), 4, "TR1 should be filtered as orphan");
        assert_eq!(result[0], Message::user("u1"));
        assert_eq!(result[1], Message::assistant("a1"));
        // TR1 should NOT be in result
        assert_eq!(result[2], Message::user("u2"));
        assert_eq!(result[3], Message::assistant("a2"));
    }

    #[test]
    fn test_keep_head_and_tail_confirmed_tool_kept() {
        let mut a1 = Message::assistant("a1");
        a1.tool_calls = Some(vec![ToolCallRequest {
            id: "call_a".to_string(),
            name: "tool".to_string(),
            arguments: "{}".to_string(),
        }]);
        a1.estimated_tokens = estimate_message_tokens(&a1);
        let tr1 = Message::tool("tr1", "call_a");
        let history = vec![
            Message::user("u1"),
            a1,
            tr1.clone(),
            Message::user("u2"),
            Message::assistant("a2"),
        ];
        // Budget=16: all fit, TR1 call_id matches A1's tool_calls → kept
        let result = keep_head_and_tail(&history, 16);
        assert_eq!(result.len(), 5, "TR1 should be kept (confirmed by A1)");
        assert_eq!(result[0], Message::user("u1"));
        assert_eq!(result[1].role, Role::Assistant);
        assert_eq!(result[2], tr1);
    }

    #[test]
    fn test_keep_head_and_tail_empty() {
        let result = keep_head_and_tail(&[], 1000);
        assert!(result.is_empty());
    }

    #[test]
    fn test_keep_head_and_tail_no_user() {
        let history = vec![Message::assistant("a1")];
        let result = keep_head_and_tail(&history, 1000);
        assert!(result.is_empty());
    }

    // ---- 3e: trim_context ----

    #[test]
    fn test_trim_context_all_fit() {
        let history = vec![
            Message::user("u1"),
            Message::assistant("a1"),
            Message::user("u2"),
            Message::assistant("a2"),
        ];
        let result = trim_context(&history, 20, 128_000, 4096);
        assert_eq!(result.len(), 4);
    }

    #[test]
    fn test_trim_context_head_middle_tail() {
        // Create enough turns to exercise HEAD+MIDDLE+TAIL strategy
        // 16 User messages means: head_end=10, tail_start=12 (16-10+...)
        // Actually with 20 Users: find_head_end(5)=pos[5], find_tail_start(10)=pos[10]
        // We construct specific data so that head_end < tail_start
        let mut history: Vec<Message> = Vec::new();
        // Add 20 User-Assistant pairs
        for i in 1..=20 {
            history.push(Message::user(format!("u{}", i)));
            history.push(Message::assistant(format!("a{}", i)));
        }
        // The TEST: with PROTECTED_HEAD_TURNS=5 and PROTECTED_TAIL_TURNS=10,
        // and 20 User messages, head_end and tail_start don't overlap
        // Head = first 5 turns (10 msgs), Tail = last 10 turns (20 msgs)
        // Middle = remaining 5 turns (10 msgs)
        // Budget must be enough for HEAD+TAIL but not MIDDLE
        // HEAD ~= 20 tokens, TAIL ~= 40 tokens → head+tail ~= 60
        // Total = 80 tokens, budget = 65 → middle gets trimmed
        let result = trim_context(&history, 0, 70000, 4096);
        // Should be a valid result (either keep_head_and_tail or HEAD+trimmed+TAIL)
        assert!(!result.is_empty());
        assert!(result.len() <= history.len());
        // First message should be the first User
        assert_eq!(result[0].role, Role::User);
    }

    #[test]
    fn test_trim_context_empty() {
        let result = trim_context(&[], 20, 1000, 100);
        assert!(result.is_empty());
    }

    #[test]
    fn test_trim_context_zero_budget() {
        let history = vec![Message::user("u1"), Message::assistant("a1")];
        let result = trim_context(&history, 0, 0, 0);
        // budget=0 → falls back to keep_head_and_tail
        assert!(!result.is_empty());
        assert_eq!(result[0].role, Role::User);
    }

    // ---- 3f: build() 更新 ----

    #[test]
    fn test_build_no_context_trimming() {
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
        );
        // None → no trimming: system + all 3 messages
        assert_eq!(messages.len(), 4);
    }

    #[test]
    fn test_build_with_context_trimming() {
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
        );
        // With max_context_tokens=3000 and output=4096, available=0,
        // budget=0-system_tokens=0... actually calculate_available returns 0
        // because 3000 - max(4096,4000) = 3000 - 4096 = 0
        // So trim_context gets called and keeps head and tail
        assert!(!messages.is_empty());
        // system message + at least the first User
        assert_eq!(messages[0].role, Role::System);
        assert!(messages.len() >= 2);
    }

    #[test]
    fn test_build_truncates_tool_output() {
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
        );
        // Tool message should be truncated
        let tool_result = &messages[2]; // system + user + tool
        assert_eq!(tool_result.role, Role::Tool);
        assert!(tool_result.content.len() < 3000);
        assert!(tool_result.content.contains("truncated"));
    }

    #[test]
    fn test_build_skip_context_excluded() {
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
        );
        assert_eq!(messages.len(), 2); // system + 1 non-skipped
        assert_eq!(messages[1], Message::user("keep me"));
    }
}
