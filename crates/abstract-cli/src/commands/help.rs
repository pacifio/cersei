pub fn run() -> anyhow::Result<()> {
    eprintln!("\x1b[36;1mCommands:\x1b[0m");
    eprintln!("  /help, /h, /?     Show this help");
    eprintln!("  /clear            Clear conversation history");
    eprintln!("  /compact          Manually compact context");
    eprintln!("  /cost             Show token usage and cost");
    eprintln!("  /commit           Generate a git commit with AI message");
    eprintln!("  /review           AI code review of current changes");
    eprintln!("  /memory, /mem     Show memory status");
    eprintln!("  /model <name>     Switch model (opus, sonnet, haiku)");
    eprintln!("  /config [key val] Show or set config");
    eprintln!("  /diff             Show git diff");
    eprintln!("  /resume [id]      Resume a previous session");
    eprintln!("  /exit, /quit, /q  Exit");
    Ok(())
}
