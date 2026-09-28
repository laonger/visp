//! `AgentEventFrame` → `HookEvent` 纯映射（设计 §6.2 事件表 / §6.4 事件通路）。
//!
//! 把 core 的**执行级**帧（工具调用、agent run 终结）按「执行级 vs 会话级」分层
//! 转换为冻结的 hook 契约事件。本模块**纯函数、无 IO、无副作用**：
//! 不发布总线、不读写环境、不 spawn 进程；`seq` 由总线发布点发放，故此处恒为 `None`。
//!
//! 只映射 daemon 转发任务可权威判定的帧。以下事件由各自宿主直接构造，
//! **不**经过本映射：`SessionStart`/`UserPromptSubmit`/`Stop`/`StopFailure`
//! （orchestrator）、`PermissionResult`/`SessionEnd`（daemon service）。

use visp_core::agent::{AgentEvent, AgentEventFrame, ToolOutcome};
use visp_hooks::{
    AgentRunEndPayload, AgentRunStatus, HookContext, HookEvent, HookEventName, HookPayload, Origin,
    PermissionKind, PermissionRequestPayload, PostToolUseFailurePayload, PostToolUsePayload,
    PreToolUsePayload, SessionSource, SubagentStopPayload, ToolCallRequestedPayload,
    VISP_HOOK_SCHEMA,
};

/// 映射所需的、无法从 [`AgentEventFrame`] 取得的上下文（设计 §6.2/§6.3）。
///
/// 各字段由**接线层**在真实来源处填充（daemon/orchestrator 绑定会话时写入）；
/// 本模块只做透传，不推导。缺省占位仅供测试与未接线路径使用。
#[derive(Debug, Clone)]
pub struct MapCtx {
    /// 信封 `cwd`（交付时的工作目录）。
    pub cwd: String,
    /// 信封 `source`：`{startup, resume}`。
    pub source: SessionSource,
    /// 信封 `origin`：`{tui, headless, other}`。
    pub origin: Origin,
    /// canonical 项目路径（交付时 `cwd` 与 `VISP_PROJECT_PATH` 的权威来源）。
    pub project_path: String,
}

impl Default for MapCtx {
    /// 缺省占位：空 `cwd`/`project_path`、`source=Startup`、`origin=Tui`。
    /// 真实值一律由接线层填充。
    fn default() -> Self {
        Self {
            cwd: String::new(),
            source: SessionSource::Startup,
            origin: Origin::Tui,
            project_path: String::new(),
        }
    }
}

/// 按帧类型映射为 hook 事件；不属 hook 域的帧返回 `None`（设计 §6.2 事件表）。
///
/// 分层规则（设计 §6.2 完成事件分层裁定 B）：
/// - 主 agent（`parent_session_id == None`）的 `Done` → 执行级 `AgentRunEnd`；
/// - 子 agent（`parent_session_id == Some`）的 `Done`/`Error` → 执行级 `SubagentStop`；
/// - 主 agent 的 `Error` **不**在此产出（会话级 `StopFailure` 由 orchestrator 发 `Hook`）。
pub fn map_frame(frame: &AgentEventFrame, ctx: &MapCtx) -> Option<HookEvent> {
    let event = match &frame.event {
        AgentEvent::ToolCallRequest {
            call_id, tool_name, ..
        } => make_hook(
            frame,
            ctx,
            HookEventName::ToolCallRequested,
            HookPayload::ToolCallRequested(ToolCallRequestedPayload {
                tool_use_id: call_id.clone(),
                tool_name: tool_name.clone(),
            }),
        ),
        AgentEvent::PreToolUse {
            call_id,
            tool_name,
            requires_approval,
        } => make_hook(
            frame,
            ctx,
            HookEventName::PreToolUse,
            HookPayload::PreToolUse(PreToolUsePayload {
                tool_use_id: call_id.clone(),
                tool_name: tool_name.clone(),
                requires_approval: *requires_approval,
            }),
        ),
        AgentEvent::ToolCallResult {
            call_id,
            tool_name,
            content,
            outcome,
            ..
        } => match outcome {
            ToolOutcome::Success => make_hook(
                frame,
                ctx,
                HookEventName::PostToolUse,
                HookPayload::PostToolUse(PostToolUsePayload {
                    tool_use_id: call_id.clone(),
                    tool_name: tool_name.clone(),
                    // 默认脱敏：原文仅在规则显式 include 时由交付层回填（设计 §8.4）。
                    tool_input: None,
                    tool_response: None,
                }),
            ),
            ToolOutcome::Failure => make_hook(
                frame,
                ctx,
                HookEventName::PostToolUseFailure,
                HookPayload::PostToolUseFailure(PostToolUseFailurePayload {
                    tool_use_id: call_id.clone(),
                    tool_name: tool_name.clone(),
                    message: content.clone(),
                    tool_input: None,
                    tool_response: None,
                }),
            ),
            // `denied` 由 `PermissionResult` 表达；`cancelled` 归 `Stop` 语义；
            // `truncated` 为参数畸形 —— 三者均**非**「工具执行完成」事实，
            // 不产出工具完成事件（设计 §6.2）。
            ToolOutcome::Denied | ToolOutcome::Cancelled | ToolOutcome::Truncated => return None,
        },
        AgentEvent::UserQuery {
            query_id,
            options,
            kind,
            ..
        } => make_hook(
            frame,
            ctx,
            HookEventName::PermissionRequest,
            HookPayload::PermissionRequest(PermissionRequestPayload {
                query_id: query_id.clone(),
                kind: core_permission_kind(*kind),
                options_count: options.len() as u64,
                // 默认脱敏：原文仅在规则显式 include 时由交付层回填（设计 §8.4）。
                message: None,
            }),
        ),
        AgentEvent::Done => match &frame.parent_session_id {
            None => make_hook(
                frame,
                ctx,
                HookEventName::AgentRunEnd,
                HookPayload::AgentRunEnd(AgentRunEndPayload {
                    status: AgentRunStatus::Completed,
                }),
            ),
            Some(parent) => subagent_stop(frame, ctx, parent, AgentRunStatus::Completed),
        },
        AgentEvent::Error { .. } => {
            // 主 agent 的错误是会话级 `StopFailure` 事实，由 orchestrator 在语义
            // 收敛后发 `Hook`；执行级映射不产出（设计 §6.2 完成事件分层）。
            let parent = frame.parent_session_id.as_ref()?;
            subagent_stop(frame, ctx, parent, AgentRunStatus::Failed)
        }
        // 其余帧（TextDelta/ThinkingBlock/Usage*/StatusUpdate/Image*/…）不属 hook 域。
        _ => return None,
    };
    Some(event)
}

/// core 侧 [`visp_core::agent::PermissionKind`] → 冻结契约 [`PermissionKind`]。
///
/// 两枚举取值域一一对应；显式转换以免 core 与 hooks 契约耦合。
fn core_permission_kind(kind: visp_core::agent::PermissionKind) -> PermissionKind {
    match kind {
        visp_core::agent::PermissionKind::Approval => PermissionKind::Approval,
        visp_core::agent::PermissionKind::Question => PermissionKind::Question,
    }
}

/// 构造子 agent 执行级完成事件（`Done`/`Error`，`parent` 由调用方保证为 `Some`）。
fn subagent_stop(
    frame: &AgentEventFrame,
    ctx: &MapCtx,
    parent_session_id: &str,
    status: AgentRunStatus,
) -> HookEvent {
    make_hook(
        frame,
        ctx,
        HookEventName::SubagentStop,
        HookPayload::SubagentStop(SubagentStopPayload {
            parent_session_id: parent_session_id.to_string(),
            agent_name: frame.agent_name.clone(),
            status,
        }),
    )
}

/// 组装「公共信封 + 逐事件载荷」。`seq` 由总线发布点发放，此处不产生。
fn make_hook(
    frame: &AgentEventFrame,
    ctx: &MapCtx,
    hook_event_name: HookEventName,
    payload: HookPayload,
) -> HookEvent {
    HookEvent {
        context: HookContext {
            schema: VISP_HOOK_SCHEMA,
            hook_event_name,
            session_id: frame.session_id.clone(),
            cwd: ctx.cwd.clone(),
            source: ctx.source,
            origin: ctx.origin,
            seq: None,
        },
        payload,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::mpsc;
    use visp_core::agent::UserQueryResult;
    use visp_core::error::AgentErrorCode;

    fn ctx() -> MapCtx {
        MapCtx {
            cwd: "/work".to_string(),
            source: SessionSource::Startup,
            origin: Origin::Tui,
            project_path: "/work".to_string(),
        }
    }

    fn frame_with(event: AgentEvent, parent: Option<&str>) -> AgentEventFrame {
        AgentEventFrame {
            event,
            session_id: "sess".to_string(),
            agent_name: "sub".to_string(),
            parent_session_id: parent.map(str::to_string),
            parent_session_name: parent.map(|_| "root".to_string()),
        }
    }

    fn frame(event: AgentEvent) -> AgentEventFrame {
        frame_with(event, None)
    }

    fn result_frame(outcome: ToolOutcome) -> AgentEventFrame {
        frame(AgentEvent::ToolCallResult {
            call_id: "c1".to_string(),
            tool_name: "bash".to_string(),
            content: "out".to_string(),
            is_error: outcome.into_is_error(),
            outcome,
        })
    }

    fn user_query_frame(kind: visp_core::agent::PermissionKind) -> AgentEventFrame {
        let (tx, _rx) = mpsc::channel::<UserQueryResult>(1);
        frame(AgentEvent::UserQuery {
            query_id: "q1".to_string(),
            message: "Allow tool: bash?".to_string(),
            options: vec!["Yes".to_string(), "No".to_string()],
            allow_other: false,
            kind,
            respond: tx,
        })
    }

    #[test]
    fn tool_call_request_maps_to_tool_call_requested() {
        let frame = frame(AgentEvent::ToolCallRequest {
            call_id: "c1".to_string(),
            tool_name: "bash".to_string(),
            arguments: "{}".to_string(),
        });

        let event = map_frame(&frame, &ctx()).expect("ToolCallRequest 应映射");

        assert_eq!(event.event_name(), HookEventName::ToolCallRequested);
        assert_eq!(event.context.session_id, "sess");
        assert_eq!(event.context.cwd, "/work");
        assert_eq!(event.context.source, SessionSource::Startup);
        assert_eq!(event.context.origin, Origin::Tui);
        assert_eq!(event.context.seq, None);
        match event.payload {
            HookPayload::ToolCallRequested(p) => {
                assert_eq!(p.tool_use_id, "c1");
                assert_eq!(p.tool_name, "bash");
            }
            other => panic!("expected ToolCallRequested, got {other:?}"),
        }
    }

    #[test]
    fn pre_tool_use_maps_to_pre_tool_use() {
        let frame = frame(AgentEvent::PreToolUse {
            call_id: "c1".to_string(),
            tool_name: "bash".to_string(),
            requires_approval: true,
        });

        let event = map_frame(&frame, &ctx()).expect("PreToolUse 应映射");

        assert_eq!(event.event_name(), HookEventName::PreToolUse);
        match event.payload {
            HookPayload::PreToolUse(p) => {
                assert_eq!(p.tool_use_id, "c1");
                assert_eq!(p.tool_name, "bash");
                assert!(p.requires_approval);
            }
            other => panic!("expected PreToolUse, got {other:?}"),
        }
    }

    #[test]
    fn tool_result_success_maps_to_post_tool_use() {
        let event = map_frame(&result_frame(ToolOutcome::Success), &ctx()).expect("Success 应映射");

        assert_eq!(event.event_name(), HookEventName::PostToolUse);
        match event.payload {
            HookPayload::PostToolUse(p) => {
                assert_eq!(p.tool_use_id, "c1");
                assert_eq!(p.tool_name, "bash");
                assert_eq!(p.tool_input, None);
                assert_eq!(p.tool_response, None);
            }
            other => panic!("expected PostToolUse, got {other:?}"),
        }
    }

    #[test]
    fn tool_result_failure_maps_to_post_tool_use_failure() {
        let event = map_frame(&result_frame(ToolOutcome::Failure), &ctx()).expect("Failure 应映射");

        assert_eq!(event.event_name(), HookEventName::PostToolUseFailure);
        match event.payload {
            HookPayload::PostToolUseFailure(p) => {
                assert_eq!(p.tool_use_id, "c1");
                assert_eq!(p.tool_name, "bash");
                assert_eq!(p.message, "out");
                assert_eq!(p.tool_input, None);
                assert_eq!(p.tool_response, None);
            }
            other => panic!("expected PostToolUseFailure, got {other:?}"),
        }
    }

    /// `Denied`/`Cancelled`/`Truncated` 非工具完成事实，**不得**产出完成事件。
    #[test]
    fn non_completion_outcomes_do_not_produce_tool_events() {
        for outcome in [
            ToolOutcome::Denied,
            ToolOutcome::Cancelled,
            ToolOutcome::Truncated,
        ] {
            let event = map_frame(&result_frame(outcome), &ctx());
            assert!(
                event.is_none(),
                "outcome {outcome:?} 不应产出工具完成事件，实际 {event:?}"
            );
        }
    }

    /// `PermissionRequest.kind` 从**帧**读取（Approval），不再依赖 `MapCtx`。
    #[test]
    fn user_query_approval_kind_read_from_frame() {
        let event = map_frame(
            &user_query_frame(visp_core::agent::PermissionKind::Approval),
            &ctx(),
        )
        .expect("UserQuery 应映射");

        assert_eq!(event.event_name(), HookEventName::PermissionRequest);
        match event.payload {
            HookPayload::PermissionRequest(p) => {
                assert_eq!(p.query_id, "q1");
                assert_eq!(p.kind, PermissionKind::Approval);
                assert_eq!(p.options_count, 2);
                assert_eq!(p.message, None);
            }
            other => panic!("expected PermissionRequest, got {other:?}"),
        }
    }

    /// `PermissionRequest.kind` 从**帧**读取（Question）。
    #[test]
    fn user_query_question_kind_read_from_frame() {
        let event = map_frame(
            &user_query_frame(visp_core::agent::PermissionKind::Question),
            &ctx(),
        )
        .expect("UserQuery 应映射");

        assert_eq!(event.event_name(), HookEventName::PermissionRequest);
        match event.payload {
            HookPayload::PermissionRequest(p) => {
                assert_eq!(p.query_id, "q1");
                assert_eq!(p.kind, PermissionKind::Question);
                assert_eq!(p.options_count, 2);
                assert_eq!(p.message, None);
            }
            other => panic!("expected PermissionRequest, got {other:?}"),
        }
    }

    #[test]
    fn done_without_parent_maps_to_agent_run_end() {
        let event = map_frame(&frame(AgentEvent::Done), &ctx()).expect("主 Done 应映射");

        assert_eq!(event.event_name(), HookEventName::AgentRunEnd);
        assert_eq!(event.context.session_id, "sess");
        match event.payload {
            HookPayload::AgentRunEnd(p) => assert_eq!(p.status, AgentRunStatus::Completed),
            other => panic!("expected AgentRunEnd, got {other:?}"),
        }
    }

    #[test]
    fn done_with_parent_maps_to_subagent_stop_completed() {
        let event =
            map_frame(&frame_with(AgentEvent::Done, Some("p1")), &ctx()).expect("子 Done 应映射");

        assert_eq!(event.event_name(), HookEventName::SubagentStop);
        match event.payload {
            HookPayload::SubagentStop(p) => {
                assert_eq!(p.parent_session_id, "p1");
                assert_eq!(p.agent_name, "sub");
                assert_eq!(p.status, AgentRunStatus::Completed);
            }
            other => panic!("expected SubagentStop, got {other:?}"),
        }
    }

    #[test]
    fn error_with_parent_maps_to_subagent_stop_failed() {
        let frame = frame_with(
            AgentEvent::Error {
                code: AgentErrorCode::Internal,
                message: "boom".to_string(),
            },
            Some("p1"),
        );

        let event = map_frame(&frame, &ctx()).expect("子 Error 应映射");

        assert_eq!(event.event_name(), HookEventName::SubagentStop);
        match event.payload {
            HookPayload::SubagentStop(p) => {
                assert_eq!(p.parent_session_id, "p1");
                assert_eq!(p.agent_name, "sub");
                assert_eq!(p.status, AgentRunStatus::Failed);
            }
            other => panic!("expected SubagentStop, got {other:?}"),
        }
    }

    /// 主 agent 的 `Error` 是会话级 `StopFailure`，执行级映射不产出。
    #[test]
    fn error_without_parent_maps_to_nothing() {
        let frame = frame(AgentEvent::Error {
            code: AgentErrorCode::MaxIterations,
            message: "loop".to_string(),
        });
        assert!(map_frame(&frame, &ctx()).is_none());
    }

    #[test]
    fn non_hook_frames_map_to_nothing() {
        let frames = [
            frame(AgentEvent::TextDelta("hi".to_string())),
            frame(AgentEvent::ThinkingBlock(serde_json::json!({}))),
            frame(AgentEvent::UsageInfo {
                input_tokens: 1,
                output_tokens: 2,
                tool_calls: 3,
                cache_creation_input_tokens: 0,
                cache_read_input_tokens: 0,
                cost: None,
            }),
            frame(AgentEvent::UsageDelta {
                input_tokens: 1,
                output_tokens: 2,
                cache_creation_input_tokens: 0,
                cache_read_input_tokens: 0,
            }),
            frame(AgentEvent::StatusUpdate("running".to_string())),
            frame(AgentEvent::ImageBlock {
                path: "/tmp/a.png".to_string(),
                mime_type: "image/png".to_string(),
                remote_url: None,
            }),
            frame(AgentEvent::ImageError {
                reason: "bad".to_string(),
            }),
        ];

        for f in &frames {
            assert!(
                map_frame(f, &ctx()).is_none(),
                "非 hook 域帧不应映射：{:?}",
                std::mem::discriminant(&f.event)
            );
        }
    }

    #[test]
    fn default_ctx_uses_placeholders() {
        let c = MapCtx::default();
        assert_eq!(c.cwd, "");
        assert_eq!(c.project_path, "");
        assert_eq!(c.source, SessionSource::Startup);
        assert_eq!(c.origin, Origin::Tui);
    }

    /// 产出事件严格符合冻结契约取值域，可经扁平 JSON round-trip。
    #[test]
    fn produced_event_round_trips_through_contract() {
        let event = map_frame(&result_frame(ToolOutcome::Success), &ctx()).unwrap();
        let value = event.to_value();
        assert_eq!(value["hook_event_name"], "PostToolUse");
        assert_eq!(value["schema"], VISP_HOOK_SCHEMA);
        assert_eq!(value["session_id"], "sess");
        assert!(value.get("seq").is_none(), "seq 为 None 时应省略");

        let decoded = HookEvent::from_value(value).expect("产物应能被契约反序列化");
        assert_eq!(decoded, event);
    }
}
