//! Streaming terminal renderer: markdown, tool badges, thinking, errors.

use crate::theme::Theme;
use crossterm::style::{Attribute, Color, Print, ResetColor, SetAttribute, SetForegroundColor};
use crossterm::execute;
use std::io::{self, Write};
use std::time::Duration;

pub struct StreamRenderer {
    theme: Theme,
    buffer: String,
    in_thinking: bool,
    json_mode: bool,
}

impl StreamRenderer {
    pub fn new(theme: &Theme, json_mode: bool) -> Self {
        Self {
            theme: theme.clone(),
            buffer: String::new(),
            in_thinking: false,
            json_mode,
        }
    }

    /// Push a text delta from the model. Flushes on newlines.
    pub fn push_text(&mut self, delta: &str) {
        if self.json_mode {
            // In JSON mode, print raw events (handled by caller)
            return;
        }

        if self.in_thinking {
            self.end_thinking();
        }

        self.buffer.push_str(delta);

        // Flush on newline boundaries to avoid partial-line flicker
        if let Some(last_nl) = self.buffer.rfind('\n') {
            let to_flush = self.buffer[..=last_nl].to_string();
            self.buffer = self.buffer[last_nl + 1..].to_string();
            self.print_markdown(&to_flush);
        }
    }

    /// Push a thinking delta (dim/italic).
    pub fn push_thinking(&mut self, delta: &str) {
        if self.json_mode {
            return;
        }
        if !self.in_thinking {
            self.in_thinking = true;
            let _ = execute!(
                io::stderr(),
                SetForegroundColor(self.theme.thinking),
                SetAttribute(Attribute::Italic),
                Print("  thinking... "),
            );
        }
        // Don't print thinking content by default — just show spinner
        let _ = delta; // consumed but not displayed
    }

    /// Show a tool start badge.
    pub fn tool_start(&mut self, name: &str, input: &serde_json::Value) {
        if self.json_mode {
            return;
        }
        self.flush();

        let summary = tool_input_summary(name, input);
        let _ = execute!(
            io::stderr(),
            Print("\n"),
            SetForegroundColor(self.theme.tool_badge),
            SetAttribute(Attribute::Bold),
            Print(format!("  [{name}]")),
            ResetColor,
            SetForegroundColor(self.theme.dim),
            Print(format!(" {summary}")),
            ResetColor,
            Print("\n"),
        );
    }

    /// Show a tool completion.
    pub fn tool_end(&mut self, name: &str, result: &str, is_error: bool, duration: Duration) {
        if self.json_mode {
            return;
        }

        let color = if is_error {
            self.theme.error
        } else {
            self.theme.success
        };
        let icon = if is_error { "x" } else { "+" };
        let ms = duration.as_millis();

        let _ = execute!(
            io::stderr(),
            SetForegroundColor(color),
            Print(format!("  {icon} {name}")),
            ResetColor,
            SetForegroundColor(self.theme.dim),
            Print(format!(" ({ms}ms)")),
            ResetColor,
        );

        // Show truncated result for errors
        if is_error {
            let preview: String = result.chars().take(200).collect();
            let _ = execute!(
                io::stderr(),
                Print("\n"),
                SetForegroundColor(self.theme.error),
                Print(format!("    {preview}")),
                ResetColor,
            );
        }

        let _ = execute!(io::stderr(), Print("\n"));
    }

    /// Show a permission prompt and return the rendered description.
    #[allow(dead_code)]
    pub fn permission_header(&self, tool_name: &str, description: &str, level: &str) {
        let _ = execute!(
            io::stderr(),
            Print("\n"),
            SetForegroundColor(self.theme.permission_accent),
            SetAttribute(Attribute::Bold),
            Print(format!("  Permission required: {tool_name}")),
            ResetColor,
            Print("\n"),
            SetForegroundColor(self.theme.dim),
            Print(format!("  {description}")),
            ResetColor,
            Print("\n"),
            SetForegroundColor(self.theme.dim),
            Print(format!("  Risk: {level}")),
            ResetColor,
            Print("\n"),
        );
    }

    /// Show an error message.
    pub fn error(&mut self, msg: &str) {
        if self.json_mode {
            return;
        }
        self.flush();
        let _ = execute!(
            io::stderr(),
            Print("\n"),
            SetForegroundColor(self.theme.error),
            SetAttribute(Attribute::Bold),
            Print("  Error: "),
            ResetColor,
            SetForegroundColor(self.theme.error),
            Print(msg),
            ResetColor,
            Print("\n"),
        );
    }

    /// Flush remaining buffered text.
    pub fn flush(&mut self) {
        if self.json_mode {
            return;
        }
        self.end_thinking();
        if !self.buffer.is_empty() {
            let remaining = std::mem::take(&mut self.buffer);
            self.print_markdown(&remaining);
        }
        let _ = io::stdout().flush();
        let _ = io::stderr().flush();
    }

    /// Print a completion separator.
    pub fn complete(&mut self) {
        self.flush();
        if !self.json_mode {
            let _ = execute!(io::stdout(), Print("\n"));
        }
    }

    fn end_thinking(&mut self) {
        if self.in_thinking {
            self.in_thinking = false;
            let _ = execute!(
                io::stderr(),
                ResetColor,
                SetAttribute(Attribute::Reset),
                Print("\n"),
            );
        }
    }

    fn print_markdown(&self, text: &str) {
        // Use termimad for rich markdown rendering
        let skin = make_skin(&self.theme);
        let rendered = skin.term_text(text);
        print!("{rendered}");
        let _ = io::stdout().flush();
    }
}

/// Print a JSON event line (for --json mode).
pub fn print_json_event(event: &cersei_agent::events::AgentEvent) {
    // AgentEvent doesn't derive Serialize, so we manually format key events
    let json = match event {
        cersei_agent::events::AgentEvent::TextDelta(t) => {
            serde_json::json!({"type": "text_delta", "text": t})
        }
        cersei_agent::events::AgentEvent::ThinkingDelta(t) => {
            serde_json::json!({"type": "thinking_delta", "text": t})
        }
        cersei_agent::events::AgentEvent::ToolStart { name, id, input } => {
            serde_json::json!({"type": "tool_start", "name": name, "id": id, "input": input})
        }
        cersei_agent::events::AgentEvent::ToolEnd { name, id, result, is_error, duration } => {
            serde_json::json!({"type": "tool_end", "name": name, "id": id, "result": result, "is_error": is_error, "duration_ms": duration.as_millis() as u64})
        }
        cersei_agent::events::AgentEvent::CostUpdate { turn_cost, cumulative_cost, input_tokens, output_tokens } => {
            serde_json::json!({"type": "cost_update", "turn_cost": turn_cost, "cumulative_cost": cumulative_cost, "input_tokens": input_tokens, "output_tokens": output_tokens})
        }
        cersei_agent::events::AgentEvent::Error(msg) => {
            serde_json::json!({"type": "error", "message": msg})
        }
        cersei_agent::events::AgentEvent::Complete(_) => {
            serde_json::json!({"type": "complete"})
        }
        _ => {
            serde_json::json!({"type": "event"})
        }
    };
    println!("{}", json);
}

fn make_skin(theme: &Theme) -> termimad::MadSkin {
    let mut skin = termimad::MadSkin::default();
    // Customize code block styling
    skin.code_block.set_fg(crossterm_to_termimad_color(theme.accent));
    skin.inline_code.set_fg(crossterm_to_termimad_color(theme.accent));
    skin.bold.set_fg(crossterm_to_termimad_color(theme.text));
    skin.italic.set_fg(crossterm_to_termimad_color(theme.dim));
    skin
}

fn crossterm_to_termimad_color(c: Color) -> termimad::crossterm::style::Color {
    // termimad re-exports crossterm, so the types are compatible
    match c {
        Color::Black => termimad::crossterm::style::Color::Black,
        Color::DarkGrey => termimad::crossterm::style::Color::DarkGrey,
        Color::Red => termimad::crossterm::style::Color::Red,
        Color::DarkRed => termimad::crossterm::style::Color::DarkRed,
        Color::Green => termimad::crossterm::style::Color::Green,
        Color::DarkGreen => termimad::crossterm::style::Color::DarkGreen,
        Color::Yellow => termimad::crossterm::style::Color::Yellow,
        Color::DarkYellow => termimad::crossterm::style::Color::DarkYellow,
        Color::Blue => termimad::crossterm::style::Color::Blue,
        Color::DarkBlue => termimad::crossterm::style::Color::DarkBlue,
        Color::Magenta => termimad::crossterm::style::Color::Magenta,
        Color::DarkMagenta => termimad::crossterm::style::Color::DarkMagenta,
        Color::Cyan => termimad::crossterm::style::Color::Cyan,
        Color::DarkCyan => termimad::crossterm::style::Color::DarkCyan,
        Color::White => termimad::crossterm::style::Color::White,
        Color::Grey => termimad::crossterm::style::Color::Grey,
        Color::Rgb { r, g, b } => termimad::crossterm::style::Color::Rgb { r, g, b },
        _ => termimad::crossterm::style::Color::Reset,
    }
}

fn tool_input_summary(name: &str, input: &serde_json::Value) -> String {
    match name {
        "Bash" | "bash" => input
            .get("command")
            .and_then(|v| v.as_str())
            .map(|s| truncate(s, 80))
            .unwrap_or_default(),
        "Read" | "file_read" => input
            .get("file_path")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        "Write" | "file_write" => input
            .get("file_path")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        "Edit" | "file_edit" => input
            .get("file_path")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        "Glob" | "glob" => input
            .get("pattern")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        "Grep" | "grep" => input
            .get("pattern")
            .and_then(|v| v.as_str())
            .map(|s| truncate(s, 60))
            .unwrap_or_default(),
        _ => {
            let s = serde_json::to_string(input).unwrap_or_default();
            truncate(&s, 80)
        }
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}...", &s[..max])
    }
}
