//! cersei-tools: Tool trait, built-in tool implementations, and permission system.

pub mod apply_patch;
pub mod ask_user;
pub mod bash;
pub mod bash_classifier;
pub mod code_search;
pub mod config_tool;
pub mod cron;
pub mod exa_search;
pub mod file_edit;
pub mod file_history;
pub mod file_read;
pub mod file_snapshot;
pub mod file_watcher;
pub mod file_write;
pub mod git_utils;
pub mod glob_tool;
pub mod grep_tool;
pub mod lsp_tool;
pub mod notebook_edit;
pub mod permissions;
pub mod plan_mode;
pub mod powershell;
pub mod remote_trigger;
pub mod send_message;
pub mod skill_tool;
pub mod skills;
pub mod sleep;
pub mod synthetic_output;
pub mod tasks;
pub mod todo_write;
pub mod tool_primitives;
pub mod tool_search;
pub mod web_fetch;
pub mod web_search;
pub mod worktree;

use async_trait::async_trait;
use cersei_mcp::McpManager;
use cersei_types::*;
use serde_json::Value;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

// ─── Tool trait ──────────────────────────────────────────────────────────────

#[async_trait]
pub trait Tool: Send + Sync {
    /// Tool name (used by the model to invoke it).
    fn name(&self) -> &str;

    /// Human-readable description shown to the model.
    fn description(&self) -> &str;

    /// JSON Schema for the tool's input parameters.
    fn input_schema(&self) -> Value;

    /// Permission level required for this tool.
    fn permission_level(&self) -> PermissionLevel {
        PermissionLevel::None
    }

    /// Category for grouping in tool listings.
    fn category(&self) -> ToolCategory {
        ToolCategory::Custom
    }

    /// Execute the tool with the given JSON input.
    async fn execute(&self, input: Value, ctx: &ToolContext) -> ToolResult;

    /// Convert to a ToolDefinition for the provider.
    fn to_definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: self.name().to_string(),
            description: self.description().to_string(),
            input_schema: self.input_schema(),
        }
    }
}

/// Typed tool execution trait — used with `#[derive(Tool)]`.
#[async_trait]
pub trait ToolExecute: Send + Sync {
    type Input: serde::de::DeserializeOwned + schemars::JsonSchema;

    async fn run(&self, input: Self::Input, ctx: &ToolContext) -> ToolResult;
}

// ─── Permission levels ───────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PermissionLevel {
    None,
    ReadOnly,
    Write,
    Execute,
    Dangerous,
    Forbidden,
}

// ─── Tool categories ─────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolCategory {
    FileSystem,
    Shell,
    Web,
    Memory,
    Orchestration,
    Mcp,
    Custom,
}

// ─── Tool result ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct ToolResult {
    pub content: String,
    pub is_error: bool,
    pub metadata: Option<Value>,
}

impl ToolResult {
    pub fn success(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            is_error: false,
            metadata: None,
        }
    }

    pub fn error(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            is_error: true,
            metadata: None,
        }
    }

    pub fn with_metadata(mut self, meta: Value) -> Self {
        self.metadata = Some(meta);
        self
    }
}

// ─── Tool context ────────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct ToolContext {
    pub working_dir: PathBuf,
    pub session_id: String,
    pub permissions: Arc<dyn permissions::PermissionPolicy>,
    pub cost_tracker: Arc<CostTracker>,
    pub mcp_manager: Option<Arc<McpManager>>,
    pub extensions: Extensions,
}

/// Type-map for injecting custom data into the tool context.
#[derive(Clone, Default)]
pub struct Extensions {
    data: Arc<dashmap::DashMap<std::any::TypeId, Arc<dyn std::any::Any + Send + Sync>>>,
}

impl Extensions {
    pub fn insert<T: Send + Sync + 'static>(&self, val: T) {
        self.data.insert(std::any::TypeId::of::<T>(), Arc::new(val));
    }

    pub fn get<T: Send + Sync + 'static>(&self) -> Option<Arc<T>> {
        self.data
            .get(&std::any::TypeId::of::<T>())
            .and_then(|v| Arc::clone(v.value()).downcast::<T>().ok())
    }
}

/// Tracks cumulative token usage and cost.
pub struct CostTracker {
    usage: parking_lot::Mutex<Usage>,
}

impl CostTracker {
    pub fn new() -> Self {
        Self {
            usage: parking_lot::Mutex::new(Usage::default()),
        }
    }

    pub fn add(&self, usage: &Usage) {
        self.usage.lock().merge(usage);
    }

    /// Add usage with cost estimation based on model pricing.
    pub fn add_with_model(&self, usage: &Usage, model: &str) {
        let mut u = usage.clone();
        if u.cost_usd.is_none() || u.cost_usd == Some(0.0) {
            u.cost_usd = Some(estimate_cost(model, u.input_tokens, u.output_tokens));
        }
        self.usage.lock().merge(&u);
    }

    pub fn current(&self) -> Usage {
        self.usage.lock().clone()
    }
}

impl Default for CostTracker {
    fn default() -> Self {
        Self::new()
    }
}

/// Estimate USD cost from token counts based on model pricing (per 1M tokens).
pub fn estimate_cost(model: &str, input_tokens: u64, output_tokens: u64) -> f64 {
    let (input_per_m, output_per_m) = match model {
        m if m.contains("gpt-5.3") => (2.0, 10.0),
        m if m.contains("gpt-5") => (2.0, 10.0),
        m if m.contains("gpt-4o") => (2.50, 10.0),
        m if m.contains("gpt-4-turbo") => (10.0, 30.0),
        m if m.starts_with("o1") => (15.0, 60.0),
        m if m.starts_with("o3") => (10.0, 40.0),
        m if m.contains("opus") => (15.0, 75.0),
        m if m.contains("sonnet") => (3.0, 15.0),
        m if m.contains("haiku") => (0.25, 1.25),
        m if m.contains("gemini-2.0-flash") => (0.075, 0.30),
        m if m.contains("gemini") => (1.25, 5.0),
        m if m.contains("deepseek") => (0.27, 1.10),
        m if m.contains("mistral-large") => (2.0, 6.0),
        m if m.contains("llama") => (0.0, 0.0), // local/free
        _ => (2.0, 10.0),
    };
    (input_tokens as f64 / 1_000_000.0) * input_per_m
        + (output_tokens as f64 / 1_000_000.0) * output_per_m
}

// ─── Shell state (persisted across Bash invocations) ─────────────────────────

#[derive(Debug, Clone, Default)]
pub struct ShellState {
    pub cwd: Option<PathBuf>,
    pub env_vars: HashMap<String, String>,
}

static SHELL_STATE_REGISTRY: once_cell::sync::Lazy<
    dashmap::DashMap<String, Arc<parking_lot::Mutex<ShellState>>>,
> = once_cell::sync::Lazy::new(dashmap::DashMap::new);

pub fn session_shell_state(session_id: &str) -> Arc<parking_lot::Mutex<ShellState>> {
    SHELL_STATE_REGISTRY
        .entry(session_id.to_string())
        .or_insert_with(|| Arc::new(parking_lot::Mutex::new(ShellState::default())))
        .clone()
}

pub fn clear_session_shell_state(session_id: &str) {
    SHELL_STATE_REGISTRY.remove(session_id);
}

// ─── Built-in tool sets ──────────────────────────────────────────────────────

/// All built-in tools (35 tools).
pub fn all() -> Vec<Box<dyn Tool>> {
    let mut tools: Vec<Box<dyn Tool>> = Vec::new();
    tools.extend(filesystem());
    tools.extend(shell());
    tools.extend(web());
    tools.extend(planning());
    tools.extend(scheduling());
    tools.extend(orchestration());
    tools.push(Box::new(ask_user::AskUserQuestionTool));
    tools.push(Box::new(synthetic_output::SyntheticOutputTool));
    tools.push(Box::new(config_tool::ConfigTool));
    tools
}

/// All coding-oriented tools (filesystem + shell + web).
pub fn coding() -> Vec<Box<dyn Tool>> {
    let mut tools: Vec<Box<dyn Tool>> = Vec::new();
    tools.extend(filesystem());
    tools.extend(shell());
    tools.extend(web());
    tools
}

/// File system tools: Read, Write, Edit, Glob, Grep, NotebookEdit.
pub fn filesystem() -> Vec<Box<dyn Tool>> {
    vec![
        Box::new(file_read::FileReadTool),
        Box::new(file_write::FileWriteTool),
        Box::new(file_edit::FileEditTool),
        Box::new(apply_patch::ApplyPatchTool),
        Box::new(glob_tool::GlobTool),
        Box::new(grep_tool::GrepTool),
        Box::new(code_search::CodeSearchTool::new()),
        Box::new(notebook_edit::NotebookEditTool),
    ]
}

/// Shell tools: Bash, PowerShell.
pub fn shell() -> Vec<Box<dyn Tool>> {
    vec![
        Box::new(bash::BashTool),
        Box::new(powershell::PowerShellTool),
    ]
}

/// Web tools: WebFetch, WebSearch, ExaSearch.
pub fn web() -> Vec<Box<dyn Tool>> {
    vec![
        Box::new(web_fetch::WebFetchTool),
        Box::new(web_search::WebSearchTool),
        Box::new(exa_search::ExaSearchTool),
    ]
}

/// Planning tools: EnterPlanMode, ExitPlanMode, TodoWrite.
pub fn planning() -> Vec<Box<dyn Tool>> {
    vec![
        Box::new(plan_mode::EnterPlanModeTool),
        Box::new(plan_mode::ExitPlanModeTool),
        Box::new(todo_write::TodoWriteTool),
    ]
}

/// Scheduling tools: Cron (Create/List/Delete), Sleep, RemoteTrigger.
pub fn scheduling() -> Vec<Box<dyn Tool>> {
    vec![
        Box::new(cron::CronCreateTool),
        Box::new(cron::CronListTool),
        Box::new(cron::CronDeleteTool),
        Box::new(sleep::SleepTool),
        Box::new(remote_trigger::RemoteTriggerTool),
    ]
}

/// Orchestration tools: SendMessage, Tasks, Worktree.
pub fn orchestration() -> Vec<Box<dyn Tool>> {
    vec![
        Box::new(send_message::SendMessageTool),
        Box::new(tasks::TaskCreateTool),
        Box::new(tasks::TaskGetTool),
        Box::new(tasks::TaskUpdateTool),
        Box::new(tasks::TaskListTool),
        Box::new(tasks::TaskStopTool),
        Box::new(tasks::TaskOutputTool),
        Box::new(worktree::EnterWorktreeTool),
        Box::new(worktree::ExitWorktreeTool),
    ]
}

/// No tools (for pure chat agents).
pub fn none() -> Vec<Box<dyn Tool>> {
    vec![]
}
