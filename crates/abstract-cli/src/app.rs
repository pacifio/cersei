//! Application state, agent construction, and lifecycle management.

use crate::config::AppConfig;
use crate::permissions::CliPermissionPolicy;
use crate::prompt;
use crate::repl;
use crate::sessions;
use crate::theme::Theme;
use crate::Cli;

use cersei_agent::effort::EffortLevel;
use cersei_memory::manager::MemoryManager;
use cersei_mcp::McpServerConfig;
use cersei_provider::{Anthropic, OpenAi};
use cersei_tools::permissions::AllowAll;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

/// Run the application (REPL or single-shot).
pub async fn run(cli: Cli, mut config: AppConfig) -> anyhow::Result<()> {
    let theme = Theme::from_name(&config.theme);

    // Resolve or create session ID
    let session_id = if let Some(ref resume) = cli.resume {
        if resume == "last" {
            sessions::last_session_id(&config)
                .ok_or_else(|| anyhow::anyhow!("No previous session found"))?
        } else {
            resume.clone()
        }
    } else {
        uuid::Uuid::new_v4().to_string()
    };

    // Build memory manager with graph memory
    let memory_manager = build_memory_manager(&config)?;

    // Auto-fix model for provider
    let provider_name = resolve_provider_name(&config);
    if provider_name == "openai" && config.model.contains("claude") {
        config.model = "gpt-4o".into();
    }

    // Build system prompt
    let system_prompt = prompt::build_cli_system_prompt(&config, &memory_manager);

    // Resolve effort
    let effort = EffortLevel::from_str(&config.effort);

    // Build the agent
    let provider = resolve_provider(&config)?;
    let cancel_token = CancellationToken::new();
    let running = Arc::new(AtomicBool::new(false));

    // Install signal handlers
    crate::signals::install(cancel_token.clone(), running.clone())?;

    // MCP servers
    let mcp_configs: Vec<McpServerConfig> = config
        .mcp_servers
        .iter()
        .map(|s| {
            let args_ref: Vec<&str> = s.args.iter().map(|a| a.as_str()).collect();
            let mut cfg = McpServerConfig::stdio(&s.name, &s.command, &args_ref);
            cfg.env = s.env.clone();
            cfg
        })
        .collect();

    // Build agent
    let mut builder = cersei::Agent::builder()
        .provider(provider)
        .tools(cersei_tools::all())
        .system_prompt(system_prompt)
        .model(&config.model)
        .max_turns(config.max_turns)
        .max_tokens(config.max_tokens)
        .auto_compact(config.auto_compact)
        .enable_broadcast(512)
        .cancel_token(cancel_token.clone());

    // Apply permission policy
    if config.permissions_mode == "allow_all" {
        builder = builder.permission_policy(AllowAll);
    } else {
        builder = builder.permission_policy(CliPermissionPolicy::new());
    }

    // Apply effort level
    let budget = effort.thinking_budget_tokens();
    builder = builder.thinking_budget(budget);
    if let Some(temp) = effort.temperature() {
        builder = builder.temperature(temp);
    }

    // Apply session
    builder = builder.session_id(&session_id);

    // Apply working dir
    builder = builder.working_dir(&config.working_dir);

    // Add MCP servers
    for mcp in mcp_configs {
        builder = builder.mcp_server(mcp);
    }

    let agent = builder.build()?;

    // Show startup banner
    if !cli.json {
        print_banner(&config, &session_id, &effort);
    }

    // Dispatch to REPL or single-shot
    if let Some(prompt_text) = &cli.prompt {
        // Single-shot mode
        repl::run_single_shot(
            &agent,
            prompt_text,
            &theme,
            &session_id,
            &config,
            cli.json,
            running,
        )
        .await
    } else {
        // REPL mode
        repl::run_repl(
            &agent,
            &theme,
            &session_id,
            &config,
            cli.json,
            running,
            cancel_token,
        )
        .await
    }
}

fn resolve_provider_name(config: &AppConfig) -> &str {
    match config.provider.as_str() {
        "openai" => "openai",
        "anthropic" => "anthropic",
        _ => {
            if std::env::var("OPENAI_API_KEY").ok().filter(|k| !k.is_empty()).is_some() {
                "openai"
            } else {
                "anthropic"
            }
        }
    }
}

fn resolve_provider(
    config: &AppConfig,
) -> anyhow::Result<Box<dyn cersei_provider::Provider>> {
    match config.provider.as_str() {
        "openai" => {
            let key = std::env::var("OPENAI_API_KEY")
                .ok()
                .filter(|k| !k.is_empty())
                .ok_or_else(|| anyhow::anyhow!("OPENAI_API_KEY not set"))?;
            Ok(Box::new(OpenAi::new(cersei_provider::Auth::ApiKey(key))))
        }
        "anthropic" => {
            let key = std::env::var("ANTHROPIC_API_KEY")
                .or_else(|_| std::env::var("ANTHROPIC_KEY"))
                .ok()
                .filter(|k| !k.is_empty())
                .ok_or_else(|| anyhow::anyhow!("ANTHROPIC_API_KEY not set"))?;
            Ok(Box::new(Anthropic::new(cersei_provider::Auth::ApiKey(key))))
        }
        _ => {
            // Auto-detect: try OpenAI first (user has key), then Anthropic
            if let Ok(key) = std::env::var("OPENAI_API_KEY") {
                if !key.is_empty() {
                    return Ok(Box::new(OpenAi::new(cersei_provider::Auth::ApiKey(key))));
                }
            }
            if let Ok(key) = std::env::var("ANTHROPIC_API_KEY")
                .or_else(|_| std::env::var("ANTHROPIC_KEY"))
            {
                if !key.is_empty() {
                    return Ok(Box::new(Anthropic::new(cersei_provider::Auth::ApiKey(key))));
                }
            }
            Err(anyhow::anyhow!(
                "No API key found.\n\nSet OPENAI_API_KEY or ANTHROPIC_API_KEY environment variable."
            ))
        }
    }
}

fn build_memory_manager(config: &AppConfig) -> anyhow::Result<MemoryManager> {
    let mut mm = MemoryManager::new(&config.working_dir);

    #[cfg(feature = "graph")]
    if config.graph_memory {
        let graph_path = crate::config::graph_db_path();
        if let Some(parent) = graph_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        mm = mm
            .with_graph(&graph_path)
            .map_err(|e| anyhow::anyhow!("Failed to open graph memory: {e}"))?;
    }

    Ok(mm)
}

fn print_banner(config: &AppConfig, session_id: &str, effort: &EffortLevel) {
    let short_id = if session_id.len() > 8 {
        &session_id[..8]
    } else {
        session_id
    };

    eprintln!(
        "\x1b[36;1mabstract\x1b[0m \x1b[90mv{} | {} | {:?} effort | session {}\x1b[0m",
        env!("CARGO_PKG_VERSION"),
        config.model,
        effort,
        short_id,
    );
    eprintln!("\x1b[90mType /help for commands, Ctrl+C to cancel, Ctrl+C×2 to exit\x1b[0m\n");
}
