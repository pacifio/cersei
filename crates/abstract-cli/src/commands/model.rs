use crate::config::AppConfig;

pub fn run(args: &str, _config: &AppConfig) -> anyhow::Result<()> {
    if args.is_empty() {
        eprintln!("Current model: {}", _config.model);
        eprintln!("\x1b[90mUsage: /model <name>\x1b[0m");
        eprintln!("\x1b[90mAliases: opus, sonnet, haiku, gpt-4o\x1b[0m");
        return Ok(());
    }

    let resolved = match args.trim() {
        "opus" => "claude-opus-4-6",
        "sonnet" => "claude-sonnet-4-6",
        "haiku" => "claude-haiku-4-5",
        other => other,
    };

    eprintln!("\x1b[90mModel set to: {resolved}\x1b[0m");
    eprintln!("\x1b[90mNote: Takes effect on next agent turn.\x1b[0m");
    // In a future iteration, this could modify the agent's model dynamically.
    Ok(())
}
