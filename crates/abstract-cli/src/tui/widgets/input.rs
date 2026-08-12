//! Input widget: multi-line textarea with wrapping.

use crate::tui::{app::AppState, theme::Theme};
use ratatui::{prelude::*, widgets::Paragraph};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

pub fn render(f: &mut Frame, area: Rect, state: &AppState, theme: &Theme) {
    let prompt = if state.is_streaming { "  " } else { "> " };
    let width = area.width as usize;
    if width < 4 {
        return;
    }

    let usable = width.saturating_sub(prompt.len());

    // Build visual lines from input (handle \n and wrapping)
    let vis_lines = visual_lines(&state.input, prompt, usable);

    // Find which visual line the cursor is on
    let (cursor_row, cursor_col) =
        cursor_visual_pos(&state.input, state.cursor_pos, prompt, usable);

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
    if width < 4 {
        return 1;
    }
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

        for (line_index, (start, end)) in wrap_ranges(seg, usable_width).into_iter().enumerate() {
            let p = if line_index == 0 { pfx } else { "  " };
            out.push(format!("{p}{}", &seg[start..end]));
        }
    }

    if out.is_empty() {
        out.push(prompt.to_string());
    }
    out
}

/// Find which visual row and column the cursor sits on.
fn cursor_visual_pos(
    input: &str,
    cursor_pos: usize,
    prompt: &str,
    usable_width: usize,
) -> (usize, usize) {
    let cursor_pos = cursor_pos.min(input.len());
    let mut row: usize = 0;
    let mut segment_start = 0;

    for (segment_index, seg) in input.split('\n').enumerate() {
        let segment_end = segment_start + seg.len();
        let ranges = wrap_ranges(seg, usable_width);

        if cursor_pos <= segment_end {
            let cursor_in_segment = cursor_pos.saturating_sub(segment_start);
            for (line_index, (start, end)) in ranges.iter().copied().enumerate() {
                let is_last_line = line_index + 1 == ranges.len();
                if cursor_in_segment < end || is_last_line {
                    let prefix_width = if segment_index == 0 && line_index == 0 {
                        prompt.width()
                    } else {
                        2
                    };
                    let column = seg[start..cursor_in_segment].width();
                    return (row + line_index, prefix_width + column);
                }
            }
        }

        row += ranges.len();
        segment_start = segment_end.saturating_add(1);
    }

    (row, 2)
}

/// Return the byte ranges of the visual rows used to render one logical line.
fn wrap_ranges(text: &str, max_width: usize) -> Vec<(usize, usize)> {
    if text.is_empty() {
        return vec![(0, 0)];
    }

    let mut ranges = Vec::new();
    let mut start = 0;

    while start < text.len() {
        let remaining = &text[start..];
        if remaining.width() <= max_width {
            ranges.push((start, text.len()));
            break;
        }

        let mut limit = byte_index_at_width(remaining, max_width);
        if limit == 0 {
            limit = remaining
                .graphemes(true)
                .next()
                .map_or(remaining.len(), str::len);
        }
        let line_end = remaining[..limit]
            .rfind(' ')
            .map(|index| index + 1)
            .unwrap_or(limit);
        let line_end = if line_end == 0 { limit } else { line_end };
        ranges.push((start, start + line_end));
        start += line_end;
    }

    ranges
}

fn byte_index_at_width(text: &str, max_width: usize) -> usize {
    let mut width = 0;
    for (index, grapheme) in text.grapheme_indices(true) {
        let next_width = width + grapheme.width();
        if next_width > max_width {
            return index;
        }
        width = next_width;
    }
    text.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn visual_lines_preserve_ascii_behavior() {
        assert_eq!(visual_lines("abcdefgh", "> ", 4), vec!["> abcd", "  efgh"]);
    }

    #[test]
    fn visual_lines_handle_unicode_display_width() {
        assert_eq!(visual_lines("éab🙂漢", "> ", 4), vec!["> éab", "  🙂漢"]);
    }

    #[test]
    fn cursor_uses_terminal_columns_for_unicode() {
        assert_eq!(cursor_visual_pos("é漢🙂", "é漢🙂".len(), "> ", 4), (1, 4));
    }

    #[test]
    fn cursor_after_full_width_unicode_line_and_newline_is_not_skipped() {
        assert_eq!(cursor_visual_pos("é漢a\n", "é漢a\n".len(), "> ", 4), (1, 2));
    }

    #[test]
    fn cursor_follows_the_same_word_wrap_as_rendering() {
        assert_eq!(cursor_visual_pos("ab cd ef", 5, "> ", 5), (1, 4));
    }
}
