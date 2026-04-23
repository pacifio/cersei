//! # Resumable Sessions
//!
//! Shows how to use the Memory trait to persist and resume conversations.
//! The agent remembers the previous context when you reply.
//!
//! ```bash
//! ANTHROPIC_API_KEY=sk-ant-... cargo run --example resumable_session
//! ```

use cersei::memory::JsonlMemory;
use cersei::prelude::*;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let tmp = tempfile::tempdir()?;
    let memory = JsonlMemory::new(tmp.path());

    println!("Session storage: {}", tmp.path().display());

    // ── First run ────────────────────────────────────────────────────────
    println!("\n\x1b[36m── Session 1: Initial prompt ──\x1b[0m");

    let agent = Agent::builder()
        .provider(Anthropic::from_env()?)
        .tools(cersei::tools::filesystem())
        .system_prompt("You are a helpful assistant. Remember context across messages.")
        .memory(JsonlMemory::new(tmp.path()))
        .session_id("demo-session")
        .max_turns(3)
        .permission_policy(AllowAll)
        .working_dir(".")
        .build()?;

    let output = agent
        .run("My name is Alice and I'm working on project Cersei. Remember this.")
        .await?;
    println!("{}", output.text());
    println!("(saved {} messages)", agent.messages().len());

    // ── Second run (fresh Agent, same session_id) ────────────────────────
    println!("\n\x1b[36m── Session 2: Resume and verify ──\x1b[0m");

    let agent2 = Agent::builder()
        .provider(Anthropic::from_env()?)
        .tools(cersei::tools::filesystem())
        .system_prompt("You are a helpful assistant. Remember context across messages.")
        .memory(JsonlMemory::new(tmp.path()))
        .session_id("demo-session")
        .max_turns(3)
        .permission_policy(AllowAll)
        .working_dir(".")
        .build()?;

    let output2 = agent2
        .run("What's my name and what project am I working on?")
        .await?;
    println!("{}", output2.text());
    println!(
        "(loaded {} messages from previous session)",
        agent2.messages().len()
    );

    // ── List sessions ────────────────────────────────────────────────────
    let sessions = memory.sessions().await?;
    println!("\n\x1b[36m── Stored sessions ──\x1b[0m");
    for s in &sessions {
        println!("  {} — {} messages", s.id, s.message_count);
    }

    Ok(())
}
