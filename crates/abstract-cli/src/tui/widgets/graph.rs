//! Graph visualization: memory nodes + relationships rendered as a node graph.
//!
//! Since tui-nodes requires ratatui 0.30 and we're on 0.29, this uses
//! a custom renderer with Block widgets and canvas lines.

use crate::tui::theme::Theme;
use ratatui::{prelude::*, widgets::{Block, Borders, Clear, Paragraph, Wrap}};

/// A node in the graph visualization.
#[derive(Debug, Clone)]
pub struct GraphNode {
    pub id: String,
    pub label: String,
    pub kind: NodeKind,
    pub x: u16,
    pub y: u16,
}

/// Edge between two nodes.
#[derive(Debug, Clone)]
pub struct GraphEdge {
    pub from: usize,
    pub to: usize,
    pub label: String,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum NodeKind {
    Memory,
    Topic,
    Session,
    LspServer,
    Tool,
}

impl NodeKind {
    pub fn color(&self) -> Color {
        match self {
            Self::Memory => Color::Cyan,
            Self::Topic => Color::Yellow,
            Self::Session => Color::Green,
            Self::LspServer => Color::Magenta,
            Self::Tool => Color::Blue,
        }
    }

    pub fn icon(&self) -> &'static str {
        match self {
            Self::Memory => "M",
            Self::Topic => "T",
            Self::Session => "S",
            Self::LspServer => "L",
            Self::Tool => "⚙",
        }
    }
}

/// Graph overlay state.
#[derive(Debug, Clone)]
pub struct GraphOverlayState {
    pub nodes: Vec<GraphNode>,
    pub edges: Vec<GraphEdge>,
    pub selected: usize,
    pub pan_x: i16,
    pub pan_y: i16,
}

impl GraphOverlayState {
    pub fn new() -> Self {
        Self {
            nodes: Vec::new(),
            edges: Vec::new(),
            selected: 0,
            pan_x: 0,
            pan_y: 0,
        }
    }

    /// Build from graph stats and memory data.
    pub fn from_memory_stats(stats: &crate::tui::widgets::graph::MemoryGraphData) -> Self {
        let mut nodes = Vec::new();
        let mut edges = Vec::new();

        // Layout nodes in a grid pattern
        let cols = 6u16;
        let node_w = 14u16;
        let node_h = 3u16;

        // Memory nodes
        for (i, mem) in stats.memories.iter().enumerate() {
            let col = (i as u16) % cols;
            let row = (i as u16) / cols;
            nodes.push(GraphNode {
                id: mem.id.clone(),
                label: truncate_label(&mem.label, 12),
                kind: NodeKind::Memory,
                x: col * (node_w + 2) + 2,
                y: row * (node_h + 1) + 2,
            });
        }

        let mem_count = nodes.len();

        // Topic nodes (below memories)
        let topic_y_offset = ((mem_count as u16 / cols) + 1) * (node_h + 1) + 3;
        for (i, topic) in stats.topics.iter().enumerate() {
            let col = (i as u16) % cols;
            nodes.push(GraphNode {
                id: topic.clone(),
                label: truncate_label(topic, 12),
                kind: NodeKind::Topic,
                x: col * (node_w + 2) + 2,
                y: topic_y_offset,
            });
        }

        // LSP server nodes (right side)
        let lsp_x = (cols) * (node_w + 2) + 4;
        for (i, server) in stats.lsp_servers.iter().enumerate() {
            nodes.push(GraphNode {
                id: format!("lsp-{server}"),
                label: truncate_label(server, 12),
                kind: NodeKind::LspServer,
                x: lsp_x,
                y: (i as u16) * (node_h + 1) + 2,
            });
        }

        // Edges: memory → topic (simple relationships)
        for (mem_idx, mem) in stats.memories.iter().enumerate() {
            for topic in &mem.topics {
                if let Some(topic_idx) = stats.topics.iter().position(|t| t == topic) {
                    edges.push(GraphEdge {
                        from: mem_idx,
                        to: mem_count + topic_idx,
                        label: "tagged".to_string(),
                    });
                }
            }
        }

        Self {
            nodes,
            edges,
            selected: 0,
            pan_x: 0,
            pan_y: 0,
        }
    }

    pub fn select_next(&mut self) {
        if !self.nodes.is_empty() {
            self.selected = (self.selected + 1) % self.nodes.len();
        }
    }

    pub fn select_prev(&mut self) {
        if !self.nodes.is_empty() {
            self.selected = self.selected.checked_sub(1).unwrap_or(self.nodes.len() - 1);
        }
    }
}

/// Data needed to build the graph.
#[derive(Debug, Clone, Default)]
pub struct MemoryGraphData {
    pub memories: Vec<MemoryNode>,
    pub topics: Vec<String>,
    pub lsp_servers: Vec<String>,
    pub total_sessions: usize,
}

#[derive(Debug, Clone)]
pub struct MemoryNode {
    pub id: String,
    pub label: String,
    pub topics: Vec<String>,
}

/// Render the graph overlay.
pub fn render(f: &mut Frame, state: &GraphOverlayState, theme: &Theme) {
    let area = f.area();

    // Centered overlay (90% of screen)
    let overlay_area = centered_rect(90, 90, area);

    // Clear background
    f.render_widget(Clear, overlay_area);

    let block = Block::default()
        .title(" Memory Graph (↑↓ select | ←→ pan | Esc close) ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.accent))
        .style(Style::default().bg(theme.bg));

    let inner = block.inner(overlay_area);
    f.render_widget(block, overlay_area);

    if state.nodes.is_empty() {
        let msg = Paragraph::new("  No graph data. Use graph memory to populate nodes.")
            .style(Style::default().fg(theme.dim));
        f.render_widget(msg, inner);
        return;
    }

    // Render nodes
    for (i, node) in state.nodes.iter().enumerate() {
        let nx = (node.x as i16 + state.pan_x) as u16;
        let ny = (node.y as i16 + state.pan_y) as u16;

        // Skip if out of bounds
        if nx >= inner.width || ny >= inner.height {
            continue;
        }

        let abs_x = inner.x + nx;
        let abs_y = inner.y + ny;
        let node_width = 14u16.min(inner.right().saturating_sub(abs_x));
        let node_height = 3u16.min(inner.bottom().saturating_sub(abs_y));

        if node_width < 4 || node_height < 1 {
            continue;
        }

        let node_area = Rect::new(abs_x, abs_y, node_width, node_height);

        let is_selected = i == state.selected;
        let border_style = if is_selected {
            Style::default().fg(Color::White).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(node.kind.color())
        };

        let node_block = Block::default()
            .borders(Borders::ALL)
            .border_style(border_style);

        let label = format!("{} {}", node.kind.icon(), node.label);
        let content = Paragraph::new(label)
            .style(Style::default().fg(node.kind.color()))
            .wrap(Wrap { trim: true });

        f.render_widget(node_block, node_area);
        let content_area = Rect::new(
            node_area.x + 1,
            node_area.y + 1,
            node_area.width.saturating_sub(2),
            node_area.height.saturating_sub(2),
        );
        if content_area.width > 0 && content_area.height > 0 {
            f.render_widget(content, content_area);
        }
    }

    // Render edge indicators (simple: show connection lines as dashes between nodes)
    for edge in &state.edges {
        if edge.from >= state.nodes.len() || edge.to >= state.nodes.len() {
            continue;
        }
        let from = &state.nodes[edge.from];
        let to = &state.nodes[edge.to];

        let fx = (from.x as i16 + state.pan_x + 7) as u16; // center of from node
        let fy = (from.y as i16 + state.pan_y + 2) as u16; // bottom of from node
        let tx = (to.x as i16 + state.pan_x + 7) as u16;
        let _ty = (to.y as i16 + state.pan_y) as u16;

        // Draw a simple vertical connector dot if nodes are vertically aligned
        if fx < inner.width && fy < inner.height && fy > 0 {
            let abs_x = inner.x + fx;
            let abs_y = inner.y + fy;
            if abs_x < inner.right() && abs_y < inner.bottom() {
                let connector = Paragraph::new("│")
                    .style(Style::default().fg(Color::DarkGray));
                f.render_widget(connector, Rect::new(abs_x, abs_y, 1, 1));
            }
        }
    }

    // Selected node details at the bottom
    if state.selected < state.nodes.len() {
        let selected_node = &state.nodes[state.selected];
        let detail = format!(
            " Selected: {} ({:?}) — {}",
            selected_node.label, selected_node.kind, selected_node.id
        );
        let detail_area = Rect::new(
            inner.x,
            inner.bottom().saturating_sub(1),
            inner.width,
            1,
        );
        let detail_widget = Paragraph::new(detail)
            .style(Style::default().fg(theme.fg).bg(theme.bg));
        f.render_widget(detail_widget, detail_area);
    }
}

fn centered_rect(percent_x: u16, percent_y: u16, area: Rect) -> Rect {
    let popup_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(area);

    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(popup_layout[1])[1]
}

fn truncate_label(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}…", &s[..max - 1])
    }
}
