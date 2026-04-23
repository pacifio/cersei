use cersei::Agent;
use cersei_compression::CompressionLevel;
use std::sync::Arc;

pub fn run(args: &str, agent: Option<&Arc<Agent>>) -> anyhow::Result<()> {
    let Some(agent) = agent else {
        eprintln!("\x1b[33mCompression can only be toggled from the interactive REPL.\x1b[0m");
        return Ok(());
    };

    if args.trim().is_empty() {
        let current = agent.compression_level();
        eprintln!("Tool-output compression: \x1b[1m{current}\x1b[0m");
        eprintln!("\x1b[90mUsage: /compression on | off | minimal | aggressive\x1b[0m");
        return Ok(());
    }

    let level: CompressionLevel = match args.trim().parse() {
        Ok(l) => l,
        Err(e) => {
            eprintln!("\x1b[31m{e}\x1b[0m");
            eprintln!("\x1b[90mAccepted: off, on, minimal, aggressive\x1b[0m");
            return Ok(());
        }
    };

    agent.set_compression_level(level);
    eprintln!("\x1b[90mTool-output compression set to: {level}\x1b[0m");
    eprintln!("\x1b[90mTakes effect on the next tool call.\x1b[0m");
    Ok(())
}
