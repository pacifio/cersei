use crate::config::AppConfig;
use crate::sessions;

pub fn run(args: &str, config: &AppConfig) -> anyhow::Result<()> {
    if args.is_empty() {
        // Show recent sessions
        sessions::list(config)?;
        eprintln!("\n\x1b[90mUsage: /resume <session-id>\x1b[0m");
        eprintln!("\x1b[90mOr start with: abstract --resume <session-id>\x1b[0m");
    } else {
        eprintln!("\x1b[90mTo resume a session, restart with: abstract --resume {args}\x1b[0m");
    }
    Ok(())
}
