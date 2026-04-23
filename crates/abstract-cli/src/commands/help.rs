pub fn run() -> anyhow::Result<()> {
    eprintln!("\x1b[36;1mCommands:\x1b[0m");
    eprintln!("  /help, /h, /?       Show this help");
    eprintln!("  /sessions, /ls      List all sessions");
    eprintln!("  /clear              Clear conversation history");
    eprintln!("  /compact            Manually compact context");
    eprintln!("  /cost               Show token usage and cost");
    eprintln!("  /commit             Generate a git commit with AI message");
    eprintln!("  /review             AI code review of current changes");
    eprintln!("  /memory, /mem       Show memory status");
    eprintln!("  /model <name>       Switch model (opus, sonnet, haiku, 4o, gemini, llama)");
    eprintln!("  /config [key val]   Show or set config");
    eprintln!("  /diff               Show git diff");
    eprintln!("  /resume [id]        Resume a previous session");
    eprintln!(
        "  /compression <lvl>  Toggle tool-output compression (off | on | minimal | aggressive)"
    );
    eprintln!("  /exit, /quit, /q    Exit");
    eprintln!();
    eprintln!("\x1b[36;1mCLI:\x1b[0m");
    eprintln!("  abstract sessions list       List all sessions");
    eprintln!("  abstract sessions show <id>  View transcript");
    eprintln!("  abstract sessions rm <id>    Delete session");
    eprintln!("  abstract --resume [id]       Resume session");
    eprintln!("  abstract login status        Provider auth status");
    Ok(())
}
