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
fn empty_env_falls_back_to_bel() {
    // 全空环境 → Bel（未识别终端兜底，不盲发 OSC 9）。
    let e = env(None, None, None);
    assert_eq!(detect(&e, None, true), Some(Protocol::Bel));
}

#[test]
fn unknown_term_program_falls_back_to_bel() {
    // 未识别 TERM_PROGRAM（如 rmux / tmux 等复用器）→ Bel 兜底，
    // 不盲发 OSC 9（复用器会丢弃 OSC，导致通知静默）。
    let e = env(None, Some("xterm"), Some("rmux"));
    assert_eq!(detect(&e, None, true), Some(Protocol::Bel));
    let e = env(None, Some("screen"), Some("tmux"));
    assert_eq!(detect(&e, None, true), Some(Protocol::Bel));
}

#[test]
fn recognized_terminals_unaffected_by_bel_fallback() {
    // 已识别终端不受 Bel 兜底影响：仍走各自 OSC 协议。
    let e = env(None, None, Some("ghostty"));
    assert_eq!(detect(&e, None, true), Some(Protocol::Osc9));
    let e = env(None, None, Some("WezTerm"));
    assert_eq!(detect(&e, None, true), Some(Protocol::Osc777));
    let e = env(Some("1"), None, None);
    assert_eq!(detect(&e, None, true), Some(Protocol::Kitty99));
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
    assert_eq!(engine.on_event(NotifyKind::Done, DONE_TEXT, now), None);
    assert_eq!(engine.on_event(NotifyKind::UserQuery, "query", now), None);
}

#[test]
fn term_program_case_insensitive() {
    // 规格要求 TERM_PROGRAM 比较不区分大小写。
    let e = env(None, None, Some("wezterm"));
    assert_eq!(detect(&e, None, true), Some(Protocol::Osc777));
    let e = env(None, None, Some("ITerm.APP"));
    assert_eq!(detect(&e, None, true), Some(Protocol::Osc9));
}

// ── Wave 2/3b：引擎编排（计划文档 step 3b，OSC 版）────────────────────
// 时间全部由参数注入（固定 Instant 时间线），禁止 sleep。
// 协议经 override 固定，断言按该协议编码的 OSC 字节（等价场景覆盖）。

/// 便捷构造：tty + 指定协议 + 全开 + 指定 min_interval。
fn engine(protocol: Protocol, min_interval: Duration) -> NotifyEngine {
    NotifyEngine::new(true, Some(protocol), true, true, true, min_interval)
}

/// 与 `event.rs::DONE_BODY` 同值：挂接点传给引擎的 Done 正文。
/// 引擎按调用方正文编码（不再内置文案），故测试须显式传入同一文案。
const DONE_TEXT: &str = "任务完成，等待输入";

#[test]
fn first_done_returns_osc_sequence() {
    let mut e = engine(Protocol::Osc9, Duration::from_secs(3));
    let t0 = Instant::now();
    assert_eq!(
        e.on_event(NotifyKind::Done, DONE_TEXT, t0),
        Some("\x1b]9;任务完成，等待输入\x1b\\".as_bytes().to_vec())
    );
}

#[test]
fn same_kind_within_interval_is_throttled() {
    let mut e = engine(Protocol::Osc9, Duration::from_secs(3));
    let t0 = Instant::now();
    assert_eq!(
        e.on_event(NotifyKind::Done, DONE_TEXT, t0),
        Some("\x1b]9;任务完成，等待输入\x1b\\".as_bytes().to_vec())
    );
    // 3s 内第二次 → 节流。
    assert_eq!(
        e.on_event(NotifyKind::Done, DONE_TEXT, t0 + Duration::from_secs(1)),
        None
    );
    assert_eq!(
        e.on_event(NotifyKind::Done, DONE_TEXT, t0 + Duration::from_secs(2)),
        None
    );
}

#[test]
fn same_kind_after_interval_sends_again() {
    let mut e = engine(Protocol::Osc9, Duration::from_secs(3));
    let t0 = Instant::now();
    assert_eq!(
        e.on_event(NotifyKind::Done, DONE_TEXT, t0),
        Some("\x1b]9;任务完成，等待输入\x1b\\".as_bytes().to_vec())
    );
    // 距上次 ≥3s → 再次发送。
    assert_eq!(
        e.on_event(NotifyKind::Done, DONE_TEXT, t0 + Duration::from_secs(3)),
        Some("\x1b]9;任务完成，等待输入\x1b\\".as_bytes().to_vec())
    );
}

#[test]
fn kinds_throttle_independently() {
    let mut e = engine(Protocol::Osc9, Duration::from_secs(3));
    let t0 = Instant::now();
    assert_eq!(
        e.on_event(NotifyKind::Done, DONE_TEXT, t0),
        Some("\x1b]9;任务完成，等待输入\x1b\\".as_bytes().to_vec())
    );
    // Done 刚发送后立刻 UserQuery → 独立计时，仍发送。
    assert_eq!(
        e.on_event(NotifyKind::UserQuery, "query", t0),
        Some(b"\x1b]9;query\x1b\\".to_vec())
    );
}

#[test]
fn zero_interval_never_throttles() {
    let mut e = engine(Protocol::Osc9, Duration::ZERO);
    let t0 = Instant::now();
    for i in 0..3 {
        assert_eq!(
            e.on_event(NotifyKind::Done, DONE_TEXT, t0 + Duration::from_secs(i)),
            Some("\x1b]9;任务完成，等待输入\x1b\\".as_bytes().to_vec())
        );
    }
}

// 会话过滤（仅根 session）不属 notify 职责；覆盖见 event_tests 的
// test_done_hookup_skips_sub_session / test_user_query_hookup_skips_sub_session。

#[test]
fn disabled_config_returns_none() {
    let mut e = NotifyEngine::new(
        true,
        Some(Protocol::Osc9),
        false,
        true,
        true,
        Duration::from_secs(3),
    );
    let t0 = Instant::now();
    assert_eq!(e.on_event(NotifyKind::Done, DONE_TEXT, t0), None);
    assert_eq!(e.on_event(NotifyKind::UserQuery, "query", t0), None);
}

#[test]
fn on_complete_off_blocks_done_only() {
    let mut e = NotifyEngine::new(
        true,
        Some(Protocol::Osc9),
        true,
        false,
        true,
        Duration::from_secs(3),
    );
    let t0 = Instant::now();
    assert_eq!(e.on_event(NotifyKind::Done, DONE_TEXT, t0), None);
    assert_eq!(
        e.on_event(NotifyKind::UserQuery, "query", t0),
        Some(b"\x1b]9;query\x1b\\".to_vec())
    );
}

#[test]
fn on_user_input_off_blocks_user_query_only() {
    let mut e = NotifyEngine::new(
        true,
        Some(Protocol::Osc9),
        true,
        true,
        false,
        Duration::from_secs(3),
    );
    let t0 = Instant::now();
    assert_eq!(e.on_event(NotifyKind::UserQuery, "query", t0), None);
    assert_eq!(
        e.on_event(NotifyKind::Done, DONE_TEXT, t0),
        Some("\x1b]9;任务完成，等待输入\x1b\\".as_bytes().to_vec())
    );
}

#[test]
fn encode_bell_returns_single_bel_byte() {
    // encode_bell 已接线（Protocol::Bel 兜底路径）：仅 BEL，无文本负载。
    assert_eq!(BEL, 0x07);
    assert_eq!(encode_bell(), vec![0x07]);
}

#[test]
fn bel_fallback_engine_rings_bell() {
    // 用户环境回归（Ghostty → rmux → visp）：rmux 未识别 → Bel，
    // Done 事件输出单个 BEL（0x07），保住响铃/🔔 提示。
    let e = env(None, Some("xterm"), Some("rmux"));
    assert_eq!(detect(&e, None, true), Some(Protocol::Bel));
    let mut engine = engine(Protocol::Bel, Duration::ZERO);
    let t0 = Instant::now();
    assert_eq!(
        engine.on_event(NotifyKind::Done, DONE_TEXT, t0),
        Some(vec![0x07])
    );
}

#[test]
fn encode_bel_emits_single_bel_byte() {
    // BEL 分支：仅 0x07，不含任何 ESC 序列、无文本负载。
    assert_eq!(encode(Protocol::Bel, "visp", "hi"), Some(vec![0x07]));
    assert_eq!(encode(Protocol::Bel, "", "任务完成"), Some(vec![0x07]));
    let out = encode(Protocol::Bel, "visp", "hi").unwrap();
    assert!(!out.contains(&0x1b));
    assert_eq!(out.len(), 1);
}

#[test]
fn encode_bel_empty_body_returns_none() {
    // 决策 D3 对 BEL 分支同样生效：空 body / 清洗后为空 → 不响铃。
    assert_eq!(encode(Protocol::Bel, "visp", ""), None);
    assert_eq!(encode(Protocol::Bel, "visp", "\x1b\x07\x01"), None);
}

// ── Wave 3b：文案规则（设计 §2.1）─────────────────────────────────────

#[test]
fn user_query_body_truncated_to_200_chars() {
    // 决策 D4：engine 层把 UserQuery body 按「字符」截断至 200。
    let mut e = engine(Protocol::Osc9, Duration::ZERO);
    let t0 = Instant::now();
    let out = e
        .on_event(NotifyKind::UserQuery, &"a".repeat(250), t0)
        .unwrap();
    assert_eq!(
        out,
        format!("\x1b]9;{}\x1b\\", "a".repeat(200))
            .as_bytes()
            .to_vec()
    );
    // 多字节字符按字符截断（200 字符 = 600 字节），不得按字节切断。
    let out = e
        .on_event(NotifyKind::UserQuery, &"中".repeat(250), t0)
        .unwrap();
    assert_eq!(
        out,
        format!("\x1b]9;{}\x1b\\", "中".repeat(200))
            .as_bytes()
            .to_vec()
    );
}

#[test]
fn empty_user_query_message_not_sent_and_not_recorded() {
    // 决策 D3：空 message → None，且不更新 last_sent（节流记录实际发送时刻）。
    let mut e = engine(Protocol::Osc9, Duration::from_secs(3));
    let t0 = Instant::now();
    assert_eq!(e.on_event(NotifyKind::UserQuery, "", t0), None);
    assert_eq!(e.last_sent_at(NotifyKind::UserQuery), None);
    // 纯控制字符 message → 清洗后为空 → 同样不发送、不记录。
    assert_eq!(e.on_event(NotifyKind::UserQuery, "\x1b\x07\x01", t0), None);
    assert_eq!(e.last_sent_at(NotifyKind::UserQuery), None);
    // 空消息未占用节流窗口：同一时刻的正常消息仍可发送。
    assert!(e.on_event(NotifyKind::UserQuery, "ok", t0).is_some());
}

#[test]
fn done_uses_passed_body() {
    // 设计 §2.1：Done 固定文案由调用方提供（event.rs 的 `DONE_BODY`），引擎原样编码。
    let mut e = engine(Protocol::Osc9, Duration::ZERO);
    let t0 = Instant::now();
    let out = e.on_event(NotifyKind::Done, DONE_TEXT, t0).unwrap();
    assert_eq!(out, "\x1b]9;任务完成，等待输入\x1b\\".as_bytes().to_vec());
}

#[test]
fn engine_dispatches_by_selected_protocol() {
    // 多协议分派：同一事件按引擎选定协议输出对应 OSC 序列；
    // 标题 `visp` 出现在 777 / 99 序列中（OSC 9 无标题字段）。
    let t0 = Instant::now();
    let mut e9 = engine(Protocol::Osc9, Duration::ZERO);
    assert_eq!(
        e9.on_event(NotifyKind::UserQuery, "hi", t0),
        Some(b"\x1b]9;hi\x1b\\".to_vec())
    );
    let mut e777 = engine(Protocol::Osc777, Duration::ZERO);
    assert_eq!(
        e777.on_event(NotifyKind::UserQuery, "hi", t0),
        Some(b"\x1b]777;notify;visp;hi\x1b\\".to_vec())
    );
    let mut e99 = engine(Protocol::Kitty99, Duration::ZERO);
    assert_eq!(
        e99.on_event(NotifyKind::UserQuery, "hi", t0),
        Some(
            b"\x1b]99;i=1:d=0:p=title:f=dmlzcA==;visp\x1b\\\x1b]99;i=1:d=1:p=body;hi\x1b\\"
                .to_vec()
        )
    );
    let mut ebel = engine(Protocol::Bel, Duration::ZERO);
    assert_eq!(
        ebel.on_event(NotifyKind::UserQuery, "hi", t0),
        Some(vec![0x07])
    );
}

// ── Wave 2：三协议编码器（计划文档 step 3a）────────────────────────────
// 断言确切字节序列（字面量 `\x1b]...\x1b\\`），不只看长度。

#[test]
fn osc9_basic() {
    assert_eq!(
        encode(Protocol::Osc9, "visp", "hi"),
        Some(b"\x1b]9;hi\x1b\\".to_vec())
    );
}

#[test]
fn osc777_basic() {
    assert_eq!(
        encode(Protocol::Osc777, "visp", "hi"),
        Some(b"\x1b]777;notify;visp;hi\x1b\\".to_vec())
    );
}

#[test]
fn osc99_no_title_single_block() {
    // 无标题 → 单块最简形态 `\x1b]99;;body\x1b\\`。
    assert_eq!(
        encode(Protocol::Kitty99, "", "hi"),
        Some(b"\x1b]99;;hi\x1b\\".to_vec())
    );
}

#[test]
fn osc99_with_title_two_segments() {
    // 两段：p=title（d=0，含 f=base64("visp")）→ p=body（d=1）。
    assert_eq!(
        encode(Protocol::Kitty99, "visp", "hi"),
        Some(
            b"\x1b]99;i=1:d=0:p=title:f=dmlzcA==;visp\x1b\\\x1b]99;i=1:d=1:p=body;hi\x1b\\"
                .to_vec()
        )
    );
}

#[test]
fn control_chars_removed() {
    // body 含 ESC / BEL / SOH → 删除；输出中除序列定界符外无 ESC。
    let out = encode(Protocol::Osc9, "", "a\x1bb\x07c\x01d").unwrap();
    assert_eq!(out, b"\x1b]9;abcd\x1b\\".to_vec());
    // 定界符恰好 2 个 ESC（起始 + ST 结尾）。
    assert_eq!(out.iter().filter(|&&b| b == 0x1b).count(), 2);
}

#[test]
fn newlines_replaced_with_space() {
    // \n / \r / \t → 空格（决策 D2）。
    assert_eq!(
        encode(Protocol::Osc9, "", "a\nb\rc\td"),
        Some(b"\x1b]9;a b c d\x1b\\".to_vec())
    );
}

#[test]
fn multibyte_utf8_preserved() {
    // 中文 + emoji 原样保留，字节序列正确。
    let out = encode(Protocol::Osc9, "", "你好 🎉").unwrap();
    assert_eq!(out, "\x1b]9;你好 🎉\x1b\\".as_bytes().to_vec());
}

#[test]
fn osc99_overlong_body_truncated_to_2048() {
    // 4000 字节 body → payload 截断至 ≤2048 字节（决策 D4）。
    let body = "a".repeat(4000);
    let out = encode(Protocol::Kitty99, "", &body).unwrap();
    let expected = format!("\x1b]99;;{}\x1b\\", "a".repeat(2048));
    assert_eq!(out, expected.as_bytes().to_vec());
}

#[test]
fn osc99_truncation_respects_utf8_boundary() {
    // 中文（3 字节/字符）超长：2048 字节内最多 682 个字符（2046 字节），
    // 截断点必须是字符边界，不得在 UTF-8 序列中间切断。
    let body = "中".repeat(1000); // 3000 字节
    let out = encode(Protocol::Kitty99, "", &body).unwrap();
    let expected = format!("\x1b]99;;{}\x1b\\", "中".repeat(682));
    assert_eq!(out, expected.as_bytes().to_vec());
}

#[test]
fn empty_body_returns_none() {
    // 决策 D3：空 body 不发送。
    assert_eq!(encode(Protocol::Osc9, "", ""), None);
    assert_eq!(encode(Protocol::Osc777, "visp", ""), None);
    assert_eq!(encode(Protocol::Kitty99, "visp", ""), None);
    // 清洗后为空（纯控制字符）同样不发送。
    assert_eq!(encode(Protocol::Osc9, "", "\x1b\x07\x01"), None);
}

#[test]
fn c1_control_chars_removed() {
    // C1 区（U+0080..U+009F）→ 删除。
    assert_eq!(
        encode(Protocol::Osc9, "", "a\u{80}b\u{9f}c"),
        Some(b"\x1b]9;abc\x1b\\".to_vec())
    );
}

#[test]
fn base64_encodes_visp() {
    // 手写 base64：`base64("visp")` = `dmlzcA==`（OSC 99 `f` 元数据）。
    assert_eq!(base64_encode(b"visp"), "dmlzcA==");
}

#[test]
fn sanitize_replaces_and_removes() {
    // D2 直接断言：\n\r\t → 空格；ESC/BEL/C1 → 删除；其余保留。
    assert_eq!(sanitize("a\nb\rc\td\x1be\x07f\u{80}g"), "a b c defg");
}

#[test]
fn truncate_utf8_safe_respects_char_boundary() {
    // D4 直接断言：不超限原样返回；超限回退到字符边界。
    assert_eq!(truncate_utf8_safe("hello", 10), "hello");
    assert_eq!(truncate_utf8_safe("hello", 3), "hel");
    // "中" 为 3 字节：max=4 时只能取 3 字节（1 个字符），不得切断。
    assert_eq!(truncate_utf8_safe("中中中", 4), "中");
    assert_eq!(truncate_utf8_safe("中中中", 6), "中中");
}
