#![allow(dead_code)]

use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Style},
    text::{Line, Text},
    widgets::{Block, Borders, Paragraph},
};

use crate::app::{AppState, LineType, MessageCache, pad_to_width};

pub fn render(app: &mut AppState, f: &mut Frame) {
    let area = f.area().inner(ratatui::layout::Margin::new(1, 0));
    let bg = Block::default().style(Style::default().bg(Color::from_u32(0x001A1A2E)));
    f.render_widget(Paragraph::new("").block(bg), f.area());

    let main_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(1),
            Constraint::Length(1),
            Constraint::Length(if app.confirm.is_some() { 8 } else { 7 }),
        ])
        .split(area);

    let chat_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(2), Constraint::Min(1)])
        .split(main_chunks[0]);
    render_chat_area(app, f, chat_chunks[1]);

    let bottom_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints(if app.confirm.is_some() {
            vec![
                Constraint::Length(1),
                Constraint::Length(2),
                Constraint::Length(4),
                Constraint::Length(1),
            ]
        } else {
            vec![
                Constraint::Length(2),
                Constraint::Length(4),
                Constraint::Length(1),
            ]
        })
        .split(main_chunks[2]);

    if app.confirm.is_some() {
        render_confirm_bar(app, f, bottom_chunks[0]);
        render_input_area(app, f, bottom_chunks[2]);
        render_status_bar(app, f, bottom_chunks[3]);
    } else {
        render_input_area(app, f, bottom_chunks[1]);
        render_status_bar(app, f, bottom_chunks[2]);
    }
}

#[derive(Copy, Clone)]
struct BlockStyle {
    inset: u16,
    bg_fill: Option<Color>,
    shadow: bool,
    bottom_pad: u16,
}

impl BlockStyle {
    fn total_height(self, line_count: u16) -> u16 {
        1 + self.inset + line_count + self.bottom_pad
    }
}

const USER_STYLE: BlockStyle = BlockStyle {
    inset: 0,
    bg_fill: None,
    shadow: true,
    bottom_pad: 2,
};
const ASSISTANT_STYLE: BlockStyle = BlockStyle {
    inset: 2,
    bg_fill: Some(Color::from_u32(0x00222A3E)),
    shadow: true,
    bottom_pad: 2,
};
const TOOL_STYLE: BlockStyle = BlockStyle {
    inset: 0,
    bg_fill: None,
    shadow: true,
    bottom_pad: 2,
};

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

fn render_block(
    f: &mut Frame,
    area: Rect,
    style: BlockStyle,
    lines: &[Line<'static>],
    line_count: u16,
    rel_y: u16,
) {
    let content_w = area.width.saturating_sub(1); // -1 for right shadow column
    let shadow_color = Color::from_u32(0x000D0D17);
    let sep_bg = Color::from_u32(0x001A1A2E);

    // Bottom pad rows - fill with bg_fill or separator style
    let bottom_start = rel_y + 1 + style.inset + line_count;
    for i in 0..style.bottom_pad {
        let sep_y = bottom_start + i;
        if sep_y >= area.bottom() {
            break;
        }
        let fill = style.bg_fill.unwrap_or(sep_bg);
        let p = Paragraph::new(Line::styled(
            " ".repeat(content_w as usize),
            Style::default().bg(fill),
        ));
        f.render_widget(p, Rect::new(area.x, sep_y, content_w, 1));
    }

    // Background fill (if bg_fill is set, fill top pad + content area)
    if let Some(bg) = style.bg_fill {
        let buf = f.buffer_mut();
        let end_x = (area.x + content_w).min(buf.area().right());
        let fill_end = (rel_y + 1 + style.inset + line_count).min(buf.area().bottom());
        for row in (rel_y + 1)..fill_end {
            for x in area.x..end_x {
                buf[(x, row)].set_bg(bg);
            }
        }
    }

    // Content Paragraph
    let content_x = area.x + style.inset;
    let content_y = rel_y + 1 + style.inset;
    let content_w_adj = content_w.saturating_sub(style.inset * 2);
    let actual_lines = line_count.min(area.bottom().saturating_sub(content_y));
    if actual_lines > 0 {
        let p = Paragraph::new(Text::from(lines[..actual_lines as usize].to_vec()));
        f.render_widget(
            p,
            Rect::new(content_x, content_y, content_w_adj, actual_lines),
        );
    }

    // Shadow
    if style.shadow {
        let buf = f.buffer_mut();
        let shadow_x = area.x + area.width.saturating_sub(1);
        let right = area.right();
        // Right edge
        for row in content_y..(content_y + line_count).min(buf.area().bottom()) {
            if shadow_x < right {
                buf[(shadow_x, row)].set_bg(shadow_color);
            }
        }
        // Bottom edge
        let bottom_y = rel_y + 1 + style.inset + line_count;
        if bottom_y < buf.area().bottom() {
            for x in (area.x + 1)..right {
                if x < buf.area().right() {
                    buf[(x, bottom_y)].set_bg(shadow_color);
                }
            }
        }
    }
}

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

fn render_chat_area(app: &mut AppState, f: &mut Frame, area: Rect) {
    let content_w = area.width.saturating_sub(1);
    ensure_all_caches(app, content_w);

    // 计算总高度
    let total: u16 = app
        .messages
        .iter()
        .map(|m| {
            let style = match m.line_type {
                LineType::User => USER_STYLE,
                LineType::Assistant => ASSISTANT_STYLE,
                _ => TOOL_STYLE,
            };
            app.message_caches
                .iter()
                .find(|c| c.msg_id == m.id)
                .map_or(0, |c| style.total_height(c.line_count))
        })
        .sum();
    let stream_lines = if app.streaming_text.is_empty() {
        0
    } else {
        ASSISTANT_STYLE.total_height(app.streaming_text.lines().count() as u16)
    };
    let total_lines = total + stream_lines;
    let visible = area.height;
    let max_scroll = total_lines.saturating_sub(visible);

    if app.scroll_following {
        app.scroll_state
            .set_offset(ratatui::layout::Position::new(0, max_scroll));
    }
    let scroll_y = app.scroll_state.offset().y.min(max_scroll);

    // 统一渲染循环
    let mut y: u16 = 0;
    for msg in &app.messages {
        let style = match msg.line_type {
            LineType::User => USER_STYLE,
            LineType::Assistant => ASSISTANT_STYLE,
            _ => TOOL_STYLE,
        };
        if let Some(cache) = app.message_caches.iter().find(|c| c.msg_id == msg.id) {
            let h = style.total_height(cache.line_count);
            if let Some((rel_y, _)) = viewport_intersect(y, h, scroll_y, visible, area.bottom()) {
                render_block(f, area, style, &cache.lines, cache.line_count, rel_y);
            }
            y += h;
        }
    }

    // 流式文本
    if !app.streaming_text.is_empty() {
        let lines: Vec<String> = app.streaming_text.lines().map(|s| s.to_string()).collect();
        let line_count = lines.len() as u16;
        let h = ASSISTANT_STYLE.total_height(line_count);
        if let Some((rel_y, _)) = viewport_intersect(y, h, scroll_y, visible, area.bottom()) {
            let style = ASSISTANT_STYLE;
            let content_w_adj = content_w.saturating_sub(style.inset * 2);
            let mut text_lines: Vec<Line> = Vec::new();
            let text_style = Style::default()
                .fg(Color::White)
                .bg(Color::from_u32(0x00222A3E));
            for line in &lines {
                text_lines.push(Line::styled(
                    pad_to_width(line, content_w_adj as usize),
                    text_style,
                ));
            }
            render_block(f, area, style, &text_lines, line_count, rel_y);
        }
    }
}

fn render_confirm_bar(app: &AppState, f: &mut Frame, area: Rect) {
    if let Some(ref confirm) = app.confirm {
        let text = format!("❓ {} [y/N]", confirm.message);
        let p = Paragraph::new(text)
            .style(
                Style::default()
                    .fg(Color::Yellow)
                    .bg(Color::from_u32(0x00222222)),
            )
            .block(
                Block::default()
                    .borders(Borders::TOP)
                    .border_style(Style::default().fg(Color::DarkGray)),
            );
        f.render_widget(p, area);
    }
}

fn render_input_area(app: &AppState, f: &mut Frame, area: Rect) {
    let mut textarea = app.textarea.clone();
    if app.generating {
        textarea.set_style(Style::default().fg(Color::DarkGray));
        textarea.set_placeholder_text("[Generating...]");
    }
    textarea.set_block(
        Block::default()
            .borders(Borders::TOP)
            .border_style(Style::default().fg(Color::DarkGray))
            .style(Style::default().bg(Color::from_u32(0x00111111))),
    );
    f.render_widget(&textarea, area);
}

fn render_status_bar(app: &AppState, f: &mut Frame, area: Rect) {
    let sid = app.session_id.chars().take(8).collect::<String>();
    let status = if app.generating { "Generating" } else { "Idle" };
    let text = format!("Session: {} | Model: {} | {}", sid, app.model, status);
    let p = Paragraph::new(text)
        .style(Style::default().fg(Color::DarkGray).bg(Color::Black))
        .block(Block::default());
    f.render_widget(p, area);
}
