//! Header bar: model | mode | tokens | cost | session

use crate::tui::{app::AppState, theme::Theme};
use ratatui::{prelude::*, widgets::Paragraph};

pub fn render(f: &mut Frame, area: Rect, state: &AppState, theme: &Theme) {
    // Estimate cost from tokens if provider didn't report it
    let estimated_cost = if state.cost_usd > 0.0 {
        state.cost_usd
    } else {
        estimate_cost(&state.model, state.input_tokens, state.output_tokens)
    };

    let cost_str = if estimated_cost > 0.0 {
        format!("${:.4}", estimated_cost)
    } else {
        "$0".into()
    };

    let tokens_str = format!(
        "{}in/{}out",
        format_tokens(state.input_tokens),
        format_tokens(state.output_tokens),
    );

    let mode_str = state.permission_mode.label();

    let text = Line::from(vec![
        Span::styled(
            " abstract",
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(" | ", Style::default().fg(theme.dim)),
        Span::styled(&state.model, Style::default().fg(theme.fg)),
        Span::styled(" | ", Style::default().fg(theme.dim)),
        Span::styled(mode_str, mode_style(state.permission_mode, theme)),
        Span::styled(" | ", Style::default().fg(theme.dim)),
        Span::styled(&tokens_str, Style::default().fg(theme.dim)),
        Span::styled(" | ", Style::default().fg(theme.dim)),
        Span::styled(&cost_str, Style::default().fg(theme.fg)),
        Span::styled(" | ", Style::default().fg(theme.dim)),
        Span::styled(&state.session_id, Style::default().fg(theme.dim)),
    ]);

    let header = Paragraph::new(text).style(theme.header_style());
    f.render_widget(header, area);
}

fn mode_style(mode: crate::tui::app::PermissionMode, theme: &Theme) -> Style {
    use crate::tui::app::PermissionMode;
    match mode {
        PermissionMode::Auto => Style::default().fg(theme.success),
        PermissionMode::Plan => Style::default().fg(Color::Cyan),
        PermissionMode::Editor => Style::default().fg(Color::Blue),
        PermissionMode::Bypass => Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD),
        PermissionMode::BypassAlert => Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
    }
}

fn format_tokens(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}K", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}

/// Estimate USD cost from token counts based on model pricing.
fn estimate_cost(model: &str, input_tokens: u64, output_tokens: u64) -> f64 {
    // Pricing per 1M tokens (input, output)
    let (input_per_m, output_per_m) = match model {
        m if m.contains("gpt-5.3") => (2.0, 10.0),
        m if m.contains("gpt-5") => (2.0, 10.0),
        m if m.contains("gpt-4o") => (2.50, 10.0),
        m if m.contains("gpt-4-turbo") => (10.0, 30.0),
        m if m.starts_with("o1") => (15.0, 60.0),
        m if m.starts_with("o3") => (10.0, 40.0),
        m if m.contains("opus") => (15.0, 75.0),
        m if m.contains("sonnet") => (3.0, 15.0),
        m if m.contains("haiku") => (0.25, 1.25),
        m if m.contains("gemini-2.0-flash") => (0.075, 0.30),
        m if m.contains("gemini") => (1.25, 5.0),
        _ => (2.0, 10.0), // default estimate
    };

    let input_cost = (input_tokens as f64 / 1_000_000.0) * input_per_m;
    let output_cost = (output_tokens as f64 / 1_000_000.0) * output_per_m;
    input_cost + output_cost
}
