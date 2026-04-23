//! Footer bar: context-aware keybinding hints

use crate::tui::theme::Theme;
use ratatui::{prelude::*, widgets::Paragraph};

pub fn render(
    f: &mut Frame,
    area: Rect,
    is_streaming: bool,
    side_panel_open: bool,
    side_panel_focused: bool,
    theme: &Theme,
) {
    let hints = if side_panel_focused {
        " j/k scroll | d/u page | Tab switch | r refresh | Esc back | ^B close".to_string()
    } else if is_streaming {
        " ^C cancel | PgUp/Dn scroll | ^B panel".to_string()
    } else if side_panel_open {
        " Enter send | ^B focus panel | Shift+Tab mode | /help | ^D exit".to_string()
    } else {
        " Enter send | Opt+Enter newline | ^B panel | Shift+Tab mode | /help | ^D exit".to_string()
    };

    let footer = Paragraph::new(hints).style(theme.dimmed());
    f.render_widget(footer, area);
}
