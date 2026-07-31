    use super::*;

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
        assert_eq!(s, "abc12345 | DeepSeek(Ollama) | Generating | /help = help");
    }

    #[test]
    fn test_format_status_left_idle() {
        let s = format_status_left("sess_xyz", "Anthropic/Claude Sonnet", false);
        assert_eq!(
            s,
            "sess_xyz | Claude Sonnet(Anthropic) | Idle | /help = help"
        );
    }

    #[test]
    fn test_format_status_left_empty_provider() {
        let s = format_status_left("abcdefgh", "ollama/deepseek-v4-flash", false);
        assert_eq!(
            s,
            "abcdefgh | deepseek-v4-flash(ollama) | Idle | /help = help"
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
