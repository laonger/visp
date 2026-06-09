/// SSE 事件
#[derive(Debug, Clone, PartialEq)]
pub struct SseEvent {
    pub event: Option<String>,
    pub data: Option<String>,
}

/// 解析 SSE 事件流
#[must_use]
pub fn parse_sse_events(input: &str) -> Vec<SseEvent> {
    let mut events = Vec::new();
    let mut current_event: Option<String> = None;
    let mut current_data: Option<String> = None;

    for line in input.lines() {
        if let Some(stripped) = line.strip_prefix("event:") {
            current_event = Some(stripped.trim().to_string());
        } else if let Some(stripped) = line.strip_prefix("data:") {
            let chunk = stripped.trim().to_string();
            match &mut current_data {
                Some(existing) => {
                    existing.push('\n');
                    existing.push_str(&chunk);
                }
                None => current_data = Some(chunk),
            }
        } else if line.is_empty() {
            // 空行 — 触发当前事件
            if current_event.is_some() || current_data.is_some() {
                events.push(SseEvent {
                    event: current_event.take(),
                    data: current_data.take(),
                });
            }
        }
        // 其他行（注释、未知字段等）忽略
    }

    // 文件末尾没有空行时，也将最后一个事件推入
    if current_event.is_some() || current_data.is_some() {
        events.push(SseEvent {
            event: current_event.take(),
            data: current_data.take(),
        });
    }

    events
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_valid_event() {
        let input = "event: message\ndata: {\"hello\":\"world\"}\n\n";
        let events = parse_sse_events(input);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event.as_deref(), Some("message"));
        assert_eq!(events[0].data.as_deref(), Some(r#"{"hello":"world"}"#));
    }

    #[test]
    fn test_parse_missing_event() {
        let input = "data: {\"key\":\"val\"}\n\n";
        let events = parse_sse_events(input);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event, None);
        assert_eq!(events[0].data.as_deref(), Some(r#"{"key":"val"}"#));
    }

    #[test]
    fn test_parse_empty_input() {
        let input = "\n\n";
        let events = parse_sse_events(input);
        assert_eq!(events.len(), 0);
    }

    #[test]
    fn test_parse_data_across_lines() {
        let input = "data: line1\ndata: line2\ndata: line3\n\n";
        let events = parse_sse_events(input);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event, None);
        assert_eq!(events[0].data.as_deref(), Some("line1\nline2\nline3"));
    }
}
