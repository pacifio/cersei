use crate::config::AppConfig;
use std::process::Command;

pub async fn run(config: &AppConfig) -> anyhow::Result<()> {
    let output = Command::new("git")
        .args(["diff"])
        .current_dir(&config.working_dir)
        .output()?;

    let diff = String::from_utf8_lossy(&output.stdout);
    if diff.is_empty() {
        eprintln!("No changes to review.");
        return Ok(());
    }

    let line_count = diff.lines().count();
    eprintln!("\x1b[90m{line_count} lines of changes detected.\x1b[0m");
    eprintln!("\x1b[90mTip: Type 'review my changes' in the prompt.\x1b[0m");
    Ok(())
}
