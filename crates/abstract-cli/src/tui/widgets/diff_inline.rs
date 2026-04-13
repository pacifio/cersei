//! Inline diff renderer: shows file changes with syntax-highlighted diff coloring.

use crate::tui::theme::Theme;
use ratatui::prelude::*;

const MAX_DIFF_LINES: usize = 15;

/// Render a tool result as an inline diff if it contains diff-like content.
/// Returns None if the content doesn't look like a diff.
pub fn render_diff_output(output: &str, tool_name: &str, theme: &Theme) -> Option<Vec<Line<'static>>> {
    let is_file_tool = matches!(tool_name, "Edit" | "Write" | "ApplyPatch" | "edit" | "write");
    if !is_file_tool {
        return None;
    }

    // Check if output contains diff-like content
    let has_diff_markers = output.lines().any(|l| {
        (l.starts_with('+') && !l.starts_with("+++"))
            || (l.starts_with('-') && !l.starts_with("---"))
            || l.starts_with("@@")
            || l.starts_with("diff ")
    });

    if !has_diff_markers && output.lines().count() < 2 {
        return None; // Too short or no diff content — use default rendering
    }

    let mut lines = Vec::new();

    // Header
    let label = if has_diff_markers { "diff" } else { "content" };
    lines.push(Line::from(Span::styled(
        format!("    ┌─ {label}"),
        Style::default().fg(theme.border),
    )));

    // Content lines with diff coloring
    let content_lines: Vec<&str> = output.lines().take(MAX_DIFF_LINES).collect();
    let total = output.lines().count();

    for line in &content_lines {
        let (prefix_style, text_style) = if line.starts_with('+') && !line.starts_with("+++") {
            (
                Style::default().fg(theme.diff_added),
                Style::default().fg(theme.diff_added),
            )
        } else if line.starts_with('-') && !line.starts_with("---") {
            (
                Style::default().fg(theme.diff_removed),
                Style::default().fg(theme.diff_removed),
            )
        } else if line.starts_with("@@") {
            (
                Style::default().fg(Color::Cyan),
                Style::default().fg(Color::Cyan),
            )
        } else if line.starts_with("diff ") || line.starts_with("index ") || line.starts_with("---") || line.starts_with("+++") {
            (
                Style::default().fg(theme.text_tertiary),
                Style::default().fg(theme.text_tertiary),
            )
        } else {
            (
                Style::default().fg(theme.border),
                Style::default().fg(theme.text_tertiary),
            )
        };

        lines.push(Line::from(vec![
            Span::styled("    │ ", prefix_style),
            Span::styled(line.to_string(), text_style),
        ]));
    }

    if total > MAX_DIFF_LINES {
        lines.push(Line::from(Span::styled(
            format!("    │ ... ({} more lines)", total - MAX_DIFF_LINES),
            Style::default().fg(theme.text_ghost),
        )));
    }

    // Footer
    lines.push(Line::from(Span::styled(
        "    └─".to_string(),
        Style::default().fg(theme.border),
    )));

    Some(lines)
}
