//! 全局 UI 主题：颜色、BlockStyle、LineType 前景色映射。
//!
//! 所有 UI 颜色和样式在此统一声明，ui.rs 和 app.rs 从中引用。

use ratatui::style::Color;

use crate::app::LineType;

// ════════════════════════════════════════════════════════════════
// 背景色
// ════════════════════════════════════════════════════════════════

/// 全局聊天背景（深紫黑）
pub const BG: Color = Color::from_u32(0x001A1A2E);

/// 用户消息块底色（深蓝）
pub const USER_BG: Color = Color::from_u32(0x001A3A5E);

/// 助理消息块底色（深灰蓝）
pub const ASSISTANT_BG: Color = Color::from_u32(0x00222A3E);

/// 工具调用/结果块底色（深灰）
pub const TOOL_BG: Color = Color::from_u32(0x00222222);

/// 输入区背景
pub const INPUT_BG: Color = Color::from_u32(0x00111111);

/// 确认栏字体底色
pub const CONFIRM_FONT_BG: Color = Color::from_u32(0x00222222);

/// 阴影色（drop shadow）
pub const SHADOW: Color = Color::from_u32(0x000D0D17);

/// 状态栏背景
pub const STATUS_BG: Color = Color::Black;

// ════════════════════════════════════════════════════════════════
// 前景色
// ════════════════════════════════════════════════════════════════

/// 助理文字
pub const ASSISTANT_FG: Color = Color::White;

/// 用户文字
pub const USER_FG: Color = Color::Cyan;

/// Thinking 文字
pub const THINKING_FG: Color = Color::Green;

/// 工具调用行（首行）
pub const TOOL_CALL_FG: Color = Color::Yellow;

/// 工具结果行 / Usage 行
pub const TOOL_RESULT_FG: Color = Color::DarkGray;

/// 错误文字
pub const ERROR_FG: Color = Color::Red;

/// 状态文字
pub const STATUS_FG: Color = Color::DarkGray;

/// 输入框 border / notice 文字
pub const INPUT_BORDER_FG: Color = Color::DarkGray;
pub const INPUT_NOTICE_FG: Color = Color::DarkGray;
pub const INPUT_FG: Color = Color::White;

/// 确认栏文字
pub const CONFIRM_FG: Color = Color::Yellow;
pub const CONFIRM_BLOCK_BG: Color = Color::DarkGray;
/// 确认栏选中项高亮背景
pub const CONFIRM_SELECTED_BG: Color = Color::from_u32(0x004A6A8E);
/// 确认栏选项标签（[A] 部分）前景色
pub const CONFIRM_OPTION_LABEL_FG: Color = Color::Cyan;
/// 确认栏普通选项文字
pub const CONFIRM_OPTION_FG: Color = Color::White;

// ════════════════════════════════════════════════════════════════
// BlockStyle — 消息块布局参数
// ════════════════════════════════════════════════════════════════

/// 消息块的统一布局参数。所有消息类型共用同一套渲染流程，差异由此数据驱动。
#[derive(Copy, Clone)]
pub struct BlockStyle {
    /// 垂直两端留白（字符数）
    pub margin_vertical: u16,
    /// 水平两端留白（字符数）
    pub margin_horizontal: u16,
    /// 底色；None → bottom_pad 画分隔线，Some → 画底色
    pub bg_fill: Option<Color>,
    /// 是否绘制右侧+底部 drop shadow
    pub shadow: bool,
    /// 内容下方行数（底色或分隔线）
    pub bottom_pad: u16,
}

impl BlockStyle {
    /// 计算该 block 占用的总行数
    pub const fn total_height(self, line_count: u16) -> u16 {
        1 + self.margin_vertical + line_count + self.bottom_pad
    }
}

pub const USER_STYLE: BlockStyle = BlockStyle {
    margin_vertical: 1,
    margin_horizontal: 1,
    bg_fill: Some(USER_BG),
    shadow: true,
    bottom_pad: 2,
};

pub const ASSISTANT_STYLE: BlockStyle = BlockStyle {
    margin_vertical: 1,
    margin_horizontal: 1,
    bg_fill: Some(ASSISTANT_BG),
    shadow: true,
    bottom_pad: 2,
};

pub const THINKING_STYLE: BlockStyle = BlockStyle {
    margin_vertical: 1,
    margin_horizontal: 1,
    bg_fill: None,
    shadow: false,
    bottom_pad: 1,
};

/// 工具调用/结果的 fallback 样式（无底色）
pub const TOOL_STYLE: BlockStyle = BlockStyle {
    margin_vertical: 1,
    margin_horizontal: 1,
    bg_fill: Some(TOOL_BG),
    shadow: true,
    bottom_pad: 0,
};

/// 工具调用样式（完整框 + 阴影）
pub const TOOL_CALL_STYLE: BlockStyle = BlockStyle {
    margin_vertical: 1,
    margin_horizontal: 1,
    bg_fill: Some(TOOL_BG),
    shadow: true,
    bottom_pad: 0,
};

/// 工具结果样式（缩进 2 格，无阴影，从属于调用）
pub const TOOL_RESULT_STYLE: BlockStyle = BlockStyle {
    margin_vertical: 0,
    margin_horizontal: 3,
    bg_fill: Some(TOOL_BG),
    shadow: false,
    bottom_pad: 1,
};

/// 工具错误样式（缩进 2 格，红色底色强调）
pub const TOOL_ERROR_STYLE: BlockStyle = BlockStyle {
    margin_vertical: 0,
    margin_horizontal: 3,
    bg_fill: Some(TOOL_BG),
    shadow: false,
    bottom_pad: 1,
};

/// Usage 统计行（已废弃，保留兼容）
pub const USAGE_STYLE: BlockStyle = BlockStyle {
    margin_vertical: 0,
    margin_horizontal: 1,
    bg_fill: Some(TOOL_BG),
    shadow: false,
    bottom_pad: 1,
};

// ════════════════════════════════════════════════════════════════
// LineType → 前景色 / BlockStyle 映射
// ════════════════════════════════════════════════════════════════

/// 获取消息类型对应的前景色
pub fn fg_for(line_type: LineType) -> Color {
    match line_type {
        LineType::User => USER_FG,
        LineType::Assistant => ASSISTANT_FG,
        LineType::Thinking => THINKING_FG,
        LineType::ToolCall { .. } => TOOL_CALL_FG,
        LineType::ToolResult { .. } => TOOL_RESULT_FG,
        LineType::ToolError { .. } => ERROR_FG,
        LineType::Error => ERROR_FG,
        LineType::Status => STATUS_FG,
        LineType::Usage => TOOL_RESULT_FG,
    }
}

/// 获取消息类型对应的 BlockStyle
pub fn style_for(line_type: LineType) -> BlockStyle {
    match line_type {
        LineType::User => USER_STYLE,
        LineType::Assistant => ASSISTANT_STYLE,
        LineType::Thinking => THINKING_STYLE,
        LineType::ToolCall { .. } => TOOL_CALL_STYLE,
        LineType::ToolResult { .. } => TOOL_RESULT_STYLE,
        LineType::ToolError { .. } => TOOL_ERROR_STYLE,
        LineType::Usage => USAGE_STYLE,
        LineType::Error | LineType::Status => TOOL_STYLE,
    }
}
