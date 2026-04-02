pub fn run(session_id: &str) -> anyhow::Result<()> {
    let short_id = if session_id.len() > 8 {
        &session_id[..8]
    } else {
        session_id
    };
    eprintln!("\x1b[90mSession: {short_id}\x1b[0m");
    eprintln!("\x1b[90mCost tracking is displayed in the status line during agent runs.\x1b[0m");
    // Full cost tracking would require access to the agent's CostTracker.
    // In a future iteration, we can expose this via the App struct.
    Ok(())
}
