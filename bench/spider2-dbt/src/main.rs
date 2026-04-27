//! Spider2-DBT bench entry point.
//!
//! Targets ≥ 65 % pass rate on the 64-task dbt suite, beating SignalPilot's
//! 51.6 % (Sonnet 4.6) by switching to `gemini-3-pro-preview` and adding the
//! 0.1.8 cersei primitives (delegate, skills) on top of their workflow.

mod comparator;
mod dataset;
mod dates;
mod report;
mod runner;
mod tools;
mod workdir;

use anyhow::{Context, Result};
use chrono::Utc;
use clap::Parser;
use std::path::PathBuf;
use std::sync::Arc;
use tracing_subscriber::EnvFilter;

use crate::dataset::{load_tasks, SuiteLayout};
use crate::dates::GoldDates;
use crate::report::{write_summary, Summary};
use crate::runner::{run_one, RunConfig};

#[derive(Debug, Parser)]
#[command(name = "spider2-dbt-bench", about = "Spider2.0 dbt benchmark for Cersei")]
struct Cli {
    /// Path to the cloned spider2-dbt dataset (root that contains
    /// `spider2-dbt.jsonl` + `evaluation_suite/`). Defaults to `$SPIDER2_DBT_DIR`,
    /// then `~/spider2-repo/spider2-dbt`.
    #[arg(long)]
    dataset: Option<PathBuf>,

    /// JSON file mapping `instance_id -> YYYY-MM-DD` for libfaketime injection.
    #[arg(long)]
    gold_dates: Option<PathBuf>,

    /// Number of tasks to run. `0` = all.
    #[arg(long, default_value_t = 0)]
    limit: usize,

    /// Filter by instance id substring.
    #[arg(long)]
    only: Option<String>,

    /// Suite mode (smoke = first 5, full = all 64). Overrides --limit when set.
    #[arg(long, default_value = "full")]
    suite: String,

    /// Provider model.
    #[arg(long, default_value = "gemini-3.1-pro-preview")]
    model: String,

    /// Per-task agent turn cap.
    #[arg(long, default_value_t = 80)]
    max_turns: u32,

    /// Working directory root for per-task workdirs (gitignored).
    #[arg(long, default_value = "bench/spider2-dbt/workdirs")]
    workdir_root: PathBuf,

    /// Where to write the summary JSON.
    #[arg(long, default_value = "bench/spider2-dbt/results/summary-spider2-dbt.json")]
    output: PathBuf,

    /// Use libfaketime + gold-dates JSON when invoking dbt (Linux only).
    /// Default ON — matches SignalPilot. On macOS the env vars are still
    /// set but libfaketime is a no-op; tasks using `current_date` will
    /// produce non-deterministic rows that fail the comparator's
    /// value-spot-check. Pass `--deterministic-dates=false` to opt out.
    #[arg(long, default_value_t = true)]
    deterministic_dates: bool,

    /// Concurrent in-flight tasks. Tune to match GOOGLE rate limits.
    #[arg(long, default_value_t = 2)]
    concurrency: usize,

    /// List the tasks (and pre-flight checks) without running the agent.
    #[arg(long, default_value_t = false)]
    dry_run: bool,

    /// Phase-2: spawn a verifier subagent after the main agent finishes.
    /// Default ON — SignalPilot makes this mandatory; we match.
    #[arg(long, default_value_t = true)]
    verifier: bool,

    /// Phase-2: spawn a fan-out / cardinality auditor subagent in parallel
    /// with the verifier. Default ON.
    #[arg(long, default_value_t = true)]
    auditor: bool,

    /// Phase-2: max repair passes after verifier+auditor (0 disables).
    /// Default 1 — matches SignalPilot's post-agent fix-pass.
    #[arg(long, default_value_t = 1)]
    repair_pass: u32,

    /// Phase-2: optional second-model fallback when the primary model still
    /// fails after one repair pass. e.g. `gemini-2.5-flash`.
    #[arg(long)]
    repair_model: Option<String>,

    /// Phase-2: per-child agent turn cap inside the delegate batch.
    #[arg(long, default_value_t = 60)]
    child_max_turns: u32,

    /// Hard wall-clock cap (seconds) per phase. Prevents a single stuck turn
    /// from burning hours. `0` disables. Default: 1200 (20 min) per phase.
    #[arg(long, default_value_t = 1200)]
    phase_timeout_secs: u64,
}

#[tokio::main]
async fn main() -> Result<()> {
    let _ = dotenvy::from_filename(".env");
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| {
            // Default surfaces: bench progress (info), agent turn-by-turn
            // (info), tool-call results (info from cersei_tools). Bump to
            // `cersei_agent=debug` if you need full event traces.
            EnvFilter::new(
                "info,spider2_dbt_bench=debug,cersei_agent=info,cersei_tools=info",
            )
        }))
        .with_target(true)
        .init();

    let cli = Cli::parse();

    let dataset_root = cli
        .dataset
        .clone()
        .or_else(|| std::env::var("SPIDER2_DBT_DIR").ok().map(PathBuf::from))
        .unwrap_or_else(|| {
            dirs::home_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join("spider2-repo")
                .join("spider2-dbt")
        });
    let layout = SuiteLayout::discover(&dataset_root).context("discover dataset layout")?;
    tracing::info!(root = %layout.root.display(), "dataset layout ok");

    let mut tasks = load_tasks(&layout)?;
    if let Some(filter) = &cli.only {
        tasks.retain(|t| t.instance_id.contains(filter));
    }
    let cap = match cli.suite.as_str() {
        "smoke" => 5,
        _ if cli.limit > 0 => cli.limit,
        _ => tasks.len(),
    };
    tasks.truncate(cap);
    tracing::info!(count = tasks.len(), "tasks to run");

    let gold_dates = cli
        .gold_dates
        .as_ref()
        .map(|p| GoldDates::load(p))
        .transpose()?
        .unwrap_or_default();
    let gold_dates = Arc::new(gold_dates);

    // Build the skill pack once (verbatim concat of bench/spider2-dbt/skills/).
    let skill_pack = build_skill_pack();

    let cfg = RunConfig {
        model: cli.model.clone(),
        max_turns: cli.max_turns,
        working_root: cli.workdir_root.clone(),
        deterministic_dates: cli.deterministic_dates,
        gold_dates: gold_dates.clone(),
        skill_pack,
        enable_verifier: cli.verifier,
        enable_auditor: cli.auditor,
        repair_pass_max: cli.repair_pass,
        repair_model: cli.repair_model.clone(),
        child_max_turns: cli.child_max_turns,
        phase_timeout_secs: cli.phase_timeout_secs,
    };

    if cli.dry_run {
        for t in &tasks {
            println!("- {}: {}", t.instance_id, &t.instruction.chars().take(80).collect::<String>());
        }
        println!(
            "{} tasks queued; gold_dates={}; deterministic={}; model={}",
            tasks.len(),
            gold_dates.get("__present__").is_some() as u8,
            cli.deterministic_dates,
            cfg.model
        );
        return Ok(());
    }

    let started_all = std::time::Instant::now();
    let results = run_all(&layout, tasks.clone(), cfg.clone(), cli.concurrency).await?;

    let total = results.len();
    let passed = results.iter().filter(|r| r.pass).count();
    let pass_rate = if total > 0 {
        passed as f64 / total as f64
    } else {
        0.0
    };
    let total_elapsed_ms = started_all.elapsed().as_millis();
    let avg_elapsed_ms = if total > 0 {
        results.iter().map(|r| r.elapsed_ms).sum::<u128>() / total as u128
    } else {
        0
    };

    let summary = Summary {
        config: cli.suite.clone(),
        model: cfg.model.clone(),
        total,
        passed,
        pass_rate,
        avg_elapsed_ms,
        total_elapsed_ms,
        deterministic_dates: cli.deterministic_dates,
        timestamp: Utc::now().to_rfc3339(),
        per_task: results,
    };
    write_summary(&cli.output, &summary).context("write summary")?;
    println!(
        "✓ {} / {} pass ({:.1}%) — written to {}",
        summary.passed,
        summary.total,
        summary.pass_rate * 100.0,
        cli.output.display()
    );
    Ok(())
}

async fn run_all(
    layout: &SuiteLayout,
    tasks: Vec<crate::dataset::Task>,
    cfg: RunConfig,
    concurrency: usize,
) -> Result<Vec<crate::report::TaskResult>> {
    use futures::future::BoxFuture;
    use futures::stream::{FuturesUnordered, StreamExt};

    type Item = (usize, String, Result<crate::report::TaskResult>);
    let mut in_flight: FuturesUnordered<BoxFuture<'static, Item>> = FuturesUnordered::new();
    let mut iter = tasks.into_iter().enumerate();
    let mut collected: Vec<(usize, crate::report::TaskResult)> = Vec::new();
    let layout = Arc::new(layout.clone());

    let spawn_one = |i: usize, task: crate::dataset::Task| -> BoxFuture<'static, Item> {
        let layout = layout.clone();
        let cfg = cfg.clone();
        Box::pin(async move {
            let r = run_one(&layout, &task, &cfg).await;
            (i, task.instance_id.clone(), r)
        })
    };

    for _ in 0..concurrency.max(1) {
        if let Some((i, task)) = iter.next() {
            in_flight.push(spawn_one(i, task));
        } else {
            break;
        }
    }
    while let Some((i, instance_id, res)) = in_flight.next().await {
        match res {
            Ok(r) => {
                tracing::info!(
                    instance_id = %r.instance_id,
                    pass = r.pass,
                    turns = r.turns,
                    elapsed_ms = r.elapsed_ms,
                    "task done"
                );
                collected.push((i, r));
            }
            Err(e) => {
                tracing::warn!(instance_id = %instance_id, error = %e, "task error");
                collected.push((
                    i,
                    crate::report::TaskResult {
                        instance_id,
                        pass: false,
                        turns: 0,
                        elapsed_ms: 0,
                        fail_reason: Some(format!("runner error: {e}")),
                        deterministic_dates: false,
                        details: String::new(),
                    },
                ));
            }
        }
        if let Some((i2, task2)) = iter.next() {
            in_flight.push(spawn_one(i2, task2));
        }
    }
    collected.sort_by_key(|(i, _)| *i);
    Ok(collected.into_iter().map(|(_, r)| r).collect())
}

fn build_skill_pack() -> String {
    let mut out = String::new();
    for (label, body) in [
        ("dbt-workflow", include_str!("../skills/dbt-workflow.md")),
        ("dbt-write", include_str!("../skills/dbt-write.md")),
        ("duckdb-sql", include_str!("../skills/duckdb-sql.md")),
        ("dbt-debugging", include_str!("../skills/dbt-debugging.md")),
    ] {
        out.push_str(&format!("\n### Skill: {label}\n\n"));
        out.push_str(body);
        out.push_str("\n---\n");
    }
    out
}
