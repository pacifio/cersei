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
use cersei_tools::permissions::AllowAll;
use cersei_types::Message;
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

    let cancel_token = CancellationToken::new();
    let running = Arc::new(AtomicBool::new(false));

    // Install signal handlers
    crate::signals::install(cancel_token.clone(), running.clone())?;

    // Build the initial agent with shared permission mode and TUI permission channel
    let shared_mode = crate::permissions::new_shared_mode();
    let (perm_tx, perm_rx) = crate::permissions::permission_channel();
    let (agent, resolved_model) = build_agent(
        &config.model,
        &config,
        &memory_manager,
        &session_id,
        cancel_token.clone(),
        None,
        Some(shared_mode.clone()),
        Some(perm_tx),
    )?;
    config.model = resolved_model;

    // Show startup banner
    let effort = EffortLevel::from_str(&config.effort);
    // JSON mode: --json flag OR --output-format stream-json
    let json_mode = cli.json || config.output_format == "stream-json";
    if !json_mode {
        print_banner(&config, &session_id, &effort);
    }

    // Dispatch to REPL or single-shot
    // "." means "start interactive in current directory"
    let prompt = cli.prompt.as_deref().filter(|p| *p != ".");
    if let Some(prompt_text) = prompt {
        let prompt_text = prompt_text.to_string();
        repl::run_single_shot(
            agent,
            &prompt_text,
            &theme,
            &session_id,
            &config,
            &memory_manager,
            json_mode,
            running,
            cancel_token,
        )
        .await
    } else if json_mode {
        // JSON mode uses the old REPL (no TUI)
        repl::run_repl(
            agent,
            &theme,
            &session_id,
            &config,
            &memory_manager,
            json_mode,
            running,
            cancel_token.clone(),
        )
        .await
    } else {
        // TUI mode (default interactive)
        crate::tui::run(
            agent,
            &config,
            &memory_manager,
            &session_id,
            cancel_token,
            shared_mode,
            perm_rx,
        )
        .await
    }
}

/// Detect if a local proxy (VibeProxy or compatible) is running.
/// Returns the proxy URL if detected and no direct API key is available.
fn detect_proxy(config: &AppConfig) -> Option<String> {
    if !config.proxy.enabled {
        return None;
    }

    // Only auto-detect if no direct API keys are set (unless --proxy forces it)
    if !config.proxy.force {
        let has_anthropic = std::env::var("ANTHROPIC_API_KEY")
            .ok()
            .filter(|k| !k.is_empty())
            .is_some();
        let has_openai = std::env::var("OPENAI_API_KEY")
            .ok()
            .filter(|k| !k.is_empty())
            .is_some();

        if has_anthropic || has_openai {
            return None; // Direct API keys available, no need for proxy
        }
    }

    // Quick TCP check on proxy port
    let base = config.proxy.url.trim_end_matches("/v1").trim_end_matches('/');
    let addr = base
        .trim_start_matches("http://")
        .trim_start_matches("https://");

    if let Ok(addr) = addr.parse::<std::net::SocketAddr>() {
        if std::net::TcpStream::connect_timeout(&addr, std::time::Duration::from_millis(200)).is_ok() {
            return Some(config.proxy.url.clone());
        }
    } else {
        // Try resolving as host:port
        use std::net::ToSocketAddrs;
        if let Ok(mut addrs) = addr.to_socket_addrs() {
            if let Some(sock_addr) = addrs.next() {
                if std::net::TcpStream::connect_timeout(&sock_addr, std::time::Duration::from_millis(200)).is_ok() {
                    return Some(config.proxy.url.clone());
                }
            }
        }
    }

    None
}

/// Build an agent for a given model string. Reusable for initial build and provider switching.
pub fn build_agent(
    model_string: &str,
    config: &AppConfig,
    memory_manager: &MemoryManager,
    session_id: &str,
    cancel_token: CancellationToken,
    existing_messages: Option<Vec<Message>>,
    shared_mode: Option<crate::permissions::SharedPermissionMode>,
    perm_tx: Option<tokio::sync::mpsc::Sender<crate::permissions::TuiPermissionRequest>>,
) -> anyhow::Result<(cersei::Agent, String)> {
    // Check for proxy (VibeProxy or compatible) before resolving provider
    let (provider, resolved_model) = if let Some(proxy_url) = detect_proxy(config) {
        let model = if model_string == "auto" { "claude-sonnet-4-6" } else { model_string };
        let provider = cersei_provider::OpenAi::builder()
            .api_key("vibeproxy")
            .base_url(&proxy_url)
            .model(model)
            .build()
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        (Box::new(provider) as Box<dyn cersei_provider::Provider>, format!("{model} via proxy"))
    } else {
        cersei_provider::from_model_string(model_string)
            .map_err(|e| anyhow::anyhow!("{e}"))?
    };

    let system_prompt = prompt::build_cli_system_prompt(config, memory_manager, &resolved_model);
    let effort = EffortLevel::from_str(&config.effort);

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

    // Build tool list: built-in + LSP
    let mut tools = cersei_tools::all();
    tools.push(Box::new(cersei_tools::lsp_tool::LspTool::new(&config.working_dir)));

    let mut builder = cersei::Agent::builder()
        .provider(provider)
        .tools(tools)
        .system_prompt(system_prompt)
        .model(&resolved_model)
        .max_turns(config.max_turns)
        .max_tokens(config.max_tokens)
        .auto_compact(config.auto_compact)
        .enable_broadcast(512)
        .cancel_token(cancel_token)
        .session_id(session_id)
        .working_dir(&config.working_dir)
        .benchmark_mode(config.benchmark_mode);

    // Permission policy
    if config.permissions_mode == "allow_all" {
        builder = builder.permission_policy(AllowAll);
    } else if let (Some(mode), Some(tx)) = (shared_mode, perm_tx) {
        // TUI mode: use channel-based permission flow (no stdin conflict)
        builder = builder.permission_policy(
            crate::permissions::TuiPermissionPolicy::new(mode, tx)
        );
    } else {
        builder = builder.permission_policy(CliPermissionPolicy::new());
    }

    // Effort level
    let budget = effort.thinking_budget_tokens();
    builder = builder.thinking_budget(budget);
    if let Some(temp) = effort.temperature() {
        builder = builder.temperature(temp);
    }

    // MCP servers
    for mcp in mcp_configs {
        builder = builder.mcp_server(mcp);
    }

    // Inject existing messages (for provider switching)
    if let Some(msgs) = existing_messages {
        builder = builder.with_messages(msgs);
    }

    let agent = builder.build()?;
    Ok((agent, resolved_model))
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
