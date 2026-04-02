//! Signal handling: Ctrl+C (single = cancel, double = exit), SIGTERM.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;
use tokio_util::sync::CancellationToken;

static LAST_CTRLC: parking_lot::Mutex<Option<Instant>> = parking_lot::Mutex::new(None);

/// Install signal handlers. Returns a CancellationToken that gets cancelled on Ctrl+C.
pub fn install(cancel_token: CancellationToken, running: Arc<AtomicBool>) -> anyhow::Result<()> {
    let ct = cancel_token.clone();
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
            // Agent is running — cancel it
            ct.cancel();
            eprintln!("\n  Cancelling... (press Ctrl+C again to force exit)");
        } else {
            // Not running — exit
            eprintln!("\nGoodbye.");
            std::process::exit(0);
        }
    });

    Ok(())
}

fn ctrlc_handler(f: impl Fn() + Send + 'static) {
    let _ = ctrlc::set_handler(f);
}

/// Reset the cancel token for a new agent run.
#[allow(dead_code)]
pub fn fresh_cancel_token() -> CancellationToken {
    CancellationToken::new()
}
