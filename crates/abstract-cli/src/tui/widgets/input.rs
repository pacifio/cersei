//! Input widget: multi-line textarea with wrapping.

use crate::tui::{app::AppState, theme::Theme};
use ratatui::{prelude::*, widgets::Paragraph};

pub fn render(f: &mut Frame, area: Rect, state: &AppState, theme: &Theme) {
    let prompt = if state.is_streaming { "  " } else { "> " };
    let width = area.width as usize;
    if width < 4 { return; }

    let usable = width.saturating_sub(prompt.len());

    // Build visual lines from input (handle \n and wrapping)
    let vis_lines = visual_lines(&state.input, prompt, usable);

    // Find which visual line the cursor is on
    let (cursor_row, cursor_col) = cursor_visual_pos(&state.input, state.cursor_pos, prompt, usable);

    // Scroll so cursor row is visible
    let scroll = if cursor_row as u16 >= area.height {
        cursor_row as u16 - area.height + 1
    } else {
        0
    };

    let lines: Vec<Line> = vis_lines.iter().map(|s| Line::raw(s.as_str())).collect();
    let widget = Paragraph::new(lines)
        .style(Style::default().fg(theme.fg).bg(theme.input_bg))
        .scroll((scroll, 0));
    f.render_widget(widget, area);

    if !state.is_streaming {
        let cx = area.x + cursor_col as u16;
        let cy = area.y + (cursor_row as u16).saturating_sub(scroll);
        f.set_cursor_position((
            cx.min(area.right().saturating_sub(1)),
            cy.min(area.bottom().saturating_sub(1)),
        ));
    }
}

/// Desired height for the input area.
pub fn desired_height(input: &str, width: u16) -> u16 {
    if width < 4 { return 1; }
    let usable = (width as usize).saturating_sub(2);
    let lines = visual_lines(input, "> ", usable);
    (lines.len() as u16).clamp(1, 10)
}

/// Build the visual lines as they appear on screen.
fn visual_lines(input: &str, prompt: &str, usable_width: usize) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let logical: Vec<&str> = input.split('\n').collect();

    for (i, seg) in logical.iter().enumerate() {
        let pfx = if i == 0 { prompt } else { "  " };

        if seg.is_empty() {
            out.push(pfx.to_string());
            continue;
        }

        // Word-wrap this segment
        let mut rem = *seg;
        let mut first = true;
        while !rem.is_empty() {
            let p = if first { pfx } else { "  " };
            let cap = usable_width;
            if rem.len() <= cap {
                out.push(format!("{p}{rem}"));
                break;
            }
            let brk = rem[..cap].rfind(' ').map(|i| i + 1).unwrap_or(cap);
            let brk = if brk == 0 { cap } else { brk };
            out.push(format!("{p}{}", &rem[..brk]));
            rem = &rem[brk..];
            first = false;
        }
    }

    if out.is_empty() {
        out.push(prompt.to_string());
    }
    out
}

/// Find which visual row and column the cursor sits on.
fn cursor_visual_pos(input: &str, cursor_pos: usize, prompt: &str, usable_width: usize) -> (usize, usize) {
    let before = &input[..cursor_pos.min(input.len())];
    let logical: Vec<&str> = before.split('\n').collect();

    let mut row: usize = 0;

    for (i, seg) in logical.iter().enumerate() {
        let is_last = i == logical.len() - 1;
        let pfx_len = if i == 0 { prompt.len() } else { 2 };

        if is_last {
            // Cursor is somewhere in this segment
            let len = seg.len();
            if usable_width == 0 {
                return (row, pfx_len);
            }
            let wrapped_full_rows = len / usable_width;
            let col_in_last = len % usable_width;
            row += wrapped_full_rows;
            return (row, pfx_len + col_in_last);
        }

        // Not the last — count full visual rows this segment occupies
        let len = seg.len();
        if len == 0 {
            row += 1;
        } else if usable_width > 0 {
            row += (len / usable_width) + 1;
        } else {
            row += 1;
        }
    }

    // Cursor is right after a trailing newline
    let pfx_len = if logical.is_empty() { prompt.len() } else { 2 };
    (row, pfx_len)
}
