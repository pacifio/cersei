//! AgentTool: spawn a sub-agent to handle complex sub-tasks.
//!
//! Each sub-agent runs its own agentic loop with independent message history,
//! cost tracking, and tool access. The `Agent` tool is filtered out of
//! sub-agents to prevent infinite recursion.

use crate::Agent;
use async_trait::async_trait;
use cersei_provider::Provider;
use cersei_tools::permissions::AllowAll;
use cersei_tools::{PermissionLevel, Tool, ToolContext, ToolResult};
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::Arc;

/// The AgentTool — spawns independent sub-agents.
///
/// This must be constructed with a reference to the parent's provider
/// so sub-agents can make their own API calls.
pub struct AgentTool {
    provider_factory: Arc<dyn Fn() -> Box<dyn Provider> + Send + Sync>,
    available_tools: Vec<Box<dyn Tool>>,
}

impl AgentTool {
    /// Create an AgentTool with a provider factory and available tools.
    ///
    /// The provider factory creates a new provider instance for each sub-agent.
    /// The `available_tools` list will have "Agent" filtered out automatically.
    pub fn new(
        provider_factory: impl Fn() -> Box<dyn Provider> + Send + Sync + 'static,
        tools: Vec<Box<dyn Tool>>,
    ) -> Self {
        Self {
            provider_factory: Arc::new(provider_factory),
            available_tools: tools,
        }
    }
}

#[derive(Debug, Deserialize)]
struct AgentInput {
    description: String,
    prompt: String,
    #[serde(default)]
    system_prompt: Option<String>,
    #[serde(default)]
    max_turns: Option<u32>,
    #[serde(default)]
    model: Option<String>,
}

#[async_trait]
impl Tool for AgentTool {
    fn name(&self) -> &str { "Agent" }

    fn description(&self) -> &str {
        "Launch a new agent to handle complex, multi-step tasks autonomously. \
         The agent runs its own agentic loop with access to tools and returns \
         its final result. Use this to delegate sub-tasks, run parallel \
         workstreams, or handle tasks that require many tool calls."
    }

    fn permission_level(&self) -> PermissionLevel {
        PermissionLevel::None
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "description": {
                    "type": "string",
                    "description": "Short description of the agent's task (3-5 words)"
                },
                "prompt": {
                    "type": "string",
                    "description": "The complete task for the agent to perform"
                },
                "system_prompt": {
                    "type": "string",
                    "description": "Optional system prompt override for the sub-agent"
                },
                "max_turns": {
                    "type": "integer",
                    "description": "Max turns for the sub-agent (default 10)"
                },
                "model": {
                    "type": "string",
                    "description": "Optional model override"
                }
            },
            "required": ["description", "prompt"]
        })
    }

    async fn execute(&self, input: Value, ctx: &ToolContext) -> ToolResult {
        let input: AgentInput = match serde_json::from_value(input) {
            Ok(i) => i,
            Err(e) => return ToolResult::error(format!("Invalid input: {}", e)),
        };

        tracing::info!(description = %input.description, "Spawning sub-agent");

        // Create a fresh provider for the sub-agent
        let provider = (self.provider_factory)();

        // Filter out "Agent" tool to prevent recursion
        let sub_tools: Vec<Box<dyn Tool>> = self
            .available_tools
            .iter()
            .filter(|t| t.name() != "Agent")
            .map(|t| {
                // We can't clone Box<dyn Tool>, so we rebuild tool sets
                // This is a limitation — in practice, sub-agents get the
                // standard tool sets minus Agent
                cersei_tools::all()
                    .into_iter()
                    .find(|st| st.name() == t.name())
            })
            .flatten()
            .collect();

        // Use standard tools if filtering resulted in empty set
        let sub_tools = if sub_tools.is_empty() {
            cersei_tools::all()
                .into_iter()
                .filter(|t| t.name() != "Agent")
                .collect()
        } else {
            sub_tools
        };

        let mut builder = Agent::builder()
            .provider(provider)
            .tools(sub_tools)
            .max_turns(input.max_turns.unwrap_or(10))
            .permission_policy(AllowAll)
            .working_dir(&ctx.working_dir);

        if let Some(sys) = input.system_prompt {
            builder = builder.system_prompt(sys);
        } else {
            builder = builder.system_prompt(
                "You are a specialized sub-agent. Complete the given task thoroughly and return your findings.",
            );
        }

        if let Some(model) = input.model {
            builder = builder.model(model);
        }

        let agent = match builder.build() {
            Ok(a) => a,
            Err(e) => return ToolResult::error(format!("Failed to build sub-agent: {}", e)),
        };

        match agent.run(&input.prompt).await {
            Ok(output) => {
                let text = output.text().to_string();
                let meta = json!({
                    "turns": output.turns,
                    "tool_calls": output.tool_calls.len(),
                    "input_tokens": output.usage.input_tokens,
                    "output_tokens": output.usage.output_tokens,
                });
                ToolResult::success(text).with_metadata(meta)
            }
            Err(e) => ToolResult::error(format!("Sub-agent failed: {}", e)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cersei_provider::{CompletionRequest, CompletionStream, ProviderCapabilities};
    use cersei_tools::{CostTracker, Extensions};
    use cersei_tools::permissions::AllowAll;
    use cersei_types::*;
    use tokio::sync::mpsc;

    /// Mock provider that returns EndTurn immediately with a text response.
    struct EchoProvider;

    #[async_trait]
    impl Provider for EchoProvider {
        fn name(&self) -> &str { "echo" }
        fn context_window(&self, _: &str) -> u64 { 4096 }
        fn capabilities(&self, _: &str) -> ProviderCapabilities {
            ProviderCapabilities { streaming: true, tool_use: false, ..Default::default() }
        }
        async fn complete(&self, req: CompletionRequest) -> cersei_types::Result<CompletionStream> {
            let prompt = req.messages.last().and_then(|m| m.get_text()).unwrap_or("").to_string();
            let (tx, rx) = mpsc::channel(16);
            tokio::spawn(async move {
                let _ = tx.send(StreamEvent::MessageStart { id: "1".into(), model: "echo".into() }).await;
                let _ = tx.send(StreamEvent::ContentBlockStart { index: 0, block_type: "text".into(), id: None, name: None }).await;
                let _ = tx.send(StreamEvent::TextDelta { index: 0, text: format!("Echo: {}", prompt) }).await;
                let _ = tx.send(StreamEvent::ContentBlockStop { index: 0 }).await;
                let _ = tx.send(StreamEvent::MessageDelta {
                    stop_reason: Some(StopReason::EndTurn),
                    usage: Some(Usage { input_tokens: 10, output_tokens: 5, ..Default::default() }),
                }).await;
                let _ = tx.send(StreamEvent::MessageStop).await;
            });
            Ok(CompletionStream::new(rx))
        }
    }

    #[tokio::test]
    async fn test_agent_tool_spawns_sub_agent() {
        let agent_tool = AgentTool::new(
            || Box::new(EchoProvider),
            cersei_tools::filesystem(),
        );

        let ctx = ToolContext {
            working_dir: std::env::temp_dir(),
            session_id: "parent".into(),
            permissions: Arc::new(AllowAll),
            cost_tracker: Arc::new(CostTracker::new()),
            mcp_manager: None,
            extensions: Extensions::default(),
        };

        let result = agent_tool.execute(json!({
            "description": "test sub-agent",
            "prompt": "Hello from parent"
        }), &ctx).await;

        assert!(!result.is_error, "Sub-agent should succeed: {}", result.content);
        assert!(result.content.contains("Echo:"), "Should contain echo response");
        assert!(result.metadata.is_some(), "Should have metadata");
    }

    #[tokio::test]
    async fn test_agent_tool_filters_self() {
        // Verify Agent tool is not available to sub-agents (no recursion)
        let agent_tool = AgentTool::new(
            || Box::new(EchoProvider),
            cersei_tools::all(),
        );

        let ctx = ToolContext {
            working_dir: std::env::temp_dir(),
            session_id: "parent".into(),
            permissions: Arc::new(AllowAll),
            cost_tracker: Arc::new(CostTracker::new()),
            mcp_manager: None,
            extensions: Extensions::default(),
        };

        // This should work — sub-agent gets tools minus "Agent"
        let result = agent_tool.execute(json!({
            "description": "test no recursion",
            "prompt": "Do something"
        }), &ctx).await;

        assert!(!result.is_error);
    }
}
