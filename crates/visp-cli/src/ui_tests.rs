use super::*;

// ── cache_width 同步（鼠标 hit-test 依赖） ───────────────────

#[test]
fn test_render_sets_cache_width_for_hit_test() {
    // 回归：cache_width 此前从未被赋值（恒为 0），导致 AgentCall 头部的
    // "[show in new tab]" 按钮命中矩形按宽度 0 计算，点击永远无效。
    let mut app = AppState::new("main".into(), "m".into(), "".into(), String::new());
    app.add_message(
        LineType::AgentCall {
            name: "explorer".into(),
        },
        r#"{"prompt":"test"}"#.into(),
    );
    app.tab_bar.tabs[0].messages[0].sub_session_id = Some("sub-1".into());

    let backend = ratatui::backend::TestBackend::new(80, 24);
    let mut terminal = ratatui::Terminal::new(backend).unwrap();
    terminal.draw(|f| crate::ui::render(&mut app, f)).unwrap();

    // 渲染后 cache_width 必须等于渲染宽度（area.width 76 - 1 - 2 = 73）
    assert_eq!(app.cache_width, 73);
    assert!(!app.message_caches.is_empty());

    // 用渲染产出的 cache_width 做按钮命中检测：header 行在 block 内
    // 第 3 行（top_margin=1 + margin_vertical=1），列取按钮区域中部
    let virtual_row = 2u16;
    let content_w = app.cache_width;
    let button_start = 1u16 + content_w - (2 + 18);
    let hit = crate::tool_ui::agent_open_tab_hit_test(
        app.messages(),
        &app.message_caches,
        virtual_row,
        button_start + 2,
        content_w,
    );
    assert_eq!(hit.as_deref(), Some("sub-1"));
}

#[test]
fn test_split_model_name_normal() {
    assert_eq!(
        split_model_name("Ollama/deepseek-v4-flash"),
        ("Ollama", "deepseek-v4-flash")
    );
}

#[test]
fn test_split_model_name_no_slash() {
    assert_eq!(
        split_model_name("deepseek-v4-flash"),
        ("", "deepseek-v4-flash")
    );
}

#[test]
fn test_split_model_name_with_parens_no_slash() {
    assert_eq!(
        split_model_name("DeepSeek v4 Flash(Ollama)"),
        ("", "DeepSeek v4 Flash(Ollama)")
    );
}

#[test]
fn test_split_model_name_multi_word() {
    assert_eq!(
        split_model_name("Anthropic/Claude Sonnet"),
        ("Anthropic", "Claude Sonnet")
    );
}

#[test]
fn test_format_status_left_generating() {
    let s = format_status_left("abc12345", "Ollama/DeepSeek", true);
    assert_eq!(s, "abc12345 | DeepSeek(Ollama) | Generating");
}

#[test]
fn test_format_status_left_idle() {
    let s = format_status_left("sess_xyz", "Anthropic/Claude Sonnet", false);
    assert_eq!(s, "sess_xyz | Claude Sonnet(Anthropic) | Idle");
}

#[test]
fn test_format_status_left_empty_provider() {
    let s = format_status_left("abcdefgh", "ollama/deepseek-v4-flash", false);
    assert_eq!(s, "abcdefgh | deepseek-v4-flash(ollama) | Idle");
}

#[test]
fn test_format_status_tokens() {
    let mut app = AppState::new("sess".into(), "m".into(), "m".into(), "/tmp/p".into());
    app.total_input_tokens = 1234;
    app.total_output_tokens = 567;
    app.total_cache_creation_input_tokens = 89;
    app.total_cache_read_input_tokens = 10000;
    assert_eq!(
        format_status_tokens(&app),
        "Tokens: 1,234 input / 567 output | Cache: 89 create / 10,000 read"
    );
}

#[test]
fn test_render_status_bar_two_rows() {
    // 冒烟测试：状态栏渲染为 2 行，第 1 行含 model/status + 快捷键，
    // 第 2 行含工作目录 + token 统计
    let mut app = AppState::new(
        "sess_12345678".into(),
        "m".into(),
        "Anthropic/Claude Sonnet".into(),
        "/tmp/project".into(),
    );
    app.total_input_tokens = 1234;
    app.total_output_tokens = 567;

    let backend = ratatui::backend::TestBackend::new(120, 30);
    let mut terminal = ratatui::Terminal::new(backend).unwrap();
    terminal.draw(|f| crate::ui::render(&mut app, f)).unwrap();

    let buffer = terminal.backend().buffer();
    let lines: Vec<String> = buffer
        .content()
        .chunks(buffer.area().width as usize)
        .map(|row| row.iter().map(|c| c.symbol()).collect::<String>())
        .collect();

    // 第 1 行：model(provider) + status
    assert!(
        lines
            .iter()
            .any(|l| l.contains("Claude Sonnet(Anthropic)") && l.contains("| Idle"))
    );
    // 第 1 行右侧：快捷键
    assert!(
        lines
            .iter()
            .any(|l| l.contains("Ctrl-d: quit | /help = help"))
    );
    // 第 2 行：工作目录 + token 统计
    assert!(
        lines
            .iter()
            .any(|l| l.contains("/tmp/project") && l.contains("Tokens:"))
    );
}

// ── 状态栏：token 数值压缩（k/m/b） ──────────────────

#[test]
fn test_format_number_compression() {
    // 阈值以内：千位分隔符（“超过一万”指严格大于）
    assert_eq!(format_number(0), "0");
    assert_eq!(format_number(9_999), "9,999");
    assert_eq!(format_number(10_000), "10,000");
    // 超过一万 → k（两位小数）
    assert_eq!(format_number(10_001), "10.00k");
    assert_eq!(format_number(123_456), "123.46k");
    // 超过一千万 → m
    assert_eq!(format_number(10_000_001), "10.00m");
    assert_eq!(format_number(12_345_678), "12.35m");
    // 超过一百亿 → b
    assert_eq!(format_number(10_000_000_001), "10.00b");
    assert_eq!(format_number(987_654_321_098), "987.65b");
}

#[test]
fn test_format_status_tokens_compression() {
    let mut app = AppState::new("sess".into(), "m".into(), "m".into(), "/tmp/p".into());
    app.total_input_tokens = 123_456;
    app.total_output_tokens = 12_345_678;
    app.total_cache_creation_input_tokens = 89;
    app.total_cache_read_input_tokens = 10_000_001;
    assert_eq!(
        format_status_tokens(&app),
        "Tokens: 123.46k input / 12.35m output | Cache: 89 create / 10.00m read"
    );
}

// ── 状态栏：workdir 动态压缩 ─────────────────────────

#[test]
fn test_compress_workdir_fits() {
    // 优先全长度
    assert_eq!(compress_workdir("/tmp/project", 20), "/tmp/project");
    assert_eq!(compress_workdir("/tmp/project", 12), "/tmp/project");
}

#[test]
fn test_compress_workdir_zero_width() {
    assert_eq!(compress_workdir("/tmp/project", 0), "");
}

#[test]
fn test_compress_workdir_middle() {
    // 阶段 1：压缩中间路径段，保留头尾、以 "..." 替代
    assert_eq!(
        compress_workdir("/Users/laonger/visp", 15),
        "/Users/.../visp"
    );
}

#[test]
fn test_compress_workdir_middle_keeps_max_segments() {
    let path = "/Users/laonger/Documents/Work/self/coding_agent/visp";
    let out = compress_workdir(path, 45);
    // 在宽度允许下尽量多保留头部与尾部路径段
    assert_eq!(out, "/Users/laonger/.../self/coding_agent/visp");
    assert!(out.chars().count() <= 45);
}

#[test]
fn test_compress_workdir_head() {
    // 阶段 2：中间压缩仍超长 → 压缩头部，形如 ".../xxx"
    assert_eq!(compress_workdir("/Users/laonger/visp", 12), ".../visp");
}

#[test]
fn test_compress_workdir_no_slash_fallback() {
    // 无 '/' 边界时退化为尾部截断
    assert_eq!(compress_workdir("abcdefghij", 6), "...hij");
}

#[test]
fn test_render_status_bar_compresses_long_workdir() {
    // 渲染级：窄终端下长 workdir 被压缩，且仍显示 Tokens 统计
    let mut app = AppState::new(
        "sess_12345678".into(),
        "m".into(),
        "Anthropic/Claude Sonnet".into(),
        "/Users/laonger/Documents/Work/self/coding_agent/visp".into(),
    );

    let backend = ratatui::backend::TestBackend::new(80, 30);
    let mut terminal = ratatui::Terminal::new(backend).unwrap();
    terminal.draw(|f| crate::ui::render(&mut app, f)).unwrap();

    let buffer = terminal.backend().buffer();
    let lines: Vec<String> = buffer
        .content()
        .chunks(buffer.area().width as usize)
        .map(|row| row.iter().map(|c| c.symbol()).collect::<String>())
        .collect();

    let row = lines
        .iter()
        .find(|l| l.contains("Tokens:"))
        .expect("status bar row 2 should contain Tokens stats");
    assert!(row.contains("..."), "workdir should be compressed: {row}");
    assert!(
        row.trim_start().starts_with('/'),
        "compressed workdir should keep head segment: {row}"
    );
}

// ── tab_label_line 测试 ──────────────────────────────

#[test]
fn test_tab_label_running_shows_yellow_arrow() {
    let tab = TabEntry::new("sid".to_string(), "agentA");
    // 默认状态为 Running
    assert_eq!(tab.status, AgentStatus::Running);
    let line = tab_label_line(&tab, false);
    assert_eq!(line.spans[1].content, "▶ ");
    assert_eq!(line.spans[1].style.fg, Some(Color::Yellow));
}

#[test]
fn test_tab_label_done_shows_green_check() {
    let mut tab = TabEntry::new("sid".to_string(), "agentB");
    tab.status = AgentStatus::Done;
    let line = tab_label_line(&tab, false);
    assert_eq!(line.spans[1].content, "✓ ");
    assert_eq!(line.spans[1].style.fg, Some(Color::Green));
}

#[test]
fn test_tab_label_error_shows_red_cross() {
    let mut tab = TabEntry::new("sid".to_string(), "agentC");
    tab.status = AgentStatus::Error;
    let line = tab_label_line(&tab, false);
    assert_eq!(line.spans[1].content, "✗ ");
    assert_eq!(line.spans[1].style.fg, Some(Color::Red));
}

#[test]
fn test_tab_label_contains_agent_name() {
    let tab = TabEntry::new("sid".to_string(), "my-agent");
    let line = tab_label_line(&tab, false);
    assert_eq!(line.spans[2].content, "my-agent");
}

#[test]
fn test_default_tab_also_shows_status() {
    let tab = TabEntry::new("main-sid".to_string(), "default");
    assert_eq!(tab.status, AgentStatus::Running);
    let line = tab_label_line(&tab, false);
    assert_eq!(line.spans[1].content, "▶ ");
    assert_eq!(line.spans[1].style.fg, Some(Color::Yellow));
    assert_eq!(line.spans[2].content, "default");
}

#[test]
fn test_tab_label_inactive_uses_space_padding() {
    let tab = TabEntry::new("sid".to_string(), "X");
    let line = tab_label_line(&tab, false);
    assert_eq!(line.spans[0].content, " ");
    assert_eq!(line.spans[3].content, " ");
}

#[test]
fn test_tab_label_active_uses_brackets() {
    let tab = TabEntry::new("sid".to_string(), "X");
    let line = tab_label_line(&tab, true);
    assert_eq!(line.spans[0].content, "[");
    assert_eq!(line.spans[3].content, "]");
}

#[test]
fn test_tab_label_view_only_shows_gray_icon() {
    let mut tab = TabEntry::new("sid".to_string(), "agentV");
    tab.status = AgentStatus::ViewOnly;
    let line = tab_label_line(&tab, false);
    assert_eq!(line.spans[1].content, "◷ ");
    assert_eq!(line.spans[1].style.fg, Some(Color::DarkGray));
    assert_eq!(line.spans[2].content, "agentV");
}

#[test]
fn test_tab_label_render_width_unchanged_for_active() {
    // 方括号与空格同宽（1 col），label 渲染宽度对 active/inactive 一致
    let mut tab = TabEntry::new("sid".to_string(), "default");
    tab.is_main = true; // 主 tab 无 ✕
    assert_eq!(tab_label_render_width(&tab), 11);
}

// ── tab_label_render_width ───────────────────────────────────

#[test]
fn test_tab_label_render_width_ascii_name() {
    let mut tab = TabEntry::new("sid".to_string(), "default");
    tab.is_main = true; // 主 tab 无 ✕
    // 1 (左空格) + 2 (符号 "▶ ") + 7 ("default") + 1 (右空格) = 11
    assert_eq!(tab_label_render_width(&tab), 11);
}

#[test]
fn test_tab_label_render_width_short_name() {
    let mut tab = TabEntry::new("sid".to_string(), "X");
    tab.is_main = true; // 主 tab 无 ✕
    // 1 + 2 + 1 + 1 = 5
    assert_eq!(tab_label_render_width(&tab), 5);
}

// ── hit_test_tab_x ───────────────────────────────────────────

#[test]
fn test_hit_test_first_tab() {
    // 单 tab，label_w=5, ratatui pad_l=0, pad_r=1 → 占 [0..6)
    let widths = vec![5u16];
    assert_eq!(hit_test_tab_x(0, &widths), Some(0));
    assert_eq!(hit_test_tab_x(5, &widths), Some(0));
    assert_eq!(hit_test_tab_x(6, &widths), None); // 越界
}

#[test]
fn test_hit_test_two_tabs_with_divider() {
    // tab0 label_w=5, tab1 label_w=4
    // 布局: tab0_span=6 [0..6) + divider[6..7) + tab1_span=5 [7..12)
    let widths = vec![5u16, 4u16];
    assert_eq!(hit_test_tab_x(0, &widths), Some(0));
    assert_eq!(hit_test_tab_x(5, &widths), Some(0));
    assert_eq!(hit_test_tab_x(6, &widths), None); // divider
    assert_eq!(hit_test_tab_x(7, &widths), Some(1));
    assert_eq!(hit_test_tab_x(11, &widths), Some(1));
    assert_eq!(hit_test_tab_x(12, &widths), None);
}

#[test]
fn test_hit_test_empty_widths() {
    assert_eq!(hit_test_tab_x(0, &[]), None);
}

// ── tab_at_screen ────────────────────────────────────────────

#[test]
fn test_tab_at_screen_default_tab() {
    let mut tab_bar = crate::app::TabBar::new("main".into());
    tab_bar.last_tab_area_x = 2;
    tab_bar.last_tab_area_y = 1;
    tab_bar.last_term_width = 80;
    // default 名 = "default", label_w=11, +pad_r=1 → span=12. 屏幕范围 col [2..14)
    assert_eq!(tab_at_screen(&tab_bar, 2, 1), Some(0));
    assert_eq!(tab_at_screen(&tab_bar, 13, 1), Some(0));
    assert_eq!(tab_at_screen(&tab_bar, 14, 1), None);
}

#[test]
fn test_tab_at_screen_sub_tab() {
    let mut tab_bar = crate::app::TabBar::new("main".into());
    tab_bar.insert_sub_agent("sub-1", "X", false); // label_w = 1+2+1+1+1(✕) = 6
    tab_bar.last_tab_area_x = 2;
    tab_bar.last_tab_area_y = 1;
    tab_bar.last_term_width = 80;
    // 布局（rel_x 起算）：default span=12 [0..12) + divider[12..13) + sub span=7 [13..20)
    // 屏幕坐标 = 2 + rel_x
    assert_eq!(tab_at_screen(&tab_bar, 2, 1), Some(0)); // default 起点
    assert_eq!(tab_at_screen(&tab_bar, 13, 1), Some(0)); // default 末位
    assert_eq!(tab_at_screen(&tab_bar, 14, 1), None); // divider (rel_x=12)
    assert_eq!(tab_at_screen(&tab_bar, 15, 1), Some(1)); // sub 起点 (rel_x=13)
    assert_eq!(tab_at_screen(&tab_bar, 21, 1), Some(1)); // sub 末位 (rel_x=19)
    assert_eq!(tab_at_screen(&tab_bar, 22, 1), None); // 超出
}

#[test]
fn test_tab_at_screen_wrong_row_returns_none() {
    let mut tab_bar = crate::app::TabBar::new("main".into());
    tab_bar.last_tab_area_x = 2;
    tab_bar.last_tab_area_y = 1;
    tab_bar.last_term_width = 80;
    // 点击在分隔线行（y+1）或其他行 → None
    assert_eq!(tab_at_screen(&tab_bar, 5, 0), None);
    assert_eq!(tab_at_screen(&tab_bar, 5, 2), None);
}

#[test]
fn test_tab_at_screen_left_of_area_returns_none() {
    let mut tab_bar = crate::app::TabBar::new("main".into());
    tab_bar.last_tab_area_x = 5;
    tab_bar.last_tab_area_y = 1;
    tab_bar.last_term_width = 80;
    assert_eq!(tab_at_screen(&tab_bar, 4, 1), None);
}

// ── 关闭按钮 ✕ 测试 ──────────────────────────────────────────

#[test]
fn test_tab_label_done_has_close_button() {
    let mut tab = TabEntry::new("sid".to_string(), "agent");
    tab.status = AgentStatus::Done;
    let line = tab_label_line(&tab, false);
    // 最后一个 span 是 ✕
    let last = line.spans.last().unwrap();
    assert_eq!(last.content, "✕");
}

#[test]
fn test_tab_label_running_has_close_button() {
    let tab = TabEntry::new("sid".to_string(), "agent");
    // 默认 Running，子 tab 也显示 ✕
    let line = tab_label_line(&tab, false);
    let last = line.spans.last().unwrap();
    assert_eq!(last.content, "✕");
}

#[test]
fn test_tab_label_main_no_close_button() {
    let mut tab = TabEntry::new("main".to_string(), "default");
    tab.is_main = true;
    tab.status = AgentStatus::Done;
    let line = tab_label_line(&tab, false);
    let last = line.spans.last().unwrap();
    assert_ne!(last.content, "✕");
}

#[test]
fn test_tab_label_render_width_includes_close_button() {
    let mut tab = TabEntry::new("sid".to_string(), "agent");
    tab.status = AgentStatus::Done;
    // 1(lpad) + 2(symbol) + 5("agent") + 1(rpad) + 1(✕) = 10
    assert_eq!(tab_label_render_width(&tab), 10);
}

#[test]
fn test_tab_label_render_width_running_has_close() {
    let tab = TabEntry::new("sid".to_string(), "agent");
    // Running: 1+2+5+1+1(✕) = 10
    assert_eq!(tab_label_render_width(&tab), 10);
}

#[test]
fn test_close_tab_by_index() {
    let mut tb = crate::app::TabBar::new("main".into());
    tb.insert_sub_agent("sub1".to_string(), "agentA".to_string(), false);
    tb.insert_sub_agent("sub2".to_string(), "agentB".to_string(), false);
    // insert_sub_agent inserts at index 1, so order is [main, sub2, sub1]
    assert_eq!(tb.tabs[1].agent_name, "agentB");
    assert_eq!(tb.tabs[2].agent_name, "agentA");

    // Running tabs can now be closed (agent continues in background)
    assert!(tb.close_tab(1));
    assert_eq!(tb.tabs.len(), 2);
    assert_eq!(tb.tabs[1].agent_name, "agentA");
}

#[test]
fn test_close_tab_main_returns_false() {
    let mut tb = crate::app::TabBar::new("main".into());
    assert!(!tb.close_tab(0));
}

#[test]
fn test_close_tab_stores_in_closed_tabs() {
    let mut tb = crate::app::TabBar::new("main".into());
    tb.insert_sub_agent("sub1".to_string(), "agentA".to_string(), false);
    assert!(tb.close_tab(1));
    assert_eq!(tb.tabs.len(), 1);
    assert_eq!(tb.closed_tabs.len(), 1);
    assert_eq!(tb.closed_tabs[0].session_id, "sub1");
}

#[test]
fn test_find_or_restore_tab_from_closed() {
    let mut tb = crate::app::TabBar::new("main".into());
    tb.insert_sub_agent("sub1".to_string(), "agentA".to_string(), false);
    // Close the tab -> moved to closed_tabs
    assert!(tb.close_tab(1));
    assert_eq!(tb.tabs.len(), 1);
    // Restore it
    let idx = tb.find_or_restore_tab("sub1").unwrap();
    assert_eq!(idx, 1);
    assert_eq!(tb.tabs.len(), 2);
    assert_eq!(tb.tabs[1].session_id, "sub1");
    assert_eq!(tb.closed_tabs.len(), 0);
}

#[test]
fn test_find_or_restore_tab_already_active() {
    let mut tb = crate::app::TabBar::new("main".into());
    tb.insert_sub_agent("sub1".to_string(), "agentA".to_string(), false);
    // Tab is active -> find_or_restore returns existing index
    let idx = tb.find_or_restore_tab("sub1").unwrap();
    assert_eq!(idx, 1);
    assert_eq!(tb.tabs.len(), 2); // no new tab created
}

#[test]
fn test_find_or_restore_tab_not_found() {
    let mut tb = crate::app::TabBar::new("main".into());
    assert!(tb.find_or_restore_tab("nonexistent").is_none());
}
