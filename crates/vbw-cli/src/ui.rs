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
    let bg = Block::default().style(Style::default().bg(Color::from_u32(0x001A1A2E)));
    f.render_widget(Paragraph::new("").block(bg), f.area());

    let main_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(1),
            Constraint::Length(1),
            Constraint::Length(if app.confirm.is_some() { 6 } else { 5 }),
        ])
        .split(f.area());

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

    // 1. 确保每条消息有对应的缓存（惰性创建/更新）
    for msg in &app.messages {
        if !app.message_caches.iter().any(|c| c.matches(msg, width)) {
            if let Some(existing_pos) = app.message_caches.iter().position(|c| c.msg_id == msg.id) {
                app.message_caches[existing_pos] = MessageCache::from_message(msg, width);
            } else {
                app.message_caches
                    .push(MessageCache::from_message(msg, width));
            }
        }
    }

    // 2. 清理残留缓存（消息已删除但缓存还在的）
    app.message_caches
        .retain(|c| app.messages.iter().any(|m| m.id == c.msg_id));

    // 3. 组装所有消息行
    let mut text_lines: Vec<Line> = Vec::new();
    for msg in &app.messages {
        if let Some(cache) = app.message_caches.iter().find(|c| c.msg_id == msg.id) {
            text_lines.extend(cache.lines.clone());
        }
    }

    // 4. 流式文本处理
    if !app.streaming_text.is_empty() {
        let style = Style::default()
            .fg(Color::White)
            .bg(Color::from_u32(0x001A1A2E));
        let total_lines = app.streaming_text.lines().count();
        const FREEZE_THRESHOLD: usize = 200;
        const TAIL_LINES: usize = 50;

        if total_lines > FREEZE_THRESHOLD {
            // 冻结前段
            if app.frozen_cache.is_empty() {
                let all_stream_lines: Vec<String> = wrap_text(&app.streaming_text, width);
                let freeze_count = all_stream_lines.len().saturating_sub(TAIL_LINES);
                app.frozen_cache = all_stream_lines[..freeze_count]
                    .iter()
                    .map(|s| Line::styled(pad_to_width(s, width as usize), style))
                    .collect();
            }
            // 只渲染尾巴
            let tail_text: String = app
                .streaming_text
                .lines()
                .skip(total_lines.saturating_sub(TAIL_LINES))
                .collect::<Vec<_>>()
                .join("\n");
            for wl in wrap_text(&tail_text, width) {
                text_lines.push(Line::styled(
                    if wl.is_empty() {
                        " ".repeat(width as usize)
                    } else {
                        pad_to_width(&wl, width as usize)
                    },
                    style,
                ));
            }
        } else {
            app.frozen_cache.clear();
            // 正常渲染全部流式文本
            for wl in wrap_text(&app.streaming_text, width) {
                text_lines.push(Line::styled(
                    if wl.is_empty() {
                        " ".repeat(width as usize)
                    } else {
                        pad_to_width(&wl, width as usize)
                    },
                    style,
                ));
            }
        }

        // 拼接 frozen_cache
        if !app.frozen_cache.is_empty() {
            let mut combined = app.frozen_cache.clone();
            combined.extend(text_lines);
            text_lines = combined;
        }
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
