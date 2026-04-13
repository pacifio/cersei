//! Modal overlays: help, permission, recovery.

use crate::tui::{
    app::{AppState, Overlay, PermissionOverlay, RecoveryOverlay},
    theme::Theme,
};
use ratatui::{
    prelude::*,
    widgets::{Block, Borders, Clear, Paragraph, Wrap},
};

pub fn render(f: &mut Frame, state: &AppState, theme: &Theme) {
    match &state.overlay {
        Overlay::None => {}
        Overlay::Help => render_help(f, theme),
        Overlay::Permission(p) => render_permission(f, p, theme),
        Overlay::Recovery(r) => render_recovery(f, r, theme),
        Overlay::Graph(g) => super::graph::render(f, g, theme),
    }
}

fn centered_rect(area: Rect, width_pct: u16, height_pct: u16) -> Rect {
    let vert = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - height_pct) / 2),
            Constraint::Percentage(height_pct),
            Constraint::Percentage((100 - height_pct) / 2),
        ])
        .split(area);
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - width_pct) / 2),
            Constraint::Percentage(width_pct),
            Constraint::Percentage((100 - width_pct) / 2),
        ])
        .split(vert[1])[1]
}

fn render_help(f: &mut Frame, theme: &Theme) {
    let area = centered_rect(f.area(), 60, 60);
    f.render_widget(Clear, area);

    let help_text = vec![
        Line::from(Span::styled("Commands", Style::default().fg(theme.accent).add_modifier(Modifier::BOLD))),
        Line::default(),
        Line::from("  /help        Show this help"),
        Line::from("  /clear       Clear conversation"),
        Line::from("  /cost        Show usage and cost"),
        Line::from("  /model       Show current model"),
        Line::from("  /memory      Memory info"),
        Line::from("  /sessions    Session info"),
        Line::from("  /diff        Open git diff panel"),
        Line::from("  /files       Open file tree panel"),
        Line::from("  /panel       Toggle side panel"),
        Line::from("  /graph       Show memory graph"),
        Line::from("  /undo        Undo last file change"),
        Line::from("  /rewind      Remove last assistant turn"),
        Line::from("  /compact     Context compaction info"),
        Line::from("  /exit        Exit"),
        Line::default(),
        Line::from(Span::styled("Keys", Style::default().fg(theme.accent).add_modifier(Modifier::BOLD))),
        Line::default(),
        Line::from("  Enter        Send message"),
        Line::from("  Ctrl+C       Cancel / clear / quit"),
        Line::from("  Ctrl+D       Exit"),
        Line::from("  Ctrl+B       Toggle side panel"),
        Line::from("  Shift+Tab    Cycle permission mode"),
        Line::from("  Tab          Switch panel tabs"),
        Line::from("  PgUp/PgDn    Scroll messages"),
        Line::from("  Ctrl+↑↓     Scroll side panel"),
        Line::from("  Up/Down      Scroll (empty) / history"),
        Line::from("  Esc          Close overlay"),
        Line::default(),
        Line::from(Span::styled("Modes (Shift+Tab)", Style::default().fg(theme.accent).add_modifier(Modifier::BOLD))),
        Line::default(),
        Line::from("  Auto         Ask for permissions"),
        Line::from("  Plan         Read-only, no execution"),
        Line::from("  Editor       All except shell commands"),
        Line::from("  Bypass       All permissions bypassed"),
        Line::from("  Bypass+Alert Bypass + notify on shell"),
        Line::default(),
        Line::from(Span::styled("Press Esc to close", theme.dimmed())),
    ];

    let block = Block::default()
        .title(" Help ")
        .borders(Borders::ALL)
        .border_style(theme.border_style())
        .style(Style::default().bg(theme.bg));

    let help = Paragraph::new(help_text).block(block).wrap(Wrap { trim: false });
    f.render_widget(help, area);
}

fn render_permission(f: &mut Frame, p: &PermissionOverlay, theme: &Theme) {
    // Use larger area to prevent overflow on small terminals
    let area = centered_rect(f.area(), 75, 55);
    f.render_widget(Clear, area);

    let options = vec!["Allow once", "Allow for session", "Always allow", "Deny"];
    let mut lines = vec![
        Line::default(),
        Line::from(Span::styled(
            format!("  Tool: {}", p.tool_name),
            theme.warning_style().add_modifier(Modifier::BOLD),
        )),
        Line::default(),
        Line::from(Span::styled(format!("  {}", p.description), theme.text())),
        Line::default(),
    ];

    for (i, opt) in options.iter().enumerate() {
        let style = if i == p.selected {
            Style::default().fg(theme.accent).add_modifier(Modifier::BOLD)
        } else {
            theme.text()
        };
        let marker = if i == p.selected { ">" } else { " " };
        lines.push(Line::from(Span::styled(format!("    {marker} {opt}"), style)));
    }

    lines.push(Line::default());
    lines.push(Line::from(Span::styled("  Up/Down select, Enter confirm, Esc deny", theme.dimmed())));
    lines.push(Line::default());

    let block = Block::default()
        .title(" Permission Required ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.warning))
        .style(Style::default().bg(theme.bg));

    let widget = Paragraph::new(lines).block(block).wrap(Wrap { trim: true });
    f.render_widget(widget, area);
}

fn render_recovery(f: &mut Frame, r: &RecoveryOverlay, theme: &Theme) {
    let area = centered_rect(f.area(), 75, 55);
    f.render_widget(Clear, area);

    let mut lines = vec![
        Line::from(Span::styled("Provider error", theme.error_style().add_modifier(Modifier::BOLD))),
        Line::default(),
        Line::from(Span::styled(&r.error_msg, theme.dimmed())),
        Line::default(),
    ];

    for (i, opt) in r.options.iter().enumerate() {
        let style = if i == r.selected {
            Style::default().fg(theme.accent).add_modifier(Modifier::BOLD)
        } else {
            theme.text()
        };
        let marker = if i == r.selected { ">" } else { " " };
        lines.push(Line::from(Span::styled(format!("  {marker} {opt}"), style)));
    }

    lines.push(Line::default());
    lines.push(Line::from(Span::styled("Up/Down to select, Enter to confirm, Esc to skip", theme.dimmed())));

    let block = Block::default()
        .title(" Provider Error ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.error))
        .style(Style::default().bg(theme.bg));

    let widget = Paragraph::new(lines).block(block);
    f.render_widget(widget, area);
}
