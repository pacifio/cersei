//! Tool call rendering with output preview and inline diffs.

use crate::tui::app::{ToolCall, ToolStatus};
use crate::tui::theme::Theme;
use crate::tui::widgets::diff_inline;
use ratatui::prelude::*;

const MAX_OUTPUT_LINES: usize = 5;
const MAX_FILE_TOOL_LINES: usize = 12;

/// Render a tool call as lines: badge + optional diff or output preview.
pub fn render_tool_call(tool: &ToolCall, theme: &Theme, frame_count: u64) -> Vec<Line<'static>> {
    let mut lines = Vec::new();

    let (icon, icon_style) = match tool.status {
        ToolStatus::Running => {
            let spinner = match (frame_count / 4) % 4 {
                0 => "⠋",
                1 => "⠙",
                2 => "⠹",
                _ => "⠸",
            };
            (spinner.to_string(), Style::default().fg(theme.accent))
        }
        ToolStatus::Done => ("✓".into(), Style::default().fg(theme.success)),
        ToolStatus::Error => ("✗".into(), Style::default().fg(theme.error)),
    };

    let dur = tool
        .duration_ms
        .map(|d| format!(" ({d}ms)"))
        .unwrap_or_default();

    lines.push(Line::from(vec![
        Span::styled(format!("  {icon} "), icon_style),
        Span::styled(
            tool.name.clone(),
            Style::default()
                .fg(theme.tool_badge)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" "),
        Span::styled(tool.input_summary.clone(), Style::default().fg(theme.dim)),
        Span::styled(dur, Style::default().fg(theme.dim)),
    ]));

    if let Some(ref output) = tool.output_preview {
        if tool.status != ToolStatus::Running && !output.is_empty() {
            // Try rendering as inline diff for file tools
            if let Some(diff_lines) = diff_inline::render_diff_output(output, &tool.name, theme) {
                lines.extend(diff_lines);
            } else {
                // Default: plain text preview
                let is_file_tool = matches!(tool.name.as_str(), "Edit" | "Write" | "ApplyPatch");
                let max_lines = if is_file_tool {
                    MAX_FILE_TOOL_LINES
                } else {
                    MAX_OUTPUT_LINES
                };

                let preview_lines: Vec<&str> = output.lines().take(max_lines).collect();
                let total = output.lines().count();
                let style = if tool.status == ToolStatus::Error {
                    Style::default().fg(theme.error)
                } else {
                    Style::default().fg(Color::DarkGray)
                };

                for pl in &preview_lines {
                    lines.push(Line::from(Span::styled(format!("    {pl}"), style)));
                }
                if total > max_lines {
                    lines.push(Line::from(Span::styled(
                        format!("    ... ({} more lines)", total - max_lines),
                        Style::default().fg(Color::DarkGray),
                    )));
                }
            }
        }
    }

    lines
}
