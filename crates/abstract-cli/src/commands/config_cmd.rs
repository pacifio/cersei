use crate::config::AppConfig;

pub fn run(args: &str, config: &AppConfig) -> anyhow::Result<()> {
    if args.is_empty() {
        // Show config
        let toml_str = toml::to_string_pretty(config)?;
        println!("{toml_str}");
        return Ok(());
    }

    eprintln!(
        "\x1b[90mUse `abstract config set <key> <value>` from the shell to set config.\x1b[0m"
    );
    Ok(())
}
