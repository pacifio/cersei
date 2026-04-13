//! REPL loop and single-shot execution with provider continuity.
//!
//! On provider errors (rate limits, overloaded, etc.), shows an interactive
//! prompt letting the user retry, switch providers, wait, or skip.

use crate::app;
use crate::commands;
use crate::config::AppConfig;
use crate::input::InputReader;
use crate::render::{self, StreamRenderer};
use crate::status::StatusLine;
use crate::theme::Theme;
use cersei::Agent;
use cersei::events::AgentEvent;
use cersei_memory::manager::MemoryManager;
use cersei_types::Role;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

// ─── Recovery prompt ───────────────────────────────────────────────────────

enum Recovery {
    Retry,
    Switch(String),
    Wait(u64),
    Skip,
}

fn is_provider_error(msg: &str) -> bool {
    let lower = msg.to_lowercase();
    lower.contains("429")
        || lower.contains("529")
        || lower.contains("503")
        || lower.contains("rate limit")
        || lower.contains("overloaded")
        || lower.contains("capacity")
        || lower.contains("too many requests")
}

fn prompt_recovery(current_model: &str, config: &AppConfig) -> Recovery {
    // Build options list
    let mut options: Vec<(String, String)> = Vec::new(); // (key, model_string)

    // Configured fallbacks
    for (i, model) in config.fallback_models.iter().enumerate() {
        options.push((format!("{}", i + 1), model.clone()));
    }

    // Other available providers not already listed
    let available = cersei_provider::router::available_providers();
    for entry in &available {
        let model_str = format!("{}/{}", entry.id, entry.default_model);
        if model_str != current_model
            && !config.fallback_models.contains(&model_str)
            && !config.fallback_models.iter().any(|f| f.starts_with(entry.id))
        {
            let key = format!("{}", options.len() + 1);
            options.push((key, model_str));
        }
    }

    eprintln!();
    eprintln!("  \x1b[33mOptions:\x1b[0m");
    eprintln!("    \x1b[36m[r]\x1b[0m Retry with {current_model}");
    for (key, model) in &options {
        eprintln!("    \x1b[36m[{key}]\x1b[0m Switch to {model}");
    }
    eprintln!("    \x1b[36m[w]\x1b[0m Wait 30s then retry");
    eprintln!("    \x1b[90m[Enter]\x1b[0m Skip, return to prompt");
    eprint!("\n  Choice: ");
    let _ = std::io::Write::flush(&mut std::io::stderr());

    // Read single keypress
    let choice = read_choice();

    match choice.as_str() {
        "r" | "R" => Recovery::Retry,
        "w" | "W" => Recovery::Wait(30),
        "" => Recovery::Skip,
        key => {
            // Check if it's a numbered option
            if let Some((_, model)) = options.iter().find(|(k, _)| k == key) {
                Recovery::Switch(model.clone())
            } else {
                Recovery::Skip
            }
        }
    }
}

fn read_choice() -> String {
    use crossterm::event::{self, Event, KeyCode, KeyEvent};
    use crossterm::terminal;

    if terminal::enable_raw_mode().is_ok() {
        let result = loop {
            if let Ok(Event::Key(KeyEvent { code, .. })) = event::read() {
                break match code {
                    KeyCode::Char(c) => c.to_string(),
                    KeyCode::Enter => String::new(),
                    KeyCode::Esc => String::new(),
                    _ => continue,
                };
            }
        };
        let _ = terminal::disable_raw_mode();
        eprint!("\n");
        result
    } else {
        let mut input = String::new();
        let _ = std::io::stdin().read_line(&mut input);
        input.trim().to_string()
    }
}

// ─── REPL ──────────────────────────────────────────────────────────────────

/// Run the interactive REPL.
pub async fn run_repl(
    agent: Agent,
    theme: &Theme,
    session_id: &str,
    config: &AppConfig,
    memory_manager: &MemoryManager,
    json_mode: bool,
    running: Arc<AtomicBool>,
    cancel_token: CancellationToken,
) -> anyhow::Result<()> {
    let mut input_reader = InputReader::new()?;
    let mut renderer = StreamRenderer::new(theme, json_mode);
    let mut status = StatusLine::new(theme, &config.model, session_id, !json_mode);
    let mut cmd_registry = commands::CommandRegistry::new();
    let mut is_first_turn = true;
    let mut current_model = config.model.clone();
    let mut agent = Arc::new(agent);

    loop {
        let prompt_str = "\x1b[36m> \x1b[0m";

        let input = match input_reader.readline(prompt_str) {
            Some(line) => line,
            None => {
                input_reader.save_history();
                break;
            }
        };

        if input.is_empty() {
            continue;
        }

        // Handle slash commands
        if input.starts_with('/') {
            let (cmd, args) = parse_command(&input);
            match cmd {
                "exit" | "quit" | "q" => {
                    input_reader.save_history();
                    break;
                }
                _ => {
                    cmd_registry.execute(cmd, args, config, session_id).await;
                    continue;
                }
            }
        }

        // Run agent with retry/recovery loop
        let mut should_retry = true;
        while should_retry {
            should_retry = false;

            running.store(true, Ordering::Relaxed);
            let result = run_agent_streaming(
                &agent,
                &input,
                &mut renderer,
                &mut status,
                json_mode,
                is_first_turn,
            )
            .await;
            running.store(false, Ordering::Relaxed);

            match result {
                Ok(_) => {
                    is_first_turn = false;
                }
                Err(err_msg) => {
                    renderer.error(&err_msg);

                    if is_provider_error(&err_msg) {
                        match prompt_recovery(&current_model, config) {
                            Recovery::Retry => {
                                should_retry = true;
                            }
                            Recovery::Switch(new_model) => {
                                // Snapshot messages, pop the last user msg (runner already added it)
                                let mut msgs = agent.messages();
                                if msgs.last().map(|m| m.role == Role::User).unwrap_or(false) {
                                    msgs.pop();
                                }

                                match app::build_agent(
                                    &new_model,
                                    config,
                                    memory_manager,
                                    session_id,
                                    cancel_token.clone(),
                                    Some(msgs),
                                    None,
                                    None,
                                ) {
                                    Ok((new_agent, resolved)) => {
                                        agent = Arc::new(new_agent);
                                        current_model = format!("{}/{}", new_model.split('/').next().unwrap_or(""), &resolved);
                                        if current_model.starts_with('/') {
                                            current_model = resolved.clone();
                                        }
                                        renderer.model_switched(&resolved);
                                        should_retry = true;
                                    }
                                    Err(e) => {
                                        renderer.error(&format!("Switch failed: {e}"));
                                    }
                                }
                            }
                            Recovery::Wait(secs) => {
                                eprintln!("\x1b[90m  Waiting {secs}s...\x1b[0m");
                                tokio::time::sleep(Duration::from_secs(secs)).await;
                                should_retry = true;
                            }
                            Recovery::Skip => {}
                        }
                    }
                }
            }
        }
    }

    eprintln!("\x1b[90mSession saved.\x1b[0m");
    Ok(())
}

/// Run a single prompt (non-interactive).
pub async fn run_single_shot(
    agent: Agent,
    prompt: &str,
    theme: &Theme,
    session_id: &str,
    config: &AppConfig,
    memory_manager: &MemoryManager,
    json_mode: bool,
    running: Arc<AtomicBool>,
    _cancel_token: CancellationToken,
) -> anyhow::Result<()> {
    let mut renderer = StreamRenderer::new(theme, json_mode);
    let mut status = StatusLine::new(theme, &config.model, session_id, false);
    let agent = Arc::new(agent);

    running.store(true, Ordering::Relaxed);
    let result = run_agent_streaming(&agent, prompt, &mut renderer, &mut status, json_mode, true).await;
    running.store(false, Ordering::Relaxed);

    match result {
        Ok(_) => Ok(()),
        Err(msg) => {
            renderer.error(&msg);
            Err(anyhow::anyhow!("{msg}"))
        }
    }
}

/// Core event loop: stream agent events and render them.
/// Returns Ok(()) on success or Err(error_message) on failure.
async fn run_agent_streaming(
    agent: &Arc<Agent>,
    prompt: &str,
    renderer: &mut StreamRenderer,
    status: &mut StatusLine,
    json_mode: bool,
    _is_first: bool,
) -> Result<(), String> {
    let mut stream = agent.run_stream(prompt);

    while let Some(event) = stream.next().await {
        if json_mode {
            render::print_json_event(&event);
        }

        match event {
            AgentEvent::TextDelta(text) => {
                renderer.push_text(&text);
            }
            AgentEvent::ThinkingDelta(text) => {
                renderer.push_thinking(&text);
            }
            AgentEvent::ToolStart { name, id: _, input } => {
                renderer.tool_start(&name, &input);
            }
            AgentEvent::ToolEnd {
                name,
                id: _,
                result,
                is_error,
                duration,
            } => {
                renderer.tool_end(&name, &result, is_error, duration);
            }
            AgentEvent::PermissionRequired(_request) => {}
            AgentEvent::CostUpdate {
                cumulative_cost,
                input_tokens,
                output_tokens,
                ..
            } => {
                status.update_cost(input_tokens, output_tokens, cumulative_cost);
            }
            AgentEvent::TurnComplete { usage, .. } => {
                if let Some(cost) = usage.cost_usd {
                    status.update_cost(usage.input_tokens, usage.output_tokens, cost);
                }
            }
            AgentEvent::TokenWarning { pct_used, .. } => {
                status.update_context(pct_used);
            }
            AgentEvent::CompactStart { reason, .. } => {
                if !json_mode {
                    eprintln!("\x1b[90m  Compacting context ({:?})...\x1b[0m", reason);
                }
            }
            AgentEvent::CompactEnd {
                messages_after,
                tokens_freed,
            } => {
                if !json_mode {
                    eprintln!(
                        "\x1b[90m  Compacted: {} messages, ~{} tokens freed\x1b[0m",
                        messages_after, tokens_freed
                    );
                }
            }
            AgentEvent::SessionLoaded {
                session_id,
                message_count,
            } => {
                if !json_mode {
                    eprintln!(
                        "\x1b[90m  Resumed session {} ({} messages)\x1b[0m",
                        &session_id[..8.min(session_id.len())],
                        message_count
                    );
                }
            }
            AgentEvent::SubAgentSpawned {
                agent_id, prompt, ..
            } => {
                if !json_mode {
                    let preview: String = prompt.chars().take(60).collect();
                    eprintln!("\x1b[90m  Sub-agent {}: {preview}...\x1b[0m", &agent_id[..8.min(agent_id.len())]);
                }
            }
            AgentEvent::Error(msg) => {
                renderer.flush();
                return Err(msg);
            }
            AgentEvent::Complete(_output) => {
                renderer.complete();
                return Ok(());
            }
            _ => {}
        }
    }

    Ok(())
}

fn parse_command(input: &str) -> (&str, &str) {
    let input = input.trim_start_matches('/');
    if let Some(space) = input.find(char::is_whitespace) {
        (&input[..space], input[space..].trim())
    } else {
        (input, "")
    }
}
