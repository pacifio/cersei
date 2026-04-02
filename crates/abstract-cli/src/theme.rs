//! Color theme system for terminal output.

use crossterm::style::Color;

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct Theme {
    pub text: Color,
    pub dim: Color,
    pub accent: Color,
    pub success: Color,
    pub error: Color,
    pub warning: Color,
    pub tool_badge: Color,
    pub thinking: Color,
    pub code_bg: Color,
    pub prompt: Color,
    pub status_bg: Color,
    pub status_fg: Color,
    pub permission_accent: Color,
}

impl Theme {
    pub fn dark() -> Self {
        Self {
            text: Color::White,
            dim: Color::DarkGrey,
            accent: Color::Cyan,
            success: Color::Green,
            error: Color::Red,
            warning: Color::Yellow,
            tool_badge: Color::Magenta,
            thinking: Color::DarkGrey,
            code_bg: Color::Rgb { r: 30, g: 30, b: 30 },
            prompt: Color::Cyan,
            status_bg: Color::Rgb { r: 30, g: 30, b: 30 },
            status_fg: Color::DarkGrey,
            permission_accent: Color::Yellow,
        }
    }

    pub fn light() -> Self {
        Self {
            text: Color::Black,
            dim: Color::DarkGrey,
            accent: Color::DarkCyan,
            success: Color::DarkGreen,
            error: Color::DarkRed,
            warning: Color::DarkYellow,
            tool_badge: Color::DarkMagenta,
            thinking: Color::Grey,
            code_bg: Color::Rgb { r: 240, g: 240, b: 240 },
            prompt: Color::DarkCyan,
            status_bg: Color::Rgb { r: 240, g: 240, b: 240 },
            status_fg: Color::DarkGrey,
            permission_accent: Color::DarkYellow,
        }
    }

    pub fn from_name(name: &str) -> Self {
        match name {
            "light" => Self::light(),
            _ => Self::dark(),
        }
    }
}
