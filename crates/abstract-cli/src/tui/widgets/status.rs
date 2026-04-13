//! Status bar: streaming indicator, tool count, elapsed time

use crate::tui::{app::AppState, theme::Theme};
use ratatui::{prelude::*, widgets::Paragraph};

pub fn render(f: &mut Frame, area: Rect, state: &AppState, theme: &Theme) {
    let text = if state.is_streaming {
        let elapsed = state.elapsed_ms();
        let secs = elapsed as f64 / 1000.0;
        let tools = state.active_tools.len();
        let tokens = state.input_tokens + state.output_tokens;
        format!(
            " streaming... | {} tool(s) | {:.1}s | {} tokens",
            tools, secs, tokens
        )
    } else if state.turn_count > 0 {
        format!(
            " {} turn(s) | {} tool call(s) | {} in / {} out tokens",
            state.turn_count, state.tool_count, state.input_tokens, state.output_tokens
        )
    } else {
        " ready".into()
    };

    let style = if state.is_streaming {
        theme.accent_style()
    } else {
        theme.dimmed()
    };

    let status = Paragraph::new(text).style(style);
    f.render_widget(status, area);
}
