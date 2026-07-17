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
#[path = "streaming_tests.rs"]
mod tests;
