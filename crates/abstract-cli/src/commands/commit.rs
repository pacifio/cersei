use crate::config::AppConfig;
use std::process::Command;

pub async fn run(config: &AppConfig) -> anyhow::Result<()> {
    // Get staged diff
    let output = Command::new("git")
        .args(["diff", "--staged"])
        .current_dir(&config.working_dir)
        .output()?;

    let diff = String::from_utf8_lossy(&output.stdout);

    if diff.is_empty() {
        // Try unstaged diff
        let output = Command::new("git")
            .args(["diff"])
            .current_dir(&config.working_dir)
            .output()?;
        let diff = String::from_utf8_lossy(&output.stdout);
        if diff.is_empty() {
            eprintln!("No changes to commit.");
            return Ok(());
        }
        eprintln!("\x1b[33mNo staged changes. Showing unstaged diff.\x1b[0m");
        eprintln!("Stage changes with `git add` first, then run /commit again.");
        return Ok(());
    }

    eprintln!(
        "\x1b[90mStaged changes detected. Ask the agent to generate a commit message.\x1b[0m"
    );
    eprintln!("\x1b[90mTip: Type 'commit these changes' in the prompt.\x1b[0m");
    Ok(())
}
