//! Interactive permission UI for the CLI.
//!
//! Implements PermissionPolicy by prompting the user in the terminal.
//! Caches session-level allow decisions.

use cersei_tools::permissions::{PermissionDecision, PermissionPolicy, PermissionRequest};
use cersei_tools::PermissionLevel;
use parking_lot::Mutex;
use std::collections::HashSet;
use std::io::{self, Write};

/// Interactive permission policy for the CLI.
/// Prompts user for Write/Execute/Dangerous tools, auto-allows ReadOnly/None.
pub struct CliPermissionPolicy {
    /// Tools allowed for the entire session (by tool name).
    session_allowed: Mutex<HashSet<String>>,
    /// Tools permanently allowed (by tool name).
    always_allowed: Mutex<HashSet<String>>,
}

impl CliPermissionPolicy {
    pub fn new() -> Self {
        Self {
            session_allowed: Mutex::new(HashSet::new()),
            always_allowed: Mutex::new(HashSet::new()),
        }
    }
}

#[async_trait::async_trait]
impl PermissionPolicy for CliPermissionPolicy {
    async fn check(&self, request: &PermissionRequest) -> PermissionDecision {
        // Auto-allow safe operations
        match request.permission_level {
            PermissionLevel::None | PermissionLevel::ReadOnly => {
                return PermissionDecision::Allow;
            }
            PermissionLevel::Forbidden => {
                return PermissionDecision::Deny("Operation is forbidden".into());
            }
            _ => {}
        }

        // Check caches
        if self.always_allowed.lock().contains(&request.tool_name) {
            return PermissionDecision::Allow;
        }
        if self.session_allowed.lock().contains(&request.tool_name) {
            return PermissionDecision::Allow;
        }

        // Prompt user
        let level_str = format!("{:?}", request.permission_level);
        eprint!("\n");
        eprint!("  \x1b[33;1mPermission required: {}\x1b[0m\n", request.tool_name);
        eprint!("  \x1b[90m{}\x1b[0m\n", request.description);
        eprint!("  \x1b[90mRisk: {level_str}\x1b[0m\n");
        eprint!("  \x1b[33m[Y]es  [N]o  [S]ession  [A]lways\x1b[0m ");
        let _ = io::stderr().flush();

        let decision = read_permission_char();

        match decision {
            'y' | 'Y' | '\n' => PermissionDecision::AllowOnce,
            's' | 'S' => {
                self.session_allowed
                    .lock()
                    .insert(request.tool_name.clone());
                PermissionDecision::AllowForSession
            }
            'a' | 'A' => {
                self.always_allowed
                    .lock()
                    .insert(request.tool_name.clone());
                PermissionDecision::Allow
            }
            _ => PermissionDecision::Deny("User denied".into()),
        }
    }
}

fn read_permission_char() -> char {
    // Try to read a single character
    use crossterm::event::{self, Event, KeyCode, KeyEvent};
    use crossterm::terminal;

    if terminal::enable_raw_mode().is_ok() {
        let result = loop {
            if let Ok(Event::Key(KeyEvent { code, .. })) = event::read() {
                break match code {
                    KeyCode::Char(c) => c,
                    KeyCode::Enter => 'y',
                    KeyCode::Esc => 'n',
                    _ => continue,
                };
            }
        };
        let _ = terminal::disable_raw_mode();
        eprint!("\n");
        result
    } else {
        // Fallback: read a line
        let mut input = String::new();
        let _ = io::stdin().read_line(&mut input);
        input.trim().chars().next().unwrap_or('n')
    }
}
