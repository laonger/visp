//! 进程内事件总线（设计 §5 D3）。
//!
//! 基于 `tokio::sync::broadcast`：单源、连接无关、生产者永不阻塞，
//! 订阅者滞后（满 1024）时仅丢最旧并得到 `Lagged(n)`。
//!
//! 本模块当前尚未接线（main.rs/service.rs 仍是旧的 `orchestrator_grpc_tx/rx`）；
//! 接线在 1a-2 移除下面的 `dead_code` 允许。

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use tokio::sync::broadcast;
use visp_core::agent::AgentEventFrame;

/// 总线容量（设计 §5 D3：1024 起步）。
const BUS_CAPACITY: usize = 1024;

/// 总线信封：事件帧 + 发布序号。
#[derive(Clone)]
pub struct BusEnvelope {
    /// 发布序号；`base = epoch_ms`，每次发布 +1。
    pub seq: u64,
    /// 被广播的 agent 事件帧。
    pub frame: AgentEventFrame,
}

/// 事件总线。
///
/// 常持一个 sentinel `Receiver`，使「无其它订阅者」时 `send` 也不报错。
#[allow(dead_code)] // 接线在 1a-2 移除
pub struct EventBus {
    tx: broadcast::Sender<BusEnvelope>,
    /// 单一生序号，初值为启动时刻 epoch 毫秒。
    seq: AtomicU64,
    /// sentinel：常持一个 receiver，保证无外部订阅者时 `send` 不返回错误。
    _sentinel: broadcast::Receiver<BusEnvelope>,
}

#[allow(dead_code)] // 接线在 1a-2 移除
impl EventBus {
    /// 创建总线：容量 1024，seq 基数为当前 epoch 毫秒。
    #[allow(clippy::new_without_default)]
    pub fn new() -> Self {
        let (tx, sentinel) = broadcast::channel(BUS_CAPACITY);
        Self {
            tx,
            seq: AtomicU64::new(epoch_ms()),
            _sentinel: sentinel,
        }
    }

    /// 发布一帧：发放 seq 后广播。
    ///
    /// **不返回任何错误**：无订阅者、订阅者 lagged 均被忽略，发布端永不阻塞、
    /// 调用方不会因发布结果而 `break`。
    pub fn publish(&self, frame: AgentEventFrame) {
        let seq = self.seq.fetch_add(1, Ordering::Relaxed);
        let _ = self.tx.send(BusEnvelope { seq, frame });
    }

    /// 订阅总线。只收到订阅之后发布的事件；连接结束即 drop。
    pub fn subscribe(&self) -> broadcast::Receiver<BusEnvelope> {
        self.tx.subscribe()
    }

    /// 当前序号（已发放的最后一个 seq；未发布时即 base）。
    pub fn seq(&self) -> u64 {
        self.seq.load(Ordering::Relaxed)
    }
}

/// 当前 epoch 毫秒。
fn epoch_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;
    use tokio::sync::broadcast::error::{RecvError, TryRecvError};

    fn text_frame(text: &str) -> AgentEventFrame {
        AgentEventFrame {
            event: visp_core::agent::AgentEvent::TextDelta(text.to_string()),
            session_id: "sess".to_string(),
            agent_name: "agent".to_string(),
            parent_session_id: None,
            parent_session_name: None,
        }
    }

    fn text_of(env: &BusEnvelope) -> String {
        match &env.frame.event {
            visp_core::agent::AgentEvent::TextDelta(text) => text.clone(),
            _ => panic!("expected TextDelta frame"),
        }
    }

    /// 1. 多订阅者各自收到全量帧，顺序一致。
    #[tokio::test]
    async fn multiple_subscribers_receive_all_frames_in_order() {
        let bus = EventBus::new();
        let mut a = bus.subscribe();
        let mut b = bus.subscribe();

        const N: usize = 5;
        for i in 0..N {
            bus.publish(text_frame(&format!("frame-{i}")));
        }

        for rx in [&mut a, &mut b] {
            for i in 0..N {
                let env = rx.recv().await.unwrap();
                assert_eq!(text_of(&env), format!("frame-{i}"));
            }
        }
    }

    /// 2. 订阅前发布的事件会丢（与 mpsc 单消费者语义的显式差异）。
    #[tokio::test]
    async fn events_published_before_subscribe_are_lost() {
        let bus = EventBus::new();
        bus.publish(text_frame("before"));

        let mut rx = bus.subscribe();
        assert!(matches!(rx.try_recv(), Err(TryRecvError::Empty)));

        bus.publish(text_frame("after"));
        let env = rx.recv().await.unwrap();
        assert_eq!(text_of(&env), "after");
    }

    /// 3. 无其它订阅者时 publish 不报错（sentinel receiver 兜底）。
    #[tokio::test]
    async fn publish_without_subscribers_does_not_fail() {
        let bus = EventBus::new();
        assert_eq!(bus.tx.receiver_count(), 1, "sentinel must be held");

        let before = bus.seq();
        bus.publish(text_frame("lonely"));
        assert_eq!(bus.seq(), before + 1);
    }

    /// 4. publish 后订阅者收到，且发布不阻塞。
    #[tokio::test]
    async fn publish_is_received_without_blocking() {
        let bus = EventBus::new();
        let mut rx = bus.subscribe();

        bus.publish(text_frame("hello"));
        let env = tokio::time::timeout(std::time::Duration::from_millis(500), rx.recv())
            .await
            .expect("publish should not block receiver")
            .unwrap();
        assert_eq!(text_of(&env), "hello");
    }

    /// 5. seq 单调严格递增；base ≈ epoch_ms；无重复。
    #[tokio::test]
    async fn seq_is_monotonic_and_based_on_epoch_ms() {
        let before = epoch_ms();
        let bus = EventBus::new();
        let base = bus.seq();
        let after = epoch_ms();
        assert!(
            before <= base && base <= after,
            "base {base} not within epoch_ms window [{before}, {after}]"
        );

        let mut rx = bus.subscribe();
        const N: u64 = 100;
        for i in 0..N {
            bus.publish(text_frame(&format!("f-{i}")));
        }
        let mut seqs = Vec::new();
        for _ in 0..N {
            seqs.push(rx.recv().await.unwrap().seq);
        }

        for pair in seqs.windows(2) {
            assert!(pair[1] > pair[0], "seq must strictly increase");
        }
        assert_eq!(seqs[0], base, "first seq starts at base");
        let unique: HashSet<u64> = seqs.iter().copied().collect();
        assert_eq!(unique.len() as u64, N, "seq must be unique");
    }

    /// 6. 容量满 → 订阅者读到 `Lagged(n)`，发布端不阻塞。
    #[tokio::test]
    async fn overflow_yields_lagged_without_blocking_publisher() {
        let bus = EventBus::new();
        let mut rx = bus.subscribe();

        // 发布超过容量：若发布端会阻塞，本循环无法返回。
        for i in 0..(BUS_CAPACITY + 10) {
            bus.publish(text_frame(&format!("f-{i}")));
        }

        match rx.recv().await {
            Err(RecvError::Lagged(n)) => assert!(n > 0),
            _ => panic!("expected Lagged after exceeding capacity"),
        }
    }
}
