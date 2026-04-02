//! # Simple Agent
//!
//! The most basic Cersei usage: create an agent with Anthropic, give it
//! filesystem + shell tools, run a prompt, and print the result.
//!
//! ```bash
//! ANTHROPIC_API_KEY=sk-ant-... cargo run --example simple_agent
//! ```

use cersei::prelude::*;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // ── Minimal: 3 lines to run a coding agent ──────────────────────────

    let output = Agent::builder()
        .provider(Anthropic::from_env()?)
        .tools(cersei::tools::coding())  // filesystem + shell tools
        .system_prompt("You are a helpful coding assistant. Be concise.")
        .max_turns(5)
        .permission_policy(AllowAll)
        .working_dir(".")
        .run_with("List all Rust source files in the current directory and count them.")
        .await?;

    println!("─── Agent Output ───");
    println!("{}", output.text());
    println!("─── Stats ───");
    println!("Turns:       {}", output.turns);
    println!("Tool calls:  {}", output.tool_calls.len());
    println!("Input tok:   {}", output.usage.input_tokens);
    println!("Output tok:  {}", output.usage.output_tokens);
    if let Some(cost) = output.usage.cost_usd {
        println!("Cost:        ${:.4}", cost);
    }

    // Print each tool call
    for tc in &output.tool_calls {
        println!(
            "  {} ({}) → {}",
            tc.name,
            if tc.is_error { "ERR" } else { "OK" },
            tc.duration.as_millis()
        );
    }

    Ok(())
}
