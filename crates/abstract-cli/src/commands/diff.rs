use crate::config::AppConfig;
use std::process::Command;

pub fn run(config: &AppConfig) -> anyhow::Result<()> {
    let output = Command::new("git")
        .args(["diff", "--stat"])
        .current_dir(&config.working_dir)
        .output()?;

    let stat = String::from_utf8_lossy(&output.stdout);
    if stat.is_empty() {
        eprintln!("No changes.");
        return Ok(());
    }

    println!("{stat}");

    // Also show full diff (abbreviated)
    let output = Command::new("git")
        .args(["diff"])
        .current_dir(&config.working_dir)
        .output()?;

    let diff = String::from_utf8_lossy(&output.stdout);
    let lines: Vec<&str> = diff.lines().collect();
    let display_lines = if lines.len() > 100 {
        eprintln!("\x1b[90m(showing first 100 of {} lines)\x1b[0m", lines.len());
        &lines[..100]
    } else {
        &lines
    };

    for line in display_lines {
        if line.starts_with('+') && !line.starts_with("+++") {
            println!("\x1b[32m{line}\x1b[0m");
        } else if line.starts_with('-') && !line.starts_with("---") {
            println!("\x1b[31m{line}\x1b[0m");
        } else if line.starts_with("@@") {
            println!("\x1b[36m{line}\x1b[0m");
        } else {
            println!("{line}");
        }
    }

    Ok(())
}
