//! Signal handling: Ctrl+C (single = cancel the run, double = exit), SIGTERM.

use cersei::Agent;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;
use tokio_util::sync::CancellationToken;

static LAST_CTRLC: parking_lot::Mutex<Option<Instant>> = parking_lot::Mutex::new(None);

/// The agent whose current run Ctrl+C should cancel, if one is running.
///
/// Ctrl+C cancels the *run*, not the process-wide shutdown token: that token is
/// one-shot, and every run's cancellation token descends from it, so firing it
/// would stop every later run too — the first Ctrl+C would leave the agent
/// unable to answer anything.
pub type ActiveAgent = Arc<parking_lot::Mutex<Option<Arc<Agent>>>>;

pub fn new_active_agent() -> ActiveAgent {
    Arc::new(parking_lot::Mutex::new(None))
}

/// Install signal handlers.
///
/// `active` names the agent to cancel while a run is in flight; `shutdown` is
/// fired on the exit path so the TUI can unwind before the process goes away.
pub fn install(
    active: ActiveAgent,
    shutdown: CancellationToken,
    running: Arc<AtomicBool>,
) -> anyhow::Result<()> {
    let r = running.clone();

    ctrlc_handler(move || {
        let mut last = LAST_CTRLC.lock();
        let now = Instant::now();

        // Double Ctrl+C within 500ms = hard exit
        if let Some(prev) = *last {
            if now.duration_since(prev).as_millis() < 500 {
                eprintln!("\nForce exit.");
                std::process::exit(130);
            }
        }
        *last = Some(now);

        if r.load(Ordering::Relaxed) {
            // Agent is running — cancel that run, and only that run.
            if let Some(agent) = active.lock().as_ref() {
                agent.cancel();
            }
            eprintln!("\n  Cancelling... (press Ctrl+C again to force exit)");
        } else {
            // Not running — exit
            eprintln!("\nGoodbye.");
            shutdown.cancel();
            std::process::exit(0);
        }
    });

    Ok(())
}

fn ctrlc_handler(f: impl Fn() + Send + 'static) {
    let _ = ctrlc::set_handler(f);
}
