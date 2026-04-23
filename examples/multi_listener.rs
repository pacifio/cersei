//! # Multi-Listener Dashboard
//!
//! Demonstrates the broadcast channel for multiple concurrent event consumers:
//! - A TUI-style dashboard that tracks tool calls and cost
//! - A JSON log writer
//! - The built-in ConsoleReporter
//!
//! ```bash
//! ANTHROPIC_API_KEY=sk-ant-... cargo run --example multi_listener
//! ```

use cersei::events::AgentEvent;
use cersei::prelude::*;
use cersei::reporters::{AgentMetrics, CollectorReporter, ConsoleReporter, MetricsReporter};
use std::sync::Arc;
use std::time::Duration;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Build agent with broadcast + multiple reporters
    let collector = Arc::new(CollectorReporter::new());

    let agent = Agent::builder()
        .provider(Anthropic::from_env()?)
        .tools(cersei::tools::coding())
        .system_prompt("You are a helpful coding assistant. Be concise.")
        .max_turns(5)
        .permission_policy(AllowAll)
        .working_dir(".")
        .enable_broadcast(512)
        .reporter(ConsoleReporter { verbose: true })
        .reporter(MetricsReporter::new(
            Duration::from_secs(5),
            |metrics: AgentMetrics| {
                eprintln!(
                    "\n\x1b[36m[metrics] turns={} tools={} cost=${:.4} tokens={}in/{}out\x1b[0m",
                    metrics.total_turns,
                    metrics.total_tool_calls,
                    metrics.total_cost_usd,
                    metrics.total_input_tokens,
                    metrics.total_output_tokens,
                );
                if !metrics.tool_call_histogram.is_empty() {
                    eprint!("  tools used: ");
                    for (name, count) in &metrics.tool_call_histogram {
                        eprint!("{}×{} ", name, count);
                    }
                    eprintln!();
                }
            },
        ))
        .build()?;

    // Spawn listener 1: Dashboard summary
    if let Some(mut rx) = agent.subscribe() {
        tokio::spawn(async move {
            let mut tool_count = 0u32;
            let mut last_cost = 0.0f64;
            while let Ok(event) = rx.recv().await {
                match &event {
                    AgentEvent::ToolStart { name, .. } => {
                        tool_count += 1;
                        eprintln!("\x1b[35m[dashboard] [{tool_count}] → {name}\x1b[0m");
                    }
                    AgentEvent::CostUpdate {
                        cumulative_cost, ..
                    } => {
                        last_cost = *cumulative_cost;
                    }
                    AgentEvent::TurnComplete { turn, .. } => {
                        eprintln!(
                            "\x1b[35m[dashboard] turn {turn} done — ${last_cost:.4} total\x1b[0m"
                        );
                    }
                    _ => {}
                }
            }
            eprintln!("\x1b[35m[dashboard] Final: {tool_count} tool calls, ${last_cost:.4}\x1b[0m");
        });
    }

    // Spawn listener 2: Event counter
    if let Some(mut rx) = agent.subscribe() {
        tokio::spawn(async move {
            let mut event_count = 0u64;
            while let Ok(_event) = rx.recv().await {
                event_count += 1;
            }
            eprintln!("\x1b[34m[counter] Total events: {event_count}\x1b[0m");
        });
    }

    // Run the agent
    let output = agent
        .run("List files in the current directory, then read the Cargo.toml and summarize it.")
        .await?;

    // Give listeners a moment to finish
    tokio::time::sleep(Duration::from_millis(100)).await;

    println!("\n─── Final Output ───");
    println!("{}", output.text());

    Ok(())
}
