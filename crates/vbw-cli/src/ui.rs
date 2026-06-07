#![allow(dead_code)]

use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Style},
    text::{Line, Text},
    widgets::{Block, Borders, Paragraph},
};

use crate::app::{AppState, LineType, MessageCache, pad_to_width};

// #1A12E
const COLOR_BG: Color = Color::from_u32(0x001A1A2E);

// #111111
const COLOR_INPUT_BG: Color = Color::from_u32(0x00111111);
const COLOR_INPUT_BLOCK_BORDER_FG: Color = Color::DarkGray;
const COLOR_INPUT_NOTICE_FG: Color = Color::DarkGray;

const COLOR_CONFIRM_FG: Color = Color::Yellow;
// #222222
const COLOR_CONFIRM_FONT_BG: Color = Color::from_u32(0x00222222);
const COLOR_TOOL_RESULT_BG: Color = Color::from_u32(0x00222222);
const COLOR_CONFIRM_BLOCK_BG: Color = Color::DarkGray;

// #FFFFFF
const COLOR_ASSISTANT_FG: Color = Color::White;
// #1A3A5E
const COLOR_USER_BG: Color = Color::from_u32(0x001A3A5E);
// #222A3E
const COLOR_ASSISTANT_BG: Color = Color::from_u32(0x00222A3E);

// #0D0D17
const COLOR_SHADOW: Color = Color::from_u32(0x000D0D17);

const COLOR_STATUS_FG: Color = Color::DarkGray;
const COLOR_STATUS_BG: Color = Color::Black;

/// 顶层渲染入口：将当前 AppState 绘制到终端
pub fn render(app: &mut AppState, f: &mut Frame) {
    // 四周1列留白（上/下不留），绘制背景色
    let area = f.area().inner(ratatui::layout::Margin::new(2, 1));

    let bg = Block::default().style(Style::default().bg(COLOR_BG));
    f.render_widget(Paragraph::new("").block(bg), f.area());

    let input_area_height = 6;
    let bottom_chunks_height = input_area_height + (if app.confirm.is_some() { 3 } else { 2 });

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

    // 底部区域内部再分割：确认栏(可选) → 输入区 → 状态栏
    //let bottom_area = main_chunks[2].inner(ratatui::layout::Margin::new(0, 2));
    let bottom_area = main_chunks[2];
    let bottom_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints(if app.confirm.is_some() {
            vec![
                Constraint::Length(1),
                Constraint::Min(2),    // input area
                Constraint::Length(1), // status area
                Constraint::Length(1), // status area
            ]
        } else {
            vec![
                Constraint::Min(2),    // input area
                Constraint::Length(1), // status area
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
// BlockStyle — 消息块布局配置（栈分配，零开销）
// ════════════════════════════════════════════════════════════════

/// 消息块的统一布局参数。所有消息类型共用同一套渲染流程，差异由此数据驱动。
#[derive(Copy, Clone)]
struct BlockStyle {
    margin_vertical: u16,   // 垂直两端留白（字符数）
    margin_horizontal: u16, // 水平两端留白（字符数）
    bg_fill: Option<Color>, // 底色；None → bottom_pad 画分隔线，Some → 画底色
    shadow: bool,           // 是否绘制右侧+底部 drop shadow
    bottom_pad: u16,        // 内容下方行数（底色或分隔线）
}

impl BlockStyle {
    /// 计算该 block 占用的总行数
    fn total_height(self, line_count: u16) -> u16 {
        1 + self.margin_horizontal + line_count + self.bottom_pad
    }
}

const THINKING_STYLE: BlockStyle = BlockStyle {
    margin_vertical: 1,
    margin_horizontal: 1,
    bg_fill: None,
    shadow: false,
    bottom_pad: 1,
};
const USAGE_STYLE: BlockStyle = BlockStyle {
    margin_vertical: 0,
    margin_horizontal: 1,
    bg_fill: Some(COLOR_TOOL_RESULT_BG),
    shadow: false,
    bottom_pad: 1,
};
// 四种消息类型的样式常量
const USER_STYLE: BlockStyle = BlockStyle {
    margin_vertical: 1,
    margin_horizontal: 1,
    bg_fill: Some(COLOR_USER_BG),
    shadow: true,
    bottom_pad: 2,
};
const ASSISTANT_STYLE: BlockStyle = BlockStyle {
    margin_vertical: 1,
    margin_horizontal: 1,
    bg_fill: Some(COLOR_ASSISTANT_BG),
    shadow: true,
    bottom_pad: 2,
};
const TOOL_STYLE: BlockStyle = BlockStyle {
    margin_vertical: 1,
    margin_horizontal: 1,
    bg_fill: Some(COLOR_TOOL_RESULT_BG),
    shadow: true,
    bottom_pad: 0,
};
const TOOL_RESULT_STYLE: BlockStyle = BlockStyle {
    margin_vertical: 1,
    margin_horizontal: 1,
    bg_fill: Some(COLOR_TOOL_RESULT_BG),
    shadow: true,
    bottom_pad: 0,
};
// 流式文本使用 ASSISTANT_STYLE

// ════════════════════════════════════════════════════════════════
// 工具函数
// ════════════════════════════════════════════════════════════════

/// 计算 block 在视窗中的可见范围。不可见返回 None。
/// - y: block 在全局内容中的起始行
/// - h: block 高度
/// - scroll: 当前滚动偏移
/// - visible: 视窗高度
/// - area_bottom: buffer 底部边界
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
    let rel_y = y.saturating_sub(scroll); // block 在视窗中的相对 y
    let max_h = h.min(area_bottom.saturating_sub(rel_y)); // 裁剪高度
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
///
///    ┌─────── margin_horizontal
///  ├─┴┤
///  ▼  ▼
///  ┌───────────────────────────────────────────────────────┐ ◄─────┐◄─────────  rel_y
///  │                                                       │       ├─── margin_vertical
///  │  ┌─────────────────────────────────────────────────┐  ├───┐ ◄─┘
///  │  │                                     ▲           │  │   │
///  │  │◄──────────── content_w ─────────────┼─────────► │  │   │
///  │  │                                 line_count      │  │   │
///  │  │                                     │           │  │   │
///  │  │                                     ▼           │  │   │
///  │  └─────────────────────────────────────────────────┘  │   │
///  │                                                       │   │
///  └───┬───────────────────────────────────────────────────┘   │
///      │                                                       │
///      └───────────────────────────────────────────────────────┘ ◄───┐
///                                                                    ├────  bottom_pad
///                                                                    │
///  ┌───────────────────────────────────────────────────────┐    ◄────┘
///  │                                                       │
///  │  ┌─────────────────────────────────────────────────┐  ├───┐
///  │  │                                                 │  │   │
///  │  │                                                 │  │   │
///  │  │                                                 │  │   │
///  │  └─────────────────────────────────────────────────┘  │   │
///  │                                                       │   │
///  └───┬───────────────────────────────────────────────────┘   │
///      │                                                       │
///      └───────────────────────────────────────────────────────┘
///   
fn render_block(
    f: &mut Frame,
    area: Rect,              // 对话区全宽区域
    style: BlockStyle,       // 布局样式
    lines: &[Line<'static>], // 预渲染的行
    line_count: u16,
    rel_y: u16, // 在视窗中的 y 偏移
) {
    let content_w = area.width.saturating_sub(1); // -1 给右侧阴影列
    //let sep_bg = COLOR_BG; // 分隔线 = 聊天背景色

    //// 1) 底部留白/分隔（bottom_pad 行）：有底色则填底色，否则填聊天背景色
    //let bottom_start = rel_y + style.margin_vertical*2 + line_count;
    //for i in 0..(style.margin_vertical) {
    //    let sep_y = bottom_start + i;
    //    if sep_y >= area.bottom() {
    //        break;
    //    }
    //    let fill = style.bg_fill.unwrap_or(sep_bg);
    //    let p = Paragraph::new(Line::styled(
    //        " ".repeat(content_w as usize),
    //        Style::default().bg(fill),
    //    ));
    //    f.render_widget(p, Rect::new(area.x, sep_y, content_w, 1));
    //}

    let mut shadow_y = rel_y;
    // 2) 底色填充（覆盖顶部留白 + 内容区域+底部留白）
    if let Some(bg) = style.bg_fill {
        let buf = f.buffer_mut();
        let end_x = (area.x + content_w).min(buf.area().right());
        //let fill_end = (rel_y + 1 + style.margin_vertical + line_count).min(buf.area().bottom());
        // TODO buf.area().bottom() 可能不对，要考虑输入框的padding
        let fill_end = (rel_y + style.margin_vertical + line_count + style.margin_vertical)
            .min(buf.area().bottom());
        for row in (rel_y)..fill_end {
            for x in area.x..end_x {
                buf[(x, row)].set_bg(bg);
            }
        }
        shadow_y = fill_end;
    }

    // 3) 内容 Paragraph（按 margin 缩进，裁剪到 buffer 边界）
    let content_x = area.x + style.margin_horizontal;
    //let content_y = rel_y + 1 + style.margin_vertical;
    let content_y = rel_y + style.margin_vertical;
    let content_w_adj = content_w.saturating_sub(style.margin_horizontal * 2);
    let actual_lines = line_count.min(area.bottom().saturating_sub(content_y)); // content的行数（高度）
    if actual_lines > 0 {
        let p = Paragraph::new(Text::from(lines[..actual_lines as usize].to_vec()));
        f.render_widget(
            p,
            Rect::new(content_x, content_y, content_w_adj, actual_lines),
        );
    }

    // 4) drop shadow：右侧一列 + 底部一行（用背景色，空格也可见）
    if style.shadow && actual_lines > 0 && style.bg_fill.is_some() {
        let buf = f.buffer_mut();
        let shadow_right_x = area.x + content_w; // 最右列（chat 区边界），content_w 已 -1 预留
        let right = area.right();
        // 右侧阴影（在内容行上）
        for row in content_y..(shadow_y).min(buf.area().bottom()) {
            if shadow_right_x < right {
                buf[(shadow_right_x, row)].set_bg(COLOR_SHADOW);
            }
        }
        // 底部阴影（在内容下方第一行）
        let bottom_y = shadow_y;
        if bottom_y < buf.area().bottom() {
            for x in (area.x + 1)..=shadow_right_x {
                if x < right {
                    buf[(x, bottom_y)].set_bg(COLOR_SHADOW);
                }
            }
        }
    }
}

/// 确保所有消息的渲染缓存有效（惰性渲染）
///
/// 用 HashMap 以 msg_id → cache_index 建立索引，避免 O(N²) 扫描。
/// 仅在 width 变化或 version 不匹配时重新渲染单条消息。
fn ensure_all_caches(app: &mut AppState, width: u16) {
    if width != app.cache_width {
        app.cache_width = width;
    }
    // 构建 msg_id → index 映射
    let mut cache_map: std::collections::HashMap<u64, usize> = std::collections::HashMap::new();
    for (i, cache) in app.message_caches.iter().enumerate() {
        cache_map.insert(cache.msg_id, i);
    }
    // 逐条检查，未命中或 version/width 不匹配则重新渲染
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
    // 清理残留缓存（消息已删除但缓存还在的）
    app.message_caches
        .retain(|c| app.messages.iter().any(|m| m.id == c.msg_id));
}

// ════════════════════════════════════════════════════════════════
// 对话区渲染
// ════════════════════════════════════════════════════════════════

/// 渲染对话消息列表 + 流式文本。使用统一的 BlockStyle 驱动。
fn render_chat_area(app: &mut AppState, f: &mut Frame, area: Rect) {
    let content_w = area.width.saturating_sub(1); // -1 给右侧阴影列
    ensure_all_caches(app, content_w);

    const CHAT_PAD: u16 = 1; // 对话区顶部留白行数

    // ── 计算总高度 + 滚动 ────────────────────────────────
    let total: u16 = app
        .messages
        .iter()
        .map(|m| {
            let style = match m.line_type {
                LineType::User => USER_STYLE,
                LineType::Assistant => ASSISTANT_STYLE,
                LineType::Thinking => THINKING_STYLE,
                LineType::ToolResult => TOOL_RESULT_STYLE,
                LineType::Usage => USAGE_STYLE,
                _ => TOOL_STYLE,
            };
            app.message_caches
                .iter()
                .find(|c| c.msg_id == m.id)
                .map_or(0, |c| style.total_height(c.line_count))
        })
        .sum::<u16>();

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

    // ── 统一渲染循环 ──────────────────────────────────────
    // 先画顶部留白行
    let sep_style = Style::default().bg(COLOR_BG);
    f.render_widget(
        Block::default().style(sep_style),
        Rect::new(area.x, area.y, content_w, CHAT_PAD),
    );

    let mut y: u16 = CHAT_PAD; // 内容从此偏移开始

    // 遍历每条消息，按类型取对应 style，走统一 render_block
    for msg in &app.messages {
        let style = match msg.line_type {
            LineType::User => USER_STYLE,
            LineType::Assistant => ASSISTANT_STYLE,
            LineType::Thinking => THINKING_STYLE,
            LineType::ToolResult => TOOL_RESULT_STYLE,
            LineType::Usage => USAGE_STYLE,
            _ => TOOL_STYLE,
        };
        if let Some(cache) = app.message_caches.iter().find(|c| c.msg_id == msg.id) {
            let h = style.total_height(cache.line_count);
            if let Some((rel_y, visible_h)) =
                viewport_intersect(y, h, scroll_y, visible, area.bottom())
            {
                // 计算被滚出屏幕上方的行数
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

    // 流式文本（同 ASSISTANT 样式，实时构建行）
    if !app.streaming_text.is_empty() {
        let lines: Vec<String> = app.streaming_text.lines().map(|s| s.to_string()).collect();
        let line_count = lines.len() as u16;
        let h = ASSISTANT_STYLE.total_height(line_count);
        if let Some((rel_y, visible_h)) = viewport_intersect(y, h, scroll_y, visible, area.bottom())
        {
            let hidden_top = scroll_y.saturating_sub(y);
            let remain = line_count.saturating_sub(hidden_top);
            let visible_lines = remain.min(visible_h);
            if visible_lines > 0 {
                let style = ASSISTANT_STYLE;
                let content_w_adj = content_w.saturating_sub(style.margin_horizontal * 2);
                let mut text_lines: Vec<Line> = Vec::new();
                let text_style = Style::default().fg(COLOR_ASSISTANT_FG);
                for line in &lines[hidden_top as usize..] {
                    if text_lines.len() >= visible_lines as usize {
                        break;
                    }
                    text_lines.push(Line::styled(
                        pad_to_width(line, content_w_adj as usize),
                        text_style,
                    ));
                }
                render_block(f, area, style, &text_lines, visible_lines, rel_y);
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
        let text = format!("❓ {} [y/N]", confirm.message);
        let p = Paragraph::new(text)
            .style(
                Style::default()
                    .fg(COLOR_CONFIRM_FG)
                    .bg(COLOR_CONFIRM_FONT_BG),
            )
            .block(
                Block::default()
                    .borders(Borders::TOP)
                    .border_style(Style::default().fg(COLOR_CONFIRM_BLOCK_BG)),
            );
        f.render_widget(p, area);
    }
}

/// 输入区：tui-textarea 封装，带 2 行顶部留白
fn render_input_area(app: &AppState, f: &mut Frame, area: Rect) {
    let mut textarea = app.textarea.clone();
    if app.generating {
        textarea.set_style(Style::default().fg(COLOR_INPUT_NOTICE_FG));
        textarea.set_placeholder_text("[Generating...]");
    }
    textarea.set_block(
        Block::default()
            .borders(Borders::TOP)
            .border_style(Style::default().fg(COLOR_INPUT_BLOCK_BORDER_FG))
            .style(Style::default().bg(COLOR_INPUT_BG)),
    );
    // 内部 2 行顶部留白（不改变 layout 区域）
    let input_area = Rect::new(
        area.x,
        area.y + 2,
        area.width,
        area.height.saturating_sub(2),
    );
    f.render_widget(&textarea, input_area);
}

/// 底部状态栏：会话 ID / 模型 / 状态
fn render_status_bar(app: &AppState, f: &mut Frame, area: Rect) {
    let sid = app.session_id.chars().take(8).collect::<String>();
    let status = if app.generating { "Generating" } else { "Idle" };
    let text = format!("Session: {} | Model: {} | {}", sid, app.model, status);
    let p = Paragraph::new(text)
        .style(Style::default().fg(COLOR_STATUS_FG).bg(COLOR_STATUS_BG))
        .block(Block::default());
    f.render_widget(p, area);
}
