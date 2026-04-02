//! # Cersei
//!
//! A modular Rust SDK for building coding agents programmatically.
//!
//! Cersei provides a high-level `Agent` builder that combines LLM providers,
//! tools, memory, permissions, and hooks into a complete agentic system.
//! Every component is modular and replaceable.
//!
//! ## Quick Start
//!
//! ```rust,ignore
//! use cersei::prelude::*;
//!
//! let output = Agent::builder()
//!     .provider(Anthropic::from_env()?)
//!     .tools(cersei::tools::coding())
//!     .run_with("Fix the failing tests")
//!     .await?;
//!
//! println!("{}", output.text());
//! ```

// Re-export sub-crates
pub use cersei_types as types;
pub use cersei_provider as provider;
pub use cersei_tools as tools;
pub use cersei_agent::{Agent, AgentBuilder, AgentOutput, Reporter};
pub use cersei_agent::events::{self as events, AgentEvent, AgentStream, CompactReason, WarningState};
pub use cersei_agent::reporters;
pub use cersei_memory as memory;
pub use cersei_hooks as hooks;
pub use cersei_mcp as mcp;

// Convenience re-exports for common providers
pub use cersei_provider::anthropic::Anthropic;
pub use cersei_provider::openai::OpenAi;

/// The prelude — import this for the most common types.
pub mod prelude {
    // Core agent
    pub use crate::{Agent, AgentBuilder, AgentOutput};
    pub use crate::{AgentEvent, AgentStream, Reporter};

    // Providers
    pub use crate::{Anthropic, OpenAi};
    pub use crate::provider::{Auth, CompletionRequest, Provider, ProviderOptions};

    // Types
    pub use cersei_types::{
        CerseiError, ContentBlock, Message, MessageContent, MessageMetadata,
        Result, Role, StopReason, StreamEvent, ToolDefinition, Usage,
    };

    // Tools
    pub use cersei_tools::{
        CostTracker, Extensions, PermissionLevel, Tool, ToolCategory, ToolContext,
        ToolExecute, ToolResult,
    };
    pub use cersei_tools::permissions::{
        AllowAll, AllowReadOnly, DenyAll, InteractivePolicy, PermissionDecision,
        PermissionPolicy, PermissionRequest, RuleBased,
    };

    // Memory
    pub use cersei_memory::Memory;

    // Hooks
    pub use cersei_hooks::{Hook, HookAction, HookContext, HookEvent};

    // Derive macro
    pub use cersei_tools_derive::Tool;

    // Re-export for derive macro usage
    pub use async_trait::async_trait;
    pub use schemars;
    pub use serde;
    pub use serde_json;
}
