pub fn run() -> anyhow::Result<()> {
    // Clear terminal
    print!("\x1b[2J\x1b[H");
    eprintln!("\x1b[90mConversation cleared. Starting fresh.\x1b[0m");
    // Note: agent message history is cleared on next run_stream call
    // because we'd need to rebuild the agent. For now, this clears the display.
    Ok(())
}
