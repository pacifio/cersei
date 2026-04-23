//! Status line: model, tokens, cost, context usage.

use crate::theme::Theme;
use crossterm::execute;
use crossterm::style::{Print, ResetColor, SetForegroundColor};
use std::io::{self, Write};

#[allow(dead_code)]
pub struct StatusLine {
    theme: Theme,
    model: String,
    input_tokens: u64,
    output_tokens: u64,
    cost_usd: f64,
    context_pct: f64,
    session_id: String,
    enabled: bool,
}

#[allow(dead_code)]
impl StatusLine {
    pub fn new(theme: &Theme, model: &str, session_id: &str, enabled: bool) -> Self {
        Self {
            theme: theme.clone(),
            model: model.to_string(),
            input_tokens: 0,
            output_tokens: 0,
            cost_usd: 0.0,
            context_pct: 0.0,
            session_id: if session_id.len() > 8 {
                session_id[..8].to_string()
            } else {
                session_id.to_string()
            },
            enabled,
        }
    }

    pub fn update_cost(&mut self, input_tokens: u64, output_tokens: u64, cost_usd: f64) {
        self.input_tokens = input_tokens;
        self.output_tokens = output_tokens;
        self.cost_usd = cost_usd;
    }

    pub fn update_context(&mut self, pct: f64) {
        self.context_pct = pct;
    }

    pub fn set_model(&mut self, model: &str) {
        self.model = model.to_string();
    }

    /// Render the status line to stderr (bottom of terminal).
    pub fn render(&self) {
        if !self.enabled {
            return;
        }

        let cost_str = if self.cost_usd > 0.0 {
            format!("${:.4}", self.cost_usd)
        } else {
            "$0".into()
        };

        let tokens_str = format!("{}in/{}out", self.input_tokens, self.output_tokens);
        let ctx_str = format!("ctx:{:.0}%", self.context_pct * 100.0);

        let status = format!(
            " {} | {} | {} | {} | {} ",
            self.model, tokens_str, cost_str, ctx_str, self.session_id,
        );

        let _ = execute!(
            io::stderr(),
            SetForegroundColor(self.theme.status_fg),
            Print(format!("\r\x1b[K{status}")),
            ResetColor,
        );
        let _ = io::stderr().flush();
    }

    /// Clear the status line.
    pub fn clear(&self) {
        if !self.enabled {
            return;
        }
        eprint!("\r\x1b[K");
        let _ = io::stderr().flush();
    }
}
