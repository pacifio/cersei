# Getting Started

## Installation

Add Cersei to your `Cargo.toml`:

```toml
[dependencies]
cersei = { path = "../src-cersei/crates/cersei" }
tokio = { version = "1", features = ["full"] }
anyhow = "1"
```

Or, if published to crates.io:

```toml
[dependencies]
cersei = "0.1"
```

## Prerequisites

- Rust 1.75+ (edition 2021)
- An LLM provider API key (Anthropic, OpenAI, or a local model)
- For the `Grep` tool: `rg` (ripgrep) is preferred but falls back to `grep`

## Your First Agent

```rust
use cersei::prelude::*;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let output = Agent::builder()
        .provider(Anthropic::from_env()?)       // reads ANTHROPIC_API_KEY
        .tools(cersei::tools::coding())          // filesystem + shell
        .permission_policy(AllowAll)             // allow all tool calls
        .run_with("Count the number of .rs files in this directory")
        .await?;

    println!("{}", output.text());
    println!("Turns: {}, Tools used: {}", output.turns, output.tool_calls.len());
    Ok(())
}
```

Set your API key and run:

```bash
export ANTHROPIC_API_KEY=sk-ant-...
cargo run
```

## How It Works

1. **You provide a prompt** to the agent
2. **The agent sends it to the LLM** (via the Provider) with available tool definitions
3. **The LLM responds** — either with text (done) or with tool calls
4. **Tools execute locally** — Bash commands, file reads, grep searches, etc.
5. **Results feed back** to the LLM for the next turn
6. **Loop repeats** until the LLM says "end_turn" or `max_turns` is reached
7. **You get `AgentOutput`** with the final message, usage stats, and tool call history

## Next Steps

- [Providers](02-providers.md) — configure Anthropic, OpenAI, or build your own
- [Tools](03-tools.md) — built-in tools and how to create custom ones
- [Agent Lifecycle](04-agent-lifecycle.md) — the agentic loop in detail
- [Events & Streaming](05-events-streaming.md) — real-time observation
- [Memory](06-memory.md) — session persistence
- [Hooks](07-hooks.md) — middleware and interception
- [Permissions](08-permissions.md) — controlling what tools can do
