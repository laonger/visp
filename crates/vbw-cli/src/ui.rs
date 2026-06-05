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
    let sep = Line::styled(
        " ".repeat(width as usize),
        Style::default().bg(Color::from_u32(0x001A1A2E)),
    );
    for (i, msg) in app.messages.iter().enumerate() {
        if i > 0 {
            text_lines.push(sep.clone());
            text_lines.push(sep.clone());
        }
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
        if !app.messages.is_empty() {
            text_lines.push(sep.clone());
            text_lines.push(sep.clone());
        }
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
    // 更新消息缓存（副作用），不再使用返回值
    let border_w: u16 = 2; // 左右边框各占 1 列
    let content_w = area.width.saturating_sub(border_w + 1); // -1 for right shadow column
    let _ = build_text_stack(app, content_w);

    let total: u16 = app
        .messages
        .iter()
        .map(|m| {
            if let Some(c) = app.message_caches.iter().find(|c| c.msg_id == m.id) {
                c.line_count + 2 + 2 // border(2) + 2 separator lines
            } else {
                0
            }
        })
        .sum();
    // 加上流式文本行数
    let stream_lines = if app.streaming_text.is_empty() {
        0
    } else {
        app.streaming_text.lines().count() as u16 + 2
    }; // +2 for separators
    let total_lines = total + stream_lines;
    let visible = area.height;
    let max_scroll = total_lines.saturating_sub(visible);

    if app.scroll_following {
        app.scroll_state
            .set_offset(ratatui::layout::Position::new(0, max_scroll));
    }
    let scroll_y = app.scroll_state.offset().y.min(max_scroll);

    let shadow_color = Color::from_u32(0x000D0D17);
    let border_style = Style::default().fg(Color::White);
    let mut y: u16 = 0;

    for msg in &app.messages {
        if let Some(cache) = app.message_caches.iter().find(|c| c.msg_id == msg.id) {
            let block_h = cache.line_count + 2; // border top(1) + content + border bottom(1)
            let total_h = block_h + 2; // +2 separator lines

            if y + total_h > scroll_y && y < scroll_y + visible {
                let rel_y = area.y + y.saturating_sub(scroll_y);
                let msg_area = Rect::new(area.x, rel_y, area.width, block_h);

                // 白边框 Block
                let block = Block::bordered().border_style(border_style);
                let inner = block.inner(msg_area);
                let p = Paragraph::new(Text::from(cache.lines.clone()));
                f.render_widget(block, msg_area);
                f.render_widget(p, inner);

                // Drop shadow: right edge column + bottom row
                let buf = f.buffer_mut();
                let shadow_x = area.x + area.width.saturating_sub(1);
                let right = area.right();
                // Right-edge shadow (only on content rows, skip border rows)
                for row in (rel_y + 1)..(rel_y + block_h - 1).min(buf.area().bottom()) {
                    if shadow_x < right {
                        buf[(shadow_x, row)].set_bg(shadow_color);
                    }
                }
                // Bottom shadow row (below the block, first separator line)
                let bottom_y = rel_y + block_h;
                if bottom_y < buf.area().bottom() {
                    for x in (area.x + 1)..right {
                        buf[(x, bottom_y)].set_bg(shadow_color);
                    }
                }
            }
            y += total_h;
        }
    }

    // 流式文本渲染（如有）
    if !app.streaming_text.is_empty() {
        let stream_lines_count = app.streaming_text.lines().count() as u16;
        let stream_h = stream_lines_count + 2; // +2 separator
        if y + stream_h > scroll_y && y < scroll_y + visible {
            let style = Style::default()
                .fg(Color::White)
                .bg(Color::from_u32(0x001A1A2E));
            let rel_y = area.y + y.saturating_sub(scroll_y);
            let stream_area = Rect::new(area.x, rel_y + 1, content_w, stream_lines_count);
            let mut stream_text: Vec<Line> = Vec::new();
            for line in app.streaming_text.lines() {
                stream_text.push(Line::styled(pad_to_width(line, content_w as usize), style));
            }
            f.render_widget(Paragraph::new(Text::from(stream_text)), stream_area);
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
