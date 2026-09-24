use super::*;
use std::collections::VecDeque;
use std::time::{Duration, Instant};
use visp_proto::visp::ServerMessage;

// ── 7a-1：recv=None（流干净关闭）→ Reconnecting ─────────────────────

#[test]
fn test_stream_closed_enters_reconnecting() {
    let (state, action) = transition(ConnState::Connected, ConnEvent::StreamClosed);
    assert_eq!(
        state,
        ConnState::Reconnecting { attempt: 0 },
        "流干净关闭应立即迁移到 Reconnecting"
    );
    assert_eq!(action, ConnAction::Reconnect, "应调度首次重连尝试");
}

/// 用户退出事件无条件生效（两个状态都必须退出）。
#[test]
fn test_user_exit_applies_unconditionally() {
    assert_eq!(
        transition(ConnState::Connected, ConnEvent::UserExit),
        (ConnState::Connected, ConnAction::Exit)
    );
    assert_eq!(
        transition(ConnState::Reconnecting { attempt: 5 }, ConnEvent::UserExit),
        (ConnState::Reconnecting { attempt: 5 }, ConnAction::Exit),
        "重连期间 Ctrl+D 必须仍可退出"
    );
}

// ── 7a-2：idle 超时 + 探测失败 → Reconnecting ──────────────────────

#[test]
fn test_idle_timeout_probe_failed_enters_reconnecting() {
    // idle 到期只触发探测，不直接迁移
    let (s1, a1) = transition(ConnState::Connected, ConnEvent::IdleTimeout);
    assert_eq!(a1, ConnAction::Probe);
    assert!(s1.is_connected(), "idle 到期本身不迁移状态");

    // 探测失败/超时 → 立即重连
    let (s2, a2) = transition(s1, ConnEvent::ProbeFailed);
    assert_eq!(s2, ConnState::Reconnecting { attempt: 0 });
    assert_eq!(a2, ConnAction::Reconnect);
}

// ── 7a-3：idle 超时 + 探测成功 → 重置（不迁移、计时重置）────────────

#[test]
fn test_idle_timeout_probe_success_resets_without_migration() {
    let (s1, _) = transition(ConnState::Connected, ConnEvent::IdleTimeout);
    let (s2, a2) = transition(s1, ConnEvent::ProbeSuccess);
    assert_eq!(s2, ConnState::Connected, "探测成功不得触发任何重建");
    assert_eq!(a2, ConnAction::ResetIdle, "探测成功应重置 idle 计时");
}

// ── 7a-4：退避序列与重连成功归零 ───────────────────────────────────

#[test]
fn test_backoff_sequence_and_reset_after_reconnect() {
    let seq: Vec<u64> = (0..7).map(|a| backoff_delay(a).as_secs()).collect();
    assert_eq!(seq, vec![1, 2, 4, 8, 16, 30, 30], "1s 起步、倍增、30s 封顶");

    // 重连成功 → Connected（尝试计数归零）
    let (state, action) = transition(
        ConnState::Reconnecting { attempt: 3 },
        ConnEvent::Reconnected,
    );
    assert_eq!(state, ConnState::Connected);
    assert_eq!(action, ConnAction::ResetIdle);

    // 再次断线从 attempt=0 重新计数
    let (state, action) = transition(state, ConnEvent::StreamClosed);
    assert_eq!(state, ConnState::Reconnecting { attempt: 0 });
    assert_eq!(action, ConnAction::Reconnect);
}

/// 重连尝试失败递增计数并按新计数继续重连。
#[test]
fn test_reconnect_failure_increments_attempt() {
    let (state, action) = transition(
        ConnState::Reconnecting { attempt: 2 },
        ConnEvent::ReconnectFailed,
    );
    assert_eq!(state, ConnState::Reconnecting { attempt: 3 });
    assert_eq!(action, ConnAction::Reconnect);
}

// ── 7a-5：exit 通道保留（已在上方 test_user_exit_applies_unconditionally）─

// ── 7a-6：重连成功 → Connected ─────────────────────────────────────

#[test]
fn test_reconnect_success_returns_connected() {
    let (state, action) = transition(
        ConnState::Reconnecting { attempt: 1 },
        ConnEvent::Reconnected,
    );
    assert_eq!(state, ConnState::Connected);
    assert_eq!(action, ConnAction::ResetIdle);
}

// ── 7a-7：负向断言——连续多次探测成功不触发任何重建 ─────────────────

#[test]
fn test_repeated_probe_success_never_reconnects() {
    let mut state = ConnState::Connected;
    for i in 0..5 {
        let (next, action) = transition(state, ConnEvent::ProbeSuccess);
        assert!(
            next.is_connected(),
            "第 {i} 次探测成功不得触发重建（无第三路径）"
        );
        assert_eq!(action, ConnAction::ResetIdle);
        state = next;
    }
}

/// 重连期间的陈旧信号（流关闭/idle/探测结果）一律忽略。
#[test]
fn test_stale_events_ignored_while_reconnecting() {
    let reconnecting = ConnState::Reconnecting { attempt: 2 };
    for event in [
        ConnEvent::StreamClosed,
        ConnEvent::IdleTimeout,
        ConnEvent::ProbeSuccess,
        ConnEvent::ProbeFailed,
    ] {
        let (state, action) = transition(reconnecting, event);
        assert_eq!(state, reconnecting, "{event:?} 在重连期间应被忽略");
        assert_eq!(action, ConnAction::None);
    }
}

// ── 7c-1 / 7c-3 / 7c-4：idle 判定与周期探测稳态 ────────────────────

#[test]
fn test_idle_clock_triggers_at_45s() {
    let t0 = Instant::now();
    let clock = IdleClock::new(t0);
    assert!(!clock.expired(t0 + Duration::from_secs(44)), "44s 未到期");
    assert!(clock.expired(t0 + Duration::from_secs(45)), "45s 到期");
    assert!(
        clock.expired(t0 + Duration::from_secs(120)),
        "持续超时仍到期"
    );
}

#[test]
fn test_frame_arrival_resets_idle_timer() {
    let t0 = Instant::now();
    let mut clock = IdleClock::new(t0);
    // 40s 时收到一帧 → 计时重置
    clock.on_frame(t0 + Duration::from_secs(40));
    assert!(
        !clock.expired(t0 + Duration::from_secs(84)),
        "收帧后 44s 不应到期"
    );
    assert!(
        clock.expired(t0 + Duration::from_secs(85)),
        "收帧后 45s 到期"
    );
}

#[test]
fn test_probe_success_cycles_periodically() {
    let t0 = Instant::now();
    let mut clock = IdleClock::new(t0);

    // 第一周期：45s 到期 → 探测 → 成功 → 重置
    assert!(clock.expired(t0 + Duration::from_secs(45)));
    let (state, action) = transition(ConnState::Connected, ConnEvent::IdleTimeout);
    assert_eq!(action, ConnAction::Probe);
    let (state, action) = transition(state, ConnEvent::ProbeSuccess);
    assert_eq!(action, ConnAction::ResetIdle);
    assert!(state.is_connected());

    // 重置后进入下一 45s 周期（稳态：无重连）
    let reset_at = t0 + Duration::from_secs(45);
    clock.reset(reset_at);
    assert!(!clock.expired(reset_at + Duration::from_secs(44)));
    assert!(clock.expired(reset_at + Duration::from_secs(45)));

    let (state, action) = transition(state, ConnEvent::IdleTimeout);
    assert_eq!(action, ConnAction::Probe);
    assert!(state.is_connected(), "周期探测不得产生任何重建");
}

// ── 7c-2：探测超时按失败处理 ───────────────────────────────────────

#[tokio::test(start_paused = true)]
async fn test_probe_timeout_treated_as_failure() {
    let result = probe_with_timeout(async {
        tokio::time::sleep(Duration::from_secs(10)).await;
        Ok(true)
    })
    .await;
    assert!(result.is_err(), "探测无响应（超时）应按失败处理");

    // 超时结果交状态机 → 进入 Reconnecting
    let (state, action) = transition(ConnState::Connected, ConnEvent::ProbeFailed);
    assert_eq!(state, ConnState::Reconnecting { attempt: 0 });
    assert_eq!(action, ConnAction::Reconnect);
}

// ── 7c-5：常量集中核对（45s / 5s 仅在连接模块定义）────────────────

#[test]
fn test_watchdog_constants_centralized() {
    assert_eq!(IDLE_TIMEOUT, Duration::from_secs(45));
    assert_eq!(PROBE_TIMEOUT, Duration::from_secs(5));
    assert_eq!(BACKOFF_INITIAL, Duration::from_secs(1));
    assert_eq!(BACKOFF_MAX, Duration::from_secs(30));

    // event.rs 只应引用本模块常量，不得内联 45s / 5s 看门狗字面量。
    let event_src = include_str!("event.rs");
    assert!(
        !event_src.contains("from_secs(45)"),
        "idle 阈值应仅定义在 connection 模块"
    );
    assert!(
        !event_src.contains("from_secs(5)"),
        "探测超时应仅定义在 connection 模块"
    );
}

// ── 脚本化假体连接：帧序列 + 故障注入 ──────────────────────────────

/// 假体连接：脚本化帧序列与故障注入（流关闭 / 无响应 / 延迟）。
/// 沿用 `ChatHandle::new_mock` 的既有假体先例。
struct FakeConnection {
    establish_result: Result<(), String>,
    probe_result: Result<bool, String>,
    models: Result<(Vec<String>, Vec<String>), String>,
    frames: VecDeque<ServerMessage>,
    established: bool,
    joined: bool,
    cancelled: bool,
    dropped: bool,
}

impl FakeConnection {
    fn healthy() -> Self {
        Self {
            establish_result: Ok(()),
            probe_result: Ok(true),
            models: Ok((vec!["gpt-x".into()], vec!["gpt-x".into()])),
            frames: VecDeque::new(),
            established: false,
            joined: false,
            cancelled: false,
            dropped: false,
        }
    }
}

impl Connection for FakeConnection {
    async fn establish(&mut self) -> Result<(), String> {
        self.establish_result.clone()?;
        self.established = true;
        Ok(())
    }

    async fn recv(&mut self) -> Option<ServerMessage> {
        self.frames.pop_front()
    }

    fn send_join(&mut self) {
        self.joined = true;
    }

    fn send_cancel(&mut self) {
        self.cancelled = true;
    }

    async fn health_probe(&mut self) -> Result<bool, String> {
        self.probe_result.clone()
    }

    async fn refresh_models(&mut self) -> Result<(Vec<String>, Vec<String>), String> {
        self.models.clone()
    }

    fn drop_old(&mut self) {
        self.dropped = true;
    }
}

#[tokio::test]
async fn test_fake_connection_recv_stream_ends_as_none() {
    let mut conn = FakeConnection::healthy();
    // 帧序列为空 → 流结束（recv=None），对应干净关闭入口
    assert!(conn.recv().await.is_none());
    let (state, _) = transition(ConnState::Connected, ConnEvent::StreamClosed);
    assert!(state.is_reconnecting());
}

#[tokio::test]
async fn test_recover_fails_on_establish_error() {
    let mut conn = FakeConnection::healthy();
    conn.establish_result = Err("connection refused".into());
    let result = recover(&mut conn).await;
    assert!(result.is_err(), "建立连接失败应使本次恢复尝试失败");
    assert!(!conn.established);
}

#[tokio::test]
async fn test_recover_fails_on_probe_failure() {
    let mut conn = FakeConnection::healthy();
    conn.probe_result = Err("health probe timed out".into());
    let result = recover(&mut conn).await;
    assert!(result.is_err(), "探测失败/超时应使恢复失败");
    assert!(conn.established, "建立连接本身成功");
    assert!(conn.dropped, "探测失败应主动丢弃旧连接");
}

#[tokio::test]
async fn test_recover_fails_on_probe_not_alive() {
    let mut conn = FakeConnection::healthy();
    conn.probe_result = Ok(false);
    let result = recover(&mut conn).await;
    assert!(result.is_err());
    assert!(conn.dropped);
}

#[tokio::test]
async fn test_recover_success_joins_and_refreshes_models() {
    let mut conn = FakeConnection::healthy();
    let recovered = recover(&mut conn).await.expect("恢复应成功");
    assert!(conn.established, "应先建立新连接");
    assert!(conn.joined, "应 send_join 回放主 session");
    assert_eq!(recovered.available_models, vec!["gpt-x".to_string()]);
    assert_eq!(recovered.model_keys, vec!["gpt-x".to_string()]);
}

/// refresh_models 非致命：失败时恢复仍成功，模型列表为空。
#[tokio::test]
async fn test_recover_model_refresh_failure_is_non_fatal() {
    let mut conn = FakeConnection::healthy();
    conn.models = Err("get_session failed".into());
    let recovered = recover(&mut conn)
        .await
        .expect("刷新模型失败不应让恢复失败");
    assert!(recovered.available_models.is_empty());
    assert!(conn.joined);
}
