//! Virtualized list: only renders visible items with height caching.
//!
//! O(visible_height) per frame instead of O(total_lines).

use ratatui::prelude::*;
use ratatui::widgets::Paragraph;
use std::collections::HashMap;

/// A single pre-built line in the virtual list.
#[derive(Clone)]
pub struct VItem {
    pub line: Line<'static>,
}

impl VItem {
    pub fn new(line: Line<'static>) -> Self {
        Self { line }
    }

    /// Height is always 1 row (lines are pre-wrapped).
    pub fn height(&self) -> u16 {
        1
    }
}

/// Virtualized list that only renders visible items.
pub struct VirtualList {
    /// Pre-built committed items (not rebuilt per frame).
    committed_items: Vec<VItem>,
    /// Streaming items (rebuilt every frame — only a few lines).
    streaming_items: Vec<VItem>,
    /// Scroll offset in rows from the top.
    pub scroll_offset: u16,
    /// Viewport height in rows.
    pub viewport_height: u16,
    /// Auto-scroll to bottom on new content.
    pub sticky_bottom: bool,
    /// Width used when items were last built (for invalidation on resize).
    last_width: u16,
}

impl VirtualList {
    pub fn new() -> Self {
        Self {
            committed_items: Vec::new(),
            streaming_items: Vec::new(),
            scroll_offset: 0,
            viewport_height: 0,
            sticky_bottom: true,
            last_width: 0,
        }
    }

    /// Total number of rows.
    pub fn total_height(&self) -> u16 {
        (self.committed_items.len() + self.streaming_items.len()) as u16
    }

    /// Maximum valid scroll offset.
    fn max_offset(&self) -> u16 {
        self.total_height().saturating_sub(self.viewport_height)
    }

    /// Effective scroll offset (respects sticky_bottom).
    pub fn effective_offset(&self) -> u16 {
        if self.sticky_bottom {
            self.max_offset()
        } else {
            self.scroll_offset.min(self.max_offset())
        }
    }

    /// Set committed items (pre-built, cached).
    pub fn set_committed(&mut self, items: Vec<VItem>) {
        self.committed_items = items;
    }

    /// Set streaming items (rebuilt every frame).
    pub fn set_streaming(&mut self, items: Vec<VItem>) {
        self.streaming_items = items;
    }

    /// Set viewport height.
    pub fn set_viewport(&mut self, height: u16) {
        self.viewport_height = height;
    }

    /// Check if width changed (needs rebuild).
    pub fn width_changed(&self, new_width: u16) -> bool {
        self.last_width != new_width
    }

    /// Record the width used for building.
    pub fn set_width(&mut self, width: u16) {
        self.last_width = width;
    }

    /// Scroll up by n rows.
    pub fn scroll_up(&mut self, n: u16) {
        self.sticky_bottom = false;
        self.scroll_offset = self.scroll_offset.saturating_sub(n);
    }

    /// Scroll down by n rows.
    pub fn scroll_down(&mut self, n: u16) {
        self.sticky_bottom = false;
        self.scroll_offset = (self.scroll_offset + n).min(self.max_offset());
        if self.scroll_offset >= self.max_offset() {
            self.sticky_bottom = true;
        }
    }

    /// Page up.
    pub fn page_up(&mut self) {
        self.scroll_up(self.viewport_height.saturating_sub(2));
    }

    /// Page down.
    pub fn page_down(&mut self) {
        self.scroll_down(self.viewport_height.saturating_sub(2));
    }

    /// Scroll to bottom.
    pub fn scroll_to_bottom(&mut self) {
        self.sticky_bottom = true;
        self.scroll_offset = self.max_offset();
    }

    /// Render only visible items directly to the buffer.
    pub fn render(&self, area: Rect, buf: &mut Buffer) {
        let offset = self.effective_offset();
        let total = self.total_height();
        let committed_len = self.committed_items.len() as u16;

        if area.height == 0 {
            return;
        }

        // Clear the entire message area first to prevent stale content bleed-through.
        // This is critical: without it, lines from previous scroll positions persist
        // in the buffer and corrupt the display.
        for y in area.y..area.y + area.height {
            for x in area.x..area.x + area.width {
                if let Some(cell) = buf.cell_mut((x, y)) {
                    cell.reset();
                }
            }
        }

        if total == 0 {
            return;
        }

        let mut screen_y = area.y;
        let end_y = area.y + area.height;

        // Walk through all items, only render visible ones
        for idx in 0..total {
            if screen_y >= end_y {
                break; // Past viewport
            }

            if idx < offset {
                continue; // Above viewport
            }

            // Get the item
            let item = if idx < committed_len {
                &self.committed_items[idx as usize]
            } else {
                let stream_idx = (idx - committed_len) as usize;
                if stream_idx < self.streaming_items.len() {
                    &self.streaming_items[stream_idx]
                } else {
                    continue;
                }
            };

            // Render this single line at screen_y
            let line_area = Rect {
                x: area.x,
                y: screen_y,
                width: area.width,
                height: 1,
            };

            // Render line directly to buffer
            let para = Paragraph::new(vec![item.line.clone()]);
            para.render(line_area, buf);

            screen_y += 1;
        }
    }
}

impl Default for VirtualList {
    fn default() -> Self {
        Self::new()
    }
}
