//! Permission policies for tool execution.

use super::PermissionLevel;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

// ─── Permission policy trait ─────────────────────────────────────────────────

#[async_trait]
pub trait PermissionPolicy: Send + Sync {
    async fn check(&self, request: &PermissionRequest) -> PermissionDecision;

    /// Whether the agent loop should put this decision to the caller as an
    /// `AgentEvent::PermissionRequired` before falling back to [`Self::check`].
    ///
    /// Defaults to `false`, so existing policies are unaffected. Only
    /// [`StreamDeferredPolicy`] opts in.
    fn defers_to_stream(&self) -> bool {
        false
    }
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

/// Policy that puts each decision to the caller over the agent stream.
///
/// The agent loop sees [`PermissionPolicy::defers_to_stream`], emits
/// `AgentEvent::PermissionRequired`, and waits for
/// `AgentStream::respond_permission`. [`Self::check`] is only reached when no
/// stream can answer — the non-streaming `run()` path — where it falls back to
/// allowing, matching the behaviour of the other headless policies.
pub struct StreamDeferredPolicy;

#[async_trait]
impl PermissionPolicy for StreamDeferredPolicy {
    async fn check(&self, request: &PermissionRequest) -> PermissionDecision {
        tracing::warn!(
            tool = %request.tool_name,
            "StreamDeferredPolicy has no stream to ask — allowing. Use run_stream(), \
             or pick an explicit policy for headless runs."
        );
        PermissionDecision::Allow
    }

    fn defers_to_stream(&self) -> bool {
        true
    }
}
