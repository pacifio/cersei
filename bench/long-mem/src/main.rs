//! longmem-bench — head-to-head LongMemEval runner for the Cersei memory stack.
//!
//! Usage:
//!   longmem-bench --dataset s --config all --limit 10
//!
//! Datasets:   s | m | oracle       (must be pre-downloaded into ./data/)
//! Configs:    all | baseline | embed | graph | hybrid
//!
//! Environment:
//!   OPENAI_API_KEY   — required for embeddings, judge, answerer, extractor
//!
//! Output: JSON + summary JSON in ./results/<config>-<dataset>.json

mod configs;
mod dataset;
mod judge;
mod mastra_prompts;
mod report;
mod runner;

use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use cersei_embeddings::{EmbeddingError, EmbeddingProvider, GeminiEmbeddings, OpenAiEmbeddings};
use cersei_provider::{Auth, Gemini, OpenAi, Provider};
use clap::{Parser, ValueEnum};
use std::path::PathBuf;
use std::sync::Arc;
use tracing_subscriber::EnvFilter;

/// Dispatching enum so we can swap the underlying embedding backend at runtime
/// while keeping the generic `EmbedConfig<P>` / `HybridConfig<P, E>` bounds
/// happy. Both variants delegate to their inner concrete provider.
pub enum AnyEmbeddings {
    Openai(OpenAiEmbeddings),
    Gemini(GeminiEmbeddings),
}

#[async_trait]
impl EmbeddingProvider for AnyEmbeddings {
    fn name(&self) -> &str {
        match self {
            AnyEmbeddings::Openai(p) => p.name(),
            AnyEmbeddings::Gemini(p) => p.name(),
        }
    }
    fn dimensions(&self) -> usize {
        match self {
            AnyEmbeddings::Openai(p) => p.dimensions(),
            AnyEmbeddings::Gemini(p) => p.dimensions(),
        }
    }
    async fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, EmbeddingError> {
        match self {
            AnyEmbeddings::Openai(p) => p.embed_batch(texts).await,
            AnyEmbeddings::Gemini(p) => p.embed_batch(texts).await,
        }
    }
}

#[derive(Parser, Debug)]
#[command(
    name = "longmem-bench",
    about = "LongMemEval runner for the Cersei memory stack"
)]
struct Cli {
    /// Dataset variant. Files expected at `./data/longmemeval_<name>.json`.
    #[arg(long, default_value = "oracle")]
    dataset: DatasetArg,

    /// Which config to run.
    #[arg(long, default_value = "all")]
    config: ConfigArg,

    /// Cap the number of questions (useful for smoke runs).
    #[arg(long)]
    limit: Option<usize>,

    /// Where to write per-config JSON results.
    #[arg(long, default_value = "./results")]
    results_dir: PathBuf,

    /// Which LLM provider to use for answerer + judge + extractor + embeddings.
    /// Default: gemini (uses `GOOGLE_API_KEY` / `GEMINI_API_KEY`).
    #[arg(long, default_value = "gemini")]
    provider: ProviderArg,

    /// Which model to use for the answerer.
    #[arg(long, default_value = "gemini-2.5-flash")]
    answerer_model: String,

    /// Judge model. Mastra's published numbers use gpt-4o-mini; keep this
    /// default on gemini-2.5-flash for Google-only runs, override to
    /// `gpt-4o-mini` when you want Mastra-comparable scoring.
    #[arg(long, default_value = "gemini-2.5-flash")]
    judge_model: String,

    /// Observer / fact-extractor model (only used by the hybrid config).
    /// Mastra's OM default is `google/gemini-2.5-flash`.
    #[arg(long, default_value = "gemini-2.5-flash")]
    extractor_model: String,

    /// Top-k for retrieval-based configs. Matches Mastra's RAG config.
    #[arg(long, default_value = "20")]
    top_k: usize,

    /// Parallel in-flight questions per config. Higher = faster, but bounded
    /// by provider rate limits. Gemini 2.5 flash tolerates higher concurrency.
    #[arg(long, default_value = "8")]
    concurrency: usize,
}

#[derive(ValueEnum, Clone, Copy, Debug, PartialEq, Eq)]
#[value(rename_all = "lower")]
enum ProviderArg {
    Gemini,
    Openai,
}

#[derive(ValueEnum, Clone, Debug, PartialEq, Eq)]
#[value(rename_all = "lower")]
enum DatasetArg {
    S,
    M,
    Oracle,
}

impl DatasetArg {
    fn file_name(&self) -> &'static str {
        match self {
            Self::S => "longmemeval_s.json",
            Self::M => "longmemeval_m.json",
            Self::Oracle => "longmemeval_oracle.json",
        }
    }
    fn label(&self) -> &'static str {
        match self {
            Self::S => "longmemeval_s",
            Self::M => "longmemeval_m",
            Self::Oracle => "longmemeval_oracle",
        }
    }
}

#[derive(ValueEnum, Clone, Debug, PartialEq, Eq)]
#[value(rename_all = "lower")]
enum ConfigArg {
    All,
    Baseline,
    Embed,
    Graph,
    Hybrid,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("warn,longmem_bench=info")),
        )
        .with_target(false)
        .without_time()
        .init();

    let cli = Cli::parse();

    // Load dataset
    let dataset_path = PathBuf::from("./data").join(cli.dataset.file_name());
    if !dataset_path.exists() {
        return Err(anyhow!(
            "Dataset {} not found. Run ./setup.sh or fetch it manually.",
            dataset_path.display()
        ));
    }
    let mut questions = dataset::load_dataset(&dataset_path)
        .with_context(|| format!("loading {}", dataset_path.display()))?;
    eprintln!(
        "Loaded {} questions from {}",
        questions.len(),
        cli.dataset.label()
    );
    if let Some(n) = cli.limit {
        questions.truncate(n);
        eprintln!("Limit applied → running {} questions", questions.len());
    }

    // Set up providers (api key is shared across embeddings + completion)
    let (provider, embed_factory): (Arc<dyn Provider + Send + Sync>, EmbedFactoryArc) =
        match cli.provider {
            ProviderArg::Gemini => {
                let api_key = std::env::var("GOOGLE_API_KEY")
                    .or_else(|_| std::env::var("GEMINI_API_KEY"))
                    .map_err(|_| {
                        anyhow!(
                            "GOOGLE_API_KEY (or GEMINI_API_KEY) is required for --provider=gemini"
                        )
                    })?;
                let p: Arc<dyn Provider + Send + Sync> = Arc::new(Gemini::new(api_key.clone()));
                let key = api_key.clone();
                let f: EmbedFactoryArc = Arc::new(move || {
                    AnyEmbeddings::Gemini(GeminiEmbeddings::new(key.clone()))
                });
                (p, f)
            }
            ProviderArg::Openai => {
                let api_key = std::env::var("OPENAI_API_KEY")
                    .map_err(|_| anyhow!("OPENAI_API_KEY is required for --provider=openai"))?;
                let p: Arc<dyn Provider + Send + Sync> =
                    Arc::new(OpenAi::new(Auth::ApiKey(api_key.clone())));
                let key = api_key.clone();
                let f: EmbedFactoryArc = Arc::new(move || {
                    AnyEmbeddings::Openai(OpenAiEmbeddings::new(key.clone()))
                });
                (p, f)
            }
        };

    std::fs::create_dir_all(&cli.results_dir)?;

    let selected: Vec<ConfigArg> = match &cli.config {
        ConfigArg::All => vec![
            ConfigArg::Baseline,
            ConfigArg::Embed,
            ConfigArg::Graph,
            ConfigArg::Hybrid,
        ],
        other => vec![other.clone()],
    };

    let mut summaries: Vec<report::BenchmarkMetrics> = Vec::new();

    for cfg_choice in selected {
        let out_path = cli.results_dir.join(format!(
            "{}-{}.json",
            config_slug(&cfg_choice),
            cli.dataset.label()
        ));

        let metrics = run_one_config(
            cfg_choice.clone(),
            &questions,
            provider.clone(),
            &embed_factory,
            &cli,
            cli.dataset.label(),
        )
        .await?;

        std::fs::write(&out_path, serde_json::to_vec_pretty(&metrics)?)
            .with_context(|| format!("writing {}", out_path.display()))?;
        eprintln!(
            "✓ {} → {:.3} overall ({} correct / {} total), {:.3} abstention, wrote {}",
            metrics.config,
            metrics.overall_accuracy,
            metrics.correct_answers,
            metrics.total_questions,
            metrics.abstention_accuracy,
            out_path.display()
        );
        summaries.push(metrics);
    }

    // Write combined summary
    let summary_path = cli
        .results_dir
        .join(format!("summary-{}.json", cli.dataset.label()));
    std::fs::write(&summary_path, serde_json::to_vec_pretty(&summaries)?)?;
    eprintln!(
        "→ {} written with {} configs",
        summary_path.display(),
        summaries.len()
    );

    Ok(())
}

fn config_slug(c: &ConfigArg) -> &'static str {
    match c {
        ConfigArg::Baseline => "a-baseline-jsonl",
        ConfigArg::Embed => "b-embed-only",
        ConfigArg::Graph => "c-graph-substring",
        ConfigArg::Hybrid => "d-hybrid-embed-graph",
        ConfigArg::All => "all",
    }
}

type EmbedFactoryArc = Arc<dyn Fn() -> AnyEmbeddings + Send + Sync + 'static>;

async fn run_one_config(
    choice: ConfigArg,
    questions: &[dataset::Question],
    provider: Arc<dyn Provider + Send + Sync>,
    embed_factory: &EmbedFactoryArc,
    cli: &Cli,
    dataset_label: &str,
) -> Result<report::BenchmarkMetrics> {
    eprintln!(
        "─── running config: {:?}  (concurrency={})  ───",
        choice, cli.concurrency
    );

    use tokio::sync::Semaphore;
    let sem = Arc::new(Semaphore::new(cli.concurrency));

    // Kick off all questions; use JoinSet so completions arrive in order of
    // finish, not submission — good for progress reporting.
    let mut set = tokio::task::JoinSet::new();
    let total = questions.len();
    for (i, q) in questions.iter().cloned().enumerate() {
        let permit = sem.clone().acquire_owned().await.unwrap();
        let provider = provider.clone();
        let choice = choice.clone();
        let factory = embed_factory.clone();
        let answerer_model = cli.answerer_model.clone();
        let judge_model = cli.judge_model.clone();
        let extractor_model = cli.extractor_model.clone();
        let top_k = cli.top_k;
        set.spawn(async move {
            let _permit = permit; // released on drop
            let factory_for_task = factory.clone();
            // Wrap the Arc'd factory in a fresh `Fn` closure so the concrete
            // EmbedConfig / HybridConfig can consume it via their `impl Fn`
            // constructors.
            let closure = move || factory_for_task();
            let res = run_one(
                choice,
                &q,
                provider,
                closure,
                &answerer_model,
                &judge_model,
                &extractor_model,
                top_k,
            )
            .await;
            (i, q, res)
        });
    }

    let mut rows: Vec<report::PerQuestion> = Vec::with_capacity(total);
    let mut completed = 0usize;
    while let Some(joined) = set.join_next().await {
        let (_i, q, res) = joined.context("task join failed")?;
        completed += 1;
        if completed % 10 == 0 || completed == total {
            eprintln!("  [{}/{}] done (last={})", completed, total, q.question_id);
        }
        match res {
            Ok(r) => rows.push(r),
            Err(e) => {
                eprintln!("  ✗ {}: {e:#}", q.question_id);
                rows.push(report::PerQuestion {
                    question_id: q.question_id.clone(),
                    question_type: q.question_type,
                    is_abstention: q.is_abstention(),
                    question: q.question.clone(),
                    expected_answer: q.answer.clone(),
                    hypothesis: format!("<error: {e:#}>"),
                    is_correct: false,
                    input_tokens: 0,
                    output_tokens: 0,
                    judge_tokens: 0,
                    elapsed_ms: 0,
                });
            }
        }
    }

    // Also dump per-question rows alongside the summary so we can inspect
    // failures after the fact.
    let rows_path = cli.results_dir.join(format!(
        "{}-rows-{}.json",
        config_slug(&choice),
        dataset_label
    ));
    std::fs::write(&rows_path, serde_json::to_vec_pretty(&rows)?)
        .with_context(|| format!("writing {}", rows_path.display()))?;

    Ok(report::summarize(
        config_slug(&choice),
        dataset_label,
        &cli.judge_model,
        &rows,
    ))
}

async fn run_one<F>(
    choice: ConfigArg,
    q: &dataset::Question,
    provider: Arc<dyn Provider + Send + Sync>,
    embed_factory: F,
    answerer_model: &str,
    judge_model: &str,
    extractor_model: &str,
    top_k: usize,
) -> Result<report::PerQuestion>
where
    F: Fn() -> AnyEmbeddings + Send + Sync + 'static + Clone,
{
    match choice {
        ConfigArg::Baseline => {
            let mut c = configs::baseline::BaselineConfig::new();
            runner::run_question(&mut c, provider, answerer_model, judge_model, q).await
        }
        ConfigArg::Embed => {
            let mut c = configs::embed::EmbedConfig::new(embed_factory).with_top_k(top_k);
            runner::run_question(&mut c, provider, answerer_model, judge_model, q).await
        }
        ConfigArg::Graph => {
            let mut c = configs::graph::GraphConfig::new().with_top_k(top_k);
            runner::run_question(&mut c, provider, answerer_model, judge_model, q).await
        }
        ConfigArg::Hybrid => {
            let mut c = configs::hybrid::HybridConfig::<
                AnyEmbeddings,
                dyn Provider + Send + Sync,
            >::new(embed_factory, provider.clone(), extractor_model.to_string())
            .with_top_k(top_k);
            runner::run_question(&mut c, provider, answerer_model, judge_model, q).await
        }
        ConfigArg::All => unreachable!("expanded above"),
    }
}
