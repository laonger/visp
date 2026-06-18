#![allow(dead_code)]

use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, Clear, List, ListItem, Paragraph, Tabs},
};

use crate::app::{AgentStatus, AppState, MessageCache, TabEntry, pad_to_width};
use crate::debug_log;

/// 将数字格式化为千位分隔符形式，如 `1234567` → `1,234,567`
fn format_number(n: u32) -> String {
    let s = n.to_string();
    let mut result = String::with_capacity(s.len() + s.len() / 3);
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            result.push(',');
        }
        result.push(c);
    }
    result
}

use crate::theme;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

// ════════════════════════════════════════════════════════════════
// Tab Bar 渲染
// ════════════════════════════════════════════════════════════════

/// 为单个 TabEntry 生成标签行：状态符号 + agent_name
fn tab_label_line(tab: &TabEntry) -> Line<'static> {
    let (symbol, color) = match tab.status {
        AgentStatus::Running => ("▶ ", Color::Yellow),
        AgentStatus::Done => ("✓ ", Color::Green),
        AgentStatus::Error => ("✗ ", Color::Red),
    };
    Line::from(vec![
        Span::styled(symbol, Style::default().fg(color)),
        Span::styled(tab.agent_name.clone(), Style::default().fg(theme::TAB_FG)),
    ])
}

/// 渲染顶部 Tab 栏（2 行：1 行 tab 内容 + 1 行分隔线）
fn render_tab_bar(tab_bar: &mut crate::app::TabBar, f: &mut Frame, area: Rect) {
    // 整条 tab bar 先铺底色（深紫黑），与对话区无缝连贯
    f.render_widget(Block::default().style(Style::default().bg(theme::BG)), area);

    // 2 行布局：tab 内容(1) + 分隔线(1)
    let tab_rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // tab 内容
            Constraint::Length(1), // 分隔线
        ])
        .split(area);

    let content_area = tab_rows[0];
    let sep_area = tab_rows[1];

    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Fill(1), Constraint::Length(8)])
        .split(content_area);

    // 写入终端宽度供 event.rs 翻页按键使用
    tab_bar.last_term_width = content_area.width;
    tab_bar.ensure_active_visible(content_area.width);

    let range = tab_bar.current_page_subs(content_area.width);
    // 所有 visible titles: [default] + 当前页的 sub
    let mut visible: Vec<Line<'static>> = vec![tab_label_line(&tab_bar.tabs[0])];
    for i in range.clone() {
        visible.push(tab_label_line(&tab_bar.tabs[i]));
    }

    let tabs = Tabs::new(visible)
        .style(Style::default().bg(theme::BG))
        .select(
            tab_bar
                .select_idx_for_current_page(content_area.width)
                .unwrap_or(0),
        )
        .highlight_style(
            Style::default()
                .bg(theme::TAB_ACTIVE_BG)
                .fg(theme::TAB_ACTIVE_FG)
                .add_modifier(Modifier::BOLD),
        )
        .divider(Span::styled(
            "│",
            Style::default().fg(theme::TAB_DIVIDER_FG),
        ))
        .padding("", " ");
    f.render_widget(tabs, chunks[0]);

    // 分隔线
    let sep = "─".repeat(sep_area.width as usize);
    f.render_widget(
        Paragraph::new(sep).style(Style::default().fg(SEP_FG)),
        sep_area,
    );

    // 页码指示器 [N/M]，多页时显示
    let pages = tab_bar.layout_pages(content_area.width);
    let page_label = if pages.len() > 1 {
        let current = tab_bar.current_page().min(pages.len().saturating_sub(1)) + 1;
        format!("[{}/{}]", current, pages.len())
    } else {
        String::new()
    };
    let p = Paragraph::new(page_label)
        .style(Style::default().bg(theme::BG).fg(theme::TAB_PAGE_FG))
        .alignment(Alignment::Right);
    f.render_widget(p, chunks[1]);
}

/// 分隔线颜色
const SEP_FG: Color = Color::DarkGray;

/// 计算输入框所需的行数（含文本折行），最小 3 行，最大 10 行
fn calc_input_height(textarea: &ratatui_textarea::TextArea<'static>, width_approx: u16) -> u16 {
    // TextArea 的 block 只有 Borders::TOP（无左右边框），折行宽度 = input_area.width = width_approx
    let content_width = width_approx;
    if content_width < 3 {
        return 3;
    }
    let mut total: u16 = 0;
    for line in textarea.lines() {
        if line.is_empty() {
            total += 1;
        } else {
            let w = UnicodeWidthStr::width(line.as_str()) as u16;
            total += std::cmp::max(1, w.div_ceil(content_width));
        }
    }
    total.clamp(3, 10)
}

/// 顶层渲染入口：将当前 AppState 绘制到终端
pub fn render(app: &mut AppState, f: &mut Frame) {
    // 四周1列留白（上/下不留），绘制背景色
    let area = f.area().inner(ratatui::layout::Margin::new(2, 1));

    let bg = Block::default().style(Style::default().bg(theme::BG));
    f.render_widget(Paragraph::new("").block(bg), f.area());

    let input_area_height = calc_input_height(&app.textarea, area.width);
    let bottom_chunks_height = input_area_height + (if app.confirm.is_some() { 5 } else { 4 });

    // 纵向分割：Tab栏(2) | 对话区 | 分隔线 | 底部区域
    let main_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2),                    // Tab bar（1 行内容 + 1 行分隔线）
            Constraint::Min(1),                       // 对话区（占满剩余）
            Constraint::Length(1),                    // 分隔线
            Constraint::Length(bottom_chunks_height), // 底部：确认栏/输入区/状态栏
        ])
        .split(area);

    render_tab_bar(&mut app.tab_bar, f, main_chunks[0]);
    render_chat_area(app, f, main_chunks[1]);

    // 分隔线
    let sep_line = "─".repeat(main_chunks[2].width as usize);
    f.render_widget(
        Paragraph::new(sep_line).style(Style::default().fg(SEP_FG)),
        main_chunks[2],
    );

    // 底部区域内部再分割：确认栏(可选) → 输入区 → 状态栏
    let bottom_area = main_chunks[3];
    let bottom_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints(if app.confirm.is_some() {
            vec![
                Constraint::Length(3), // 确认栏
                Constraint::Min(2),    // input area
                Constraint::Length(1), // 分隔线
                Constraint::Length(1), // status area
            ]
        } else {
            vec![
                Constraint::Min(2),    // input area
                Constraint::Length(1), // 分隔线
                Constraint::Length(1), // status area
            ]
        })
        .split(bottom_area);

    if app.confirm.is_some() {
        render_confirm_bar(app, f, bottom_chunks[0]);
        render_input_area(app, f, bottom_chunks[1]);
        render_status_bar(app, f, bottom_chunks[3]);
    } else {
        render_input_area(app, f, bottom_chunks[0]);
        render_status_bar(app, f, bottom_chunks[2]);
    }

    // 帮助弹窗（在最上层绘制）
    if app.show_help {
        render_help_popup(f, f.area());
    }

    // Session 选择器弹出面板
    if app.session_select.is_some() {
        render_session_select(f, f.area(), app);
    }

    // 模型选择器弹出面板
    if app.model_select.is_some() {
        render_model_select(f, f.area(), app);
    }
}

// ════════════════════════════════════════════════════════════════
// 工具函数
// ════════════════════════════════════════════════════════════════

/// 计算 block 在视窗中的可见范围。不可见返回 None。
///
/// 返回 `(rel_y, visible_h)`：
/// - `rel_y`：相对于视窗顶部的偏移（0 表示视窗第一行）
/// - `visible_h`：在视窗内可见的高度
fn viewport_intersect(
    y: u16,
    h: u16,
    scroll: u16,
    visible: u16,
    _area_bottom: u16,
) -> Option<(u16, u16)> {
    if y + h <= scroll || y >= scroll + visible {
        return None;
    }
    let rel_y = y.saturating_sub(scroll);
    let max_h = h.min(visible.saturating_sub(rel_y));
    if max_h == 0 {
        None
    } else {
        Some((rel_y, max_h))
    }
}

/// 统一渲染一个消息 block。所有消息类型走同一路径。
///
/// 渲染顺序：
///   1. 底部留白/分隔（bottom_pad 行）
///   2. 底色填充（如有）
///   3. 内容 Paragraph
///   4. drop shadow（右侧列 + 底部行）
fn render_block(
    f: &mut Frame,
    area: Rect,
    style: theme::BlockStyle,
    lines: &[Line<'static>],
    line_count: u16,
    rel_y: u16,
) {
    let content_w = area.width.saturating_sub(1);

    // 顶部 top_margin 行保持空气（不被 bg_fill 覆盖）
    let block_y = rel_y + style.top_margin;
    let mut shadow_y = block_y;
    // 2) 底色填充
    if let Some(bg) = style.bg_fill {
        let buf = f.buffer_mut();
        let end_x = (area.x + content_w).min(buf.area().right());
        let fill_end = (block_y + style.margin_vertical + line_count + style.margin_vertical)
            .min(buf.area().bottom());
        for row in (block_y)..fill_end {
            for x in area.x..end_x {
                buf[(x, row)].set_bg(bg);
            }
        }
        shadow_y = fill_end;
    }

    // 3) 内容 Paragraph
    let content_x = area.x + style.margin_horizontal;
    let content_y = block_y + style.margin_vertical;
    let content_w_adj = content_w.saturating_sub(style.margin_horizontal * 2);
    let actual_lines = line_count.min(area.bottom().saturating_sub(content_y));
    if actual_lines > 0 {
        let p = Paragraph::new(Text::from(lines[..actual_lines as usize].to_vec()));
        f.render_widget(
            p,
            Rect::new(content_x, content_y, content_w_adj, actual_lines),
        );
    }

    // 4) drop shadow（右侧 1 列 + 底部 1 行）
    if style.shadow && actual_lines > 0 && style.bg_fill.is_some() {
        let buf = f.buffer_mut();
        let right = area.right();
        let shadow_col = area.x + content_w; // 阴影起始列
        let bottom_y = shadow_y; // 阴影起始行

        // 右侧阴影：1 列
        for row in content_y..(shadow_y).min(buf.area().bottom()) {
            let x = shadow_col;
            if x < right {
                buf[(x, row)].set_bg(theme::SHADOW);
            }
        }
        // 底部阴影：1 行
        let row = bottom_y;
        if row < buf.area().bottom() {
            for x in (area.x + 1)..=shadow_col {
                if x < right {
                    buf[(x, row)].set_bg(theme::SHADOW);
                }
            }
        }
    }
}

/// 确保所有消息的渲染缓存有效（惰性渲染）
fn ensure_all_caches(app: &mut AppState, width: u16) {
    if width != app.cache_width {
        app.cache_width = width;
    }
    let mut cache_map: std::collections::HashMap<u64, usize> = std::collections::HashMap::new();
    for (i, cache) in app.message_caches.iter().enumerate() {
        cache_map.insert(cache.msg_id, i);
    }
    // Clone to avoid borrow conflict: app.messages() borrows all of &self.
    let msgs = app.messages().to_vec();
    for msg in &msgs {
        if let Some(&idx) = cache_map.get(&msg.id) {
            if !app.message_caches[idx].matches(msg, width) {
                app.message_caches[idx] = MessageCache::from_message(msg, width);
            }
        } else {
            app.message_caches
                .push(MessageCache::from_message(msg, width));
        }
    }
    app.message_caches
        .retain(|c| msgs.iter().any(|m| m.id == c.msg_id));
}

// ════════════════════════════════════════════════════════════════
// 对话区渲染
// ════════════════════════════════════════════════════════════════

/// 渲染对话消息列表 + 流式文本。使用统一的 BlockStyle 驱动。
fn render_chat_area(app: &mut AppState, f: &mut Frame, area: Rect) {
    let content_w = area.width.saturating_sub(1);
    // render_block 中 content_w_adj = content_w - margin_horizontal*2
    // 所有 style 的 margin_horizontal 均为 1，按渲染实际宽度折行
    let render_w = content_w.saturating_sub(2);
    ensure_all_caches(app, render_w);

    const CHAT_PAD: u16 = 0;

    // ── 计算总高度 + 滚动 ────────────────────────────────
    let total: u16 = app
        .messages()
        .iter()
        .map(|m| {
            let style = theme::style_for(m.line_type.clone());
            app.message_caches
                .iter()
                .find(|c| c.msg_id == m.id)
                .map_or(0, |c| style.total_height(c.line_count))
        })
        .sum::<u16>();

    let stream_lines = if app.streaming_is_empty() {
        0
    } else {
        theme::ASSISTANT_STYLE.total_height(app.streaming_lines_count() as u16)
    };

    let total_lines = total + stream_lines;
    let visible = area.height;
    let max_scroll = total_lines.saturating_sub(visible);

    if app.scroll_following {
        app.scroll_state.y = max_scroll;
    }
    let scroll_y = app.scroll_state.y.min(max_scroll);

    // ── 统一渲染循环 ──────────────────────────────────────
    let sep_style = Style::default().bg(theme::BG);
    f.render_widget(
        Block::default().style(sep_style),
        Rect::new(area.x, area.y, content_w, CHAT_PAD),
    );

    let mut y: u16 = CHAT_PAD;

    for msg in app.messages() {
        let style = theme::style_for(msg.line_type.clone());
        if let Some(cache) = app.message_caches.iter().find(|c| c.msg_id == msg.id) {
            let h = style.total_height(cache.line_count);
            if let Some((rel_y, visible_h)) =
                viewport_intersect(y, h, scroll_y, visible, area.bottom())
            {
                let hidden_top = scroll_y.saturating_sub(y);
                let remain = cache.line_count.saturating_sub(hidden_top);
                let visible_lines = remain.min(visible_h);
                if visible_lines > 0 {
                    let start = hidden_top as usize;
                    let end = (start + visible_lines as usize).min(cache.lines.len());
                    render_block(
                        f,
                        area,
                        style,
                        &cache.lines[start..end],
                        visible_lines,
                        area.y + rel_y,
                    );
                }
            }
            y += h;
        }
    }

    // 流式文本
    if !app.streaming_is_empty() {
        let lines: Vec<String> = app
            .streaming_text()
            .lines()
            .map(|s| s.to_string())
            .collect();
        let line_count = lines.len() as u16;
        let h = theme::ASSISTANT_STYLE.total_height(line_count);
        if let Some((rel_y, visible_h)) = viewport_intersect(y, h, scroll_y, visible, area.bottom())
        {
            let hidden_top = scroll_y.saturating_sub(y);
            let remain = line_count.saturating_sub(hidden_top);
            let visible_lines = remain.min(visible_h);
            if visible_lines > 0 {
                let bs = theme::ASSISTANT_STYLE;
                let content_w_adj = content_w.saturating_sub(bs.margin_horizontal * 2);
                let mut text_lines: Vec<Line> = Vec::new();
                let text_style = Style::default().fg(theme::ASSISTANT_FG);
                for line in &lines[hidden_top as usize..] {
                    if text_lines.len() >= visible_lines as usize {
                        break;
                    }
                    text_lines.push(Line::styled(
                        pad_to_width(line, content_w_adj as usize),
                        text_style,
                    ));
                }
                render_block(f, area, bs, &text_lines, visible_lines, area.y + rel_y);
            }
        }
    }
}

// ════════════════════════════════════════════════════════════════
// 底部区域组件
// ════════════════════════════════════════════════════════════════

/// 确认栏：工具调用需用户确认时显示
fn render_confirm_bar(app: &AppState, f: &mut Frame, area: Rect) {
    if let Some(ref confirm) = app.confirm {
        // 构建选项标签
        let options: Vec<String> = if confirm.options.is_empty() {
            vec!["Approve".into(), "Deny".into(), "Always Allow".into()]
        } else {
            confirm.options.clone()
        };

        // 计算可用宽度
        let avail_w = area.width as usize;
        let has_other = confirm.allow_other;
        let other_label = "[X] Other";
        let other_full_w = if has_other {
            UnicodeWidthStr::width(other_label)
        } else {
            0
        };
        let num_opts = options.len();
        let sep_w = 2; // "  " between options
        let prefix_w = 4; // "[X] "

        // 先预留 Other 的完整宽度，剩余宽度平分给其他选项
        let other_w = if has_other { other_full_w } else { 0 };
        let other_sep = if has_other && num_opts > 0 { sep_w } else { 0 }; // Other 前的分隔符
        let avail_for_normal = avail_w.saturating_sub(other_w + other_sep);

        let total_sep_normal = num_opts.saturating_sub(1) * sep_w;
        let total_prefix_normal = num_opts * prefix_w;
        let text_w = if num_opts > 0 {
            let overhead = total_prefix_normal + total_sep_normal;
            if overhead >= avail_for_normal {
                0
            } else {
                (avail_for_normal - overhead)
                    .checked_div(num_opts)
                    .unwrap_or(0)
            }
        } else {
            0
        };

        // 截断工具函数（用 UnicodeWidthStr 确保 CJK 显示宽度正确）
        let truncate = |s: &str, max: usize| -> String {
            let w = UnicodeWidthStr::width(s);
            if w <= max || max == 0 {
                s.to_string()
            } else if max <= 2 {
                // 空间太小，最多放一个字符
                let first = s.chars().next().map(|c| c.to_string()).unwrap_or_default();
                if UnicodeWidthStr::width(first.as_str()) <= max {
                    first
                } else {
                    String::new()
                }
            } else {
                // 按列宽逐字符构建，预留 1 列给 "…"
                let mut result = String::new();
                let mut current_w = 0;
                for c in s.chars() {
                    let cw = UnicodeWidthChar::width(c).unwrap_or(0);
                    if current_w + cw < max {
                        result.push(c);
                        current_w += cw;
                    } else {
                        break;
                    }
                }
                format!("{}…", result)
            }
        };

        let all_labels: Vec<String> = {
            let mut labels: Vec<String> = options
                .iter()
                .enumerate()
                .map(|(i, opt)| {
                    let letter = (b'A' + i as u8) as char;
                    let truncated = truncate(opt, text_w);
                    format!("[{}] {}", letter, truncated)
                })
                .collect();
            if has_other {
                let letter = (b'A' + num_opts as u8) as char;
                labels.push(format!("[{}] Other", letter)); // 不截断 Other
            }
            labels
        };

        // 消息行
        let msg_line = Line::from(vec![
            Span::styled("❓ ", Style::default().fg(theme::CONFIRM_FG)),
            Span::styled(&confirm.message, Style::default().fg(theme::ASSISTANT_FG)),
        ]);

        // 构建选项行
        let mut option_spans: Vec<Span> = Vec::new();

        for (i, label) in all_labels.iter().enumerate() {
            let is_selected = i == confirm.selected_index && !confirm.other_active;

            if is_selected {
                option_spans.push(Span::styled(
                    label.clone(),
                    Style::default()
                        .fg(theme::CONFIRM_FG)
                        .bg(theme::CONFIRM_SELECTED_BG),
                ));
            } else {
                // 解析 [A] 部分和选项名称部分，分别着色
                if let Some(bracket_end) = label.find(']') {
                    let (tag, rest) = label.split_at(bracket_end + 1);
                    option_spans.push(Span::styled(
                        tag.to_string(),
                        Style::default().fg(theme::CONFIRM_OPTION_LABEL_FG),
                    ));
                    option_spans.push(Span::styled(
                        rest.to_string(),
                        Style::default().fg(theme::CONFIRM_OPTION_FG),
                    ));
                } else {
                    option_spans.push(Span::styled(
                        label.clone(),
                        Style::default().fg(theme::CONFIRM_OPTION_FG),
                    ));
                }
            }

            if i < all_labels.len() - 1 {
                option_spans.push(Span::raw("  "));
            }
        }

        // 两行文本
        let text = Text::from(vec![msg_line, Line::from(option_spans)]);

        let p = Paragraph::new(text)
            .style(Style::default().bg(theme::CONFIRM_FONT_BG))
            .block(
                Block::default()
                    .borders(Borders::TOP)
                    .border_style(Style::default().fg(theme::CONFIRM_BLOCK_BG)),
            );
        f.render_widget(p, area);
    }
}

/// 输入区：tui-textarea 封装
fn render_input_area(app: &mut AppState, f: &mut Frame, area: Rect) {
    let input_area = Rect::new(area.x, area.y, area.width, area.height);

    // debug: log textarea state before rendering
    let _line_count = app.textarea.lines().len();
    let _total_chars: usize = app.textarea.lines().iter().map(|l| l.len()).sum();
    debug_log!(
        "render_input_area: input_area.width={}, lines={}, total_chars={}",
        input_area.width,
        _line_count,
        _total_chars
    );

    // 非 default tab 时输入框视觉禁用（行为禁用在 event.rs TODO Step 11）
    let input_disabled = app.tab_bar.active != 0;

    let is_other_mode = app.confirm.as_ref().is_some_and(|c| c.other_active);

    if input_disabled {
        // 非 default tab：显示灰色提示
        app.textarea.set_style(Style::default().fg(Color::DarkGray));
        app.textarea.set_placeholder_text("切回 default tab 输入");
    } else if is_other_mode {
        app.textarea.set_style(Style::default().fg(theme::INPUT_FG));
        app.textarea
            .set_placeholder_text("Type your custom input...");
    } else if app.generating() {
        app.textarea
            .set_style(Style::default().fg(theme::INPUT_NOTICE_FG));
        app.textarea
            .set_placeholder_text(format!("[Generating {}]", app.spinner_glyph()));
    } else {
        app.textarea.set_style(Style::default().fg(theme::INPUT_FG));
        app.textarea.set_placeholder_text("Type your message...");
        // 命令提示
        let current = app
            .textarea
            .lines()
            .first()
            .map(|s| s.as_str())
            .unwrap_or("");
        if current.starts_with('/') {
            let all_cmds = [
                "/clear",
                "/help",
                "/list",
                "/sessions",
                "/new",
                "/temp",
                "/model",
                "/init",
                "/mouse",
            ];
            let hint: Vec<&str> = if current.len() > 1 {
                all_cmds
                    .iter()
                    .filter(|c| c.starts_with(current))
                    .copied()
                    .collect()
            } else {
                all_cmds.to_vec()
            };
            if !hint.is_empty() {
                let hint_line = format!("  {}", hint.join("  "));
                let hint_y = area.y + area.height.saturating_sub(1);
                if hint_y > area.y {
                    f.render_widget(
                        Paragraph::new(hint_line)
                            .style(Style::default().fg(theme::INPUT_NOTICE_FG)),
                        Rect::new(area.x, hint_y, area.width, 1),
                    );
                }
            }
        }
    }
    app.textarea.set_block(
        Block::default()
            .borders(Borders::TOP)
            .border_style(Style::default().fg(theme::INPUT_BORDER_FG))
            .style(Style::default().bg(theme::INPUT_BG)),
    );
    // 单次渲染：既设置内部 area（折行/导航所需的 screen map），也完成可视输出
    f.render_widget(&app.textarea, input_area);
}

/// 将 model 字符串拆为 (provider, model_label)
/// 期望格式为 "{provider}.{name}"，如 "Ollama.deepseek-v4-flash"
/// 无分隔点时 provider 为空字符串
fn split_model_name(model: &str) -> (&str, &str) {
    model.split_once('.').unwrap_or(("", model))
}

/// 格式化状态栏左侧字符串
fn format_status_left(
    session_id: &str,
    model_key: &str,
    generating: bool,
    mouse_captured: bool,
) -> String {
    let sid: String = session_id.chars().take(8).collect();
    let status = if generating { "Generating" } else { "Idle" };
    let mouse = if mouse_captured { "Mouse" } else { "Select" };
    let (provider, model_label) = split_model_name(model_key);
    format!(
        "{sid} | {model}({provider}) | {status} | [{mouse}] | /help = help",
        sid = sid,
        model = model_label,
        provider = provider
    )
}

/// 底部状态栏：左对齐显示会话 ID / 模型 / 状态 / 鼠标模式，token 统计靠右对齐
fn render_status_bar(app: &AppState, f: &mut Frame, area: Rect) {
    let left_text = format_status_left(
        &app.session_id,
        &app.model_key,
        app.generating(),
        app.mouse_captured,
    );

    // 有 token 时左右分割显示，否则整行给左侧
    if app.total_input_tokens > 0 || app.total_output_tokens > 0 {
        let chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Min(1), Constraint::Length(36)])
            .split(area);

        // 左侧常规信息
        let left = Paragraph::new(left_text)
            .style(Style::default().fg(theme::STATUS_FG).bg(theme::STATUS_BG))
            .block(Block::default());
        f.render_widget(left, chunks[0]);

        // 右侧 token 统计（靠右显示）
        let token_text =
            if app.total_cache_creation_input_tokens > 0 || app.total_cache_read_input_tokens > 0 {
                format!(
                    "T:{}i/{}o C:{}r/{}c ",
                    format_number(app.total_input_tokens),
                    format_number(app.total_output_tokens),
                    format_number(app.total_cache_read_input_tokens),
                    format_number(app.total_cache_creation_input_tokens),
                )
            } else {
                format!(
                    "T:{}i/{}o ",
                    format_number(app.total_input_tokens),
                    format_number(app.total_output_tokens),
                )
            };
        let right = Paragraph::new(token_text)
            .style(
                Style::default()
                    .fg(theme::TOOL_RESULT_FG)
                    .bg(theme::STATUS_BG),
            )
            .alignment(Alignment::Right)
            .block(Block::default());
        f.render_widget(right, chunks[1]);
    } else {
        // 无 token 时直接用整行
        let left = Paragraph::new(left_text)
            .style(Style::default().fg(theme::STATUS_FG).bg(theme::STATUS_BG))
            .block(Block::default());
        f.render_widget(left, area);
    }
}

// ════════════════════════════════════════════════════════════════
// 帮助弹窗
// ════════════════════════════════════════════════════════════════

/// 渲染帮助弹窗覆盖层，居中显示，内容为所有命令和快捷键
fn render_help_popup(f: &mut Frame, area: Rect) {
    let cmd_items = [
        ("/clear", "Clear chat history"),
        ("/help", "Show this help popup"),
        ("/list", "List all sessions"),
        ("/sessions <id>", "Switch to a session by short-id"),
        ("/new", "Start a new session"),
        ("/temp <n>", "Set temperature (0.0–1.0)"),
        ("/model <m>", "Switch model"),
        ("/init", "Initialize session with system prompt"),
        ("/mouse", "Toggle mouse capture mode"),
    ];
    let key_items = [
        ("F1 / /help", "Toggle this help popup"),
        ("Alt+M", "Toggle mouse capture mode"),
        ("Ctrl+C", "Cancel generation / confirm"),
        ("↑ / ↓", "Input history navigation"),
        ("Ctrl+D", "Quit"),
        ("Enter", "Send message / confirm selection"),
    ];

    let mut lines: Vec<Line<'static>> = Vec::new();

    // 命令小节
    lines.push(Line::from(Span::styled(
        " Commands:",
        Style::default().fg(theme::HELP_SECTION_FG),
    )));
    lines.push(Line::from(""));
    for (key, desc) in &cmd_items {
        lines.push(Line::from(vec![
            Span::styled(
                format!("  {:<14}", key),
                Style::default().fg(theme::HELP_KEY_FG),
            ),
            Span::styled(*desc, Style::default().fg(theme::HELP_DESC_FG)),
        ]));
    }

    lines.push(Line::from(""));
    lines.push(Line::from(""));

    // 快捷键小节
    lines.push(Line::from(Span::styled(
        " Shortcuts:",
        Style::default().fg(theme::HELP_SECTION_FG),
    )));
    lines.push(Line::from(""));
    for (key, desc) in &key_items {
        lines.push(Line::from(vec![
            Span::styled(
                format!("  {:<14}", key),
                Style::default().fg(theme::HELP_KEY_FG),
            ),
            Span::styled(*desc, Style::default().fg(theme::HELP_DESC_FG)),
        ]));
    }

    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        " Press any key or click to close",
        Style::default().fg(theme::HELP_HINT_FG),
    )));

    // 计算弹窗尺寸
    let popup_width = 46.min(area.width.saturating_sub(4));
    let popup_height = (lines.len() + 2) as u16; // +2 for top/bottom border
    let popup_height = popup_height.min(area.height.saturating_sub(4));

    let x = (area.width.saturating_sub(popup_width)) / 2;
    let y = (area.height.saturating_sub(popup_height)) / 2;

    let popup_area = Rect::new(x, y, popup_width, popup_height);

    // 背景覆盖层
    let overlay = Block::default().style(Style::default().bg(theme::HELP_BG));
    f.render_widget(overlay, popup_area);

    // 弹窗主体（带边框）
    let popup = Paragraph::new(Text::from(lines))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(theme::HELP_BORDER_FG))
                .title(" Help ")
                .title_style(Style::default().fg(theme::HELP_TITLE_FG))
                .style(Style::default().bg(theme::HELP_BG)),
        )
        .style(Style::default().bg(theme::HELP_BG));
    f.render_widget(popup, popup_area);
}

// ════════════════════════════════════════════════════════════════
// Session 选择器弹出面板
// ════════════════════════════════════════════════════════════════

/// 渲染 Session 选择器弹出面板（/list 或 /sessions 无参触发）
fn render_session_select(f: &mut Frame, area: Rect, app: &mut AppState) {
    use crate::theme;
    use ratatui::style::Modifier;

    if let Some(ref mut ss) = app.session_select {
        let session_count = ss.session_ids.len();
        let content_height = (session_count as u16).clamp(5, 20);
        let popup_width = (area.width * 3 / 4).clamp(50, 100);
        let popup_height = content_height + 4;

        let x = (area.width.saturating_sub(popup_width)) / 2;
        let y = (area.height.saturating_sub(popup_height)) / 2;
        let popup_area = Rect::new(x, y, popup_width, popup_height);

        // 清除背景
        f.render_widget(Clear, popup_area);

        // 构建 ListItems
        let items: Vec<ListItem> = ss
            .labels
            .iter()
            .map(|label| {
                ListItem::new(Span::styled(
                    label.as_str(),
                    Style::default().fg(theme::SELECT_ITEM_FG),
                ))
            })
            .collect();

        let list = List::new(items)
            .block(
                Block::default()
                    .title(Line::from(Span::styled(
                        " Sessions ",
                        Style::default().fg(theme::SELECT_TITLE_FG),
                    )))
                    .title_bottom(Line::from(Span::styled(
                        " ↑↓ navigate  Enter switch  Esc/q cancel ",
                        Style::default().fg(theme::HELP_HINT_FG),
                    )))
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(theme::SELECT_BORDER_FG))
                    .style(Style::default().bg(theme::SELECT_BG)),
            )
            .highlight_style(
                Style::default()
                    .bg(theme::SELECT_HIGHLIGHT_BG)
                    .add_modifier(Modifier::BOLD),
            );

        f.render_stateful_widget(list, popup_area, &mut ss.state);
    }
}

/// 渲染模型选择器弹出面板（/model 无参触发）
fn render_model_select(f: &mut Frame, area: Rect, app: &mut AppState) {
    use crate::theme;
    use ratatui::style::Modifier;

    if let Some(ref mut ms) = app.model_select {
        let model_count = ms.display_labels.len();
        let content_height = (model_count as u16).clamp(3, 15);
        let popup_width = (area.width / 2).clamp(36, 80);
        let popup_height = content_height + 4;

        let x = (area.width.saturating_sub(popup_width)) / 2;
        let y = (area.height.saturating_sub(popup_height)) / 2;
        let popup_area = Rect::new(x, y, popup_width, popup_height);

        f.render_widget(Clear, popup_area);

        let items: Vec<ListItem> = ms
            .display_labels
            .iter()
            .map(|label| {
                ListItem::new(Span::styled(
                    format!("  {label}"),
                    Style::default().fg(theme::SELECT_ITEM_FG),
                ))
            })
            .collect();

        let list = List::new(items)
            .block(
                Block::default()
                    .title(Line::from(Span::styled(
                        " Model ",
                        Style::default().fg(theme::SELECT_TITLE_FG),
                    )))
                    .title_bottom(Line::from(Span::styled(
                        " ↑↓ navigate  Enter switch  Esc/q cancel ",
                        Style::default().fg(theme::HELP_HINT_FG),
                    )))
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(theme::SELECT_BORDER_FG))
                    .style(Style::default().bg(theme::SELECT_BG)),
            )
            .highlight_style(
                Style::default()
                    .bg(theme::SELECT_HIGHLIGHT_BG)
                    .add_modifier(Modifier::BOLD),
            );

        f.render_stateful_widget(list, popup_area, &mut ms.state);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_split_model_name_normal() {
        assert_eq!(
            split_model_name("Ollama.deepseek-v4-flash"),
            ("Ollama", "deepseek-v4-flash")
        );
    }

    #[test]
    fn test_split_model_name_no_dot() {
        assert_eq!(
            split_model_name("deepseek-v4-flash"),
            ("", "deepseek-v4-flash")
        );
    }

    #[test]
    fn test_split_model_name_with_parens_no_dot() {
        assert_eq!(
            split_model_name("DeepSeek v4 Flash(Ollama)"),
            ("", "DeepSeek v4 Flash(Ollama)")
        );
    }

    #[test]
    fn test_split_model_name_multi_word() {
        assert_eq!(
            split_model_name("Anthropic.Claude Sonnet"),
            ("Anthropic", "Claude Sonnet")
        );
    }

    #[test]
    fn test_format_status_left_generating() {
        let s = format_status_left("abc12345", "Ollama.DeepSeek", true, false);
        assert_eq!(
            s,
            "abc12345 | DeepSeek(Ollama) | Generating | [Select] | /help = help"
        );
    }

    #[test]
    fn test_format_status_left_idle_mouse() {
        let s = format_status_left("sess_xyz", "Anthropic.Claude Sonnet", false, true);
        assert_eq!(
            s,
            "sess_xyz | Claude Sonnet(Anthropic) | Idle | [Mouse] | /help = help"
        );
    }

    #[test]
    fn test_format_status_left_empty_provider() {
        let s = format_status_left("abcdefgh", "ollama.deepseek-v4-flash", false, false);
        assert_eq!(
            s,
            "abcdefgh | deepseek-v4-flash(ollama) | Idle | [Select] | /help = help"
        );
    }

    // ── tab_label_line 测试 ──────────────────────────────

    #[test]
    fn test_tab_label_running_shows_yellow_arrow() {
        let tab = TabEntry::new("sid".to_string(), "agentA");
        // 默认状态为 Running
        assert_eq!(tab.status, AgentStatus::Running);
        let line = tab_label_line(&tab);
        assert_eq!(line.spans[0].content, "▶ ");
        assert_eq!(line.spans[0].style.fg, Some(Color::Yellow));
    }

    #[test]
    fn test_tab_label_done_shows_green_check() {
        let mut tab = TabEntry::new("sid".to_string(), "agentB");
        tab.status = AgentStatus::Done;
        let line = tab_label_line(&tab);
        assert_eq!(line.spans[0].content, "✓ ");
        assert_eq!(line.spans[0].style.fg, Some(Color::Green));
    }

    #[test]
    fn test_tab_label_error_shows_red_cross() {
        let mut tab = TabEntry::new("sid".to_string(), "agentC");
        tab.status = AgentStatus::Error;
        let line = tab_label_line(&tab);
        assert_eq!(line.spans[0].content, "✗ ");
        assert_eq!(line.spans[0].style.fg, Some(Color::Red));
    }

    #[test]
    fn test_tab_label_contains_agent_name() {
        let tab = TabEntry::new("sid".to_string(), "my-agent");
        let line = tab_label_line(&tab);
        assert_eq!(line.spans[1].content, "my-agent");
    }

    #[test]
    fn test_default_tab_also_shows_status() {
        let tab = TabEntry::new("main-sid".to_string(), "default");
        assert_eq!(tab.status, AgentStatus::Running);
        let line = tab_label_line(&tab);
        assert_eq!(line.spans[0].content, "▶ ");
        assert_eq!(line.spans[0].style.fg, Some(Color::Yellow));
        assert_eq!(line.spans[1].content, "default");
    }
}
