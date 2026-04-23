//! Side panel: tabbed view with git diff and file tree.

use crate::tui::{
    app::{AppState, SidePanelTab},
    theme::Theme,
};
use ratatui::{
    prelude::*,
    widgets::{Block, Borders, Paragraph, Tabs, Wrap},
};

pub fn render(f: &mut Frame, area: Rect, state: &mut AppState, theme: &Theme) {
    let border_color = if state.side_panel_focused {
        theme.accent
    } else {
        theme.dim
    };
    let block = Block::default()
        .borders(Borders::LEFT)
        .border_style(Style::default().fg(border_color))
        .style(Style::default().bg(theme.bg));

    let inner = block.inner(area);
    f.render_widget(block, area);

    if inner.height < 3 || inner.width < 10 {
        return;
    }

    // Tab bar
    let tabs = Tabs::new(vec!["Git Diff", "Files"])
        .select(match state.side_panel_tab {
            SidePanelTab::GitDiff => 0,
            SidePanelTab::FileTree => 1,
        })
        .style(Style::default().fg(theme.dim))
        .highlight_style(
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        )
        .divider(" | ");

    let tab_area = Rect { height: 1, ..inner };
    f.render_widget(tabs, tab_area);

    let content_area = Rect {
        y: inner.y + 1,
        height: inner.height.saturating_sub(1),
        ..inner
    };

    // Content based on tab
    let content = match state.side_panel_tab {
        SidePanelTab::GitDiff => render_diff(&state.side_panel_diff, content_area.width),
        SidePanelTab::FileTree => render_tree(&state.side_panel_tree, theme),
    };

    // Update side panel scroll
    let total_lines = content.len() as u16;
    state
        .side_panel_scroll
        .update_dimensions(total_lines, content_area.height);
    let scroll = state.side_panel_scroll.effective_offset();

    let paragraph = Paragraph::new(content)
        .scroll((scroll, 0))
        .wrap(Wrap { trim: false });

    f.render_widget(paragraph, content_area);
}

fn render_diff(diff_text: &str, _width: u16) -> Vec<Line<'static>> {
    if diff_text.is_empty() {
        return vec![Line::from(Span::styled(
            "  No changes",
            Style::default().fg(Color::DarkGray),
        ))];
    }

    diff_text
        .lines()
        .map(|line| {
            let style = if line.starts_with("=== ") {
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD)
            } else if line.starts_with('+') && !line.starts_with("+++") {
                Style::default().fg(Color::Green)
            } else if line.starts_with('-') && !line.starts_with("---") {
                Style::default().fg(Color::Red)
            } else if line.starts_with("@@") {
                Style::default().fg(Color::Cyan)
            } else if line.starts_with("diff ") || line.starts_with("index ") {
                Style::default().fg(Color::Yellow)
            } else if line.contains("untracked") {
                Style::default().fg(Color::Magenta)
            } else if line.contains("modified") {
                Style::default().fg(Color::Yellow)
            } else if line.contains("added") {
                Style::default().fg(Color::Green)
            } else if line.contains("deleted") {
                Style::default().fg(Color::Red)
            } else if line.contains("renamed") {
                Style::default().fg(Color::Cyan)
            } else {
                Style::default().fg(Color::DarkGray)
            };
            Line::from(Span::styled(format!(" {line}"), style))
        })
        .collect()
}

fn render_tree(tree_text: &str, theme: &Theme) -> Vec<Line<'static>> {
    if tree_text.is_empty() {
        return vec![Line::from(Span::styled(
            "  No files",
            Style::default().fg(Color::DarkGray),
        ))];
    }

    // Compact view: group by top-level directory, show counts
    let mut dirs: std::collections::BTreeMap<String, Vec<String>> =
        std::collections::BTreeMap::new();
    let mut root_files: Vec<String> = Vec::new();

    for line in tree_text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Some(slash_pos) = line.find('/') {
            let dir = &line[..slash_pos];
            let rest = &line[slash_pos + 1..];
            dirs.entry(dir.to_string())
                .or_default()
                .push(rest.to_string());
        } else {
            root_files.push(line.to_string());
        }
    }

    let mut lines = Vec::new();
    let total: usize = dirs.values().map(|v| v.len()).sum::<usize>() + root_files.len();
    lines.push(Line::from(Span::styled(
        format!(" {} files", total),
        Style::default().fg(theme.text_tertiary),
    )));
    lines.push(Line::default());

    // Directories first (compact: just name + count)
    for (dir, files) in &dirs {
        let file_count = count_recursive(files);
        lines.push(Line::from(vec![
            Span::styled(" ▸ ", Style::default().fg(theme.text_ghost)),
            Span::styled(
                format!("{dir}/"),
                Style::default()
                    .fg(theme.accent)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!("  ({file_count})"),
                Style::default().fg(theme.text_ghost),
            ),
        ]));
    }

    // Root files
    if !root_files.is_empty() && !dirs.is_empty() {
        lines.push(Line::default());
    }
    for file in &root_files {
        lines.push(Line::from(vec![
            Span::styled("   ", Style::default()),
            Span::styled(file.clone(), Style::default().fg(theme.text_secondary)),
        ]));
    }

    lines
}

/// Count files recursively in a flat path list.
fn count_recursive(files: &[String]) -> usize {
    files.len()
}

/// Simple tree node for building a file tree.
struct TreeNode {
    name: String,
    children: std::collections::BTreeMap<String, TreeNode>,
    is_file: bool,
}

impl TreeNode {
    fn new(name: &str) -> Self {
        Self {
            name: name.to_string(),
            children: std::collections::BTreeMap::new(),
            is_file: false,
        }
    }

    fn insert(&mut self, path: &str) {
        let parts: Vec<&str> = path.split('/').collect();
        self.insert_parts(&parts);
    }

    fn insert_parts(&mut self, parts: &[&str]) {
        if parts.is_empty() {
            return;
        }
        if parts.len() == 1 {
            let entry = self
                .children
                .entry(parts[0].to_string())
                .or_insert_with(|| TreeNode::new(parts[0]));
            entry.is_file = true;
        } else {
            let entry = self
                .children
                .entry(parts[0].to_string())
                .or_insert_with(|| TreeNode::new(parts[0]));
            entry.insert_parts(&parts[1..]);
        }
    }

    fn render(&self, lines: &mut Vec<Line<'static>>, prefix: &str, is_root: bool, theme: &Theme) {
        if is_root {
            // Render children of root directly
            let entries: Vec<(&String, &TreeNode)> = self.children.iter().collect();
            for (i, (name, node)) in entries.iter().enumerate() {
                let is_last = i == entries.len() - 1;
                let connector = if is_last { "└── " } else { "├── " };
                let child_prefix = if is_last { "    " } else { "│   " };

                if node.is_file && node.children.is_empty() {
                    lines.push(Line::from(vec![
                        Span::styled(
                            format!(" {prefix}{connector}"),
                            Style::default().fg(Color::DarkGray),
                        ),
                        Span::styled(name.to_string(), Style::default().fg(theme.fg)),
                    ]));
                } else {
                    lines.push(Line::from(vec![
                        Span::styled(
                            format!(" {prefix}{connector}"),
                            Style::default().fg(Color::DarkGray),
                        ),
                        Span::styled(
                            format!("{name}/"),
                            Style::default()
                                .fg(theme.accent)
                                .add_modifier(Modifier::BOLD),
                        ),
                    ]));
                    let new_prefix = format!("{prefix}{child_prefix}");
                    node.render(lines, &new_prefix, false, theme);
                }
            }
        } else {
            let entries: Vec<(&String, &TreeNode)> = self.children.iter().collect();
            for (i, (name, node)) in entries.iter().enumerate() {
                let is_last = i == entries.len() - 1;
                let connector = if is_last { "└── " } else { "├── " };
                let child_prefix = if is_last { "    " } else { "│   " };

                if node.is_file && node.children.is_empty() {
                    lines.push(Line::from(vec![
                        Span::styled(
                            format!(" {prefix}{connector}"),
                            Style::default().fg(Color::DarkGray),
                        ),
                        Span::styled(name.to_string(), Style::default().fg(theme.fg)),
                    ]));
                } else {
                    lines.push(Line::from(vec![
                        Span::styled(
                            format!(" {prefix}{connector}"),
                            Style::default().fg(Color::DarkGray),
                        ),
                        Span::styled(
                            format!("{name}/"),
                            Style::default()
                                .fg(theme.accent)
                                .add_modifier(Modifier::BOLD),
                        ),
                    ]));
                    let new_prefix = format!("{prefix}{child_prefix}");
                    node.render(lines, &new_prefix, false, theme);
                }
            }
        }
    }
}

/// Refresh side panel content (git status + diff + file tree). Call from event loop.
pub fn refresh_content(state: &mut AppState, working_dir: &std::path::Path) {
    use std::process::Command;

    // Git status (shows all changes including untracked) — human-readable format
    let raw_status = Command::new("git")
        .args(["status", "--short"])
        .current_dir(working_dir)
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .unwrap_or_default();

    // Convert short codes to readable labels
    let status: String = raw_status
        .lines()
        .map(|line| {
            if line.len() < 4 {
                return line.to_string();
            }
            let code = &line[..2];
            let file = line[3..].trim();
            match code.trim() {
                "??" => format!("  untracked  {file}"),
                "M" | " M" => format!("  modified   {file}"),
                "MM" => format!("  modified*  {file}"),
                "A" | " A" => format!("  added      {file}"),
                "D" | " D" => format!("  deleted    {file}"),
                "R" => format!("  renamed    {file}"),
                "C" => format!("  copied     {file}"),
                _ => format!("  {code} {file}"),
            }
        })
        .collect::<Vec<_>>()
        .join("\n");

    // Git diff (tracked modifications)
    let diff = Command::new("git")
        .args(["diff", "--patch"])
        .current_dir(working_dir)
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .unwrap_or_default();

    // Git staged diff
    let cached = Command::new("git")
        .args(["diff", "--cached", "--patch"])
        .current_dir(working_dir)
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .unwrap_or_default();

    let mut parts = Vec::new();
    if !status.is_empty() {
        parts.push(format!("=== Status ===\n{status}"));
    }
    if !cached.is_empty() {
        parts.push(format!("=== Staged ===\n{cached}"));
    }
    if !diff.is_empty() {
        parts.push(format!("=== Unstaged ===\n{diff}"));
    }

    state.side_panel_diff = if parts.is_empty() {
        String::new()
    } else {
        parts.join("\n")
    };

    // File tree
    state.side_panel_tree = Command::new("git")
        .args(["ls-files"])
        .current_dir(working_dir)
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .unwrap_or_default();
}
