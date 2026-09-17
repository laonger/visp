use super::*;

/// 便捷构造测试环境（避免污染进程环境变量）。
fn env(kitty_window_id: Option<&str>, term: Option<&str>, term_program: Option<&str>) -> Env {
    Env::from_pairs(kitty_window_id, term, term_program)
}

#[test]
fn kitty_window_id_present_detects_kitty99() {
    let e = env(Some("12345"), None, None);
    assert_eq!(detect(&e, None, true), Some(Protocol::Kitty99));
}

#[test]
fn term_xterm_kitty_detects_kitty99() {
    let e = env(None, Some("xterm-kitty"), None);
    assert_eq!(detect(&e, None, true), Some(Protocol::Kitty99));
}

#[test]
fn term_program_wezterm_detects_osc777() {
    let e = env(None, None, Some("WezTerm"));
    assert_eq!(detect(&e, None, true), Some(Protocol::Osc777));
}

#[test]
fn term_program_ghostty_detects_osc9() {
    let e = env(None, None, Some("ghostty"));
    assert_eq!(detect(&e, None, true), Some(Protocol::Osc9));
}

#[test]
fn term_program_iterm_app_detects_osc9() {
    let e = env(None, None, Some("iTerm.app"));
    assert_eq!(detect(&e, None, true), Some(Protocol::Osc9));
}

#[test]
fn term_xterm_ghostty_detects_osc9() {
    // SSH 透传场景：TERM_PROGRAM 为空，TERM=xterm-ghostty。
    let e = env(None, Some("xterm-ghostty"), None);
    assert_eq!(detect(&e, None, true), Some(Protocol::Osc9));
}

#[test]
fn empty_env_falls_back_to_osc9() {
    // 全空环境 → Osc9（盲发兜底）。
    let e = env(None, None, None);
    assert_eq!(detect(&e, None, true), Some(Protocol::Osc9));
}

#[test]
fn override_wins_over_probe() {
    // 环境为 ghostty，但配置 override 为 Kitty99 → Kitty99（配置优先）。
    let e = env(None, None, Some("ghostty"));
    assert_eq!(
        detect(&e, Some(Protocol::Kitty99), true),
        Some(Protocol::Kitty99)
    );
}

#[test]
fn non_tty_disables_even_with_override() {
    // is_tty=false → None，override 存在也是 None。
    let e = env(None, None, Some("ghostty"));
    assert_eq!(detect(&e, Some(Protocol::Kitty99), false), None);
}

#[test]
fn kitty_beats_ghostty_priority() {
    // KITTY_WINDOW_ID 与 TERM_PROGRAM=ghostty 同时存在 → Kitty99
    // （优先级：kitty > 777 系 > 9 系）。
    let e = env(Some("42"), None, Some("ghostty"));
    assert_eq!(detect(&e, None, true), Some(Protocol::Kitty99));
}

#[test]
fn disabled_engine_returns_none() {
    let mut engine = NotifyEngine::disabled();
    assert_eq!(engine.selected_protocol(), None);
    let now = Instant::now();
    assert_eq!(engine.on_event(NotifyKind::Done, true, "done", now), None);
    assert_eq!(
        engine.on_event(NotifyKind::UserQuery, false, "query", now),
        None
    );
}

#[test]
fn term_program_case_insensitive() {
    // 规格要求 TERM_PROGRAM 比较不区分大小写。
    let e = env(None, None, Some("wezterm"));
    assert_eq!(detect(&e, None, true), Some(Protocol::Osc777));
    let e = env(None, None, Some("ITerm.APP"));
    assert_eq!(detect(&e, None, true), Some(Protocol::Osc9));
}

// ── Wave 2：BEL 编码 + 节流闸门（计划文档 step 3b，裁剪为 BEL 版）──────
// 时间全部由参数注入（固定 Instant 时间线），禁止 sleep。
// 注意：文档中 OSC 专属用例（D3 空 body 不发送、D4 超长截断）不适用于
// BEL——BEL 不带文本负载，body 被忽略。

/// 便捷构造：tty + 全开 + 指定 min_interval。
fn engine(min_interval: Duration) -> NotifyEngine {
    NotifyEngine::new(true, None, true, true, true, min_interval)
}

#[test]
fn first_done_returns_bell() {
    let mut e = engine(Duration::from_secs(3));
    let t0 = Instant::now();
    assert_eq!(
        e.on_event(NotifyKind::Done, true, "done", t0),
        Some(vec![0x07])
    );
}

#[test]
fn same_kind_within_interval_is_throttled() {
    let mut e = engine(Duration::from_secs(3));
    let t0 = Instant::now();
    assert_eq!(
        e.on_event(NotifyKind::Done, true, "done", t0),
        Some(vec![0x07])
    );
    // 3s 内第二次 → 节流。
    assert_eq!(
        e.on_event(NotifyKind::Done, true, "done", t0 + Duration::from_secs(1)),
        None
    );
    assert_eq!(
        e.on_event(NotifyKind::Done, true, "done", t0 + Duration::from_secs(2)),
        None
    );
}

#[test]
fn same_kind_after_interval_sends_again() {
    let mut e = engine(Duration::from_secs(3));
    let t0 = Instant::now();
    assert_eq!(
        e.on_event(NotifyKind::Done, true, "done", t0),
        Some(vec![0x07])
    );
    // 距上次 ≥3s → 再次发送。
    assert_eq!(
        e.on_event(NotifyKind::Done, true, "done", t0 + Duration::from_secs(3)),
        Some(vec![0x07])
    );
}

#[test]
fn kinds_throttle_independently() {
    let mut e = engine(Duration::from_secs(3));
    let t0 = Instant::now();
    assert_eq!(
        e.on_event(NotifyKind::Done, true, "done", t0),
        Some(vec![0x07])
    );
    // Done 刚发送后立刻 UserQuery → 独立计时，仍发送。
    assert_eq!(
        e.on_event(NotifyKind::UserQuery, true, "query", t0),
        Some(vec![0x07])
    );
}

#[test]
fn zero_interval_never_throttles() {
    let mut e = engine(Duration::ZERO);
    let t0 = Instant::now();
    for i in 0..3 {
        assert_eq!(
            e.on_event(NotifyKind::Done, true, "done", t0 + Duration::from_secs(i)),
            Some(vec![0x07])
        );
    }
}

#[test]
fn sub_session_never_rings() {
    let mut e = engine(Duration::ZERO);
    let t0 = Instant::now();
    assert_eq!(e.on_event(NotifyKind::Done, false, "done", t0), None);
    assert_eq!(e.on_event(NotifyKind::UserQuery, false, "query", t0), None);
}

#[test]
fn disabled_config_returns_none() {
    let mut e = NotifyEngine::new(true, None, false, true, true, Duration::from_secs(3));
    let t0 = Instant::now();
    assert_eq!(e.on_event(NotifyKind::Done, true, "done", t0), None);
    assert_eq!(e.on_event(NotifyKind::UserQuery, true, "query", t0), None);
}

#[test]
fn on_complete_off_blocks_done_only() {
    let mut e = NotifyEngine::new(true, None, true, false, true, Duration::from_secs(3));
    let t0 = Instant::now();
    assert_eq!(e.on_event(NotifyKind::Done, true, "done", t0), None);
    assert_eq!(
        e.on_event(NotifyKind::UserQuery, true, "query", t0),
        Some(vec![0x07])
    );
}

#[test]
fn on_user_input_off_blocks_user_query_only() {
    let mut e = NotifyEngine::new(true, None, true, true, false, Duration::from_secs(3));
    let t0 = Instant::now();
    assert_eq!(e.on_event(NotifyKind::UserQuery, true, "query", t0), None);
    assert_eq!(
        e.on_event(NotifyKind::Done, true, "done", t0),
        Some(vec![0x07])
    );
}

#[test]
fn encode_bell_returns_single_bel_byte() {
    assert_eq!(BEL, 0x07);
    assert_eq!(encode_bell(), vec![0x07]);
}
