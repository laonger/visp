#![allow(dead_code)]

use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, Paragraph},
};

use crate::app::{AppState, MessageCache, pad_to_width};
use crate::theme;
use unicode_width::UnicodeWidthStr;

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

    // 纵向分割：对话区 | 分隔线 | 底部区域
    let main_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(1),                       // 对话区（占满剩余）
            Constraint::Length(1),                    // 分隔线
            Constraint::Length(bottom_chunks_height), // 底部：确认栏/输入区/状态栏
        ])
        .split(area);

    render_chat_area(app, f, main_chunks[0]);

    // 分隔线
    let sep_line = "─".repeat(main_chunks[1].width as usize);
    f.render_widget(
        Paragraph::new(sep_line).style(Style::default().fg(SEP_FG)),
        main_chunks[1],
    );

    // 底部区域内部再分割：确认栏(可选) → 输入区 → 状态栏
    let bottom_area = main_chunks[2];
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
}

// ════════════════════════════════════════════════════════════════
// 工具函数
// ════════════════════════════════════════════════════════════════

/// 计算 block 在视窗中的可见范围。不可见返回 None。
fn viewport_intersect(
    y: u16,
    h: u16,
    scroll: u16,
    visible: u16,
    area_bottom: u16,
) -> Option<(u16, u16)> {
    if y + h <= scroll || y >= scroll + visible {
        return None;
    }
    let rel_y = y.saturating_sub(scroll);
    let max_h = h.min(area_bottom.saturating_sub(rel_y));
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

    let mut shadow_y = rel_y;
    // 2) 底色填充
    if let Some(bg) = style.bg_fill {
        let buf = f.buffer_mut();
        let end_x = (area.x + content_w).min(buf.area().right());
        let fill_end = (rel_y + style.margin_vertical + line_count + style.margin_vertical)
            .min(buf.area().bottom());
        for row in (rel_y)..fill_end {
            for x in area.x..end_x {
                buf[(x, row)].set_bg(bg);
            }
        }
        shadow_y = fill_end;
    }

    // 3) 内容 Paragraph
    let content_x = area.x + style.margin_horizontal;
    let content_y = rel_y + style.margin_vertical;
    let content_w_adj = content_w.saturating_sub(style.margin_horizontal * 2);
    let actual_lines = line_count.min(area.bottom().saturating_sub(content_y));
    if actual_lines > 0 {
        let p = Paragraph::new(Text::from(lines[..actual_lines as usize].to_vec()));
        f.render_widget(
            p,
            Rect::new(content_x, content_y, content_w_adj, actual_lines),
        );
    }

    // 4) drop shadow
    if style.shadow && actual_lines > 0 && style.bg_fill.is_some() {
        let buf = f.buffer_mut();
        let shadow_right_x = area.x + content_w;
        let right = area.right();
        for row in content_y..(shadow_y).min(buf.area().bottom()) {
            if shadow_right_x < right {
                buf[(shadow_right_x, row)].set_bg(theme::SHADOW);
            }
        }
        let bottom_y = shadow_y;
        if bottom_y < buf.area().bottom() {
            for x in (area.x + 1)..=shadow_right_x {
                if x < right {
                    buf[(x, bottom_y)].set_bg(theme::SHADOW);
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
    for msg in &app.messages {
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
        .retain(|c| app.messages.iter().any(|m| m.id == c.msg_id));
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

    const CHAT_PAD: u16 = 1;

    // ── 计算总高度 + 滚动 ────────────────────────────────
    let total: u16 = app
        .messages
        .iter()
        .map(|m| {
            let style = theme::style_for(m.line_type.clone());
            app.message_caches
                .iter()
                .find(|c| c.msg_id == m.id)
                .map_or(0, |c| style.total_height(c.line_count))
        })
        .sum::<u16>();

    let stream_lines = if app.streaming_text.is_empty() {
        0
    } else {
        theme::ASSISTANT_STYLE.total_height(app.streaming_text.lines().count() as u16)
    };

    let total_lines = total + stream_lines;
    let visible = area.height;
    let max_scroll = total_lines.saturating_sub(visible);

    if app.scroll_following {
        app.scroll_state
            .set_offset(ratatui::layout::Position::new(0, max_scroll));
    }
    let scroll_y = app.scroll_state.offset().y.min(max_scroll);

    // ── 统一渲染循环 ──────────────────────────────────────
    let sep_style = Style::default().bg(theme::BG);
    f.render_widget(
        Block::default().style(sep_style),
        Rect::new(area.x, area.y, content_w, CHAT_PAD),
    );

    let mut y: u16 = CHAT_PAD;

    for msg in &app.messages {
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
                        rel_y,
                    );
                }
            }
            y += h;
        }
    }

    // 流式文本
    if !app.streaming_text.is_empty() {
        let lines: Vec<String> = app.streaming_text.lines().map(|s| s.to_string()).collect();
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
                render_block(f, area, bs, &text_lines, visible_lines, rel_y);
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
        let other_full_w = if has_other { other_label.len() } else { 0 };
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

        // 截断工具函数（用 chars().count() 确保中文字符正确处理）
        let truncate = |s: &str, max: usize| -> String {
            let char_count = s.chars().count();
            if char_count <= max || max == 0 {
                s.to_string()
            } else if max <= 1 {
                s.chars().take(1).collect()
            } else {
                let result: String = s.chars().take(max - 1).collect();
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

    // 直接在 app.textarea 上设 style/block，再通过 Widget::render 设置内部 area。
    // 这样后续事件处理中 screen_map_load 使用正确宽度，使折行和上下键导航正常。
    let is_other_mode = app.confirm.as_ref().is_some_and(|c| c.other_active);

    if is_other_mode {
        app.textarea.set_style(Style::default().fg(theme::INPUT_FG));
        app.textarea
            .set_placeholder_text("Type your custom input...");
    } else if app.generating {
        app.textarea
            .set_style(Style::default().fg(theme::INPUT_NOTICE_FG));
        app.textarea.set_placeholder_text("[Generating...]");
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
            let all_cmds = ["/clear", "/help", "/temp", "/model", "/init", "/mouse"];
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

/// 底部状态栏：会话 ID / 模型 / 状态
fn render_status_bar(app: &AppState, f: &mut Frame, area: Rect) {
    let sid = app.session_id.chars().take(8).collect::<String>();
    let status = if app.generating { "Generating" } else { "Idle" };
    let mouse = if app.mouse_captured {
        "Mouse"
    } else {
        "Select"
    };
    let text = format!("{sid} | {model} | {status} | [{mouse}]", model = app.model);
    let p = Paragraph::new(text)
        .style(Style::default().fg(theme::STATUS_FG).bg(theme::STATUS_BG))
        .block(Block::default());
    f.render_widget(p, area);
}
