//! End-to-end verification that `cersei-compression` actually reduces the
//! input-token count billed by a real OpenAI model.
//!
//! Skipped by default. Run with:
//!
//!   OPENAI_API_KEY=sk-... cargo test -p cersei-agent --test e2e_openai_compression --ignored -- --nocapture
//!
//! If `OPENAI_API_KEY` is unset the test prints a notice and passes — so CI
//! without credentials is a no-op.

use async_trait::async_trait;
use cersei_agent::Agent;
use cersei_compression::CompressionLevel;
use cersei_provider::gemini::Gemini;
use cersei_provider::openai::OpenAi;
use cersei_provider::Provider;
use cersei_tools::{PermissionLevel, Tool, ToolCategory, ToolContext, ToolResult};
use cersei_types::Usage;
use serde_json::{json, Value};
use std::sync::Once;
use tracing_subscriber::EnvFilter;

/// Install a subscriber that prints every `cersei_compression` info event once
/// per process, so both test bodies see intercepted compression logs in
/// `--nocapture` output. Respects RUST_LOG when set, otherwise forces
/// `cersei_compression=info`.
fn init_tracing_once() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let filter = EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| EnvFilter::new("cersei_compression=info"));
        let _ = tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_test_writer()
            .with_target(true)
            .without_time()
            .try_init();
    });
}

/// A synthetic tool that ignores its input and returns a large, noisy stdout
/// similar to what `cargo test` produces. Every call returns the same 400+
/// line fixture, so the only source of token variation between runs is the
/// compression pipeline.
struct NoisyCargoOutputTool;

const NOISY_FIXTURE: &str = concat!(
    "\x1b[32m   Compiling\x1b[0m foo v0.1.0 (/tmp/foo)\n",
    "\x1b[32m   Compiling\x1b[0m bar v0.2.0 (/tmp/bar)\n",
    "\x1b[32m   Compiling\x1b[0m baz v0.3.0 (/tmp/baz)\n",
    "\x1b[32m   Compiling\x1b[0m qux v0.4.0 (/tmp/qux)\n",
    "\x1b[32m   Compiling\x1b[0m quux v0.5.0 (/tmp/quux)\n",
    "\x1b[32m    Finished\x1b[0m test profile [unoptimized + debuginfo] target(s) in 7.82s\n",
    "\x1b[32m     Running\x1b[0m unittests src/lib.rs (target/debug/deps/foo-abcdef0123456789)\n",
    "\n",
    "running 20 tests\n",
    "test tests::case_01_compress_is_off_by_default ... ok\n",
    "test tests::case_02_level_parses_minimal ... ok\n",
    "test tests::case_03_level_parses_aggressive ... ok\n",
    "test tests::case_04_unknown_level_errors ... ok\n",
    "test tests::case_05_ansi_is_stripped ... ok\n",
    "test tests::case_06_json_unchanged ... ok\n",
    "test tests::case_07_rust_doc_comments_preserved ... ok\n",
    "test tests::case_08_rust_line_comments_stripped ... ok\n",
    "test tests::case_09_python_docstrings_preserved ... ok\n",
    "test tests::case_10_shell_hash_comments_stripped ... ok\n",
    "test tests::case_11_ruby_begin_end_blocks ... ok\n",
    "test tests::case_12_aggressive_keeps_signatures ... ok\n",
    "test tests::case_13_aggressive_drops_bodies ... ok\n",
    "test tests::case_14_unicode_preserved ... ok\n",
    "test tests::case_15_large_input_cap_enforced ... ok\n",
    "test tests::case_16_empty_input_passthrough ... ok\n",
    "test tests::case_17_single_line_input ... ok\n",
    "test tests::case_18_crlf_handling ... ok\n",
    "test tests::case_19_mixed_tab_space_indent ... ok\n",
    "test tests::case_20_tool_name_case_insensitive ... ok\n",
    "\n",
    "test result: ok. 20 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.04s\n",
    "\n",
    "\x1b[32m     Running\x1b[0m tests/savings.rs (target/debug/deps/savings-fedcba9876543210)\n",
    "\n",
    "running 4 tests\n",
    "test off_level_is_exact_passthrough ... ok\n",
    "test rust_source_aggressive_drops_bodies ... ok\n",
    "test git_log_saves_at_least_30pct_minimal ... ok\n",
    "test cargo_test_saves_at_least_25pct_minimal ... ok\n",
    "\n",
    "test result: ok. 4 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.02s\n",
    "\n",
    "   Doc-tests cersei_compression\n",
    "\n",
    "running 0 tests\n",
    "\n",
    "test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s\n",
    "\n",
    // Extra filler so the compression win is measurable.
    "# --- additional build log (repeated progress lines) ---\n",
    "   Compiling crate_a v0.0.1\n",
    "   Compiling crate_b v0.0.1\n",
    "   Compiling crate_c v0.0.1\n",
    "   Compiling crate_d v0.0.1\n",
    "   Compiling crate_e v0.0.1\n",
    "   Compiling crate_f v0.0.1\n",
    "   Compiling crate_g v0.0.1\n",
    "   Compiling crate_h v0.0.1\n",
    "   Compiling crate_i v0.0.1\n",
    "   Compiling crate_j v0.0.1\n",
    "   Compiling crate_k v0.0.1\n",
    "   Compiling crate_l v0.0.1\n",
    "   Compiling crate_m v0.0.1\n",
    "   Compiling crate_n v0.0.1\n",
    "   Compiling crate_o v0.0.1\n",
    "   Compiling crate_p v0.0.1\n",
    "   Compiling crate_q v0.0.1\n",
    "   Compiling crate_r v0.0.1\n",
    "   Compiling crate_s v0.0.1\n",
    "   Compiling crate_t v0.0.1\n",
    "   Compiling crate_u v0.0.1\n",
    "   Compiling crate_v v0.0.1\n",
    "   Compiling crate_w v0.0.1\n",
    "   Compiling crate_x v0.0.1\n",
    "   Compiling crate_y v0.0.1\n",
    "   Compiling crate_z v0.0.1\n",
    "    Finished test profile [unoptimized + debuginfo] target(s) in 12.14s\n",
);

#[async_trait]
impl Tool for NoisyCargoOutputTool {
    fn name(&self) -> &str {
        "Bash"
    }
    fn description(&self) -> &str {
        "Runs a shell command. In this test it always returns a precomputed cargo-test-like stdout."
    }
    fn permission_level(&self) -> PermissionLevel {
        PermissionLevel::None
    }
    fn category(&self) -> ToolCategory {
        ToolCategory::Shell
    }
    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "command": { "type": "string" }
            },
            "required": ["command"]
        })
    }
    async fn execute(&self, _input: Value, _ctx: &ToolContext) -> ToolResult {
        ToolResult::success(NOISY_FIXTURE.to_string())
    }
}

struct RunStats {
    usage: Usage,
    tool_calls: usize,
    turns: u32,
}

async fn run_once_with<P: Provider + 'static>(
    provider: P,
    model: &str,
    level: CompressionLevel,
) -> anyhow::Result<RunStats> {
    let agent = Agent::builder()
        .provider(provider)
        .tool(NoisyCargoOutputTool)
        .system_prompt(
            "You are a terse CI reporter. You MUST call the Bash tool exactly \
             once with the command `cargo test`. After you receive the tool \
             result, reply with a single short sentence that says whether all \
             tests passed. Never skip the tool call. Never quote the tool output.",
        )
        .model(model)
        .max_turns(4)
        .max_tokens(128)
        .compression_level(level)
        .build()?;

    let out = agent
        .run("Run `cargo test` via the Bash tool and summarise the result.")
        .await?;
    Ok(RunStats {
        usage: out.usage,
        tool_calls: out.tool_calls.len(),
        turns: out.turns,
    })
}

async fn run_once(level: CompressionLevel) -> anyhow::Result<RunStats> {
    run_once_with(OpenAi::from_env()?, "gpt-4o-mini", level).await
}

async fn run_once_gemini(level: CompressionLevel) -> anyhow::Result<RunStats> {
    run_once_with(Gemini::from_env()?, "gemini-2.5-flash", level).await
}

fn log_stats(label: &str, s: &RunStats) {
    eprintln!(
        "  {label}: input={}  output={}  total={}  tool_calls={}  turns={}",
        s.usage.input_tokens, s.usage.output_tokens, s.usage.total_tokens, s.tool_calls, s.turns
    );
}

#[tokio::test]
#[ignore]
async fn compression_reduces_real_openai_token_bill() {
    if std::env::var("OPENAI_API_KEY").is_err() {
        eprintln!("OPENAI_API_KEY not set — skipping.");
        return;
    }
    init_tracing_once();

    eprintln!("\n── openai run 1: CompressionLevel::Off ──");
    let off = run_once(CompressionLevel::Off)
        .await
        .expect("Off run failed");
    log_stats("off       ", &off);

    eprintln!("\n── openai run 2: CompressionLevel::Aggressive ──");
    let aggr = run_once(CompressionLevel::Aggressive)
        .await
        .expect("Aggressive run failed");
    log_stats("aggressive", &aggr);

    assert!(
        off.tool_calls >= 1 && aggr.tool_calls >= 1,
        "openai: both runs must actually invoke the Bash tool — off={} aggr={}",
        off.tool_calls,
        aggr.tool_calls
    );

    let off_in = off.usage.input_tokens as f64;
    let aggr_in = aggr.usage.input_tokens as f64;
    let savings_pct = if off_in > 0.0 {
        100.0 * (off_in - aggr_in) / off_in
    } else {
        0.0
    };
    eprintln!(
        "\n── openai compression saved {:.1}% of input tokens ({} → {}) ──\n",
        savings_pct, off.usage.input_tokens, aggr.usage.input_tokens
    );

    assert!(
        aggr.usage.input_tokens < off.usage.input_tokens,
        "openai Aggressive input_tokens ({}) should be < Off input_tokens ({})",
        aggr.usage.input_tokens,
        off.usage.input_tokens
    );
    assert!(
        savings_pct >= 10.0,
        "openai: expected at least 10% input-token savings, got {:.1}% ({} → {})",
        savings_pct,
        off.usage.input_tokens,
        aggr.usage.input_tokens
    );
}

#[tokio::test]
#[ignore]
async fn compression_reduces_real_gemini_token_bill() {
    if std::env::var("GOOGLE_API_KEY").is_err() && std::env::var("GEMINI_API_KEY").is_err() {
        eprintln!("GOOGLE_API_KEY / GEMINI_API_KEY not set — skipping.");
        return;
    }
    init_tracing_once();

    eprintln!("\n── gemini run 1: CompressionLevel::Off ──");
    let off = run_once_gemini(CompressionLevel::Off)
        .await
        .expect("Off run failed");
    log_stats("off       ", &off);

    eprintln!("\n── gemini run 2: CompressionLevel::Aggressive ──");
    let aggr = run_once_gemini(CompressionLevel::Aggressive)
        .await
        .expect("Aggressive run failed");
    log_stats("aggressive", &aggr);

    assert!(
        off.tool_calls >= 1 && aggr.tool_calls >= 1,
        "gemini: both runs must actually invoke the Bash tool — off={} aggr={}. \
         If this trips, Gemini decided not to call the tool and we measured nothing.",
        off.tool_calls,
        aggr.tool_calls
    );

    let off_in = off.usage.input_tokens as f64;
    let aggr_in = aggr.usage.input_tokens as f64;
    let savings_pct = if off_in > 0.0 {
        100.0 * (off_in - aggr_in) / off_in
    } else {
        0.0
    };
    eprintln!(
        "\n── gemini compression saved {:.1}% of input tokens ({} → {}) ──\n",
        savings_pct, off.usage.input_tokens, aggr.usage.input_tokens
    );

    assert!(
        aggr.usage.input_tokens < off.usage.input_tokens,
        "Gemini Aggressive input_tokens ({}) should be < Off input_tokens ({})",
        aggr.usage.input_tokens,
        off.usage.input_tokens
    );
    assert!(
        savings_pct >= 10.0,
        "Gemini: expected at least 10% input-token savings, got {:.1}% ({} → {})",
        savings_pct,
        off.usage.input_tokens,
        aggr.usage.input_tokens
    );
}
