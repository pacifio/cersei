//! Parallel delegation primitive — spawn N isolated subagents with
//! restricted toolsets and collect their summaries.
//!
//! Port of `_inspirations/hermes-agent/tools/delegate_tool.py`. The key
//! invariants:
//!
//! - **Isolation**: child agents start with a fresh conversation — no
//!   parent history, own session-id-equivalent.
//! - **Restricted toolset**: caller specifies which tools the child may
//!   call. A default blocklist strips dangerous recursion paths
//!   (`delegate`, memory writes, user-interaction tools) at depth ≥ 1.
//! - **Depth cap**: `max_depth = 2` (parent = 0, child = 1, no grandchildren)
//!   prevents infinite recursion.
//! - **Bounded parallelism**: up to `max_concurrent` children run at once
//!   via `tokio::task::JoinSet`.
//! - **Best-effort**: a child failure doesn't abort the batch; the parent
//!   gets an error summary for that task and keeps going.
//!
//! This module does NOT pull in a Python RPC bridge for code execution —
//! that's the 0.1.9 lift.

use crate::Agent;
use cersei_provider::Provider;
use cersei_tools::Tool;
use cersei_types::Result;
use std::sync::Arc;

/// Function that constructs a fresh provider for each child. Needed because
/// `Provider` trait objects aren't cloneable — each delegated child needs
/// its own provider instance.
pub type ProviderFactory = Arc<dyn Fn() -> Box<dyn Provider + Send + Sync> + Send + Sync>;

/// Function that constructs a fresh toolset for each child. Same reason as
/// `ProviderFactory` — `Box<dyn Tool>` isn't cloneable.
pub type ToolsetFactory = Arc<dyn Fn() -> Vec<Box<dyn Tool>> + Send + Sync>;

/// Tools whose names match these strings are stripped from the child's
/// toolset. These are either recursion hazards (`delegate`) or cross-agent
/// side effects (memory writes, user-facing messages). Caller-provided
/// blocklists are merged with this default.
///
/// Mirrors `DELEGATE_BLOCKED_TOOLS` in hermes-agent.
pub const DELEGATE_BLOCKED_TOOLS: &[&str] = &[
    "delegate",
    "clarify",
    "memory",
    "memory_write",
    "send_message",
    "ask_user",
    "AskUserQuestion",
];

/// Maximum delegation depth (parent=0). Children cannot spawn grandchildren.
pub const MAX_DEPTH: u32 = 2;

/// Default in-flight child limit per batch — matches hermes-agent's
/// `max_concurrent_children`.
pub const DEFAULT_MAX_CONCURRENT: usize = 3;

/// Input to a single delegation.
#[derive(Debug, Clone)]
pub struct DelegateTask {
    /// What the child should accomplish. Required.
    pub goal: String,
    /// Optional background context. Injected into the child system prompt.
    pub context: Option<String>,
    /// Optional concrete local workspace path. When present, the child is
    /// told to use it for repo / workdir operations. Omit when we don't
    /// have a real absolute path — the child will discover one.
    pub workspace: Option<std::path::PathBuf>,
}

impl DelegateTask {
    pub fn new(goal: impl Into<String>) -> Self {
        Self {
            goal: goal.into(),
            context: None,
            workspace: None,
        }
    }
    pub fn with_context(mut self, ctx: impl Into<String>) -> Self {
        self.context = Some(ctx.into());
        self
    }
    pub fn with_workspace(mut self, path: impl Into<std::path::PathBuf>) -> Self {
        self.workspace = Some(path.into());
        self
    }
}

/// Result from one child agent.
#[derive(Debug, Clone)]
pub struct DelegateResult {
    pub goal: String,
    pub summary: String,
    /// None when the child ran cleanly; `Some(err)` when it failed.
    pub error: Option<String>,
    pub turns: u32,
}

impl DelegateResult {
    pub fn is_ok(&self) -> bool {
        self.error.is_none()
    }
}

/// Top-level config for a batch delegation.
pub struct DelegateConfig {
    /// Tasks to run, potentially in parallel.
    pub tasks: Vec<DelegateTask>,
    /// Factory that mints a fresh provider per child.
    pub provider_factory: ProviderFactory,
    /// Factory that mints a fresh toolset per child. The blocklist is
    /// applied inside `run_batch` — the factory returns the raw set.
    pub toolset_factory: ToolsetFactory,
    /// Model for child agents. Defaults to whatever the provider's default
    /// model is when `None`.
    pub model: Option<String>,
    /// Upper bound on child turns.
    pub max_turns: u32,
    /// In-flight concurrency cap. Defaults to `DEFAULT_MAX_CONCURRENT`.
    pub max_concurrent: usize,
    /// Current recursion depth. Parent sets this to 1; `run_batch` refuses
    /// to spawn when depth ≥ `MAX_DEPTH`.
    pub depth: u32,
    /// Extra blocked tool names (merged with `DELEGATE_BLOCKED_TOOLS`).
    pub extra_blocked: Vec<String>,
}

impl DelegateConfig {
    pub fn new(provider_factory: ProviderFactory, toolset_factory: ToolsetFactory) -> Self {
        Self {
            tasks: Vec::new(),
            provider_factory,
            toolset_factory,
            model: None,
            max_turns: 30,
            max_concurrent: DEFAULT_MAX_CONCURRENT,
            depth: 1,
            extra_blocked: Vec::new(),
        }
    }
}

/// Run a batch of delegations. Blocks until every child completes (success
/// or failure). On depth exhaustion, returns an error immediately without
/// spawning anything.
pub async fn run_batch(cfg: DelegateConfig) -> Result<Vec<DelegateResult>> {
    if cfg.depth >= MAX_DEPTH {
        return Err(cersei_types::CerseiError::Config(format!(
            "delegation depth {} exceeds MAX_DEPTH={}",
            cfg.depth, MAX_DEPTH
        )));
    }
    if cfg.tasks.is_empty() {
        return Ok(Vec::new());
    }

    let blocked: Vec<String> = DELEGATE_BLOCKED_TOOLS
        .iter()
        .map(|s| s.to_string())
        .chain(cfg.extra_blocked.iter().cloned())
        .collect();

    let sem = Arc::new(tokio::sync::Semaphore::new(cfg.max_concurrent));
    let mut set = tokio::task::JoinSet::new();

    let max_turns = cfg.max_turns;
    let model = cfg.model.clone();
    let provider_factory = cfg.provider_factory.clone();
    let toolset_factory = cfg.toolset_factory.clone();

    for (i, task) in cfg.tasks.into_iter().enumerate() {
        let permit = sem.clone().acquire_owned().await.unwrap();
        let provider = (provider_factory)();
        let tools_raw = (toolset_factory)();
        let blocked = blocked.clone();
        let model = model.clone();

        set.spawn(async move {
            let _permit = permit;
            // Filter the child's toolset. Default blocklist plus caller
            // additions. Built inside the task so parent toolset references
            // aren't held across await points.
            let tools: Vec<Box<dyn Tool>> = tools_raw
                .into_iter()
                .filter(|t| !blocked.iter().any(|b| b == t.name()))
                .collect();
            let res = run_single(&task, provider, model, tools, max_turns).await;
            (i, task.goal, res)
        });
    }

    let mut collected: Vec<(usize, DelegateResult)> = Vec::new();
    while let Some(joined) = set.join_next().await {
        let (i, goal, res) = joined.map_err(|e| {
            cersei_types::CerseiError::Config(format!("delegate join: {e}"))
        })?;
        match res {
            Ok(r) => collected.push((i, r)),
            Err(e) => collected.push((
                i,
                DelegateResult {
                    goal,
                    summary: String::new(),
                    error: Some(e.to_string()),
                    turns: 0,
                },
            )),
        }
    }

    collected.sort_by_key(|(i, _)| *i);
    Ok(collected.into_iter().map(|(_, r)| r).collect())
}

async fn run_single(
    task: &DelegateTask,
    provider: Box<dyn Provider + Send + Sync>,
    model: Option<String>,
    tools: Vec<Box<dyn Tool>>,
    max_turns: u32,
) -> Result<DelegateResult> {
    let system = build_child_system_prompt(task);

    // Cast `Box<dyn Provider + Send + Sync>` to the plain `Box<dyn Provider>`
    // the builder accepts. `Send + Sync` are strict supersets of the builder's
    // requirement, so the cast is free.
    let provider_boxed: Box<dyn Provider> = provider;
    let mut builder = Agent::builder()
        .provider_boxed(provider_boxed)
        .system_prompt(system)
        .max_turns(max_turns)
        .tools(tools);
    if let Some(m) = model {
        builder = builder.model(m);
    }

    let child = builder.build()?;
    let output = child.run(&task.goal).await?;

    Ok(DelegateResult {
        goal: task.goal.clone(),
        summary: output.text().to_string(),
        error: None,
        turns: output.turns,
    })
}

/// Build the child system prompt. Verbatim port of
/// `_inspirations/hermes-agent/tools/delegate_tool.py::_build_child_system_prompt`
/// — paraphrasing costs us the bench parity guarantee.
pub fn build_child_system_prompt(task: &DelegateTask) -> String {
    let mut parts: Vec<String> = Vec::with_capacity(6);
    parts.push("You are a focused subagent working on a specific delegated task.".into());
    parts.push(String::new());
    parts.push(format!("YOUR TASK:\n{}", task.goal));

    if let Some(ctx) = task.context.as_deref() {
        if !ctx.trim().is_empty() {
            parts.push(format!("\nCONTEXT:\n{ctx}"));
        }
    }

    if let Some(wp) = task.workspace.as_ref() {
        let s = wp.display().to_string();
        if !s.trim().is_empty() {
            parts.push(format!(
                "\nWORKSPACE PATH:\n{s}\nUse this exact path for local repository/workdir operations unless the task explicitly says otherwise."
            ));
        }
    }

    parts.push(
        "\nComplete this task using the tools available to you. When finished, provide a clear, concise summary of:\n- What you did\n- What you found or accomplished\n- Any files you created or modified\n- Any issues encountered\n\nImportant workspace rule: Never assume a repository lives at /workspace/... or any other container-style path unless the task/context explicitly gives that path. If no exact local path is provided, discover it first before issuing git/workdir-specific commands.\n\nBe thorough but concise — your response is returned to the parent agent as a summary.".into()
    );

    parts.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn child_prompt_includes_goal() {
        let p = build_child_system_prompt(&DelegateTask::new("fix the tests"));
        assert!(p.contains("YOUR TASK:\nfix the tests"));
        assert!(p.contains("focused subagent"));
        assert!(p.contains("What you did"));
    }

    #[test]
    fn child_prompt_includes_context_when_present() {
        let p = build_child_system_prompt(
            &DelegateTask::new("fix").with_context("error at line 42"),
        );
        assert!(p.contains("CONTEXT:\nerror at line 42"));
    }

    #[test]
    fn child_prompt_skips_empty_context() {
        let p = build_child_system_prompt(&DelegateTask::new("g").with_context("   "));
        assert!(!p.contains("CONTEXT:"));
    }

    #[test]
    fn child_prompt_includes_workspace_when_present() {
        let p = build_child_system_prompt(
            &DelegateTask::new("g").with_workspace("/abs/path/to/repo"),
        );
        assert!(p.contains("WORKSPACE PATH:\n/abs/path/to/repo"));
    }

    #[test]
    fn default_blocklist_covers_recursion_and_memory() {
        assert!(DELEGATE_BLOCKED_TOOLS.contains(&"delegate"));
        assert!(DELEGATE_BLOCKED_TOOLS.contains(&"memory"));
        assert!(DELEGATE_BLOCKED_TOOLS.contains(&"send_message"));
    }
}
