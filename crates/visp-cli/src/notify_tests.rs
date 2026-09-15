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
