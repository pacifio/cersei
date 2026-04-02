//! # Hooks & Middleware
//!
//! Demonstrates the hook system for intercepting tool calls:
//! - Cost guard that stops the agent if spending exceeds a budget
//! - Audit logger that records every tool execution
//! - Tool blocker that prevents specific tools from running
//!
//! ```bash
//! ANTHROPIC_API_KEY=sk-ant-... cargo run --example hooks_middleware
//! ```

use cersei::prelude::*;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

// ─── Cost Guard Hook ─────────────────────────────────────────────────────────

struct CostGuard {
    max_usd: f64,
}

#[async_trait]
impl Hook for CostGuard {
    fn events(&self) -> &[HookEvent] {
        &[HookEvent::PostModelTurn]
    }

    fn name(&self) -> &str {
        "cost-guard"
    }

    async fn on_event(&self, ctx: &HookContext) -> HookAction {
        let cost = ctx.cumulative_cost_usd();
        if cost > self.max_usd {
            eprintln!(
                "\x1b[31m[cost-guard] Budget exceeded: ${:.4} > ${:.4}\x1b[0m",
                cost, self.max_usd
            );
            HookAction::Block(format!(
                "Cost limit ${:.2} exceeded (current: ${:.4})",
                self.max_usd, cost
            ))
        } else {
            eprintln!(
                "\x1b[32m[cost-guard] Cost OK: ${:.4} / ${:.4}\x1b[0m",
                cost, self.max_usd
            );
            HookAction::Continue
        }
    }
}

// ─── Audit Logger Hook ──────────────────────────────────────────────────────

struct AuditLogger {
    call_count: Arc<AtomicU32>,
}

impl AuditLogger {
    fn new() -> Self {
        Self {
            call_count: Arc::new(AtomicU32::new(0)),
        }
    }

    fn total_calls(&self) -> u32 {
        self.call_count.load(Ordering::Relaxed)
    }
}

#[async_trait]
impl Hook for AuditLogger {
    fn events(&self) -> &[HookEvent] {
        &[HookEvent::PreToolUse, HookEvent::PostToolUse]
    }

    fn name(&self) -> &str {
        "audit-logger"
    }

    async fn on_event(&self, ctx: &HookContext) -> HookAction {
        match ctx.event {
            HookEvent::PreToolUse => {
                let n = self.call_count.fetch_add(1, Ordering::Relaxed) + 1;
                eprintln!(
                    "\x1b[34m[audit #{n}] PRE  tool={} turn={}\x1b[0m",
                    ctx.tool_name.as_deref().unwrap_or("?"),
                    ctx.turn,
                );
            }
            HookEvent::PostToolUse => {
                let is_err = ctx.tool_is_error.unwrap_or(false);
                let status = if is_err { "ERR" } else { "OK" };
                eprintln!(
                    "\x1b[34m[audit]     POST tool={} status={}\x1b[0m",
                    ctx.tool_name.as_deref().unwrap_or("?"),
                    status,
                );
            }
            _ => {}
        }
        HookAction::Continue
    }
}

// ─── Tool Blocker Hook ──────────────────────────────────────────────────────

/// Blocks specific tools from executing.
struct ToolBlocker {
    blocked: Vec<String>,
}

impl ToolBlocker {
    fn new(blocked: &[&str]) -> Self {
        Self {
            blocked: blocked.iter().map(|s| s.to_string()).collect(),
        }
    }
}

#[async_trait]
impl Hook for ToolBlocker {
    fn events(&self) -> &[HookEvent] {
        &[HookEvent::PreToolUse]
    }

    fn name(&self) -> &str {
        "tool-blocker"
    }

    async fn on_event(&self, ctx: &HookContext) -> HookAction {
        if let Some(tool_name) = &ctx.tool_name {
            if self.blocked.iter().any(|b| b == tool_name) {
                eprintln!(
                    "\x1b[31m[blocker] Blocked tool: {}\x1b[0m",
                    tool_name
                );
                return HookAction::Block(format!("Tool '{}' is blocked by policy", tool_name));
            }
        }
        HookAction::Continue
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let audit = Arc::new(AuditLogger::new());
    let audit_clone = Arc::clone(&audit);

    let agent = Agent::builder()
        .provider(Anthropic::from_env()?)
        .tools(cersei::tools::coding())
        .system_prompt("You are a helpful coding assistant. Be concise.")
        .max_turns(5)
        .permission_policy(AllowAll)
        .working_dir(".")
        // Stack multiple hooks
        .hook(CostGuard { max_usd: 1.0 })
        .hook(AuditLogger::new())
        .hook(ToolBlocker::new(&["Write"])) // Block file writing
        .build()?;

    let output = agent
        .run("List all files in the current directory.")
        .await?;

    println!("\n─── Result ───");
    println!("{}", output.text());
    println!("Tool calls: {}", output.tool_calls.len());

    Ok(())
}
