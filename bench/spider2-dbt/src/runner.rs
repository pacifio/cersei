//! Per-task orchestration loop.
//!
//! Phase-2 shape: load task → prepare workdir → main agent → parallel
//! verifier+auditor delegate → optional repair pass → optional second-model
//! repair → comparator. Each phase is a flag, so the same binary covers
//! Phase-1 baselines and Phase-2 ablations.

use anyhow::{Context, Result};
use cersei_agent::delegate::{run_batch, DelegateConfig, DelegateTask};
use cersei_agent::Agent;
use cersei_provider::{Gemini, Provider};
use cersei_tools::Tool;
use std::future::Future;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::comparator::{evaluate, find_gold_db, find_result_db, EvalOutcome};
use crate::dataset::{load_eval, SuiteLayout, Task};
use crate::dates::GoldDates;
use crate::report::{truncate_details, TaskResult};
use crate::workdir::{prepare, Workdir};

const SYSTEM_PROMPT_TEMPLATE: &str = include_str!("../prompts/system.md");
const VERIFIER_PROMPT_TEMPLATE: &str = include_str!("../prompts/verifier.md");
const AUDITOR_PROMPT_TEMPLATE: &str = include_str!("../prompts/auditor.md");

#[derive(Debug, Clone)]
pub struct RunConfig {
    pub model: String,
    pub max_turns: u32,
    pub working_root: PathBuf,
    pub deterministic_dates: bool,
    pub gold_dates: Arc<GoldDates>,
    pub skill_pack: String,
    /// Phase-2 toggles (default off). Each `enable_*` adds one extra LLM phase.
    pub enable_verifier: bool,
    pub enable_auditor: bool,
    /// Repair-pass cap. `0` = no repair after verifier/auditor. `1` = one
    /// extra agent run on the same model. SignalPilot does up to 1.
    pub repair_pass_max: u32,
    /// Optional second-model repair: when the primary model + repair pass both
    /// fail, run one more pass on this fallback model (e.g. `gemini-2.5-flash`).
    pub repair_model: Option<String>,
    /// Per-child cap inside `delegate::run_batch`.
    pub child_max_turns: u32,
    /// Hard wall-clock cap, in seconds, applied to each individual phase
    /// (main agent, delegate batch, repair, second-model repair). Prevents
    /// a single stuck turn from burning hours. `0` disables.
    pub phase_timeout_secs: u64,
}

pub async fn run_one(
    layout: &SuiteLayout,
    task: &Task,
    cfg: &RunConfig,
) -> Result<TaskResult> {
    let started = Instant::now();
    let work_subdir = cfg.working_root.join(&task.instance_id);

    // Load eval params first — `prepare()` uses `condition_tabs` to filter the
    // reference snapshot to eval-relevant tables only (matches SignalPilot's
    // direct.py:81-95 stub-and-siblings filter).
    let eval_params = load_eval(layout, &task.instance_id)
        .context("load eval params")?
        .ok_or_else(|| {
            anyhow::anyhow!(
                "no eval entry in {} for {}",
                layout.eval_jsonl.display(),
                task.instance_id
            )
        })?;
    let workdir = prepare(
        layout,
        task,
        &work_subdir,
        eval_params.condition_tabs.as_deref(),
    )
    .context("prepare workdir")?;

    let api_key = std::env::var("GOOGLE_API_KEY")
        .context("GOOGLE_API_KEY not set in env (.env is gitignored — source it)")?;

    // ── Phase 1: main agent run ──────────────────────────────────────────────
    let eval_tables_md = render_eval_tables(&eval_params);
    let system = build_system_prompt(task, &workdir, &cfg.skill_pack, &eval_tables_md);
    let provider: Box<dyn Provider> = Box::new(Gemini::new(api_key.clone()));
    let log_id = task.instance_id.clone();
    let agent = Agent::builder()
        .provider_boxed(provider)
        .model(&cfg.model)
        .system_prompt(system)
        .max_turns(cfg.max_turns)
        .tools(domain_tools())
        .working_dir(&workdir.root)
        .permission_policy(cersei_tools::permissions::AllowAll)
        .on_event(move |e| log_agent_event(&log_id, e))
        .build()
        .context("build main agent")?;

    let main_prompt = format!(
        "Task: {}\n\nWork in {}. Reference snapshot is at reference_snapshot.md \
         (read it first). Build every model in models/*.yml as a DuckDB table. \
         When done, all required tables must exist with correct column types and \
         row counts.",
        task.instruction,
        workdir.root.display()
    );

    let phase_started = Instant::now();
    tracing::info!(
        instance_id = %task.instance_id,
        max_turns = cfg.max_turns,
        timeout_secs = cfg.phase_timeout_secs,
        "phase 1: main agent starting"
    );
    let main_result = match run_with_phase_timeout(agent.run(&main_prompt), &cfg, "main agent").await {
        Ok(Ok(out)) => out,
        Ok(Err(e)) => {
            return Ok(make_failure(task, started, 0, format!("main agent: {e}"), &cfg, false));
        }
        Err(e) => {
            return Ok(make_failure(task, started, 0, e, &cfg, false));
        }
    };
    tracing::info!(
        instance_id = %task.instance_id,
        turns = main_result.turns,
        elapsed_ms = phase_started.elapsed().as_millis(),
        "phase 1: main agent done"
    );
    let mut total_turns = main_result.turns;

    // Capture the last failing `dbt run`/`dbt build` stderr from the main
    // agent's tool history. SignalPilot's repair prompt feeds the raw error
    // back to the model — without this, the repair agent only sees comparator
    // symptoms and re-derives the fix from scratch.
    let last_dbt_error = extract_last_dbt_error(&main_result.tool_calls);

    // ── Phase 2a: SEQUENTIAL verifier (sees post-main state) ─────────────────
    //
    // SignalPilot runs the verifier as the second leg of the SDK call, after
    // the main agent finishes. We mirror that: verifier first (so it can
    // re-build / fix missing tables), then auditor. Running them in parallel
    // (the previous shape) meant the auditor saw stale state half the time.
    let mut verifier_summary = String::new();
    let mut auditor_summary = String::new();
    if cfg.enable_verifier || cfg.enable_auditor {
        // Sequential one-task batches. We could also call the agent
        // directly — using delegate keeps the depth-cap + blocklist and
        // matches the established pattern.
        let mut sequential_tasks: Vec<(&'static str, DelegateTask)> = Vec::new();
        if cfg.enable_verifier {
            sequential_tasks.push((
                "verifier",
                DelegateTask::new(format_verifier_goal(task))
                    .with_context(format_verifier_context(task, &workdir))
                    .with_workspace(workdir.root.clone()),
            ));
        }
        if cfg.enable_auditor {
            sequential_tasks.push((
                "auditor",
                DelegateTask::new(format_auditor_goal(task))
                    .with_context(format_auditor_context(task, &workdir))
                    .with_workspace(workdir.root.clone()),
            ));
        }

        for (label, dt) in sequential_tasks {
            let key = api_key.clone();
            let model = cfg.model.clone();
            let provider_factory: cersei_agent::delegate::ProviderFactory =
                Arc::new(move || {
                    let p: Box<dyn Provider + Send + Sync> = Box::new(Gemini::new(key.clone()));
                    p
                });
            let toolset_factory: cersei_agent::delegate::ToolsetFactory =
                Arc::new(|| {
                    domain_tools()
                });
            let dc = DelegateConfig {
                tasks: vec![dt],
                provider_factory,
                toolset_factory,
                model: Some(model),
                max_turns: cfg.child_max_turns,
                max_concurrent: 1,
                depth: 1,
                extra_blocked: Vec::new(),
            };
            let phase_started = Instant::now();
            tracing::info!(
                instance_id = %task.instance_id,
                phase = label,
                "phase 2a: subagent starting"
            );
            let batch_outcome = run_with_phase_timeout(run_batch(dc), &cfg, label).await;
            let results_opt: Option<Vec<_>> = match batch_outcome {
                Ok(Ok(r)) => Some(r),
                Ok(Err(e)) => { tracing::warn!(error = %e, "delegate batch error"); None }
                Err(e) => { tracing::warn!(error = %e, "delegate batch timeout"); None }
            };
            tracing::info!(
                instance_id = %task.instance_id,
                phase = label,
                elapsed_ms = phase_started.elapsed().as_millis(),
                "phase 2a: subagent done"
            );
            if let Some(results) = results_opt {
                if let Some(r) = results.into_iter().next() {
                    total_turns = total_turns.saturating_add(r.turns);
                    match label {
                        "verifier" => verifier_summary = r.summary,
                        "auditor" => auditor_summary = r.summary,
                        _ => {}
                    }
                }
            }
        }
    }

    // ── Phase 2b: comparator pass-1 ──────────────────────────────────────────
    let outcome = run_comparator(layout, task, &workdir, &eval_params)?;
    if outcome.pass {
        return Ok(finish(task, started, total_turns, outcome, &cfg));
    }

    // ── Phase 2c: repair pass on the primary model ───────────────────────────
    let mut repair_used = false;
    if cfg.repair_pass_max > 0 {
        repair_used = true;
        let provider: Box<dyn Provider> = Box::new(Gemini::new(api_key.clone()));
        let repair = Agent::builder()
            .provider_boxed(provider)
            .model(&cfg.model)
            .system_prompt(build_repair_system_prompt(&cfg.skill_pack))
            .max_turns(30)
            .tools(domain_tools())
            .working_dir(&workdir.root)
            .permission_policy(cersei_tools::permissions::AllowAll)
            .build()
            .context("build repair agent")?;
        let prompt = build_repair_prompt(
            task,
            &workdir,
            &outcome,
            verifier_summary.as_str(),
            auditor_summary.as_str(),
            last_dbt_error.as_deref(),
        );
        let phase_started = Instant::now();
        tracing::info!(instance_id = %task.instance_id, "phase 2c: repair pass starting");
        match run_with_phase_timeout(repair.run(&prompt), &cfg, "repair pass").await {
            Ok(Ok(out)) => total_turns = total_turns.saturating_add(out.turns),
            Ok(Err(e)) => tracing::warn!(error = %e, "repair pass failed"),
            Err(e) => tracing::warn!(error = %e, "repair pass timed out"),
        }
        tracing::info!(
            instance_id = %task.instance_id,
            elapsed_ms = phase_started.elapsed().as_millis(),
            "phase 2c: repair pass done"
        );
    }

    // ── Phase 2d: comparator pass-2 (after repair) ───────────────────────────
    if repair_used {
        let outcome2 = run_comparator(layout, task, &workdir, &eval_params)?;
        if outcome2.pass {
            return Ok(finish(task, started, total_turns, outcome2, &cfg));
        }
        // ── Phase 2e: second-model repair (fallback) ─────────────────────────
        if let Some(repair_model) = cfg.repair_model.as_deref() {
            let provider: Box<dyn Provider> = Box::new(Gemini::new(api_key.clone()));
            let repair2 = Agent::builder()
                .provider_boxed(provider)
                .model(repair_model)
                .system_prompt(build_repair_system_prompt(&cfg.skill_pack))
                .max_turns(30)
                .tools(domain_tools())
                .working_dir(&workdir.root)
                .permission_policy(cersei_tools::permissions::AllowAll)
                .build()
                .context("build second-model repair agent")?;
            let prompt = build_repair_prompt(
                task,
                &workdir,
                &outcome2,
                verifier_summary.as_str(),
                auditor_summary.as_str(),
                last_dbt_error.as_deref(),
            );
            let phase_started = Instant::now();
            tracing::info!(
                instance_id = %task.instance_id,
                model = %repair_model,
                "phase 2e: second-model repair starting"
            );
            match run_with_phase_timeout(repair2.run(&prompt), &cfg, "second-model repair").await {
                Ok(Ok(out)) => total_turns = total_turns.saturating_add(out.turns),
                Ok(Err(e)) => tracing::warn!(error = %e, "second-model repair failed"),
                Err(e) => tracing::warn!(error = %e, "second-model repair timed out"),
            }
            tracing::info!(
                instance_id = %task.instance_id,
                elapsed_ms = phase_started.elapsed().as_millis(),
                "phase 2e: second-model repair done"
            );
            let outcome3 = run_comparator(layout, task, &workdir, &eval_params)?;
            return Ok(finish(task, started, total_turns, outcome3, &cfg));
        }
        return Ok(finish(task, started, total_turns, outcome2, &cfg));
    }

    Ok(finish(task, started, total_turns, outcome, &cfg))
}

fn run_comparator(
    layout: &SuiteLayout,
    task: &Task,
    workdir: &Workdir,
    eval_params: &crate::comparator::EvalParams,
) -> Result<EvalOutcome> {
    let gold_db = find_gold_db(&layout.gold_root, &task.instance_id).ok_or_else(|| {
        anyhow::anyhow!(
            "gold db missing under {}",
            layout.gold_root.join(&task.instance_id).display()
        )
    })?;
    let result_db = match find_result_db(&workdir.root, &eval_params.gold) {
        Some(p) => p,
        None => {
            return Ok(EvalOutcome {
                pass: false,
                details: format!(
                    "agent didn't produce a result DB under {}",
                    workdir.root.display()
                ),
            });
        }
    };
    evaluate(&result_db, &gold_db, eval_params).context("comparator")
}

fn finish(
    task: &Task,
    started: Instant,
    turns: u32,
    outcome: EvalOutcome,
    cfg: &RunConfig,
) -> TaskResult {
    TaskResult {
        instance_id: task.instance_id.clone(),
        pass: outcome.pass,
        turns,
        elapsed_ms: started.elapsed().as_millis(),
        fail_reason: if outcome.pass {
            None
        } else {
            Some("comparator: see details".into())
        },
        deterministic_dates: cfg.deterministic_dates
            && cfg.gold_dates.get(&task.instance_id).is_some(),
        details: truncate_details(&outcome.details, 4096),
    }
}

fn make_failure(
    task: &Task,
    started: Instant,
    turns: u32,
    reason: String,
    cfg: &RunConfig,
    deterministic_ok: bool,
) -> TaskResult {
    TaskResult {
        instance_id: task.instance_id.clone(),
        pass: false,
        turns,
        elapsed_ms: started.elapsed().as_millis(),
        fail_reason: Some(reason),
        deterministic_dates: deterministic_ok
            && cfg.deterministic_dates
            && cfg.gold_dates.get(&task.instance_id).is_some(),
        details: String::new(),
    }
}

fn build_system_prompt(
    task: &Task,
    workdir: &Workdir,
    skills_md: &str,
    eval_tables_md: &str,
) -> String {
    let db_repr = workdir
        .db_path
        .as_ref()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "<no seeded DB>".into());
    SYSTEM_PROMPT_TEMPLATE
        .replace("${work_dir}", &workdir.root.display().to_string())
        .replace("${instance_id}", &task.instance_id)
        .replace("${db_path}", &db_repr)
        .replace("${seeded_tables}", &workdir.seeded_tables.to_string())
        .replace("${eval_tables}", eval_tables_md)
        .replace("${skills}", skills_md)
}

/// Render the comparator's `condition_tabs` as a bulleted markdown list. When
/// the eval entry has no explicit `condition_tabs` (i.e. "compare every gold
/// table"), we fall back to a generic instruction.
fn render_eval_tables(params: &crate::comparator::EvalParams) -> String {
    match params.condition_tabs.as_ref() {
        Some(tabs) if !tabs.is_empty() => {
            let mut out = String::new();
            for t in tabs {
                out.push_str(&format!("- `{t}`\n"));
            }
            out
        }
        _ => "- (no explicit list — every table in the gold DuckDB is evaluated; \
              build every model in `models/**/*.yml`)\n"
            .into(),
    }
}

fn build_repair_system_prompt(skills_md: &str) -> String {
    let mut s = String::new();
    s.push_str(
        "You are a focused dbt repair agent. A previous build failed comparator \
         checks. Your job: read the failure list, identify the smallest set of SQL \
         edits that fix it, apply them, and rebuild only the affected models with \
         `dbt run --select <model>`. Do NOT touch unrelated models. Do NOT modify \
         YML schema. When done, query the affected tables to confirm the fix.\n\n",
    );
    s.push_str("Skills below — apply the relevant ones.\n\n");
    s.push_str(skills_md);
    s
}

fn format_verifier_goal(task: &Task) -> String {
    format!(
        "Verify the dbt build for instance {} against reference_snapshot.md and \
         the YML contracts. Fix issues you are CERTAIN about. Report a structured \
         summary at the end.",
        task.instance_id
    )
}

fn format_verifier_context(task: &Task, workdir: &Workdir) -> String {
    VERIFIER_PROMPT_TEMPLATE
        .replace(
            "${work_dir}",
            &workdir.root.display().to_string(),
        )
        .replace("${instance_id}", &task.instance_id)
        .replace(
            "${db_path}",
            &workdir
                .db_path
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| "<no seeded DB>".into()),
        )
}

fn format_auditor_goal(task: &Task) -> String {
    format!(
        "Run a fan-out / cardinality / surrogate-key audit on the dbt build for \
         instance {}. Read-only by default; fix only unambiguous issues.",
        task.instance_id
    )
}

fn format_auditor_context(task: &Task, workdir: &Workdir) -> String {
    AUDITOR_PROMPT_TEMPLATE
        .replace(
            "${work_dir}",
            &workdir.root.display().to_string(),
        )
        .replace("${instance_id}", &task.instance_id)
        .replace(
            "${db_path}",
            &workdir
                .db_path
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| "<no seeded DB>".into()),
        )
}

fn build_repair_prompt(
    task: &Task,
    workdir: &Workdir,
    outcome: &EvalOutcome,
    verifier: &str,
    auditor: &str,
    last_dbt_error: Option<&str>,
) -> String {
    let mut s = String::new();
    s.push_str(&format!(
        "Original task: {}\n\nWork dir: {}\n\n",
        task.instruction,
        workdir.root.display()
    ));
    // SignalPilot's repair prompt feeds the model the raw stderr from the
    // last failing dbt invocation. Without this the model only sees comparator
    // symptoms ("column X mismatch") and re-derives the fix; with it, the
    // model lands directly on the bad SQL.
    if let Some(err) = last_dbt_error {
        s.push_str(
            "The previous `dbt run`/`dbt build` failed with this error \
             (verbatim, truncated):\n\n",
        );
        s.push_str("```\n");
        s.push_str(&truncate_details(err, 2048));
        s.push_str("\n```\n\n");
        s.push_str(
            "Common fixes that work on Spider2-DBT:\n\
             - `current_date` / `current_timestamp` errors → replace with a \
               hardcoded date like `CAST('2024-01-01' AS DATE)` and rebuild.\n\
             - Missing upstream model → run `dbt run --select +<upstream>` \
               first, then retry.\n\
             - Stub SQL still in place → rewrite the file before re-running.\n\n",
        );
    }
    s.push_str("The comparator reports these failures (truncated):\n\n");
    s.push_str(&truncate_details(&outcome.details, 2048));
    s.push_str("\n\n");
    if !verifier.trim().is_empty() {
        s.push_str("Verifier subagent summary:\n");
        s.push_str(&truncate_details(verifier, 1024));
        s.push_str("\n\n");
    }
    if !auditor.trim().is_empty() {
        s.push_str("Auditor subagent summary:\n");
        s.push_str(&truncate_details(auditor, 1024));
        s.push_str("\n\n");
    }
    s.push_str(
        "Fix the smallest set of SQL files that make these checks pass. Rebuild \
         only the affected models — do NOT issue a bare `dbt run` (it will \
         overwrite pre-existing reference tables and corrupt the comparator \
         baseline). Do NOT modify YML schema. End by running the \
         comparator-relevant tables and confirming the row counts and key \
         column values match the snapshot.",
    );
    s
}

/// Walk the main agent's tool-call history backwards and return the most
/// recent `bash` invocation whose input contained `dbt` and whose result
/// either set `is_error=true` or whose stdout contains a typical dbt failure
/// marker. The caller feeds this verbatim to the repair agent so it can fix
/// the actual SQL error rather than re-derive it from comparator symptoms.
fn extract_last_dbt_error(records: &[cersei_agent::ToolCallRecord]) -> Option<String> {
    let dbt_failure_markers = [
        "Compilation Error",
        "Database Error",
        "Parsing Error",
        "Runtime Error",
        "ERROR",
        "FAIL ",
        "Errors found",
    ];
    for r in records.iter().rev() {
        if r.name != "Bash" && r.name != "bash" {
            continue;
        }
        let cmd = r
            .input
            .get("command")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if !cmd.contains("dbt") {
            continue;
        }
        let looks_failed = r.is_error
            || dbt_failure_markers.iter().any(|m| r.result.contains(m));
        if looks_failed {
            // Prefix the command so the model knows which invocation produced it.
            let mut out = String::new();
            out.push_str(&format!("$ {}\n\n", cmd.trim()));
            out.push_str(r.result.trim());
            return Some(out);
        }
    }
    None
}

/// Per-event progress logger. Emits one info line per high-level milestone
/// (turn boundaries, tool start/end, retries, errors). Skips token-stream
/// deltas — they are noisy and add nothing for diagnosis.
fn log_agent_event(instance_id: &str, ev: &cersei_agent::events::AgentEvent) {
    use cersei_agent::events::AgentEvent;
    match ev {
        AgentEvent::TurnStart { turn, .. } => {
            tracing::info!(instance_id, turn, "turn start");
        }
        AgentEvent::TurnComplete { turn, .. } => {
            tracing::info!(instance_id, turn, "turn complete");
        }
        AgentEvent::ToolStart { name, id, .. } => {
            tracing::info!(instance_id, tool = %name, id = %id, "tool start");
        }
        AgentEvent::ToolEnd { name, id, is_error, .. } => {
            tracing::info!(
                instance_id, tool = %name, id = %id, is_error,
                "tool end"
            );
        }
        AgentEvent::CompactStart { .. } => {
            tracing::info!(instance_id, "compact start");
        }
        AgentEvent::CompactEnd { .. } => {
            tracing::info!(instance_id, "compact end");
        }
        AgentEvent::Error(s) => {
            tracing::warn!(instance_id, error = %s, "agent error");
        }
        AgentEvent::TokenWarning { .. } => {
            tracing::warn!(instance_id, "token warning");
        }
        _ => {}
    }
}

/// Wrap a future in a per-phase wall-clock cap. Returns:
///   `Ok(Ok(value))`  — the inner future completed in time
///   `Ok(Err(e))`     — the inner future returned an error
///   `Err(reason)`    — the timeout fired
async fn run_with_phase_timeout<F, T, E>(fut: F, cfg: &RunConfig, label: &str) -> Result<std::result::Result<T, E>, String>
where
    F: Future<Output = std::result::Result<T, E>>,
{
    if cfg.phase_timeout_secs == 0 {
        return Ok(fut.await);
    }
    match tokio::time::timeout(Duration::from_secs(cfg.phase_timeout_secs), fut).await {
        Ok(r) => Ok(r),
        Err(_) => Err(format!(
            "{label} exceeded phase timeout of {}s",
            cfg.phase_timeout_secs
        )),
    }
}

/// The full agent toolset used everywhere in this bench: the standard cersei
/// coding tools (bash + file_* + glob + grep) plus the bench-local
/// dbt-aware tools that compress 3-5 turns of infra discovery into 1 call.
fn domain_tools() -> Vec<Box<dyn Tool>> {
    let mut t: Vec<Box<dyn Tool>> = cersei_tools::coding();
    t.push(Box::new(crate::tools::DbtProjectMapTool));
    t.push(Box::new(crate::tools::DuckDbQueryTool));
    t
}
