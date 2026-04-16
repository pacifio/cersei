//! abstract — A high-performance Rust-native CLI coding agent.
//!
//! Built on the Cersei SDK with sub-millisecond tool dispatch,
//! graph-backed memory, and a single static binary.

mod app;
mod commands;
mod config;
mod init;
mod input;
mod login;
mod permissions;
mod prompt;
mod render;
mod repl;
mod sessions;
mod signals;
mod status;
mod theme;
mod tui;

use clap::{Parser, Subcommand};

// ─── CLI definition ────────────────────────────────────────────────────────

#[derive(Parser)]
#[command(
    name = "abstract",
    about = "A high-performance AI coding agent",
    version,
    after_help = "Examples:\n  abstract                        Start interactive REPL\n  abstract \"fix the tests\"        Single-shot mode\n  abstract --resume               Resume last session\n  abstract --model opus --max     Use Opus with max thinking"
)]
pub struct Cli {
    /// Prompt to run in single-shot mode (omit for REPL)
    #[arg(short = 'p', long = "prompt", value_name = "PROMPT")]
    pub prompt: Option<String>,

    /// Resume a previous session
    #[arg(long, value_name = "SESSION_ID", num_args = 0..=1, default_missing_value = "last")]
    pub resume: Option<String>,

    /// Model to use (e.g., opus, sonnet, haiku, gpt-4o)
    #[arg(short, long)]
    pub model: Option<String>,

    /// Provider to use (anthropic, openai)
    #[arg(short = 'P', long)]
    pub provider: Option<String>,

    /// Fast mode (low effort, minimal thinking)
    #[arg(long, conflicts_with = "max")]
    pub fast: bool,

    /// Max mode (maximum thinking budget)
    #[arg(long, conflicts_with = "fast")]
    pub max: bool,

    /// Fallback models (comma-separated) for provider switching on error
    #[arg(long, value_delimiter = ',', value_name = "MODELS")]
    pub fallback: Vec<String>,

    /// Auto-approve all tool permissions (CI/headless mode)
    #[arg(long)]
    pub no_permissions: bool,

    /// Output events as NDJSON (for piping)
    #[arg(long)]
    pub json: bool,

    /// Enable verbose/debug logging
    #[arg(short, long)]
    pub verbose: bool,

    /// Working directory override
    #[arg(short = 'C', long)]
    pub directory: Option<String>,

    /// Headless autonomous mode: task-focused prompt, auto-approve all tools, extended turns
    #[arg(long, alias = "benchmark")]
    pub headless: bool,

    /// Enable embedding API for semantic code search reranking (uses your LLM provider's embeddings)
    #[arg(long)]
    pub embedding_api: bool,

    /// Output format: text (default) or stream-json (NDJSON events)
    #[arg(long, value_name = "FORMAT")]
    pub output_format: Option<String>,

    /// Use a local proxy (VibeProxy or compatible) instead of direct API keys
    #[arg(long)]
    pub proxy: bool,

    /// Proxy URL (default: http://localhost:8317/v1)
    #[arg(long, value_name = "URL")]
    pub proxy_url: Option<String>,

    #[command(subcommand)]
    pub command: Option<Commands>,
}

#[derive(Subcommand)]
pub enum Commands {
    /// Manage sessions
    Sessions {
        #[command(subcommand)]
        action: SessionAction,
    },
    /// Manage configuration
    Config {
        #[command(subcommand)]
        action: ConfigAction,
    },
    /// Manage memory
    Memory {
        #[command(subcommand)]
        action: MemoryAction,
    },
    /// Manage MCP servers
    Mcp {
        #[command(subcommand)]
        action: McpAction,
    },
    /// Initialize project (.abstract/ directory)
    Init,
    /// Authenticate with a provider
    Login {
        /// Provider: claude, openai, key, status (default: interactive)
        provider: Option<String>,
    },
    /// Remove saved credentials
    Logout,
}

#[derive(Subcommand)]
pub enum SessionAction {
    /// List all sessions
    #[command(alias = "ls")]
    List,
    /// Show a session transcript
    Show { id: String },
    /// Delete a session
    Rm { id: String },
}

#[derive(Subcommand)]
pub enum ConfigAction {
    /// Show current configuration
    Show,
    /// Set a configuration value
    Set { key: String, value: String },
}

#[derive(Subcommand)]
pub enum MemoryAction {
    /// Show memory status
    Show,
    /// Clear all memory
    Clear,
}

#[derive(Subcommand)]
pub enum McpAction {
    /// Add an MCP server
    Add {
        name: String,
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        command: Vec<String>,
    },
    /// List configured MCP servers
    List,
    /// Remove an MCP server
    Remove { name: String },
}

// ─── Main ──────────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    // Initialize tracing
    if cli.verbose {
        tracing_subscriber::fmt()
            .with_env_filter("abstract=debug,cersei=debug")
            .init();
    } else {
        tracing_subscriber::fmt()
            .with_env_filter("abstract=warn,cersei=warn")
            .init();
    }

    // Load config with CLI overrides
    let mut config = config::load();
    apply_cli_overrides(&cli, &mut config);

    // Dispatch
    match &cli.command {
        Some(Commands::Init) => init::run()?,
        Some(Commands::Login { provider }) => {
            login::run_login(provider.as_deref()).await?;
            return Ok(());
        }
        Some(Commands::Logout) => {
            login::run_logout()?;
            return Ok(());
        }
        Some(Commands::Sessions { action }) => match action {
            SessionAction::List => sessions::list(&config)?,
            SessionAction::Show { id } => sessions::show(&config, id)?,
            SessionAction::Rm { id } => sessions::delete(&config, id)?,
        },
        Some(Commands::Config { action }) => match action {
            ConfigAction::Show => {
                println!("{}", toml::to_string_pretty(&config)?);
            }
            ConfigAction::Set { key, value } => {
                config_set(&mut config, key, value)?;
                config::save_to(&config, &config::project_config_path())?;
                println!("Set {} = {}", key, value);
            }
        },
        Some(Commands::Memory { action }) => match action {
            MemoryAction::Show => sessions::show_memory(&config)?,
            MemoryAction::Clear => sessions::clear_memory(&config)?,
        },
        Some(Commands::Mcp { action }) => match action {
            McpAction::Add { name, command } => {
                if command.is_empty() {
                    anyhow::bail!("MCP server command is required");
                }
                config.mcp_servers.push(config::McpServerEntry {
                    name: name.clone(),
                    command: command[0].clone(),
                    args: command[1..].to_vec(),
                    env: Default::default(),
                });
                config::save_to(&config, &config::project_config_path())?;
                println!("Added MCP server: {}", name);
            }
            McpAction::List => {
                if config.mcp_servers.is_empty() {
                    println!("No MCP servers configured.");
                } else {
                    for s in &config.mcp_servers {
                        println!("  {} — {} {}", s.name, s.command, s.args.join(" "));
                    }
                }
            }
            McpAction::Remove { name } => {
                let before = config.mcp_servers.len();
                config.mcp_servers.retain(|s| s.name != *name);
                if config.mcp_servers.len() < before {
                    config::save_to(&config, &config::project_config_path())?;
                    println!("Removed MCP server: {}", name);
                } else {
                    println!("MCP server '{}' not found.", name);
                }
            }
        },
        None => {
            // REPL or single-shot mode
            app::run(cli, config).await?;
        }
    }

    Ok(())
}

fn apply_cli_overrides(cli: &Cli, config: &mut config::AppConfig) {
    if let Some(m) = &cli.model {
        config.model = resolve_model_alias(m);
    }
    if let Some(p) = &cli.provider {
        config.provider = p.clone();
    }
    if cli.fast {
        config.effort = "low".into();
    }
    if cli.max {
        config.effort = "max".into();
    }
    if cli.no_permissions {
        config.permissions_mode = "allow_all".into();
    }
    if let Some(dir) = &cli.directory {
        config.working_dir = std::path::PathBuf::from(dir);
    }
    if !cli.fallback.is_empty() {
        config.fallback_models = cli.fallback.clone();
    }
    if cli.proxy {
        config.proxy.enabled = true;
        config.proxy.force = true;
    }
    if cli.headless {
        config.benchmark_mode = true;
        config.permissions_mode = "allow_all".into();
        config.max_turns = 80;
    }
    if cli.embedding_api {
        config.embedding_api = true;
    }
    if let Some(fmt) = &cli.output_format {
        config.output_format = fmt.clone();
    }
    if let Some(url) = &cli.proxy_url {
        config.proxy.enabled = true;
        config.proxy.url = url.clone();
    }
}

fn resolve_model_alias(alias: &str) -> String {
    match alias {
        "opus" => "anthropic/claude-opus-4-6".into(),
        "sonnet" => "anthropic/claude-sonnet-4-6".into(),
        "haiku" => "anthropic/claude-haiku-4-5".into(),
        "gpt4o" | "4o" => "openai/gpt-4o".into(),
        "gemini" => "google/gemini-2.0-flash".into(),
        "llama" => "groq/llama-3.1-70b-versatile".into(),
        "deepseek" => "deepseek/deepseek-chat".into(),
        "grok" => "xai/grok-2".into(),
        "mistral" => "mistral/mistral-large-latest".into(),
        other => other.into(),
    }
}

fn config_set(config: &mut config::AppConfig, key: &str, value: &str) -> anyhow::Result<()> {
    match key {
        "model" => config.model = value.into(),
        "provider" => config.provider = value.into(),
        "max_turns" => config.max_turns = value.parse()?,
        "max_tokens" => config.max_tokens = value.parse()?,
        "effort" => config.effort = value.into(),
        "output_style" => config.output_style = value.into(),
        "theme" => config.theme = value.into(),
        "auto_compact" => config.auto_compact = value.parse()?,
        "graph_memory" => config.graph_memory = value.parse()?,
        "permissions_mode" => config.permissions_mode = value.into(),
        _ => anyhow::bail!("Unknown config key: {}", key),
    }
    Ok(())
}
