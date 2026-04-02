//! Coordinator mode: multi-agent orchestration.
//!
//! When active, the agent acts as a coordinator that spawns parallel worker
//! agents using the Agent tool. Workers have restricted tool access (no Agent,
//! SendMessage, TaskStop) to prevent uncontrolled recursion.

use cersei_tools::Tool;

/// Agent execution mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentMode {
    /// Full access to all tools including Agent spawning.
    Coordinator,
    /// Restricted: cannot spawn sub-agents or use coordination tools.
    Worker,
    /// Standard mode: all tools available (no special orchestration).
    Normal,
}

/// Tools restricted to coordinator mode only (workers can't use these).
pub const COORDINATOR_ONLY_TOOLS: &[&str] = &[
    "Agent",
    "SendMessage",
    "TaskStop",
    "TeamCreate",
    "TeamDelete",
    "SyntheticOutput",
];

/// Check if coordinator mode is active via environment variable.
pub fn is_coordinator_mode() -> bool {
    match std::env::var("CERSEI_COORDINATOR_MODE") {
        Ok(v) => !v.is_empty() && v != "0" && v != "false",
        Err(_) => false,
    }
}

/// Filter tools based on agent mode.
/// Workers lose coordinator-only tools. Coordinators and Normal keep everything.
pub fn filter_tools_for_mode(
    tools: Vec<Box<dyn Tool>>,
    mode: AgentMode,
) -> Vec<Box<dyn Tool>> {
    match mode {
        AgentMode::Worker => tools
            .into_iter()
            .filter(|t| !COORDINATOR_ONLY_TOOLS.contains(&t.name()))
            .collect(),
        AgentMode::Coordinator | AgentMode::Normal => tools,
    }
}

/// System prompt section for coordinator mode.
pub fn coordinator_system_prompt() -> &'static str {
    "## Coordinator Mode\n\n\
    You are operating as an orchestrator. Your role is to:\n\
    1. Break complex tasks into independent sub-tasks\n\
    2. Spawn parallel worker agents using the Agent tool\n\
    3. Each worker prompt must be fully self-contained\n\
    4. Synthesize findings from all workers before responding\n\
    5. Use TaskCreate/TaskUpdate to track parallel work\n\n\
    Workers cannot spawn their own sub-agents. They have access to \
    filesystem, shell, and web tools only."
}

/// Format a context section listing available tools for the coordinator.
pub fn coordinator_context(tools: &[Box<dyn Tool>]) -> String {
    let tool_list: Vec<String> = tools
        .iter()
        .filter(|t| !["Agent", "SyntheticOutput"].contains(&t.name()))
        .map(|t| format!("- {}: {}", t.name(), t.description()))
        .collect();

    format!(
        "Available tools for workers:\n{}",
        tool_list.join("\n")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_filter_worker_tools() {
        let tools = cersei_tools::all();
        let original_count = tools.len();
        let filtered = filter_tools_for_mode(tools, AgentMode::Worker);
        // Workers should have fewer tools (coordinator-only removed)
        assert!(filtered.len() <= original_count);
        assert!(filtered.iter().all(|t| !COORDINATOR_ONLY_TOOLS.contains(&t.name())));
    }

    #[test]
    fn test_filter_coordinator_keeps_all() {
        let tools = cersei_tools::all();
        let count = tools.len();
        let filtered = filter_tools_for_mode(tools, AgentMode::Coordinator);
        assert_eq!(filtered.len(), count);
    }

    #[test]
    fn test_coordinator_prompt() {
        let prompt = coordinator_system_prompt();
        assert!(prompt.contains("orchestrator"));
        assert!(prompt.contains("sub-tasks"));
    }

    #[test]
    fn test_coordinator_context() {
        let tools = cersei_tools::all();
        let ctx = coordinator_context(&tools);
        assert!(ctx.contains("Available tools"));
        assert!(ctx.contains("Read"));
        assert!(ctx.contains("Bash"));
    }
}
