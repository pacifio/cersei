//! Scroll state management with bounds checking.

/// Scroll state for a scrollable area (messages, side panel, etc.).
#[derive(Debug, Clone, Default)]
pub struct ScrollState {
    pub offset: u16,
    pub content_height: u16,
    pub viewport_height: u16,
    pub sticky_bottom: bool,
}

impl ScrollState {
    pub fn new() -> Self {
        Self {
            offset: 0,
            content_height: 0,
            viewport_height: 0,
            sticky_bottom: true,
        }
    }

    /// Maximum valid offset.
    fn max_offset(&self) -> u16 {
        self.content_height.saturating_sub(self.viewport_height)
    }

    /// Clamp offset to valid range.
    pub fn clamp(&mut self) {
        self.offset = self.offset.min(self.max_offset());
    }

    /// Effective offset: if sticky_bottom, scroll to end.
    pub fn effective_offset(&self) -> u16 {
        if self.sticky_bottom {
            self.max_offset()
        } else {
            self.offset.min(self.max_offset())
        }
    }

    /// Scroll up by n lines.
    pub fn scroll_up(&mut self, n: u16) {
        self.sticky_bottom = false;
        self.offset = self.offset.saturating_sub(n);
    }

    /// Scroll down by n lines.
    pub fn scroll_down(&mut self, n: u16) {
        self.sticky_bottom = false;
        self.offset = (self.offset + n).min(self.max_offset());
        // Re-enable sticky bottom if we've scrolled to the end
        if self.offset >= self.max_offset() {
            self.sticky_bottom = true;
        }
    }

    /// Jump to bottom and enable sticky.
    pub fn scroll_to_bottom(&mut self) {
        self.sticky_bottom = true;
        self.offset = self.max_offset();
    }

    /// Page up (one viewport).
    pub fn page_up(&mut self) {
        self.scroll_up(self.viewport_height.saturating_sub(2));
    }

    /// Page down (one viewport).
    pub fn page_down(&mut self) {
        self.scroll_down(self.viewport_height.saturating_sub(2));
    }

    /// Update content and viewport dimensions.
    pub fn update_dimensions(&mut self, content_height: u16, viewport_height: u16) {
        self.content_height = content_height;
        self.viewport_height = viewport_height;
        self.clamp();
    }
}
