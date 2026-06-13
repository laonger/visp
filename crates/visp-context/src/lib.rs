use visp_core::context::ContextTrimmer;
use visp_core::message::Message;
use visp_core::message::Role;
use visp_core::message::estimate_message_tokens;

const TOOL_OUTPUT_MAX_CHARS: usize = 2_000;
const PROTECTED_HEAD_TURNS: usize = 5;
const PROTECTED_TAIL_TURNS: usize = 10;

/// 默认的对话历史裁剪器
///
/// 使用三段式策略（HEAD + MIDDLE（drop_old_turns）+ TAIL）进行裁剪，
/// 极端情况回退到 keep_head_and_tail。
pub struct DefaultContextTrimmer {
    pub head_turns: usize,
    pub tail_turns: usize,
    pub tool_output_max_chars: usize,
}

impl Default for DefaultContextTrimmer {
    fn default() -> Self {
        Self {
            head_turns: PROTECTED_HEAD_TURNS,
            tail_turns: PROTECTED_TAIL_TURNS,
            tool_output_max_chars: TOOL_OUTPUT_MAX_CHARS,
        }
    }
}

impl ContextTrimmer for DefaultContextTrimmer {
    fn trim(
        &self,
        history: &[Message],
        max_context_tokens: u32,
        system_overhead: u32,
        output_tokens: u32,
    ) -> Vec<Message> {
        if history.is_empty() {
            return vec![];
        }

        let available = calculate_available(max_context_tokens, output_tokens);
        let budget = available.saturating_sub(system_overhead);

        let result = if budget == 0 {
            keep_head_and_tail(history, 0)
        } else {
            let total_tokens = estimate_messages_tokens_for_prompt(history);
            if total_tokens <= budget {
                history.to_vec()
            } else {
                let head_end = find_head_end(history, self.head_turns);
                let tail_start = find_tail_start(history, self.tail_turns);

                // HEAD 和 TAIL 重叠 → 使用 keep_head_and_tail
                if head_end >= tail_start {
                    keep_head_and_tail(history, budget)
                } else {
                    let head = &history[..head_end];
                    let middle = &history[head_end..tail_start];
                    let tail = &history[tail_start..];

                    let head_tokens: u32 =
                        head.iter().map(estimate_message_tokens_for_prompt).sum();
                    let tail_tokens: u32 =
                        tail.iter().map(estimate_message_tokens_for_prompt).sum();

                    if head_tokens + tail_tokens > budget {
                        keep_head_and_tail(history, budget)
                    } else {
                        let mid_budget = budget - head_tokens - tail_tokens;
                        let trimmed_middle = drop_old_turns(middle, mid_budget);

                        let mut result =
                            Vec::with_capacity(head.len() + trimmed_middle.len() + tail.len());
                        result.extend_from_slice(head);
                        result.extend(trimmed_middle);
                        result.extend_from_slice(tail);
                        result
                    }
                }
            }
        };

        // 裁剪完成后，对 Tool 消息执行输出截断
        let mut result = result;
        for msg in &mut result {
            if msg.role == Role::Tool {
                let truncated = truncate_tool_output(&msg.content);
                if truncated != msg.content {
                    msg.content = truncated;
                    msg.estimated_tokens = estimate_message_tokens(msg);
                }
            }
        }
        result
    }
}

// ========== 辅助函数 ==========

/// max_context_tokens 是 effective limit（含预留）
/// 只减去 max(output_tokens, 4000) 作为输出保留空间
pub(crate) fn calculate_available(max_context_tokens: u32, output_tokens: u32) -> u32 {
    max_context_tokens.saturating_sub(output_tokens.max(4_000))
}

/// 估算消息列表中每条消息在 prompt 中的实际 token 数
/// - 非 Tool 消息：直接返回 msg.estimated_tokens
/// - Tool 消息超过 TOOL_OUTPUT_MAX_CHARS：按截断后长度估算
pub(crate) fn estimate_message_tokens_for_prompt(msg: &Message) -> u32 {
    if msg.role == Role::Tool && msg.content.chars().count() > TOOL_OUTPUT_MAX_CHARS {
        ((TOOL_OUTPUT_MAX_CHARS as f64 / 4.0).ceil() as u32) + 1
    } else {
        msg.estimated_tokens
    }
}

/// 批量版本
pub(crate) fn estimate_messages_tokens_for_prompt(messages: &[Message]) -> u32 {
    messages
        .iter()
        .map(estimate_message_tokens_for_prompt)
        .sum()
}

/// 截断工具输出到 TOOL_OUTPUT_MAX_CHARS 字符（按字符计数，不是字节）
/// 使用 chars().take() 保证 Unicode 安全
pub(crate) fn truncate_tool_output(content: &str) -> String {
    if content.chars().count() <= TOOL_OUTPUT_MAX_CHARS {
        content.to_string()
    } else {
        let truncated: String = content.chars().take(TOOL_OUTPUT_MAX_CHARS).collect();
        format!("{}... (truncated)", truncated)
    }
}

/// 返回第 n+1 轮起点索引（轮次由 User 消息位置识别）
/// 如果不足 n 轮 User 消息，返回 history.len()
pub(crate) fn find_head_end(history: &[Message], n: usize) -> usize {
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
pub(crate) fn find_tail_start(history: &[Message], n: usize) -> usize {
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
pub(crate) fn drop_old_turns(messages: &[Message], budget: u32) -> Vec<Message> {
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
pub(crate) fn keep_head_and_tail(history: &[Message], budget: u32) -> Vec<Message> {
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

    // 反转得到原始顺序
    tail_indices.reverse();

    // 构建 confirmed_tool_ids 集合（被保留的 assistant tool_calls 需要对应的 tool_result）
    let confirmed_ids_set: std::collections::HashSet<&str> =
        confirmed_tool_ids.iter().map(|s| s.as_str()).collect();

    // 收集结果中实际存在的 tool_call_ids（从 Tool 消息）
    let mut present_tool_call_ids: std::collections::HashSet<String> =
        std::collections::HashSet::new();
    for &i in &tail_indices {
        let msg = &history[i];
        if msg.role == Role::Tool
            && let Some(ref call_id) = msg.tool_call_id
        {
            present_tool_call_ids.insert(call_id.clone());
        }
    }

    // 过滤：移除孤儿 ToolResult 和孤儿 ToolCall
    let filtered_indices: Vec<usize> = tail_indices
        .into_iter()
        .filter(|&i| {
            let msg = &history[i];
            if msg.role == Role::Tool {
                // orphan ToolResult：没有匹配的 assistant tool_call
                if let Some(ref call_id) = msg.tool_call_id {
                    return confirmed_ids_set.contains(call_id.as_str());
                }
                return false;
            }
            if msg.role == Role::Assistant
                && let Some(ref calls) = msg.tool_calls
            {
                // orphan ToolCall：assistant tool_calls 存在但 tool_result 不在结果中
                // 如果所有 tool_calls 的 id 都不在 present_tool_call_ids 中，过滤掉这个 assistant
                let any_result_present =
                    calls.iter().any(|c| present_tool_call_ids.contains(&c.id));
                if !any_result_present {
                    return false;
                }
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

#[cfg(test)]
mod tests {
    use super::*;
    use visp_core::message::ToolCallRequest;

    // ---- 1: DefaultContextTrimmer ----

    #[test]
    fn test_default_trimmer_default_values() {
        let trimmer = DefaultContextTrimmer::default();
        assert_eq!(trimmer.head_turns, 5);
        assert_eq!(trimmer.tail_turns, 10);
        assert_eq!(trimmer.tool_output_max_chars, 2000);
    }

    #[test]
    fn test_default_context_trimmer_implements_trait() {
        // 验证实现了 ContextTrimmer trait
        fn assert_trimmer<T: ContextTrimmer>() {}
        assert_trimmer::<DefaultContextTrimmer>();

        // 验证可以通过 trait object 使用
        let trimmer: Box<dyn ContextTrimmer> = Box::new(DefaultContextTrimmer::default());
        let result = trimmer.trim(&[], 1000, 0, 100);
        assert!(result.is_empty());
    }

    #[test]
    fn test_default_context_trimmer_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<DefaultContextTrimmer>();

        let trimmer: Box<dyn ContextTrimmer + Send + Sync> =
            Box::new(DefaultContextTrimmer::default());
        let _arc = std::sync::Arc::new(trimmer);
    }

    // ---- 2: Token 估算 + 预算 + 截断 ----

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

    // ---- 3: 边界函数 ----

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

    // ---- 4: drop_old_turns ----

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

    // ---- 5: keep_head_and_tail ----

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

    // ---- 6: keep_head_and_tail orphan tool_call filter ----

    #[test]
    fn test_keep_head_and_tail_filter_orphan_toolcall() {
        // Assistant 的 tool_call 在结果中，但 tool_result 不在 → tool_call 应被过滤
        let mut a1 = Message::assistant("a1");
        a1.tool_calls = Some(vec![ToolCallRequest {
            id: "call_a".to_string(),
            name: "tool".to_string(),
            arguments: "{}".to_string(),
        }]);
        a1.estimated_tokens = estimate_message_tokens(&a1);
        let history = vec![
            Message::user("u1"),
            a1.clone(),
            Message::user("u2"),
            Message::assistant("a2"),
        ];
        // budget=12: 从后往前，u2(2)+a2(2)+a1(2) fits, u1(2) fits
        // 但 a1 有 tool_calls("call_a")，没有对应的 tool_result → 应被过滤
        let result = keep_head_and_tail(&history, 12);
        // a1 应该被过滤掉（orphan tool_call）
        assert!(
            !result
                .iter()
                .any(|m| m.role == Role::Assistant && m.tool_calls.is_some()),
            "orphan tool_call should be filtered"
        );
        assert_eq!(result[0], Message::user("u1"));
    }

    #[test]
    fn test_keep_head_and_tail_orphan_toolcall_but_keep_result() {
        // 正常情况：assistant tool_call 和 tool_result 都在结果中 → 都保留
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
            a1.clone(),
            tr1.clone(),
            Message::user("u2"),
            Message::assistant("a2"),
        ];
        // budget=20: u1(2)+a1(6)+tr1(4)+u2(2)+a2(2) = 16, all fit
        let result = keep_head_and_tail(&history, 20);
        // a1 和 tr1 应该都被保留
        let has_call = result
            .iter()
            .any(|m| m.role == Role::Assistant && m.tool_calls.is_some());
        assert!(has_call, "confirmed tool_call should be kept");
        let has_result = result.iter().any(|m| m.role == Role::Tool);
        assert!(has_result, "confirmed tool_result should be kept");
    }

    // ---- 7: trim (via DefaultContextTrimmer) ----

    #[test]
    fn test_trim_context_all_fit() {
        let trimmer = DefaultContextTrimmer::default();
        let history = vec![
            Message::user("u1"),
            Message::assistant("a1"),
            Message::user("u2"),
            Message::assistant("a2"),
        ];
        let result = trimmer.trim(&history, 128_000, 20, 4096);
        assert_eq!(result.len(), 4);
    }

    #[test]
    fn test_trim_context_head_middle_tail() {
        let trimmer = DefaultContextTrimmer::default();
        // Create enough turns to exercise HEAD+MIDDLE+TAIL strategy
        let mut history: Vec<Message> = Vec::new();
        // Add 20 User-Assistant pairs
        for i in 1..=20 {
            history.push(Message::user(format!("u{}", i)));
            history.push(Message::assistant(format!("a{}", i)));
        }
        // Budget must be enough for HEAD+TAIL but not MIDDLE
        let result = trimmer.trim(&history, 70000, 0, 4096);
        // Should be a valid result (either keep_head_and_tail or HEAD+trimmed+TAIL)
        assert!(!result.is_empty());
        assert!(result.len() <= history.len());
        // First message should be the first User
        assert_eq!(result[0].role, Role::User);
    }

    #[test]
    fn test_trim_context_empty() {
        let trimmer = DefaultContextTrimmer::default();
        let result = trimmer.trim(&[], 1000, 20, 100);
        assert!(result.is_empty());
    }

    #[test]
    fn test_trim_context_zero_budget() {
        let trimmer = DefaultContextTrimmer::default();
        let history = vec![Message::user("u1"), Message::assistant("a1")];
        let result = trimmer.trim(&history, 0, 0, 0);
        // budget=0 → falls back to keep_head_and_tail
        assert!(!result.is_empty());
        assert_eq!(result[0].role, Role::User);
    }

    #[test]
    fn test_trim_truncates_tool_output() {
        let trimmer = DefaultContextTrimmer::default();
        let long_output = "x".repeat(3000);
        let tool_msg = Message::tool(&long_output, "call_1");
        let history = vec![Message::user("u1"), tool_msg];
        let result = trimmer.trim(&history, 128_000, 20, 4096);
        // Tool message should be truncated
        let tool_result = &result[1]; // user + tool
        assert_eq!(tool_result.role, Role::Tool);
        assert!(tool_result.content.len() < 3000);
        assert!(tool_result.content.contains("truncated"));
    }
}
