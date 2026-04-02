//! REPL loop and single-shot execution.
//!
//! Drives the agent via `run_stream()`, consuming `AgentEvent`s and dispatching
//! them to the renderer, status line, and permission handler.

use crate::commands;
use crate::config::AppConfig;
use crate::input::InputReader;
use crate::render::{self, StreamRenderer};
use crate::status::StatusLine;
use crate::theme::Theme;
use cersei::Agent;
use cersei::events::AgentEvent;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

/// Run the interactive REPL.
pub async fn run_repl(
    agent: &Agent,
    theme: &Theme,
    session_id: &str,
    config: &AppConfig,
    json_mode: bool,
    running: Arc<AtomicBool>,
    _cancel_token: CancellationToken,
) -> anyhow::Result<()> {
    let mut input_reader = InputReader::new()?;
    let mut renderer = StreamRenderer::new(theme, json_mode);
    let mut status = StatusLine::new(theme, &config.model, session_id, !json_mode);
    let mut cmd_registry = commands::CommandRegistry::new();
    let mut is_first_turn = true;

    loop {
        let prompt_str = if is_first_turn {
            "\x1b[36m> \x1b[0m"
        } else {
            "\x1b[36m> \x1b[0m"
        };

        let input = match input_reader.readline(prompt_str) {
            Some(line) => line,
            None => {
                // EOF or Ctrl-D
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
                    cmd_registry
                        .execute(cmd, args, config, session_id)
                        .await;
                    continue;
                }
            }
        }

        // Run agent
        running.store(true, Ordering::Relaxed);
        let result = run_agent_streaming(
            agent,
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
            Err(e) => {
                renderer.error(&e.to_string());
            }
        }
    }

    eprintln!("\x1b[90mSession saved.\x1b[0m");
    Ok(())
}

/// Run a single prompt (non-interactive).
pub async fn run_single_shot(
    agent: &Agent,
    prompt: &str,
    theme: &Theme,
    session_id: &str,
    config: &AppConfig,
    json_mode: bool,
    running: Arc<AtomicBool>,
) -> anyhow::Result<()> {
    let mut renderer = StreamRenderer::new(theme, json_mode);
    let mut status = StatusLine::new(theme, &config.model, session_id, false);

    running.store(true, Ordering::Relaxed);
    let result = run_agent_streaming(agent, prompt, &mut renderer, &mut status, json_mode, true).await;
    running.store(false, Ordering::Relaxed);

    match result {
        Ok(_) => Ok(()),
        Err(e) => {
            renderer.error(&e.to_string());
            Err(e)
        }
    }
}

/// Core event loop: stream agent events and render them.
async fn run_agent_streaming(
    agent: &Agent,
    prompt: &str,
    renderer: &mut StreamRenderer,
    status: &mut StatusLine,
    json_mode: bool,
    is_first: bool,
) -> anyhow::Result<()> {
    let mut stream = if is_first {
        agent.run_stream(prompt)
    } else {
        // For multi-turn, we use run_stream which internally accesses shared messages
        agent.run_stream(prompt)
    };

    while let Some(event) = stream.next().await {
        // JSON mode: print raw events
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
            AgentEvent::PermissionRequired(_request) => {
                // The interactive permission policy handles this synchronously
                // via the PermissionPolicy trait. The stream will get the response
                // from the policy's check() method.
                // If using StreamDeferredPolicy, we'd respond here:
                // stream.respond_permission(request.id, decision);
            }
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
                renderer.error(&msg);
                break;
            }
            AgentEvent::Complete(_output) => {
                renderer.complete();
                break;
            }
            _ => {
                // Other events: Status, HookFired, etc. — ignore in CLI
            }
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
