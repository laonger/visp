//! 鼠标文本选择 + OSC 52 剪贴板复制
//!
//! 设计参考 opencode 的 selection.ts + clipboard.ts：
//! - 在 chat area 内拖拽鼠标选择文字，渲染反色高亮
//! - Ctrl+C 将选中文字通过 OSC 52 写入系统粘贴板
//! - Esc 清除选择

use ratatui::buffer::Buffer;
use unicode_width::UnicodeWidthStr;

/// 文本选择范围（内容坐标：row 包含 scroll 偏移，滚动时高亮跟随文字）
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TextSelection {
    pub start: Option<(u16, u16)>,
    pub end: Option<(u16, u16)>,
}

impl TextSelection {
    /// 是否有选择范围（用于 Ctrl+C 复制判断）
    pub fn is_active(&self) -> bool {
        self.start.is_some() && self.end.is_some()
    }

    /// 是否有实际选中内容（start != end），用于显示高亮
    pub fn is_highlighting(&self) -> bool {
        self.is_active() && self.start != self.end
    }

    pub fn clear(&mut self) {
        self.start = None;
        self.end = None;
    }

    /// 返回选择矩形区域 (min_col, min_row, max_col, max_row) - 内容坐标
    fn bounds(&self) -> Option<(u16, u16, u16, u16)> {
        let (sc, sr) = self.start?;
        let (ec, er) = self.end?;
        Some((sc.min(ec), sr.min(er), sc.max(ec), sr.max(er)))
    }

    /// 判断某个**屏幕坐标**是否在选择范围内。
    /// `scroll_y` 为当前滚动偏移，用于将屏幕 row 转为内容 row。
    pub fn contains(&self, col: u16, screen_row: u16, scroll_y: u16) -> bool {
        if !self.is_active() {
            return false;
        }
        let (min_c, min_r, max_c, max_r) = match self.bounds() {
            Some(b) => b,
            None => return false,
        };
        // 屏幕坐标 -> 内容坐标
        let row = screen_row + scroll_y;
        if row < min_r || row > max_r {
            return false;
        }
        if min_r == max_r {
            col >= min_c && col <= max_c
        } else if row == min_r {
            col >= min_c
        } else if row == max_r {
            col <= max_c
        } else {
            true
        }
    }
}

/// 将文本写入系统剪贴板
///
/// 优先使用系统原生命令（pbcopy / xclip / wl-copy），
/// 这些在本地终端中比 OSC 52 可靠得多（OSC 52 在 tmux、screen 等
/// 多路复用器中经常被拦截或吞掉）。
pub fn osc52_copy(text: &str) {
    use std::io::Write;
    use std::process::{Command, Stdio};

    // macOS: pbcopy
    #[cfg(target_os = "macos")]
    {
        if let Ok(mut child) = Command::new("pbcopy")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        {
            if let Some(mut stdin) = child.stdin.take() {
                let _ = stdin.write_all(text.as_bytes());
            }
            let _ = child.wait();
            return;
        }
    }

    // Linux: Wayland (wl-copy) 或 X11 (xclip / xsel)
    #[cfg(target_os = "linux")]
    {
        // wl-copy (Wayland)
        if let Ok(mut child) = Command::new("wl-copy")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        {
            if let Some(mut stdin) = child.stdin.take() {
                let _ = stdin.write_all(text.as_bytes());
            }
            let _ = child.wait();
            return;
        }
        // xclip (X11)
        if let Ok(mut child) = Command::new("xclip")
            .args(["-selection", "clipboard"])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        {
            if let Some(mut stdin) = child.stdin.take() {
                let _ = stdin.write_all(text.as_bytes());
            }
            let _ = child.wait();
            return;
        }
        // xsel (X11)
        if let Ok(mut child) = Command::new("xsel")
            .args(["--clipboard", "--input"])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        {
            if let Some(mut stdin) = child.stdin.take() {
                let _ = stdin.write_all(text.as_bytes());
            }
            let _ = child.wait();
            return;
        }
    }

    // Fallback: OSC 52
    let encoded = base64_encode(text.as_bytes());
    let seq = format!("\x1b]52;c;{encoded}\x1b\\");
    let mut stdout = std::io::stdout();
    let _ = stdout.flush();
    let _ = write!(stdout, "{seq}");
    let _ = stdout.flush();
}

/// 简易 Base64 编码器
fn base64_encode(data: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut result = String::with_capacity(data.len().div_ceil(3) * 4);
    let mut i = 0;
    while i + 2 < data.len() {
        let n = ((data[i] as u32) << 16) | ((data[i + 1] as u32) << 8) | (data[i + 2] as u32);
        result.push(TABLE[((n >> 18) & 0x3f) as usize] as char);
        result.push(TABLE[((n >> 12) & 0x3f) as usize] as char);
        result.push(TABLE[((n >> 6) & 0x3f) as usize] as char);
        result.push(TABLE[(n & 0x3f) as usize] as char);
        i += 3;
    }
    let remaining = data.len() - i;
    if remaining == 1 {
        let n = (data[i] as u32) << 16;
        result.push(TABLE[((n >> 18) & 0x3f) as usize] as char);
        result.push(TABLE[((n >> 12) & 0x3f) as usize] as char);
        result.push('=');
        result.push('=');
    } else if remaining == 2 {
        let n = ((data[i] as u32) << 16) | ((data[i + 1] as u32) << 8);
        result.push(TABLE[((n >> 18) & 0x3f) as usize] as char);
        result.push(TABLE[((n >> 12) & 0x3f) as usize] as char);
        result.push(TABLE[((n >> 6) & 0x3f) as usize] as char);
        result.push('=');
    }
    result
}

/// 从 ratatui Buffer 中提取选择范围内的文本
///
/// 逐行读取，跳过每行末尾的 padding 空格，多行之间用 \n 连接。
/// `scroll_y` 用于将内容坐标转回屏幕坐标。
pub fn extract_selected_text(buf: &Buffer, selection: &TextSelection, scroll_y: u16) -> String {
    if !selection.is_active() {
        return String::new();
    }
    let (min_c, min_r, max_c, max_r) = match selection.bounds() {
        Some(b) => b,
        None => return String::new(),
    };

    // 内容坐标 -> 屏幕坐标
    let min_r_screen = min_r.saturating_sub(scroll_y);
    let max_r_screen = max_r.saturating_sub(scroll_y);

    let area = buf.area();
    let mut lines: Vec<String> = Vec::new();

    for row in min_r_screen..=max_r_screen {
        let mut line = String::new();
        let is_first = row == min_r_screen;
        let is_last = row == max_r_screen;
        let col_start = if is_first { min_c } else { 0 };
        let col_end = if is_last {
            max_c
        } else {
            area.right().saturating_sub(1)
        };

        let mut col = col_start;
        while col <= col_end {
            if col >= area.right() || row >= area.bottom() {
                break;
            }
            let cell = &buf[(col, row)];
            let symbol = cell.symbol();
            let width = UnicodeWidthStr::width(symbol).max(1) as u16;
            line.push_str(symbol);
            col = col.saturating_add(width);
        }
        // 去掉行尾空格（padding）
        let trimmed = line.trim_end();
        lines.push(trimmed.to_string());
    }

    // 移除末尾的空行
    while lines.len() > 1 && lines.last().is_none_or(|l| l.is_empty()) {
        lines.pop();
    }

    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_selection_default_inactive() {
        let sel = TextSelection::default();
        assert!(!sel.is_active());
        assert!(!sel.is_highlighting());
        assert!(!sel.contains(0, 0, 0));
    }

    #[test]
    fn test_selection_single_line() {
        let sel = TextSelection {
            start: Some((5, 3)),
            end: Some((10, 3)),
        };
        assert!(sel.is_active());
        assert!(sel.is_highlighting());
        assert!(sel.contains(5, 3, 0));
        assert!(sel.contains(7, 3, 0));
        assert!(sel.contains(10, 3, 0));
        assert!(!sel.contains(4, 3, 0));
        assert!(!sel.contains(11, 3, 0));
        assert!(!sel.contains(5, 2, 0));
    }

    #[test]
    fn test_selection_multi_line() {
        // 选择从 (3, 2) 到 (7, 4)
        let sel = TextSelection {
            start: Some((3, 2)),
            end: Some((7, 4)),
        };
        assert!(sel.is_active());
        // 首行：列 >= 3
        assert!(sel.contains(3, 2, 0));
        assert!(sel.contains(50, 2, 0));
        assert!(!sel.contains(2, 2, 0));
        // 中间行：全选
        assert!(sel.contains(0, 3, 0));
        assert!(sel.contains(99, 3, 0));
        // 末行：列 <= 7
        assert!(sel.contains(0, 4, 0));
        assert!(sel.contains(7, 4, 0));
        assert!(!sel.contains(8, 4, 0));
        // 超出范围
        assert!(!sel.contains(0, 1, 0));
        assert!(!sel.contains(0, 5, 0));
    }

    #[test]
    fn test_selection_with_scroll() {
        // 内容坐标选择 (5, 10) 到 (10, 10)，滚动 y=5
        // 屏幕坐标 row = 10 - 5 = 5
        let sel = TextSelection {
            start: Some((5, 10)),
            end: Some((10, 10)),
        };
        assert!(sel.contains(5, 5, 5));
        assert!(sel.contains(7, 5, 5));
        assert!(!sel.contains(5, 4, 5));
        assert!(!sel.contains(5, 6, 5));
    }

    #[test]
    fn test_selection_single_point_not_highlighting() {
        // start == end（纯点击），不高亮
        let sel = TextSelection {
            start: Some((5, 3)),
            end: Some((5, 3)),
        };
        assert!(sel.is_active()); // 有选择范围
        assert!(!sel.is_highlighting()); // 但不高亮
    }

    #[test]
    fn test_selection_reversed_coords() {
        // end 在 start 之前
        let sel = TextSelection {
            start: Some((10, 5)),
            end: Some((3, 2)),
        };
        assert!(sel.is_active());
        assert!(sel.contains(5, 3, 0));
        assert!(sel.contains(10, 5, 0));
        assert!(sel.contains(3, 2, 0));
    }

    #[test]
    fn test_selection_clear() {
        let mut sel = TextSelection {
            start: Some((0, 0)),
            end: Some((5, 5)),
        };
        assert!(sel.is_active());
        sel.clear();
        assert!(!sel.is_active());
        assert!(!sel.is_highlighting());
    }

    #[test]
    fn test_base64_encode_empty() {
        assert_eq!(base64_encode(b""), "");
    }

    #[test]
    fn test_base64_encode_short() {
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
        assert_eq!(base64_encode(b"foob"), "Zm9vYg==");
        assert_eq!(base64_encode(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn test_base64_encode_unicode() {
        // "你好" in UTF-8
        assert_eq!(base64_encode("你好".as_bytes()), "5L2g5aW9");
    }

    #[test]
    fn test_extract_selected_text_single_line() {
        use ratatui::buffer::Buffer;
        use ratatui::layout::Rect;
        let mut buf = Buffer::empty(Rect::new(0, 0, 20, 5));
        // 写入一些文本到第 2 行
        for (i, ch) in "Hello World".chars().enumerate() {
            buf[(i as u16 + 2, 2)].set_symbol(ch.to_string().as_str());
        }
        // 选择 col 2..=6 = "Hello"
        let sel = TextSelection {
            start: Some((2, 2)),
            end: Some((6, 2)),
        };
        let text = extract_selected_text(&buf, &sel, 0);
        assert_eq!(text, "Hello");
    }

    #[test]
    fn test_extract_selected_text_multi_line() {
        use ratatui::buffer::Buffer;
        use ratatui::layout::Rect;
        let mut buf = Buffer::empty(Rect::new(0, 0, 20, 5));
        for (i, ch) in "Line1".chars().enumerate() {
            buf[(i as u16, 1)].set_symbol(ch.to_string().as_str());
        }
        for (i, ch) in "Line2".chars().enumerate() {
            buf[(i as u16, 2)].set_symbol(ch.to_string().as_str());
        }
        // 选择从第 1 行 col 2 到第 2 行 col 3
        // 第 1 行: col 2..=19 → "ne1" (trim_end)
        // 第 2 行: col 0..=3 → "Line"
        let sel = TextSelection {
            start: Some((2, 1)),
            end: Some((3, 2)),
        };
        let text = extract_selected_text(&buf, &sel, 0);
        assert_eq!(text, "ne1\nLine");
    }

    #[test]
    fn test_extract_selected_text_chinese() {
        use ratatui::buffer::Buffer;
        use ratatui::layout::Rect;
        use ratatui::style::Style;
        let mut buf = Buffer::empty(Rect::new(0, 0, 20, 3));
        // 使用 set_string 正确渲染中文（会自动处理全角字符的 continuation cells）
        buf.set_string(0, 1, "你好世界", Style::default());
        buf.set_string(0, 2, "Hello 你好", Style::default());
        // 选择整行
        let sel = TextSelection {
            start: Some((0, 1)),
            end: Some((19, 2)),
        };
        let text = extract_selected_text(&buf, &sel, 0);
        // 不应有空格在中文之间
        assert_eq!(text, "你好世界\nHello 你好");
    }
}
