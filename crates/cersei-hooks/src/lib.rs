//! cersei-hooks: Hook/middleware system for the Cersei SDK.
//!
//! Hooks intercept events in the agent lifecycle (pre/post tool use, model turns, etc.)
//! and can block, modify, or inject messages.

use async_trait::async_trait;
use cersei_types::Message;
use serde::{Deserialize, Serialize};
use serde_json::Value;

// ─── Hook trait ──────────────────────────────────────────────────────────────

#[async_trait]
pub trait Hook: Send + Sync {
    /// Which events this hook handles.
    fn events(&self) -> &[HookEvent];

    /// Called when a matching event fires. Returns an action to control flow.
    async fn on_event(&self, ctx: &HookContext) -> HookAction;

    /// Optional name for logging/debugging.
    fn name(&self) -> &str {
        "unnamed-hook"
    }
}

// ─── Hook events ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "PascalCase")]
pub enum HookEvent {
    PreToolUse,
    PostToolUse,
    PreModelTurn,
    PostModelTurn,
    Stop,
    Error,
}

// ─── Hook context ────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct HookContext {
    pub event: HookEvent,
    pub tool_name: Option<String>,
    pub tool_input: Option<Value>,
    pub tool_result: Option<String>,
    pub tool_is_error: Option<bool>,
    pub turn: u32,
    pub cumulative_cost_usd: f64,
    pub message_count: usize,
}

impl HookContext {
    pub fn cumulative_cost_usd(&self) -> f64 {
        self.cumulative_cost_usd
    }
}

// ─── Hook actions ────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub enum HookAction {
    /// Continue normally.
    Continue,
    /// Block the operation (PreToolUse only). Includes reason.
    Block(String),
    /// Replace the tool input with modified data (PreToolUse only).
    ModifyInput(Value),
    /// Inject a message into the conversation.
    InjectMessage(Message),
}

// ─── Shell hook (compat with cc-core HookEntry) ──────────────────────────────

/// A hook that runs a shell command from settings.json configuration.
pub struct ShellHook {
    pub command: String,
    pub hook_events: Vec<HookEvent>,
    pub blocking: bool,
    hook_name: String,
}

impl ShellHook {
    pub fn new(command: impl Into<String>, events: &[HookEvent], blocking: bool) -> Self {
        let cmd = command.into();
        let name = format!("shell:{}", cmd.chars().take(40).collect::<String>());
        Self {
            command: cmd,
            hook_events: events.to_vec(),
            blocking,
            hook_name: name,
        }
    }
}

#[async_trait]
impl Hook for ShellHook {
    fn events(&self) -> &[HookEvent] {
        &self.hook_events
    }

    fn name(&self) -> &str {
        &self.hook_name
    }

    async fn on_event(&self, ctx: &HookContext) -> HookAction {
        let sh = if cfg!(windows) { "cmd" } else { "sh" };
        let flag = if cfg!(windows) { "/C" } else { "-c" };

        let ctx_json = serde_json::to_string(&serde_json::json!({
            "event": format!("{:?}", ctx.event),
            "tool_name": ctx.tool_name,
            "turn": ctx.turn,
        }))
        .unwrap_or_default();

        let output = match std::process::Command::new(sh)
            .args([flag, &self.command])
            .env("CERSEI_HOOK_CONTEXT", &ctx_json)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .output()
        {
            Ok(o) => o,
            Err(e) => {
                tracing::warn!(command = %self.command, error = %e, "Shell hook failed to spawn");
                return HookAction::Continue;
            }
        };

        if output.status.success() {
            return HookAction::Continue;
        }

        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        let body = if !stderr.trim().is_empty() {
            stderr.to_string()
        } else {
            stdout.to_string()
        };

        if self.blocking {
            HookAction::Block(format!("Hook '{}' failed: {}", self.command, body.trim()))
        } else {
            tracing::warn!(command = %self.command, body = %body.trim(), "Shell hook returned non-zero");
            HookAction::Continue
        }
    }
}

// ─── Hook runner ─────────────────────────────────────────────────────────────

/// Execute all matching hooks for a given event, returning the first non-Continue action.
pub async fn run_hooks(hooks: &[std::sync::Arc<dyn Hook>], ctx: &HookContext) -> HookAction {
    for hook in hooks {
        if hook.events().contains(&ctx.event) {
            let action = hook.on_event(ctx).await;
            match &action {
                HookAction::Continue => continue,
                _ => return action,
            }
        }
    }
    HookAction::Continue
}
