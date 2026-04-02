//! Project initialization: creates .abstract/ directory with default config.

use crate::config;
use std::path::Path;

pub fn run() -> anyhow::Result<()> {
    let project_dir = config::project_config_dir();

    if project_dir.exists() {
        println!("Project already initialized (.abstract/ exists).");
        return Ok(());
    }

    std::fs::create_dir_all(&project_dir)?;

    // Create default config
    let default_config = config::AppConfig::default();
    config::save_to(&default_config, &project_dir.join("config.toml"))?;

    // Create instructions template
    let instructions = r#"# Project Instructions

Add project-specific instructions here. These will be injected into the
system prompt for every conversation in this directory.

## Example

- This is a Rust project using Tokio for async
- Tests are run with `cargo test`
- Prefer functional patterns over OOP
"#;
    std::fs::write(project_dir.join("instructions.md"), instructions)?;

    // Add to .gitignore if present
    let gitignore = Path::new(".gitignore");
    if gitignore.exists() {
        let content = std::fs::read_to_string(gitignore)?;
        if !content.contains(".abstract/") {
            let mut f = std::fs::OpenOptions::new().append(true).open(gitignore)?;
            use std::io::Write;
            writeln!(f, "\n# Abstract CLI\n.abstract/")?;
        }
    }

    println!("Initialized .abstract/ in current directory.");
    println!("  config.toml      — project configuration");
    println!("  instructions.md  — project-specific instructions");

    Ok(())
}
