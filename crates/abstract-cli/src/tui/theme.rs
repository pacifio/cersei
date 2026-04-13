//! Color themes for the TUI.
//!
//! Default theme: Enterprise — AMOLED black, monochromatic, compact.

use ratatui::style::{Color, Modifier, Style};

#[derive(Debug, Clone)]
pub struct Theme {
    pub bg: Color,
    pub fg: Color,
    pub dim: Color,
    pub accent: Color,
    pub success: Color,
    pub error: Color,
    pub warning: Color,
    pub info: Color,
    pub border: Color,
    pub input_bg: Color,
    pub header_bg: Color,
    pub tool_badge: Color,
    pub thinking: Color,
    pub user_msg: Color,
    // Enterprise extras
    pub text_primary: Color,
    pub text_secondary: Color,
    pub text_tertiary: Color,
    pub text_ghost: Color,
    pub text_muted: Color,
    pub text_accent: Color,
    pub bg_raised: Color,
    pub bg_hover: Color,
    pub bg_selected: Color,
    pub border_subtle: Color,
    pub border_strong: Color,
    pub diff_added: Color,
    pub diff_removed: Color,
    pub diff_modified: Color,
}

impl Theme {
    /// Enterprise — AMOLED black, monochromatic. Default theme.
    pub fn enterprise() -> Self {
        Self {
            // Core
            bg: Color::Rgb(0, 0, 0),             // #000000
            fg: Color::Rgb(179, 179, 179),        // #b3b3b3 (editor.foreground)
            dim: Color::Rgb(88, 88, 88),          // #585858 (text.muted)
            accent: Color::Rgb(255, 255, 0),      // #ffff00 (text.accent / function)
            success: Color::Rgb(77, 77, 77),      // #4d4d4d
            error: Color::Rgb(244, 71, 71),       // #F44747
            warning: Color::Rgb(205, 151, 49),    // #CD9731
            info: Color::Rgb(103, 150, 230),      // #6796E6
            border: Color::Rgb(30, 30, 30),       // #1e1e1e
            input_bg: Color::Rgb(10, 10, 10),     // #0a0a0a
            header_bg: Color::Rgb(0, 0, 0),       // #000000
            tool_badge: Color::Rgb(153, 153, 153), // #999999 (terminal.ansi.magenta substitute)
            thinking: Color::Rgb(51, 51, 51),     // #333333 (text.ghost)
            user_msg: Color::Rgb(170, 170, 170),  // #aaaaaa (text.secondary)

            // Enterprise extended
            text_primary: Color::Rgb(255, 255, 255),   // #ffffff
            text_secondary: Color::Rgb(170, 170, 170), // #aaaaaa
            text_tertiary: Color::Rgb(119, 119, 119),  // #777777
            text_ghost: Color::Rgb(51, 51, 51),        // #333333
            text_muted: Color::Rgb(88, 88, 88),        // #585858
            text_accent: Color::Rgb(255, 255, 0),      // #ffff00
            bg_raised: Color::Rgb(15, 15, 15),         // #0f0f0f
            bg_hover: Color::Rgb(10, 10, 10),          // ~rgba(255,255,255,0.04)
            bg_selected: Color::Rgb(15, 15, 15),       // ~rgba(255,255,255,0.06)
            border_subtle: Color::Rgb(20, 20, 20),     // #141414
            border_strong: Color::Rgb(61, 61, 61),     // #3d3d3d
            diff_added: Color::Rgb(77, 77, 77),        // #4d4d4d
            diff_removed: Color::Rgb(119, 119, 119),   // #777777
            diff_modified: Color::Rgb(192, 192, 192),  // #c0c0c0
        }
    }

    pub fn dark() -> Self {
        Self::enterprise() // Enterprise is the default dark theme
    }

    pub fn light() -> Self {
        Self {
            bg: Color::Rgb(250, 250, 250),
            fg: Color::Rgb(30, 30, 30),
            dim: Color::Rgb(130, 130, 130),
            accent: Color::Rgb(0, 120, 200),
            success: Color::Rgb(0, 150, 50),
            error: Color::Rgb(200, 30, 30),
            warning: Color::Rgb(180, 120, 0),
            info: Color::Rgb(0, 100, 180),
            border: Color::Rgb(200, 200, 200),
            input_bg: Color::Rgb(240, 240, 240),
            header_bg: Color::Rgb(235, 235, 235),
            tool_badge: Color::Rgb(130, 50, 160),
            thinking: Color::Rgb(160, 160, 160),
            user_msg: Color::Rgb(80, 80, 80),
            text_primary: Color::Rgb(0, 0, 0),
            text_secondary: Color::Rgb(60, 60, 60),
            text_tertiary: Color::Rgb(120, 120, 120),
            text_ghost: Color::Rgb(200, 200, 200),
            text_muted: Color::Rgb(150, 150, 150),
            text_accent: Color::Rgb(0, 100, 200),
            bg_raised: Color::Rgb(240, 240, 240),
            bg_hover: Color::Rgb(235, 235, 235),
            bg_selected: Color::Rgb(225, 225, 225),
            border_subtle: Color::Rgb(220, 220, 220),
            border_strong: Color::Rgb(150, 150, 150),
            diff_added: Color::Rgb(0, 150, 50),
            diff_removed: Color::Rgb(200, 30, 30),
            diff_modified: Color::Rgb(0, 100, 200),
        }
    }

    pub fn solarized() -> Self {
        Self {
            bg: Color::Rgb(0, 43, 54),
            fg: Color::Rgb(131, 148, 150),
            dim: Color::Rgb(88, 110, 117),
            accent: Color::Rgb(38, 139, 210),
            success: Color::Rgb(133, 153, 0),
            error: Color::Rgb(220, 50, 47),
            warning: Color::Rgb(181, 137, 0),
            info: Color::Rgb(42, 161, 152),
            border: Color::Rgb(7, 54, 66),
            input_bg: Color::Rgb(7, 54, 66),
            header_bg: Color::Rgb(0, 43, 54),
            tool_badge: Color::Rgb(108, 113, 196),
            thinking: Color::Rgb(88, 110, 117),
            user_msg: Color::Rgb(147, 161, 161),
            text_primary: Color::Rgb(253, 246, 227),
            text_secondary: Color::Rgb(147, 161, 161),
            text_tertiary: Color::Rgb(88, 110, 117),
            text_ghost: Color::Rgb(7, 54, 66),
            text_muted: Color::Rgb(88, 110, 117),
            text_accent: Color::Rgb(38, 139, 210),
            bg_raised: Color::Rgb(7, 54, 66),
            bg_hover: Color::Rgb(7, 54, 66),
            bg_selected: Color::Rgb(7, 54, 66),
            border_subtle: Color::Rgb(7, 54, 66),
            border_strong: Color::Rgb(88, 110, 117),
            diff_added: Color::Rgb(133, 153, 0),
            diff_removed: Color::Rgb(220, 50, 47),
            diff_modified: Color::Rgb(38, 139, 210),
        }
    }

    pub fn from_name(name: &str) -> Self {
        match name {
            "enterprise" => Self::enterprise(),
            "light" => Self::light(),
            "solarized" => Self::solarized(),
            _ => Self::enterprise(), // Enterprise is default
        }
    }

    // ── Style helpers ──

    pub fn text(&self) -> Style {
        Style::default().fg(self.fg)
    }

    pub fn dimmed(&self) -> Style {
        Style::default().fg(self.dim)
    }

    pub fn bold(&self) -> Style {
        Style::default().fg(self.text_primary).add_modifier(Modifier::BOLD)
    }

    pub fn accent_style(&self) -> Style {
        Style::default().fg(self.accent)
    }

    pub fn success_style(&self) -> Style {
        Style::default().fg(self.success)
    }

    pub fn error_style(&self) -> Style {
        Style::default().fg(self.error)
    }

    pub fn warning_style(&self) -> Style {
        Style::default().fg(self.warning)
    }

    pub fn border_style(&self) -> Style {
        Style::default().fg(self.border)
    }

    pub fn header_style(&self) -> Style {
        Style::default().fg(self.text_secondary).bg(self.header_bg)
    }

    pub fn raised_style(&self) -> Style {
        Style::default().fg(self.fg).bg(self.bg_raised)
    }

    pub fn selected_style(&self) -> Style {
        Style::default().fg(self.text_primary).bg(self.bg_selected)
    }
}
