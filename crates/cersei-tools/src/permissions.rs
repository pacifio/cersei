//! Permission policies for tool execution.

use super::PermissionLevel;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

// ─── Permission policy trait ─────────────────────────────────────────────────

#[async_trait]
pub trait PermissionPolicy: Send + Sync {
    async fn check(&self, request: &PermissionRequest) -> PermissionDecision;
}

#[derive(Debug, Clone)]
pub struct PermissionRequest {
    pub tool_name: String,
    pub tool_input: serde_json::Value,
    pub permission_level: PermissionLevel,
    pub description: String,
    pub id: String,
}

#[derive(Debug, Clone)]
pub enum PermissionDecision {
    Allow,
    Deny(String),
    AllowOnce,
    AllowForSession,
}

// ─── Built-in policies ──────────────────────────────────────────────────────

/// Allow all tool invocations. Suitable for CI/headless/trusted environments.
pub struct AllowAll;

#[async_trait]
impl PermissionPolicy for AllowAll {
    async fn check(&self, _request: &PermissionRequest) -> PermissionDecision {
        PermissionDecision::Allow
    }
}

/// Only allow tools with PermissionLevel::None or ReadOnly.
pub struct AllowReadOnly;

#[async_trait]
impl PermissionPolicy for AllowReadOnly {
    async fn check(&self, request: &PermissionRequest) -> PermissionDecision {
        match request.permission_level {
            PermissionLevel::None | PermissionLevel::ReadOnly => PermissionDecision::Allow,
            _ => PermissionDecision::Deny(format!(
                "Tool '{}' requires {:?} permission (read-only mode)",
                request.tool_name, request.permission_level
            )),
        }
    }
}

/// Deny all tool invocations.
pub struct DenyAll;

#[async_trait]
impl PermissionPolicy for DenyAll {
    async fn check(&self, request: &PermissionRequest) -> PermissionDecision {
        PermissionDecision::Deny(format!(
            "Tool '{}' blocked by DenyAll policy",
            request.tool_name
        ))
    }
}

/// Rule-based permission policy with pattern matching.
pub struct RuleBased {
    pub rules: Vec<PermissionRule>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PermissionRule {
    pub tool_name: Option<String>,
    pub path_pattern: Option<String>,
    pub action: PermissionAction,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum PermissionAction {
    Allow,
    Deny,
}

#[async_trait]
impl PermissionPolicy for RuleBased {
    async fn check(&self, request: &PermissionRequest) -> PermissionDecision {
        for rule in &self.rules {
            let name_matches = rule
                .tool_name
                .as_ref()
                .map(|n| n == &request.tool_name || n == "all")
                .unwrap_or(true);

            if name_matches {
                return match rule.action {
                    PermissionAction::Allow => PermissionDecision::Allow,
                    PermissionAction::Deny => PermissionDecision::Deny(format!(
                        "Tool '{}' blocked by rule",
                        request.tool_name
                    )),
                };
            }
        }
        // Default: allow if no rules match
        PermissionDecision::Allow
    }
}

/// Interactive permission policy that defers to a callback.
pub struct InteractivePolicy {
    pub handler: Box<dyn Fn(&PermissionRequest) -> PermissionDecision + Send + Sync>,
}

impl InteractivePolicy {
    pub fn new(
        handler: impl Fn(&PermissionRequest) -> PermissionDecision + Send + Sync + 'static,
    ) -> Self {
        Self {
            handler: Box::new(handler),
        }
    }

    /// Create a policy that defers to the AgentStream for interactive decisions.
    pub fn via_stream() -> StreamDeferredPolicy {
        StreamDeferredPolicy
    }
}

#[async_trait]
impl PermissionPolicy for InteractivePolicy {
    async fn check(&self, request: &PermissionRequest) -> PermissionDecision {
        (self.handler)(request)
    }
}

/// Placeholder policy that emits PermissionRequired events via the agent stream.
pub struct StreamDeferredPolicy;

#[async_trait]
impl PermissionPolicy for StreamDeferredPolicy {
    async fn check(&self, _request: &PermissionRequest) -> PermissionDecision {
        // In practice, the agent loop intercepts this and emits
        // AgentEvent::PermissionRequired, then waits for a response.
        PermissionDecision::Allow
    }
}
