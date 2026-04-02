//! # Usage Report
//!
//! Runs an agent through a multi-turn coding task and produces a detailed
//! usage report: tokens consumed per turn, cumulative cost, tool call
//! breakdown, and billing estimates.
//!
//! Uses a simulated provider with realistic Claude token counts so the
//! usage tracking pipeline can be verified end-to-end without an API key.
//!
//! ```bash
//! cargo run --example usage_report --release
//! ```

use cersei::prelude::*;
use cersei::events::AgentEvent;
use cersei::provider::{CompletionStream, ProviderCapabilities, ProviderOptions};
use cersei::reporters::{AgentMetrics, MetricsReporter, Reporter};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

// ─── Claude pricing (as of 2025) ────────────────────────────────────────────

// Claude Sonnet 4.6 pricing
const SONNET_INPUT_PER_MTOK: f64 = 3.00;   // $3/M input tokens
const SONNET_OUTPUT_PER_MTOK: f64 = 15.00;  // $15/M output tokens
const SONNET_CACHE_WRITE_PER_MTOK: f64 = 3.75;
const SONNET_CACHE_READ_PER_MTOK: f64 = 0.30;

// Claude Opus 4.6 pricing
const OPUS_INPUT_PER_MTOK: f64 = 15.00;
const OPUS_OUTPUT_PER_MTOK: f64 = 75.00;

// Claude Haiku 4.5 pricing
const HAIKU_INPUT_PER_MTOK: f64 = 0.80;
const HAIKU_OUTPUT_PER_MTOK: f64 = 4.00;

fn compute_cost(model: &str, input_tokens: u64, output_tokens: u64) -> f64 {
    let (input_rate, output_rate) = if model.contains("opus") {
        (OPUS_INPUT_PER_MTOK, OPUS_OUTPUT_PER_MTOK)
    } else if model.contains("haiku") {
        (HAIKU_INPUT_PER_MTOK, HAIKU_OUTPUT_PER_MTOK)
    } else {
        (SONNET_INPUT_PER_MTOK, SONNET_OUTPUT_PER_MTOK)
    };

    (input_tokens as f64 / 1_000_000.0) * input_rate
        + (output_tokens as f64 / 1_000_000.0) * output_rate
}

// ─── Simulated Claude provider ──────────────────────────────────────────────

/// A provider that simulates Claude responses with realistic token counts.
/// Produces tool calls on turn 1 and 2, then a final text response on turn 3.
struct SimulatedClaude {
    model: String,
    turn: Arc<std::sync::atomic::AtomicU32>,
}

impl SimulatedClaude {
    fn new(model: &str) -> Self {
        Self {
            model: model.to_string(),
            turn: Arc::new(std::sync::atomic::AtomicU32::new(0)),
        }
    }
}

#[async_trait]
impl Provider for SimulatedClaude {
    fn name(&self) -> &str { "simulated-claude" }
    fn context_window(&self, _model: &str) -> u64 { 200_000 }
    fn capabilities(&self, _model: &str) -> ProviderCapabilities {
        ProviderCapabilities {
            streaming: true, tool_use: true, vision: true,
            thinking: true, system_prompt: true, caching: true,
        }
    }

    async fn complete(&self, request: CompletionRequest) -> cersei_types::Result<CompletionStream> {
        let turn = self.turn.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let model = self.model.clone();
        let msg_count = request.messages.len();

        // Simulate realistic token counts based on conversation size
        let base_input = 1200 + (msg_count as u64 * 350); // system prompt + growing context
        let cache_read = if turn > 0 { base_input / 3 } else { 0 }; // prompt caching kicks in

        let (tx, rx) = mpsc::channel(64);

        tokio::spawn(async move {
            // Small delay to simulate network latency
            tokio::time::sleep(Duration::from_millis(50)).await;

            let _ = tx.send(StreamEvent::MessageStart {
                id: format!("msg_{}", uuid::Uuid::new_v4()),
                model: model.clone(),
            }).await;

            match turn {
                0 => {
                    // Turn 1: model reads a file (tool call)
                    let _ = tx.send(StreamEvent::ContentBlockStart { index: 0, block_type: "thinking".into(), id: None, name: None }).await;
                    let _ = tx.send(StreamEvent::ThinkingDelta { index: 0, thinking: "Let me look at the project structure first...".into() }).await;
                    let _ = tx.send(StreamEvent::ContentBlockStop { index: 0 }).await;

                    let _ = tx.send(StreamEvent::ContentBlockStart { index: 1, block_type: "text".into(), id: None, name: None }).await;
                    let _ = tx.send(StreamEvent::TextDelta { index: 1, text: "I'll start by examining the project structure.".into() }).await;
                    let _ = tx.send(StreamEvent::ContentBlockStop { index: 1 }).await;

                    // Tool use: Glob
                    let _ = tx.send(StreamEvent::ContentBlockStart { index: 2, block_type: "tool_use".into(), id: None, name: None }).await;
                    let _ = tx.send(StreamEvent::InputJsonDelta { index: 2, partial_json: r#"{"pattern": "**/*.rs"}"#.into() }).await;
                    let _ = tx.send(StreamEvent::ContentBlockStop { index: 2 }).await;

                    let output_tokens = 185;
                    let cost = compute_cost(&model, base_input, output_tokens);
                    let _ = tx.send(StreamEvent::MessageDelta {
                        stop_reason: Some(StopReason::ToolUse),
                        usage: Some(Usage {
                            input_tokens: base_input - cache_read,
                            output_tokens,
                            total_tokens: base_input + output_tokens,
                            cost_usd: Some(cost),
                            provider_usage: serde_json::json!({
                                "cache_creation_input_tokens": 800,
                                "cache_read_input_tokens": cache_read,
                            }),
                        }),
                    }).await;
                }
                1 => {
                    // Turn 2: model reads a specific file (another tool call)
                    let _ = tx.send(StreamEvent::ContentBlockStart { index: 0, block_type: "text".into(), id: None, name: None }).await;
                    let _ = tx.send(StreamEvent::TextDelta { index: 0, text: "Let me read the main source file.".into() }).await;
                    let _ = tx.send(StreamEvent::ContentBlockStop { index: 0 }).await;

                    let _ = tx.send(StreamEvent::ContentBlockStart { index: 1, block_type: "tool_use".into(), id: None, name: None }).await;
                    let _ = tx.send(StreamEvent::InputJsonDelta { index: 1, partial_json: r#"{"file_path": "src/main.rs"}"#.into() }).await;
                    let _ = tx.send(StreamEvent::ContentBlockStop { index: 1 }).await;

                    let output_tokens = 142;
                    let cost = compute_cost(&model, base_input, output_tokens);
                    let _ = tx.send(StreamEvent::MessageDelta {
                        stop_reason: Some(StopReason::ToolUse),
                        usage: Some(Usage {
                            input_tokens: base_input - cache_read,
                            output_tokens,
                            total_tokens: base_input + output_tokens,
                            cost_usd: Some(cost),
                            provider_usage: serde_json::json!({
                                "cache_creation_input_tokens": 0,
                                "cache_read_input_tokens": cache_read,
                            }),
                        }),
                    }).await;
                }
                _ => {
                    // Turn 3: final response (end_turn)
                    let _ = tx.send(StreamEvent::ContentBlockStart { index: 0, block_type: "thinking".into(), id: None, name: None }).await;
                    let _ = tx.send(StreamEvent::ThinkingDelta { index: 0, thinking: "I've analyzed the project. Let me summarize my findings...".into() }).await;
                    let _ = tx.send(StreamEvent::ContentBlockStop { index: 0 }).await;

                    let _ = tx.send(StreamEvent::ContentBlockStart { index: 1, block_type: "text".into(), id: None, name: None }).await;
                    let response = "Based on my analysis of the project:\n\n\
                        - **Structure**: 9 crates in a Cargo workspace\n\
                        - **Total files**: 24 Rust source files\n\
                        - **Lines of code**: ~3,200 lines\n\
                        - **Key crate**: `cersei-agent` (agent builder + agentic loop)\n\
                        - **Architecture**: Provider-agnostic with trait-based extensibility\n\n\
                        The project is well-organized with clean separation of concerns.";
                    for chunk in response.as_bytes().chunks(40) {
                        let text = String::from_utf8_lossy(chunk).to_string();
                        let _ = tx.send(StreamEvent::TextDelta { index: 1, text }).await;
                        tokio::time::sleep(Duration::from_millis(5)).await;
                    }
                    let _ = tx.send(StreamEvent::ContentBlockStop { index: 1 }).await;

                    let output_tokens = 387;
                    let cost = compute_cost(&model, base_input, output_tokens);
                    let _ = tx.send(StreamEvent::MessageDelta {
                        stop_reason: Some(StopReason::EndTurn),
                        usage: Some(Usage {
                            input_tokens: base_input - cache_read,
                            output_tokens,
                            total_tokens: base_input + output_tokens,
                            cost_usd: Some(cost),
                            provider_usage: serde_json::json!({
                                "cache_creation_input_tokens": 0,
                                "cache_read_input_tokens": cache_read,
                            }),
                        }),
                    }).await;
                }
            }

            let _ = tx.send(StreamEvent::MessageStop).await;
        });

        Ok(CompletionStream::new(rx))
    }
}

// ─── Usage tracker ──────────────────────────────────────────────────────────

#[derive(Clone)]
struct UsageTracker {
    turns: Arc<parking_lot::Mutex<Vec<TurnUsage>>>,
    tool_calls: Arc<parking_lot::Mutex<Vec<ToolCallInfo>>>,
}

#[derive(Clone, Debug)]
struct TurnUsage {
    turn: u32,
    input_tokens: u64,
    output_tokens: u64,
    cost_usd: f64,
    stop_reason: String,
}

#[derive(Clone, Debug)]
struct ToolCallInfo {
    turn: u32,
    name: String,
    duration_ms: f64,
    is_error: bool,
}

impl UsageTracker {
    fn new() -> Self {
        Self {
            turns: Arc::new(parking_lot::Mutex::new(Vec::new())),
            tool_calls: Arc::new(parking_lot::Mutex::new(Vec::new())),
        }
    }
}

#[async_trait]
impl Reporter for UsageTracker {
    async fn on_event(&self, event: &AgentEvent) {
        match event {
            AgentEvent::TurnComplete { turn, stop_reason, usage, .. } => {
                self.turns.lock().push(TurnUsage {
                    turn: *turn,
                    input_tokens: usage.input_tokens,
                    output_tokens: usage.output_tokens,
                    cost_usd: usage.cost_usd.unwrap_or(0.0),
                    stop_reason: format!("{:?}", stop_reason),
                });
            }
            AgentEvent::ToolEnd { name, duration, is_error, .. } => {
                let turn = self.turns.lock().len() as u32 + 1;
                self.tool_calls.lock().push(ToolCallInfo {
                    turn,
                    name: name.clone(),
                    duration_ms: duration.as_secs_f64() * 1000.0,
                    is_error: *is_error,
                });
            }
            _ => {}
        }
    }
}

// ─── Main ────────────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let models = ["claude-sonnet-4-6", "claude-opus-4-6", "claude-haiku-4-5-20251001"];
    let active_model = models[0]; // Sonnet for this run

    let tracker = UsageTracker::new();
    let tracker_ref = tracker.clone(); // clone shares the Arc'd internals
    let start = Instant::now();

    let agent = Agent::builder()
        .provider(SimulatedClaude::new(active_model))
        .tools(cersei::tools::filesystem())
        .system_prompt("You are a code analyst. Examine the project and summarize it.")
        .max_turns(5)
        .permission_policy(AllowAll)
        .working_dir(".")
        .reporter(tracker_ref)
        .on_event(|e| {
            if let AgentEvent::TextDelta(t) = e {
                print!("{}", t);
            }
        })
        .build()?;

    let output = agent.run("Analyze this project's structure and give me a summary.").await?;
    let elapsed = start.elapsed();

    println!("\n");

    // ── Header ───────────────────────────────────────────────────────────
    println!("{}",   "=".repeat(64));
    println!("  CERSEI USAGE REPORT");
    println!("{}",   "=".repeat(64));
    println!();

    // ── Model info ───────────────────────────────────────────────────────
    println!("  Model & Session");
    println!("  ---------------");
    println!("  Model:           {}", active_model);
    println!("  Provider:        simulated-claude");
    println!("  Wall time:       {:.2}s", elapsed.as_secs_f64());
    println!("  Turns:           {}", output.turns);
    println!("  Tool calls:      {}", output.tool_calls.len());
    println!("  Stop reason:     {:?}", output.stop_reason);
    println!();

    // ── Token usage ──────────────────────────────────────────────────────
    println!("  Token Usage");
    println!("  -----------");
    println!("  Input tokens:    {:>8}", output.usage.input_tokens);
    println!("  Output tokens:   {:>8}", output.usage.output_tokens);
    println!("  Total tokens:    {:>8}", output.usage.input_tokens + output.usage.output_tokens);
    println!("  Cost (USD):      ${:.6}", output.usage.cost_usd.unwrap_or(0.0));
    println!();

    // ── Per-turn breakdown ───────────────────────────────────────────────
    let turns = tracker.turns.lock().clone();
    println!("  Per-Turn Breakdown");
    println!("  ------------------");
    println!("  {:<6} {:>10} {:>10} {:>10} {:<12}",
        "Turn", "Input", "Output", "Cost", "Stop");
    println!("  {}", "-".repeat(54));
    for t in &turns {
        println!("  {:<6} {:>10} {:>10} ${:>9.6} {:<12}",
            t.turn, t.input_tokens, t.output_tokens, t.cost_usd, t.stop_reason);
    }
    let total_cost: f64 = turns.iter().map(|t| t.cost_usd).sum();
    let total_in: u64 = turns.iter().map(|t| t.input_tokens).sum();
    let total_out: u64 = turns.iter().map(|t| t.output_tokens).sum();
    println!("  {}", "-".repeat(54));
    println!("  {:<6} {:>10} {:>10} ${:>9.6}",
        "TOTAL", total_in, total_out, total_cost);
    println!();

    // ── Tool call breakdown ──────────────────────────────────────────────
    let tool_calls = tracker.tool_calls.lock().clone();
    if !tool_calls.is_empty() {
        println!("  Tool Calls");
        println!("  ----------");
        println!("  {:<6} {:<14} {:>10} {:<6}",
            "Turn", "Tool", "Time (ms)", "OK?");
        println!("  {}", "-".repeat(40));
        for tc in &tool_calls {
            println!("  {:<6} {:<14} {:>10.2} {:<6}",
                tc.turn, tc.name, tc.duration_ms,
                if tc.is_error { "ERR" } else { "OK" });
        }
        println!();

        // Tool histogram
        let mut histogram: HashMap<String, u32> = HashMap::new();
        for tc in &tool_calls {
            *histogram.entry(tc.name.clone()).or_default() += 1;
        }
        println!("  Tool Histogram");
        println!("  --------------");
        for (name, count) in &histogram {
            let bar = "#".repeat(*count as usize * 4);
            println!("  {:<14} {:>3}x  {}", name, count, bar);
        }
        println!();
    }

    // ── Billing estimate ─────────────────────────────────────────────────
    println!("  Billing Estimate");
    println!("  ----------------");

    let input_cost_sonnet = (total_in as f64 / 1_000_000.0) * SONNET_INPUT_PER_MTOK;
    let output_cost_sonnet = (total_out as f64 / 1_000_000.0) * SONNET_OUTPUT_PER_MTOK;
    let input_cost_opus = (total_in as f64 / 1_000_000.0) * OPUS_INPUT_PER_MTOK;
    let output_cost_opus = (total_out as f64 / 1_000_000.0) * OPUS_OUTPUT_PER_MTOK;
    let input_cost_haiku = (total_in as f64 / 1_000_000.0) * HAIKU_INPUT_PER_MTOK;
    let output_cost_haiku = (total_out as f64 / 1_000_000.0) * HAIKU_OUTPUT_PER_MTOK;

    println!("  This session's tokens ({} in / {} out) would cost:", total_in, total_out);
    println!();
    println!("  {:<24} {:>10} {:>10} {:>10}",
        "Model", "Input", "Output", "Total");
    println!("  {}", "-".repeat(56));
    println!("  {:<24} ${:>9.6} ${:>9.6} ${:>9.6}",
        "Claude Sonnet 4.6", input_cost_sonnet, output_cost_sonnet,
        input_cost_sonnet + output_cost_sonnet);
    println!("  {:<24} ${:>9.6} ${:>9.6} ${:>9.6}",
        "Claude Opus 4.6", input_cost_opus, output_cost_opus,
        input_cost_opus + output_cost_opus);
    println!("  {:<24} ${:>9.6} ${:>9.6} ${:>9.6}",
        "Claude Haiku 4.5", input_cost_haiku, output_cost_haiku,
        input_cost_haiku + output_cost_haiku);
    println!();

    // ── Projected costs ──────────────────────────────────────────────────
    println!("  Projected Costs (at Sonnet rates)");
    println!("  ----------------------------------");
    let session_cost = input_cost_sonnet + output_cost_sonnet;
    println!("  This session:         ${:.6}", session_cost);
    println!("  10 sessions/day:      ${:.4}", session_cost * 10.0);
    println!("  100 sessions/day:     ${:.2}", session_cost * 100.0);
    println!("  Monthly (30d, 50/day): ${:.2}", session_cost * 50.0 * 30.0);
    println!();

    // ── Efficiency metrics ───────────────────────────────────────────────
    println!("  Efficiency");
    println!("  ----------");
    let tokens_per_sec = (total_in + total_out) as f64 / elapsed.as_secs_f64();
    let cost_per_turn = total_cost / turns.len().max(1) as f64;
    let cost_per_tool = if !tool_calls.is_empty() {
        total_cost / tool_calls.len() as f64
    } else { 0.0 };
    println!("  Tokens/sec:           {:.0}", tokens_per_sec);
    println!("  Cost/turn:            ${:.6}", cost_per_turn);
    println!("  Cost/tool call:       ${:.6}", cost_per_tool);
    println!("  Input/output ratio:   {:.1}x", total_in as f64 / total_out.max(1) as f64);
    println!();

    // ── Verification ─────────────────────────────────────────────────────
    println!("  Verification");
    println!("  ------------");
    let cost_matches = (total_cost - output.usage.cost_usd.unwrap_or(0.0)).abs() < 0.000001;
    let token_match = output.usage.input_tokens == total_in
        && output.usage.output_tokens == total_out;
    let tool_count_match = output.tool_calls.len() == tool_calls.len();

    println!("  Cost tracking:     {} (reporter=${:.6} vs output=${:.6})",
        if cost_matches { "PASS" } else { "FAIL" },
        total_cost, output.usage.cost_usd.unwrap_or(0.0));
    println!("  Token tracking:    {} ({}in/{}out vs {}in/{}out)",
        if token_match { "PASS" } else { "FAIL" },
        total_in, total_out, output.usage.input_tokens, output.usage.output_tokens);
    println!("  Tool call count:   {} ({} reporter vs {} output)",
        if tool_count_match { "PASS" } else { "FAIL" },
        tool_calls.len(), output.tool_calls.len());
    println!();

    println!("{}", "=".repeat(64));

    if cost_matches && token_match && tool_count_match {
        println!("  All verifications PASSED");
    } else {
        println!("  SOME VERIFICATIONS FAILED — investigate above");
    }
    println!("{}", "=".repeat(64));
    println!();

    Ok(())
}
