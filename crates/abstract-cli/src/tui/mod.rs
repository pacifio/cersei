//! Production TUI for Abstract CLI.
//!
//! Built on ratatui + crossterm with alternate screen buffer,
//! virtual list scrollback, streaming markdown, and modal overlays.

pub mod app;
pub mod event_loop;
pub mod layout;
pub mod markdown;
pub mod scroll;
pub mod theme;
pub mod virtual_list;
pub mod widgets;

use crate::config::AppConfig;
use cersei::Agent;
use cersei_memory::manager::MemoryManager;
use crossterm::{
    event::{
        DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
        KeyboardEnhancementFlags, PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
    },
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::prelude::*;
use std::io::{self, stdout};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use tokio_util::sync::CancellationToken;

pub type Terminal = ratatui::Terminal<CrosstermBackend<io::Stdout>>;

static KEYBOARD_ENHANCEMENT_PUSHED: AtomicBool = AtomicBool::new(false);

/// Set up the terminal for TUI rendering.
pub fn setup_terminal() -> io::Result<Terminal> {
    enable_raw_mode()?;
    let mut stdout = stdout();
    execute!(
        stdout,
        EnterAlternateScreen,
        EnableBracketedPaste,
        EnableMouseCapture
    )?;

    // Enable kitty keyboard protocol for Shift+Enter detection.
    // Only if the terminal actually supports it (avoids broken state on resize).
    let supports_keyboard_enhancement =
        crossterm::terminal::supports_keyboard_enhancement().unwrap_or(false);
    let enhancement_pushed = supports_keyboard_enhancement
        && execute!(
            stdout,
            PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
        )
        .is_ok();
    KEYBOARD_ENHANCEMENT_PUSHED.store(enhancement_pushed, Ordering::Release);

    let backend = CrosstermBackend::new(stdout);
    let terminal = ratatui::Terminal::new(backend)?;
    Ok(terminal)
}

/// Restore the terminal to its original state.
pub fn restore_terminal(terminal: &mut Terminal) -> io::Result<()> {
    if KEYBOARD_ENHANCEMENT_PUSHED.swap(false, Ordering::AcqRel) {
        let _ = execute!(terminal.backend_mut(), PopKeyboardEnhancementFlags);
    }
    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableBracketedPaste,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;
    Ok(())
}

/// Install a panic hook that restores the terminal before printing the panic message.
pub fn install_panic_hook() {
    let original = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = disable_raw_mode();
        let mut stdout = stdout();
        if KEYBOARD_ENHANCEMENT_PUSHED.swap(false, Ordering::AcqRel) {
            let _ = execute!(stdout, PopKeyboardEnhancementFlags);
        }
        let _ = execute!(
            stdout,
            LeaveAlternateScreen,
            DisableBracketedPaste,
            DisableMouseCapture
        );
        original(info);
    }));
}

/// Main entry point for the TUI. Sets up terminal, runs the event loop, cleans up.
pub async fn run(
    agent: Agent,
    config: &AppConfig,
    pricing_model: &str,
    memory_manager: &MemoryManager,
    session_id: &str,
    cancel_token: CancellationToken,
    shared_mode: crate::permissions::SharedPermissionMode,
    permission_rx: tokio::sync::mpsc::Receiver<crate::permissions::TuiPermissionRequest>,
) -> anyhow::Result<()> {
    install_panic_hook();

    let mut terminal = setup_terminal()?;
    let agent = Arc::new(agent);
    let result = event_loop::run(
        &mut terminal,
        agent,
        config,
        pricing_model,
        memory_manager,
        session_id,
        cancel_token,
        shared_mode,
        permission_rx,
    )
    .await;

    restore_terminal(&mut terminal)?;
    result
}
