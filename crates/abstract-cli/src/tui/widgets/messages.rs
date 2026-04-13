//! Messages widget: virtualized scrollable conversation.
//!
//! Only renders visible items. Committed turns are pre-built once;
//! streaming content is rebuilt every frame (only a few lines).

use crate::tui::{
    app::{AppState, TurnRole},
    theme::Theme,
    virtual_list::VItem,
    widgets::tool_call::render_tool_call,
};
use ratatui::prelude::*;

pub fn render(f: &mut Frame, area: Rect, state: &mut AppState, theme: &Theme) {
    let width = area.width.saturating_sub(4);

    // Rebuild committed items only when dirty or width changed
    if state.messages_dirty || state.virtual_list.width_changed(width) {
        let committed = build_committed_lines(&state.turns, theme, width, state.frame_count);
        state.virtual_list.set_committed(committed);
        state.virtual_list.set_width(width);
        state.messages_dirty = false;
    }

    // Streaming items — rebuilt every frame (cheap, only a few lines)
    let streaming = build_streaming_lines(state, theme, width);
    state.virtual_list.set_streaming(streaming);

    // Update viewport
    state.virtual_list.set_viewport(area.height);

    // Sync scroll state
    state.scroll.update_dimensions(state.virtual_list.total_height(), area.height);
    state.virtual_list.scroll_offset = state.scroll.effective_offset();
    state.virtual_list.sticky_bottom = state.scroll.sticky_bottom;

    // Render visible items to buffer
    state.virtual_list.render(area, f.buffer_mut());
}

/// Build lines for all committed turns (cached, not rebuilt per frame).
fn build_committed_lines(
    turns: &[crate::tui::app::Turn],
    theme: &Theme,
    width: u16,
    frame_count: u64,
) -> Vec<VItem> {
    let mut items = Vec::new();

    for turn in turns {
        match turn.role {
            TurnRole::User => {
                let wrapped = wrap_text(&turn.content, width as usize);
                for (i, wline) in wrapped.iter().enumerate() {
                    let prefix = if i == 0 { "> " } else { "  " };
                    items.push(VItem::new(Line::from(vec![
                        Span::styled(prefix, theme.accent_style()),
                        Span::styled(wline.clone(), Style::default().fg(theme.user_msg)),
                    ])));
                }
                items.push(VItem::new(Line::default()));
            }
            TurnRole::Assistant => {
                for tool in &turn.tools {
                    for line in render_tool_call(tool, theme, frame_count) {
                        items.push(VItem::new(line));
                    }
                }
                if !turn.content.is_empty() {
                    let md_lines = crate::tui::markdown::render_markdown(&turn.content, width);
                    for md_line in md_lines {
                        let mut spans = vec![Span::raw("  ")];
                        spans.extend(md_line.spans);
                        items.push(VItem::new(Line::from(spans)));
                    }
                }
                items.push(VItem::new(Line::default()));
            }
            TurnRole::System => {
                let wrapped = wrap_text(&turn.content, width as usize);
                for wline in &wrapped {
                    items.push(VItem::new(Line::from(Span::styled(
                        format!("  [system] {}", wline),
                        theme.dimmed(),
                    ))));
                }
                items.push(VItem::new(Line::default()));
            }
        }
    }

    items
}

/// Build lines for active streaming content (rebuilt every frame — cheap).
fn build_streaming_lines(state: &AppState, theme: &Theme, width: u16) -> Vec<VItem> {
    let mut items = Vec::new();

    if !state.is_streaming {
        return items;
    }

    // Active tool calls
    for tool in &state.active_tools {
        for line in render_tool_call(tool, theme, state.frame_count) {
            items.push(VItem::new(line));
        }
    }

    // Thinking indicator
    if !state.streaming_thinking.is_empty() {
        items.push(VItem::new(Line::from(Span::styled(
            "  thinking...",
            Style::default().fg(theme.thinking),
        ))));
    }

    // Streaming text
    if !state.streaming_text.is_empty() {
        let md_lines = crate::tui::markdown::render_markdown(&state.streaming_text, width);
        for md_line in md_lines {
            let mut spans = vec![Span::raw("  ")];
            spans.extend(md_line.spans);
            items.push(VItem::new(Line::from(spans)));
        }
    }

    // Cursor blink
    if state.streaming_text.is_empty() && state.active_tools.is_empty() {
        let dot = if state.frame_count % 8 < 4 { "▊" } else { " " };
        items.push(VItem::new(Line::from(Span::styled(
            format!("  {dot}"),
            theme.accent_style(),
        ))));
    }

    items
}

/// Word-wrap text.
fn wrap_text(text: &str, max_width: usize) -> Vec<String> {
    if max_width == 0 {
        return vec![text.to_string()];
    }
    let mut result = Vec::new();
    for line in text.lines() {
        if line.len() <= max_width {
            result.push(line.to_string());
        } else {
            let mut remaining = line;
            while remaining.len() > max_width {
                let break_at = remaining[..max_width]
                    .rfind(' ')
                    .unwrap_or(max_width);
                let break_at = if break_at == 0 { max_width } else { break_at };
                result.push(remaining[..break_at].to_string());
                remaining = remaining[break_at..].trim_start();
            }
            if !remaining.is_empty() {
                result.push(remaining.to_string());
            }
        }
    }
    if result.is_empty() {
        result.push(String::new());
    }
    result
}
