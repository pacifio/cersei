//! Slash command registry and dispatch.

mod clear;
mod commit;
mod compact;
mod compression;
mod config_cmd;
mod cost;
mod diff;
mod help;
mod memory;
mod model;
mod resume;
mod review;

use crate::config::AppConfig;
use cersei::Agent;
use std::sync::Arc;

pub struct CommandRegistry;

impl CommandRegistry {
    pub fn new() -> Self {
        Self
    }

    pub async fn execute(
        &mut self,
        cmd: &str,
        args: &str,
        config: &AppConfig,
        session_id: &str,
        agent: Option<&Arc<Agent>>,
    ) {
        let result = match cmd {
            "help" | "h" | "?" => help::run(),
            "sessions" | "ls" => crate::sessions::list(config),
            "clear" => clear::run(),
            "compact" => compact::run(config),
            "cost" => cost::run(session_id),
            "commit" => commit::run(config).await,
            "review" => review::run(config).await,
            "memory" | "mem" => memory::run(config),
            "model" => model::run(args, config),
            "config" | "cfg" => config_cmd::run(args, config),
            "diff" => diff::run(config),
            "resume" => resume::run(args, config),
            "compression" | "compress" => compression::run(args, agent),
            _ => {
                eprintln!("\x1b[33mUnknown command: /{cmd}\x1b[0m");
                eprintln!("Type /help to see available commands.");
                Ok(())
            }
        };

        if let Err(e) = result {
            eprintln!("\x1b[31mCommand error: {e}\x1b[0m");
        }
    }
}
