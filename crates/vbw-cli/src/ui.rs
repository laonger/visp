#![allow(dead_code)]

use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Style},
    text::{Line, Text},
    widgets::{Block, Borders, Paragraph},
};

use crate::app::{AppState, MessageCache, pad_to_width, wrap_text};

pub fn render(app: &mut AppState, f: &mut Frame) {
    let area = f.area().inner(ratatui::layout::Margin::new(1, 1));
    let bg = Block::default().style(Style::default().bg(Color::from_u32(0x001A1A2E)));
    f.render_widget(Paragraph::new("").block(bg), f.area());

    let main_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(1),
            Constraint::Length(1),
            Constraint::Length(if app.confirm.is_some() { 6 } else { 5 }),
        ])
        .split(area);

    render_chat_area(app, f, main_chunks[0]);

    let bottom_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints(if app.confirm.is_some() {
            vec![
                Constraint::Length(1),
                Constraint::Length(3),
                Constraint::Length(1),
            ]
        } else {
            vec![Constraint::Length(3), Constraint::Length(1)]
        })
        .split(main_chunks[2]);

    if app.confirm.is_some() {
        render_confirm_bar(app, f, bottom_chunks[0]);
        render_input_area(app, f, bottom_chunks[1]);
        render_status_bar(app, f, bottom_chunks[2]);
    } else {
        render_input_area(app, f, bottom_chunks[0]);
        render_status_bar(app, f, bottom_chunks[1]);
    }
}

fn build_text_stack(app: &mut AppState, width: u16) -> Text<'static> {
    let new_width = width != app.cache_width;
    if new_width {
        app.cache_width = width;
    }

    // 1. 构建 msg_id → cache_index 的 HashMap
    let mut cache_map: std::collections::HashMap<u64, usize> = std::collections::HashMap::new();
    for (i, cache) in app.message_caches.iter().enumerate() {
        cache_map.insert(cache.msg_id, i);
    }

    // 2. 确保每条消息有对应缓存 + 组装行
    let mut text_lines: Vec<Line> = Vec::new();
    for msg in &app.messages {
        if let Some(&idx) = cache_map.get(&msg.id) {
            if !app.message_caches[idx].matches(msg, width) {
                app.message_caches[idx] = MessageCache::from_message(msg, width);
            }
            text_lines.extend(app.message_caches[idx].lines.clone());
        } else {
            let idx = app.message_caches.len();
            app.message_caches
                .push(MessageCache::from_message(msg, width));
            text_lines.extend(app.message_caches[idx].lines.clone());
        }
    }

    // 3. 清理残留缓存（消息已删除但缓存还在的）
    app.message_caches
        .retain(|c| app.messages.iter().any(|m| m.id == c.msg_id));

    // 4. 流式文本处理（增量渲染）
    if !app.streaming_text.is_empty() {
        let style = Style::default()
            .fg(Color::White)
            .bg(Color::from_u32(0x001A1A2E));

        if app.streaming_text.len() != app.streaming_rendered_len || app.streaming_rendered_len == 0
        {
            // 找到重新渲染的起始位置：上一个已渲染部分的最后一个换行符之后
            let rewrap_from = if app.streaming_rendered_len == 0 {
                0
            } else {
                app.streaming_text[..app.streaming_rendered_len]
                    .rfind('\n')
                    .map_or(0, |p| p + 1)
            };

            // 截断缓存中属于重新渲染起点的行
            if app.streaming_rendered_len > 0 {
                let keep_lines: usize = app.streaming_text[..rewrap_from].lines().count();
                app.streaming_rendered_lines.truncate(keep_lines);
            }

            // 增量渲染从 rewrap_from 开始的文本
            let tail = &app.streaming_text[rewrap_from..];
            for wl in wrap_text(tail, width) {
                app.streaming_rendered_lines.push(Line::styled(
                    if wl.is_empty() {
                        " ".repeat(width as usize)
                    } else {
                        pad_to_width(&wl, width as usize)
                    },
                    style,
                ));
            }

            app.streaming_rendered_len = app.streaming_text.len();
        }

        // 拼接已渲染的流式行
        text_lines.extend(app.streaming_rendered_lines.clone());
    }

    Text::from(text_lines)
}

fn render_chat_area(app: &mut AppState, f: &mut Frame, area: Rect) {
    let mut text = build_text_stack(app, area.width);
    let total = text.lines.len() as u16;
    let visible = area.height;
    let max_scroll = total.saturating_sub(visible);

    if app.scroll_following {
        app.scroll_state
            .set_offset(ratatui::layout::Position::new(0, max_scroll));
    }

    let scroll_y = app.scroll_state.offset().y.min(max_scroll) as usize;
    let end = (scroll_y + visible as usize).min(text.lines.len());
    if scroll_y < end {
        text.lines = text.lines[scroll_y..end].to_vec();
    }

    let p = Paragraph::new(text).block(Block::default());
    f.render_widget(p, area);
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
