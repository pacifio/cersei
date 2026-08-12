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
pub mod multi_edit;
pub mod notebook_edit;
pub mod permissions;
pub mod plan_mode;
pub mod powershell;
pub mod pricing;
pub mod remote_trigger;
pub mod send_message;
pub mod skill_tool;
pub mod skills;
pub mod sleep;
pub mod synthetic_output;
pub mod tasks;
pub mod todo_write;
pub mod tool_feedback;
pub mod tool_primitives;
pub mod tool_search;
#[cfg(feature = "vms")]
pub mod vm_tools;
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
        let usage = Usage {
            cost_usd: Some(0.0),
            ..Default::default()
        };
        Self {
            usage: parking_lot::Mutex::new(usage),
        }
    }

    /// Restore a session tracker when Abstract rebuilds an agent after a
    /// provider/model switch.
    pub fn with_usage(mut usage: Usage) -> Self {
        if usage.total() == 0 && usage.cost_usd.is_none() {
            usage.cost_usd = Some(0.0);
        }
        Self {
            usage: parking_lot::Mutex::new(usage),
        }
    }

    pub fn add(&self, usage: &Usage) {
        self.usage.lock().merge(usage);
    }

    /// Add provider usage and price this turn exclusively from the local
    /// Portkey rate for the canonical `provider/model` identity.
    ///
    /// Once a turn is unpriced, the cumulative price remains unavailable: a
    /// known subtotal is not the cost of the complete session.
    pub fn add_with_model(&self, usage: &Usage, identity: &str) -> Option<f64> {
        let turn_cost = estimate_cost_usage(identity, usage);
        let mut total = self.usage.lock();
        let prior_cost = total.cost_usd;
        total.merge(usage);
        total.cost_usd = match (prior_cost, turn_cost) {
            (Some(prior), Some(turn)) => Some(prior + turn),
            _ => None,
        };
        turn_cost
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

/// Calculate a usage snapshot from Portkey, or return `None` when the exact
/// provider/model or a required cache rate is unavailable.
pub fn estimate_cost_usage(identity: &str, usage: &Usage) -> Option<f64> {
    pricing::resolve_identity(identity)?.cost(usage)
}

pub fn estimate_cost(identity: &str, input_tokens: u64, output_tokens: u64) -> Option<f64> {
    estimate_cost_usage(
        identity,
        &Usage {
            input_tokens,
            output_tokens,
            ..Default::default()
        },
    )
}

#[cfg(test)]
mod portkey_cost_tests {
    use super::*;

    fn assert_close(actual: Option<f64>, expected: f64) {
        let actual = actual.expect("price should be available");
        assert!(
            (actual - expected).abs() < 1e-12,
            "expected {expected}, got {actual}"
        );
    }

    fn cache_from_fixture(provider: &str, fixture: &str) -> crate::pricing::PricingCache {
        let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(fixture);
        let json = std::fs::read_to_string(path).unwrap();
        let catalog = crate::pricing::parse_catalog(&json).unwrap();
        let mut cache = crate::pricing::PricingCache::default();
        cache.providers.insert(provider.to_string(), catalog);
        cache
    }

    #[test]
    fn portkey_drives_cost_and_accumulates_all_counters() {
        let _guard = crate::pricing::_lock_global_cache_for_tests();
        crate::pricing::_set_global_cache_for_tests(cache_from_fixture(
            "deepseek",
            "portkey_deepseek.json",
        ));
        let tracker = CostTracker::new();
        let usage = Usage {
            input_tokens: 1_000_000,
            output_tokens: 1_000_000,
            cache_read_input_tokens: 1_000_000,
            ..Default::default()
        };

        assert_close(
            tracker.add_with_model(&usage, "deepseek/deepseek-chat"),
            0.4228,
        );
        assert_close(tracker.current().cost_usd, 0.4228);
        assert_eq!(tracker.current().cache_read_input_tokens, 1_000_000);
    }

    #[test]
    fn deepseek_identity_never_uses_openai_pricing() {
        let _guard = crate::pricing::_lock_global_cache_for_tests();
        let mut cache = cache_from_fixture("deepseek", "portkey_deepseek.json");
        cache
            .providers
            .extend(cache_from_fixture("openai", "portkey_openai.json").providers);
        crate::pricing::_set_global_cache_for_tests(cache);
        let usage = Usage {
            input_tokens: 1_000_000,
            output_tokens: 1_000_000,
            ..Default::default()
        };

        assert_close(estimate_cost_usage("deepseek/deepseek-chat", &usage), 0.42);
        assert_close(estimate_cost_usage("openai/gpt-4o", &usage), 12.5);
        assert_eq!(estimate_cost_usage("deepseek-chat", &usage), None);
    }

    #[test]
    fn deepseek_cache_hit_and_miss_use_distinct_portkey_rates() {
        let _guard = crate::pricing::_lock_global_cache_for_tests();
        crate::pricing::_set_global_cache_for_tests(cache_from_fixture(
            "deepseek",
            "portkey_deepseek.json",
        ));
        let usage = Usage {
            // DeepSeek prompt_cache_miss_tokens: $0.14/M.
            input_tokens: 1_000_000,
            // DeepSeek prompt_cache_hit_tokens: $0.0028/M.
            cache_read_input_tokens: 1_000_000,
            ..Default::default()
        };

        assert_close(
            estimate_cost_usage("deepseek/deepseek-chat", &usage),
            0.1428,
        );
    }

    #[test]
    fn unknown_price_stays_unknown_for_the_complete_session() {
        let _guard = crate::pricing::_lock_global_cache_for_tests();
        crate::pricing::_set_global_cache_for_tests(crate::pricing::PricingCache::default());
        let tracker = CostTracker::new();
        let usage = Usage {
            input_tokens: 100,
            ..Default::default()
        };

        assert_eq!(tracker.add_with_model(&usage, "unknown/model"), None);
        assert_eq!(tracker.current().cost_usd, None);

        crate::pricing::_set_global_cache_for_tests(cache_from_fixture(
            "deepseek",
            "portkey_deepseek.json",
        ));
        assert!(tracker
            .add_with_model(&usage, "deepseek/deepseek-chat")
            .is_some());
        assert_eq!(tracker.current().cost_usd, None);
    }

    #[test]
    fn restored_tracker_keeps_known_cost_across_repl_switch() {
        let _guard = crate::pricing::_lock_global_cache_for_tests();
        crate::pricing::_set_global_cache_for_tests(cache_from_fixture(
            "deepseek",
            "portkey_deepseek.json",
        ));
        let tracker = CostTracker::with_usage(Usage {
            input_tokens: 1_000_000,
            cost_usd: Some(1.0),
            ..Default::default()
        });
        tracker.add_with_model(
            &Usage {
                input_tokens: 1_000_000,
                output_tokens: 1_000_000,
                ..Default::default()
            },
            "deepseek/deepseek-chat",
        );

        assert_close(tracker.current().cost_usd, 1.42);
    }
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

/// All built-in tools.
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
    // Last, so it indexes every tool registered above.
    let search = tool_search::ToolSearchTool::new(&tools);
    tools.push(Box::new(search));
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

/// File system tools: Read, Write, Edit, MultiEdit, ApplyPatch, Glob, Grep, NotebookEdit.
pub fn filesystem() -> Vec<Box<dyn Tool>> {
    vec![
        Box::new(file_read::FileReadTool),
        Box::new(file_write::FileWriteTool),
        Box::new(file_edit::FileEditTool),
        Box::new(multi_edit::MultiEditTool),
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

// ─── Unknown-parameter policy (F-10) ─────────────────────────────────────────

/// One policy, applied to every tool: a parameter a tool does not declare is an
/// error, never a silent drop.
///
/// The alternative — accepting near-miss names per tool — was rejected because
/// partial leniency is what caused the bug. `Edit` accepted `path` as an alias
/// for `file_path`, so a model that guessed `path` was *rewarded*, carried the
/// hypothesis to `Grep`, and there `path` means something else entirely: the
/// unknown key was dropped, the search silently widened to the whole working
/// directory, and up to 250 matches from unrelated files came back as though
/// they came from the one file the model asked about. No layer emitted an
/// error, so nothing downstream could recover from it.
///
/// Rejecting is only viable because the rejection is *actionable*:
/// [`tool_feedback`] turns serde's unknown-field error into a message that
/// names the tool, echoes the arguments, points at the parameter the model
/// probably meant, and prints a corrected call.
#[cfg(test)]
mod unknown_parameter_policy {
    use super::*;
    use crate::permissions::AllowAll;
    use std::sync::Arc;

    /// A key no tool declares. Deliberately unmistakable in failure output,
    /// and deliberately *not* `__`-prefixed: that prefix is reserved for the
    /// provider's wire markers and is skipped by the near-miss reporter.
    const UNKNOWN_KEY: &str = "cersei_probe_bogus_param";

    /// The deserializer's wording when it refuses a key it does not know.
    ///
    /// Asserting on this specific phrase is the point of the test. A tool that
    /// merely *echoes* the arguments back inside some other complaint — "missing
    /// field `pattern`" — looks like a rejection but has still silently dropped
    /// the unknown key, which is the bug. Only a deserializer that actually
    /// refuses the key produces this.
    const REJECTION: &str = "unknown field";

    fn ctx_in(dir: &std::path::Path) -> ToolContext {
        ToolContext {
            working_dir: dir.to_path_buf(),
            session_id: "unknown-param-test".into(),
            permissions: Arc::new(AllowAll),
            cost_tracker: Arc::new(CostTracker::new()),
            mcp_manager: None,
            extensions: Extensions::default(),
        }
    }

    fn required_params(tool: &dyn Tool) -> Vec<String> {
        tool.input_schema()
            .get("required")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Every tool must reject a parameter it does not declare.
    ///
    /// The probe sends *only* the unknown key. That is deliberate, and is what
    /// makes running this against the real registry safe: every tool covered
    /// here has at least one required parameter, so the call can never
    /// deserialize into a runnable request and no tool body executes — not
    /// `Bash`, not `Write`, not `CronCreate`. The assertion is only about which
    /// *error* comes back.
    ///
    /// Before `deny_unknown_fields`, the unknown key was discarded during
    /// deserialization and the resulting complaint named the missing required
    /// field, never the key the model actually got wrong.
    #[tokio::test]
    async fn every_tool_rejects_an_unknown_parameter() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = ctx_in(tmp.path());

        let mut covered = 0usize;
        let mut failures: Vec<String> = Vec::new();

        for tool in all() {
            // A tool with no required parameter would actually run on this
            // input, so it is not probed this way.
            if required_params(tool.as_ref()).is_empty() {
                continue;
            }
            covered += 1;

            let res = tool
                .execute(serde_json::json!({ UNKNOWN_KEY: "x" }), &ctx)
                .await;

            if !res.is_error {
                failures.push(format!(
                    "{}: accepted an unknown parameter (silent drop)",
                    tool.name()
                ));
            } else if !res.content.contains(REJECTION) {
                failures.push(format!(
                    "{}: failed for some other reason, so the unknown key was still \
                     dropped rather than refused — got: {}",
                    tool.name(),
                    res.content.lines().next().unwrap_or("")
                ));
            } else if !res.content.contains(UNKNOWN_KEY) {
                failures.push(format!(
                    "{}: refused a key without naming it — got: {}",
                    tool.name(),
                    res.content.lines().next().unwrap_or("")
                ));
            }
        }

        assert!(
            covered >= 25,
            "coverage collapsed to {covered} tools; the filter is hiding the registry"
        );
        assert!(
            failures.is_empty(),
            "{} of {} tools mishandled an unknown parameter:\n  {}",
            failures.len(),
            covered,
            failures.join("\n  ")
        );
    }

    /// F-10's exact scenario: the model reads a file with `file_path`, then
    /// searches it with `Grep`, whose parameter is `path`.
    ///
    /// The dangerous outcome is not a failed search — it is a *successful* one.
    /// With `file_path` dropped, `Grep` fell back to the working directory and
    /// returned matches from files the model never asked about, with nothing in
    /// the result to say the scope had changed.
    #[tokio::test]
    async fn grep_does_not_silently_widen_to_the_whole_working_directory() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("target.txt"), "fn login() {}\n").unwrap();
        std::fs::create_dir(tmp.path().join("elsewhere")).unwrap();
        std::fs::write(
            tmp.path().join("elsewhere/decoy.txt"),
            "fn login_unrelated() {}\n",
        )
        .unwrap();

        let res = grep_tool::GrepTool
            .execute(
                serde_json::json!({
                    "pattern": "login",
                    // Wrong name: Grep declares `path`, not `file_path`.
                    "file_path": tmp.path().join("target.txt").to_str().unwrap(),
                }),
                &ctx_in(tmp.path()),
            )
            .await;

        assert!(
            res.is_error,
            "Grep accepted `file_path` and searched somewhere else instead; it returned: {}",
            res.content
        );
        assert!(
            !res.content.contains("decoy"),
            "result leaked matches from outside the requested file: {}",
            res.content
        );
        assert!(
            res.content.contains("file_path"),
            "error must quote the parameter the model sent: {}",
            res.content
        );
        assert!(
            res.content.contains("path"),
            "error must name the real parameter: {}",
            res.content
        );
    }

    /// The other half of F-10: `Edit` accepted `path` as an alias, which is
    /// where the model *learned* the wrong name before carrying it to `Grep`.
    /// One tool rewarding a guess that every other tool punishes is worse than
    /// either policy applied consistently.
    #[tokio::test]
    async fn edit_no_longer_teaches_the_path_alias() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("f.rs");
        std::fs::write(&file, "let x = 1;\n").unwrap();

        let res = file_edit::FileEditTool
            .execute(
                serde_json::json!({
                    "path": file.to_str().unwrap(),
                    "old_string": "let x = 1;",
                    "new_string": "let x = 2;",
                }),
                &ctx_in(tmp.path()),
            )
            .await;

        assert!(
            res.is_error,
            "Edit still accepts the `path` alias, so it keeps teaching a name \
             that Grep and Glob silently mis-handle"
        );
        assert!(
            res.content.contains("file_path"),
            "error must name the real parameter: {}",
            res.content
        );
        assert_eq!(
            std::fs::read_to_string(&file).unwrap(),
            "let x = 1;\n",
            "a rejected edit must not have touched the file"
        );
    }
}
