//! TUI layout — 5-zone vertical split with optional side panel.

use ratatui::prelude::*;

/// Compute the full layout: main 5-zone + optional side panel.
pub fn compute(area: Rect, input_height: u16, side_panel_open: bool) -> LayoutFull {
    let (main_area, side_panel) = if side_panel_open && area.width > 80 {
        // 62% left, 38% right
        let h_chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(62), Constraint::Percentage(38)])
            .split(area);
        (h_chunks[0], Some(h_chunks[1]))
    } else {
        (area, None)
    };

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),            // header
            Constraint::Min(3),               // messages (fills remaining)
            Constraint::Length(1),            // status bar
            Constraint::Length(input_height), // input area
            Constraint::Length(1),            // footer
        ])
        .split(main_area);

    LayoutFull {
        main: Layout5 {
            header: chunks[0],
            messages: chunks[1],
            status: chunks[2],
            input: chunks[3],
            footer: chunks[4],
        },
        side_panel,
    }
}

pub struct LayoutFull {
    pub main: Layout5,
    pub side_panel: Option<Rect>,
}

pub struct Layout5 {
    pub header: Rect,
    pub messages: Rect,
    pub status: Rect,
    pub input: Rect,
    pub footer: Rect,
}
