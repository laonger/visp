use rusqlite::{Connection, OptionalExtension, Result, params};
use visp_core::message::{Message, MessageType, Role};

/// Message table DAO (Data Access Object).
/// All methods receive a `&rusqlite::Connection` and operate on the `message` table.
pub struct MessageRepo;

impl MessageRepo {
    /// Insert a new message row. Returns the auto-generated id.
    pub fn insert(conn: &Connection, session_id: &str, msg: &Message) -> Result<i64> {
        let now = msg
            .created_at
            .unwrap_or_else(|| chrono::Utc::now().timestamp_millis());
        let role_str = match msg.role {
            Role::System => "system",
            Role::User => "user",
            Role::Assistant => "assistant",
            Role::Tool => "tool",
        };
        let type_str = match msg.kind {
            MessageType::Text => "text",
            MessageType::Thinking => "thinking",
            MessageType::ToolCall => "tool_call",
            MessageType::ToolResult => "tool_result",
            MessageType::Error => "error",
            MessageType::Status => "status",
            MessageType::System => "system",
            MessageType::User => "user",
        };

        // tool_name and tool_arguments from the first tool_call if present
        let (tool_name, tool_arguments) = if let Some(ref calls) = msg.tool_calls {
            if let Some(first) = calls.first() {
                (Some(first.name.clone()), Some(first.arguments.clone()))
            } else {
                (None, None)
            }
        } else {
            (None, None)
        };

        // Full tool_calls serialized as JSON (preserves all calls including their IDs)
        let tool_calls_json = msg
            .tool_calls
            .as_ref()
            .map(|calls| serde_json::to_string(calls).unwrap_or_default());

        let extra_blocks = msg
            .extra_blocks
            .as_ref()
            .map(|v| serde_json::to_string(v).unwrap_or_default());
        let provider_metadata = msg
            .provider_metadata
            .as_ref()
            .map(|v| serde_json::to_string(v).unwrap_or_default());

        conn.execute(
            "INSERT INTO message (session_id, role, type, content, tool_call_id, tool_name, tool_arguments, tool_calls_json, tool_result_is_error, tool_result_duration_ms, tool_call_count, estimated_tokens, extra_blocks, provider_metadata, actual_tokens_input, actual_tokens_output, actual_cache_read, actual_cache_write, actual_cost, skip_context, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21)",
            params![
                session_id,
                role_str,
                type_str,
                msg.content,
                msg.tool_call_id,
                tool_name,
                tool_arguments,
                tool_calls_json,
                msg.tool_result_is_error.map(|v| v as i64),
                msg.tool_result_duration_ms.map(|v| v as i64),
                msg.tool_calls.as_ref().map(|c| c.len() as i64).unwrap_or(0),
                msg.estimated_tokens,
                extra_blocks,
                provider_metadata,
                msg.actual_tokens_input,
                msg.actual_tokens_output,
                msg.actual_cache_read,
                msg.actual_cache_write,
                msg.actual_cost,
                msg.skip_context as i64,
                now,
            ],
        )?;
        Ok(conn.last_insert_rowid())
    }

    /// Get all messages for a session, ordered by id (insertion order).
    pub fn get_by_session(conn: &Connection, session_id: &str) -> Result<Vec<Message>> {
        let mut stmt = conn.prepare(
            "SELECT id, role, type, content, tool_call_id, tool_name, tool_arguments, tool_calls_json, tool_result_is_error, tool_result_duration_ms, tool_call_count, estimated_tokens, extra_blocks, provider_metadata, actual_tokens_input, actual_tokens_output, actual_cache_read, actual_cache_write, actual_cost, skip_context, created_at
             FROM message WHERE session_id = ?1 ORDER BY id ASC",
        )?;

        let messages = stmt
            .query_map(params![session_id], |row| {
                let _id: i64 = row.get(0)?;
                let role_str: String = row.get(1)?;
                let type_str: String = row.get(2)?;
                let content: String = row.get(3)?;
                let tool_call_id: Option<String> = row.get(4)?;
                let tool_name: Option<String> = row.get(5)?;
                let tool_arguments: Option<String> = row.get(6)?;
                let tool_calls_json: Option<String> = row.get(7)?;
                let tool_result_is_error: Option<i64> = row.get(8)?;
                let tool_result_duration_ms: Option<i64> = row.get(9)?;
                let tool_call_count: i64 = row.get(10)?;
                let estimated_tokens: u32 = row.get(11)?;
                let extra_blocks_str: Option<String> = row.get(12)?;
                let provider_metadata_str: Option<String> = row.get(13)?;
                let actual_tokens_input: Option<u32> = row.get(14)?;
                let actual_tokens_output: Option<u32> = row.get(15)?;
                let actual_cache_read: Option<u32> = row.get(16)?;
                let actual_cache_write: Option<u32> = row.get(17)?;
                let actual_cost: Option<f64> = row.get(18)?;
                let skip_context_int: i64 = row.get(19)?;
                let created_at: Option<i64> = row.get(20)?;

                let role = match role_str.as_str() {
                    "user" => Role::User,
                    "assistant" => Role::Assistant,
                    "tool" => Role::Tool,
                    _ => Role::System,
                };

                let kind = match type_str.as_str() {
                    "thinking" => MessageType::Thinking,
                    "tool_call" => MessageType::ToolCall,
                    "tool_result" => MessageType::ToolResult,
                    "error" => MessageType::Error,
                    "status" => MessageType::Status,
                    "system" => MessageType::System,
                    "user" => MessageType::User,
                    _ => MessageType::Text,
                };

                let extra_blocks = extra_blocks_str
                    .and_then(|s| serde_json::from_str::<Vec<serde_json::Value>>(&s).ok());

                let provider_metadata = provider_metadata_str
                    .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok());

                // Reconstruct tool_calls: prefer tool_calls_json if present (v2+),
                // fall back to tool_name + tool_arguments (v1 compat)
                let tool_calls = if kind == MessageType::ToolCall {
                    if let Some(ref json) = tool_calls_json {
                        // v2+: deserialize from JSON (preserves all calls & their IDs)
                        serde_json::from_str(json).ok()
                    } else if let (Some(name), Some(args)) = (&tool_name, &tool_arguments) {
                        // v1 fallback: reconstruct from tool_name + tool_arguments
                        // v1 data has empty tool_call_id, which is a known bug.
                        Some(vec![visp_core::message::ToolCallRequest {
                            id: tool_call_id.clone().unwrap_or_default(),
                            name: name.clone(),
                            arguments: args.clone(),
                        }])
                    } else {
                        None
                    }
                } else {
                    None
                };

                Ok(Message {
                    role,
                    kind,
                    content,
                    tool_call_id,
                    tool_calls,
                    extra_blocks,
                    skip_context: skip_context_int != 0,
                    estimated_tokens,
                    actual_tokens_input,
                    actual_tokens_output,
                    actual_cache_read,
                    actual_cache_write,
                    actual_cost,
                    provider_metadata,
                    tool_result_is_error: tool_result_is_error.map(|v| v != 0),
                    tool_result_duration_ms: tool_result_duration_ms.map(|v| v as u64),
                    tool_call_count: Some(tool_call_count as u32).filter(|&c| c > 0),
                    created_at,
                })
            })?
            .collect::<Result<Vec<_>>>()?;

        Ok(messages)
    }

    /// 获取某个 session 的最后一条用户消息内容（截断到 80 字符）
    pub fn get_last_user_message(conn: &Connection, session_id: &str) -> Result<Option<String>> {
        let mut stmt = conn.prepare(
            "SELECT content FROM message WHERE session_id = ?1 AND role = 'user' ORDER BY id DESC LIMIT 1",
        )?;
        let result: Option<String> = stmt
            .query_row(params![session_id], |row| row.get(0))
            .optional()?;
        Ok(result.map(|s| {
            if s.chars().count() > 80 {
                format!("{}...", s.chars().take(80).collect::<String>())
            } else {
                s
            }
        }))
    }

    /// Delete all messages for a session.
    pub fn delete_by_session(conn: &Connection, session_id: &str) -> Result<usize> {
        conn.execute(
            "DELETE FROM message WHERE session_id = ?1",
            params![session_id],
        )
    }
}

#[cfg(test)]
#[path = "message_repo_tests.rs"]
mod tests;
