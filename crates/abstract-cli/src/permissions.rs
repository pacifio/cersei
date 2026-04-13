//! Permission policies for the CLI.
//!
//! Two implementations:
//! - `CliPermissionPolicy` — for non-TUI modes (REPL, single-shot). Reads from stdin directly.
//! - `TuiPermissionPolicy` — for TUI mode. Sends requests via channel, TUI renders overlay.

use cersei_tools::permissions::{PermissionDecision, PermissionPolicy, PermissionRequest};
use cersei_tools::PermissionLevel;
use parking_lot::Mutex;
use std::collections::HashSet;
use std::io::{self, Write};
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};
use tokio::sync::{mpsc, oneshot};

/// Permission mode shared between TUI and policy.
/// Encoded as u8: 0=Auto, 1=Plan, 2=Editor, 3=Bypass, 4=BypassAlert.
pub type SharedPermissionMode = Arc<AtomicU8>;

pub fn new_shared_mode() -> SharedPermissionMode {
    Arc::new(AtomicU8::new(0))
}

// ─── Permission request sent to TUI ────────────────────────────────────────

/// A permission request sent from the policy to the TUI event loop.
pub struct TuiPermissionRequest {
    pub tool_name: String,
    pub description: String,
    pub response_tx: oneshot::Sender<PermissionDecision>,
}

/// Create the channel pair for TUI permission flow.
pub fn permission_channel() -> (mpsc::Sender<TuiPermissionRequest>, mpsc::Receiver<TuiPermissionRequest>) {
    mpsc::channel(8)
}

// ─── TUI Permission Policy (channel-based, no stdin) ───────────────────────

/// Permission policy for TUI mode. Sends requests to the TUI via channel,
/// awaits the user's decision rendered as an overlay.
pub struct TuiPermissionPolicy {
    session_allowed: Mutex<HashSet<String>>,
    always_allowed: Mutex<HashSet<String>>,
    mode: SharedPermissionMode,
    request_tx: mpsc::Sender<TuiPermissionRequest>,
}

impl TuiPermissionPolicy {
    pub fn new(mode: SharedPermissionMode, request_tx: mpsc::Sender<TuiPermissionRequest>) -> Self {
        Self {
            session_allowed: Mutex::new(HashSet::new()),
            always_allowed: Mutex::new(HashSet::new()),
            mode,
            request_tx,
        }
    }
}

#[async_trait::async_trait]
impl PermissionPolicy for TuiPermissionPolicy {
    async fn check(&self, request: &PermissionRequest) -> PermissionDecision {
        // Mode-based fast paths (same as CliPermissionPolicy)
        let mode = self.mode.load(Ordering::Relaxed);
        match mode {
            3 => return PermissionDecision::Allow, // Bypass
            4 => {
                if request.tool_name == "Bash" || request.tool_name == "PowerShell" {
                    #[cfg(target_os = "macos")]
                    {
                        let _ = std::process::Command::new("osascript")
                            .args(["-e", &format!(
                                "display notification \"{}\" with title \"Abstract CLI\"",
                                request.description.replace('"', "'")
                            )])
                            .spawn();
                    }
                }
                return PermissionDecision::Allow;
            }
            1 => {
                match request.permission_level {
                    PermissionLevel::None | PermissionLevel::ReadOnly => return PermissionDecision::Allow,
                    _ => return PermissionDecision::Deny("Plan mode: read-only".into()),
                }
            }
            2 => {
                if request.tool_name == "Bash" || request.tool_name == "PowerShell" {
                    return PermissionDecision::Deny("Editor mode: shell commands disabled".into());
                }
                return PermissionDecision::Allow;
            }
            _ => {}
        }

        // Auto-allow safe operations
        match request.permission_level {
            PermissionLevel::None | PermissionLevel::ReadOnly => return PermissionDecision::Allow,
            PermissionLevel::Forbidden => return PermissionDecision::Deny("Operation is forbidden".into()),
            _ => {}
        }

        // Check caches
        if self.always_allowed.lock().contains(&request.tool_name) {
            return PermissionDecision::Allow;
        }
        if self.session_allowed.lock().contains(&request.tool_name) {
            return PermissionDecision::Allow;
        }

        // Send request to TUI and await response
        let (response_tx, response_rx) = oneshot::channel();
        let tui_request = TuiPermissionRequest {
            tool_name: request.tool_name.clone(),
            description: request.description.clone(),
            response_tx,
        };

        if self.request_tx.send(tui_request).await.is_err() {
            return PermissionDecision::Deny("TUI channel closed".into());
        }

        // Wait for user's decision from TUI
        match response_rx.await {
            Ok(decision) => {
                // Cache session/always decisions
                match &decision {
                    PermissionDecision::AllowForSession => {
                        self.session_allowed.lock().insert(request.tool_name.clone());
                    }
                    PermissionDecision::Allow => {
                        // "Always allow" — cache permanently for session
                        self.always_allowed.lock().insert(request.tool_name.clone());
                    }
                    _ => {}
                }
                decision
            }
            Err(_) => PermissionDecision::Deny("Permission response channel closed".into()),
        }
    }
}

// ─── CLI Permission Policy (stdin-based, for non-TUI) ──────────────────────

/// Interactive permission policy for non-TUI modes (REPL, single-shot).
/// Reads from stdin directly. Do NOT use in TUI mode.
pub struct CliPermissionPolicy {
    session_allowed: Mutex<HashSet<String>>,
    always_allowed: Mutex<HashSet<String>>,
    mode: SharedPermissionMode,
}

impl CliPermissionPolicy {
    pub fn new() -> Self {
        Self {
            session_allowed: Mutex::new(HashSet::new()),
            always_allowed: Mutex::new(HashSet::new()),
            mode: new_shared_mode(),
        }
    }

    pub fn with_mode(mode: SharedPermissionMode) -> Self {
        Self {
            session_allowed: Mutex::new(HashSet::new()),
            always_allowed: Mutex::new(HashSet::new()),
            mode,
        }
    }
}

#[async_trait::async_trait]
impl PermissionPolicy for CliPermissionPolicy {
    async fn check(&self, request: &PermissionRequest) -> PermissionDecision {
        let mode = self.mode.load(Ordering::Relaxed);
        match mode {
            3 => return PermissionDecision::Allow,
            4 => return PermissionDecision::Allow,
            1 => match request.permission_level {
                PermissionLevel::None | PermissionLevel::ReadOnly => return PermissionDecision::Allow,
                _ => return PermissionDecision::Deny("Plan mode: read-only".into()),
            },
            2 => {
                if request.tool_name == "Bash" || request.tool_name == "PowerShell" {
                    return PermissionDecision::Deny("Editor mode: shell commands disabled".into());
                }
                return PermissionDecision::Allow;
            }
            _ => {}
        }

        match request.permission_level {
            PermissionLevel::None | PermissionLevel::ReadOnly => return PermissionDecision::Allow,
            PermissionLevel::Forbidden => return PermissionDecision::Deny("Operation is forbidden".into()),
            _ => {}
        }

        if self.always_allowed.lock().contains(&request.tool_name) {
            return PermissionDecision::Allow;
        }
        if self.session_allowed.lock().contains(&request.tool_name) {
            return PermissionDecision::Allow;
        }

        // Direct stdin prompt (non-TUI only)
        eprint!("\n");
        eprint!("  \x1b[33;1mPermission required: {}\x1b[0m\n", request.tool_name);
        eprint!("  \x1b[90m{}\x1b[0m\n", request.description);
        eprint!("  \x1b[33m[Y]es  [N]o  [S]ession  [A]lways\x1b[0m ");
        let _ = io::stderr().flush();

        let decision = read_permission_char();
        match decision {
            'y' | 'Y' | '\n' => PermissionDecision::AllowOnce,
            's' | 'S' => {
                self.session_allowed.lock().insert(request.tool_name.clone());
                PermissionDecision::AllowForSession
            }
            'a' | 'A' => {
                self.always_allowed.lock().insert(request.tool_name.clone());
                PermissionDecision::Allow
            }
            _ => PermissionDecision::Deny("User denied".into()),
        }
    }
}

fn read_permission_char() -> char {
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
        let mut input = String::new();
        let _ = io::stdin().read_line(&mut input);
        input.trim().chars().next().unwrap_or('n')
    }
}
