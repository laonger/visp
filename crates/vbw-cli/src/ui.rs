#![allow(dead_code)]

use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Style},
    text::Line,
    widgets::{Block, Borders, List, ListItem, Paragraph},
};

use crate::app::{AppState, LineType};

pub fn render(app: &AppState, f: &mut Frame) {
    // 三区布局：对话区（上）、输入区+状态栏（下）
    let main_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(1),                                            // 对话区
            Constraint::Length(if app.confirm.is_some() { 4 } else { 3 }), // 底部（确认区+输入+状态）
        ])
        .split(f.area());

    // 对话区
    render_chat_area(app, f, main_chunks[0]);

    // 底部
    let bottom_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints(if app.confirm.is_some() {
            vec![
                Constraint::Length(1),
                Constraint::Length(1),
                Constraint::Length(1),
            ]
        } else {
            vec![Constraint::Length(1), Constraint::Length(1)]
        })
        .split(main_chunks[1]);

    if app.confirm.is_some() {
        render_confirm_bar(app, f, bottom_chunks[0]);
        render_input_area(app, f, bottom_chunks[1]);
        render_status_bar(app, f, bottom_chunks[2]);
    } else {
        render_input_area(app, f, bottom_chunks[0]);
        render_status_bar(app, f, bottom_chunks[1]);
    }
}

fn render_chat_area(app: &AppState, f: &mut Frame, area: Rect) {
    let mut items: Vec<ListItem> = Vec::new();

    for msg in &app.messages {
        let style = match msg.line_type {
            LineType::User => Style::default().fg(Color::Cyan),
            LineType::Assistant => Style::default().fg(Color::White),
            LineType::ToolCall => Style::default().fg(Color::Yellow),
            LineType::ToolResult => Style::default().fg(Color::DarkGray),
            LineType::Error => Style::default().fg(Color::Red),
            LineType::Status => Style::default().fg(Color::Gray),
        };
        items.push(ListItem::new(Line::styled(&msg.content, style)));
    }

    // streaming_text 临时拼到末尾
    if !app.streaming_text.is_empty() {
        items.push(ListItem::new(Line::styled(
            &app.streaming_text,
            Style::default().fg(Color::White),
        )));
    }

    let list = List::new(items).block(Block::default().borders(Borders::NONE));

    f.render_widget(list, area);
}

fn render_confirm_bar(app: &AppState, f: &mut Frame, area: Rect) {
    if let Some(ref confirm) = app.confirm {
        let text = format!("❓ {} [y/N]", confirm.message);
        let p = Paragraph::new(text)
            .style(Style::default().fg(Color::Yellow))
            .block(Block::default());
        f.render_widget(p, area);
    }
}

fn render_input_area(app: &AppState, f: &mut Frame, area: Rect) {
    let mut textarea = app.textarea.clone();
    if app.generating {
        textarea.set_style(Style::default().fg(Color::DarkGray));
        textarea.set_placeholder_text("[Generating...]");
    }
    f.render_widget(&textarea, area);
}

fn render_status_bar(app: &AppState, f: &mut Frame, area: Rect) {
    let sid = app.session_id.chars().take(8).collect::<String>();
    let status = if app.generating { "Generating" } else { "Idle" };
    let text = format!("Session: {} | Model: {} | {}", sid, app.model, status);
    let p = Paragraph::new(text).style(Style::default().fg(Color::DarkGray));
    f.render_widget(p, area);
}
