//! 总线事件载荷与发布契约（设计 §5 D3 v3.1）。
//!
//! 总线载荷泛化为 [`BusEvent`]：显示域（TUI 消费）与 hook 域（hook 执行器消费）
//! 共用同一总线。`AgentEvent` 保持纯显示语义。

use crate::agent::AgentEventFrame;

/// 总线事件载荷。
#[derive(Clone)]
pub enum BusEvent {
    /// 显示域事件帧，由 TUI 消费。
    Frame(AgentEventFrame),
    /// hook 域事件，由 hook 执行器消费。
    Hook(visp_hooks::HookEvent),
}

/// 事件发布者：向总线投递 [`BusEvent`]。
pub trait EventPublisher: Send + Sync {
    fn publish(&self, event: BusEvent);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::{AgentEvent, AgentEventFrame};
    use visp_hooks::{
        HookContext, HookEvent, HookEventName, HookPayload, Origin, SessionEndPayload,
        SessionSource,
    };

    fn sample_frame() -> AgentEventFrame {
        AgentEventFrame {
            event: AgentEvent::Done,
            session_id: "s1".into(),
            agent_name: "agent".into(),
            parent_session_id: None,
            parent_session_name: None,
        }
    }

    fn sample_hook() -> HookEvent {
        HookEvent {
            context: HookContext {
                schema: 1,
                hook_event_name: HookEventName::SessionEnd,
                session_id: "s1".into(),
                cwd: "/tmp".into(),
                source: SessionSource::Startup,
                origin: Origin::Tui,
                seq: Some(1),
            },
            payload: HookPayload::SessionEnd(SessionEndPayload {
                reason: "quit".into(),
                exit_code: Some(0),
            }),
        }
    }

    #[test]
    fn frame_variant_constructs_and_matches() {
        let frame = sample_frame();
        let bus = BusEvent::Frame(frame);
        match bus {
            BusEvent::Frame(f) => {
                assert_eq!(f.session_id, "s1");
                assert_eq!(f.agent_name, "agent");
            }
            BusEvent::Hook(_) => panic!("expected Frame variant"),
        }
    }

    #[test]
    fn hook_variant_constructs_and_matches() {
        let hook = sample_hook();
        let bus = BusEvent::Hook(hook);
        match bus {
            BusEvent::Hook(h) => assert_eq!(h.event_name(), HookEventName::SessionEnd),
            BusEvent::Frame(_) => panic!("expected Hook variant"),
        }
    }

    #[test]
    fn bus_event_is_clone() {
        let bus = BusEvent::Frame(sample_frame());
        let cloned = bus.clone();
        match (bus, cloned) {
            (BusEvent::Frame(a), BusEvent::Frame(b)) => {
                assert_eq!(a.session_id, b.session_id);
                assert_eq!(a.agent_name, b.agent_name);
            }
            _ => panic!("expected Frame variants"),
        }
    }
}
