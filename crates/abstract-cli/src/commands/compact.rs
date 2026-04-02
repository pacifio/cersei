use crate::config::AppConfig;

pub fn run(_config: &AppConfig) -> anyhow::Result<()> {
    eprintln!("\x1b[90mManual compaction triggered. Will compact on next agent turn.\x1b[0m");
    // The actual compaction happens inside the agent loop when auto_compact is enabled.
    // A manual trigger would require injecting a signal to the agent.
    // For now, this is informational.
    Ok(())
}
