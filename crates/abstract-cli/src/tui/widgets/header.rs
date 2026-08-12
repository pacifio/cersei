//! Header bar: model | mode | tokens | cost | session

use crate::tui::{app::AppState, theme::Theme};
use ratatui::{
    prelude::*,
    widgets::{Clear, Paragraph},
};
use unicode_width::UnicodeWidthStr;

const UNKNOWN_PRICE_TOOLTIP: &str = "no price for this provider";

pub fn render(f: &mut Frame, area: Rect, state: &AppState, theme: &Theme) {
    let cost_span = cost_label(
        state.cost_usd,
        state.cost_available,
        state.cache_hit_ratio(),
    );

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
        Span::styled(&cost_span, Style::default().fg(theme.fg)),
        Span::styled(" | ", Style::default().fg(theme.dim)),
        Span::styled(&state.session_id, Style::default().fg(theme.dim)),
    ]);

    f.render_widget(Paragraph::new(text).style(theme.header_style()), area);

    let hitbox = cost_hitbox(area, &state.model, mode_str, &tokens_str);
    if !state.cost_available
        && state
            .mouse_position
            .is_some_and(|position| hitbox.contains(position.into()))
    {
        render_unknown_price_tooltip(f, hitbox, theme);
    }
}

fn cost_hitbox(area: Rect, model: &str, mode: &str, tokens: &str) -> Rect {
    let prefix = format!(" abstract | {model} | {mode} | {tokens} | ");
    let offset = u16::try_from(prefix.width()).unwrap_or(u16::MAX);
    Rect::new(area.x.saturating_add(offset), area.y, 2, 1).intersection(area)
}

fn cost_label(cost_usd: f64, available: bool, cache_ratio: Option<f64>) -> String {
    let cost = if available {
        format!("${cost_usd:.4}")
    } else {
        "??".to_string()
    };
    match cache_ratio {
        Some(ratio) => format!("{cost} ({:.0}%)", ratio * 100.0),
        None => cost,
    }
}

fn render_unknown_price_tooltip(f: &mut Frame, anchor: Rect, theme: &Theme) {
    let frame = f.area();
    if frame.height <= anchor.y.saturating_add(1) {
        return;
    }

    let width = u16::try_from(UNKNOWN_PRICE_TOOLTIP.width()).unwrap_or(frame.width);
    let max_x = frame.right().saturating_sub(width);
    let area = Rect::new(anchor.x.min(max_x), anchor.y + 1, width, 1);
    f.render_widget(Clear, area);
    f.render_widget(
        Paragraph::new(UNKNOWN_PRICE_TOOLTIP).style(Style::default().fg(theme.fg).bg(theme.bg)),
        area,
    );
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_price_hitbox_covers_only_the_question_marks() {
        let area = Rect::new(0, 0, 100, 1);
        let hitbox = cost_hitbox(area, "deepseek-chat", "auto", "12in/3out");
        assert_eq!(hitbox.width, 2);
        assert!(hitbox.contains((hitbox.x, 0).into()));
        assert!(hitbox.contains((hitbox.x + 1, 0).into()));
        assert!(!hitbox.contains((hitbox.x.saturating_sub(1), 0).into()));
    }

    #[test]
    fn hitbox_is_empty_when_cost_is_clipped() {
        let hitbox = cost_hitbox(
            Rect::new(0, 0, 10, 1),
            "very-long-model",
            "auto",
            "0in/0out",
        );
        assert!(hitbox.is_empty());
    }

    #[test]
    fn cost_and_cache_ratio_are_adjacent_for_known_and_unknown_prices() {
        assert_eq!(cost_label(0.4281, true, Some(0.78)), "$0.4281 (78%)");
        assert_eq!(cost_label(0.0, false, Some(0.78)), "?? (78%)");
        assert_eq!(UNKNOWN_PRICE_TOOLTIP, "no price for this provider");
    }
}
