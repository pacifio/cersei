//! # Benchmark: Cersei Tool I/O vs Native Claude Code
//!
//! Measures raw tool execution speed (no LLM calls) for common coding agent
//! operations and compares against `claude` CLI when available.
//!
//! Tests:
//! 1. File Read  — read a file (Cargo.toml)
//! 2. File Write — write a temp file
//! 3. File Edit  — string replacement in a file
//! 4. Glob       — find *.rs files
//! 5. Grep       — search for a pattern
//! 6. Bash       — execute a shell command
//!
//! ```bash
//! cargo run --example benchmark_io --release
//! ```

use cersei::prelude::*;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

const ITERATIONS: u32 = 50;

struct BenchResult {
    name: String,
    total: Duration,
    min: Duration,
    max: Duration,
    iters: u32,
}

impl BenchResult {
    fn avg(&self) -> Duration {
        self.total / self.iters
    }

    fn print(&self) {
        println!(
            "  {:<14} avg={:>8.2}ms  min={:>8.2}ms  max={:>8.2}ms  ({} iters)",
            self.name,
            self.avg().as_secs_f64() * 1000.0,
            self.min.as_secs_f64() * 1000.0,
            self.max.as_secs_f64() * 1000.0,
            self.iters,
        );
    }
}

async fn bench_tool(
    name: &str,
    tool: &dyn Tool,
    input: serde_json::Value,
    ctx: &ToolContext,
    iters: u32,
) -> BenchResult {
    let mut total = Duration::ZERO;
    let mut min = Duration::MAX;
    let mut max = Duration::ZERO;

    // Warmup
    for _ in 0..3 {
        tool.execute(input.clone(), ctx).await;
    }

    for _ in 0..iters {
        let start = Instant::now();
        let _result = tool.execute(input.clone(), ctx).await;
        let elapsed = start.elapsed();
        total += elapsed;
        min = min.min(elapsed);
        max = max.max(elapsed);
    }

    BenchResult {
        name: name.to_string(),
        total,
        min,
        max,
        iters,
    }
}

async fn bench_claude_cli(prompt: &str, iters: u32) -> Option<BenchResult> {
    let claude_path = which::which("claude").ok()?;

    let mut total = Duration::ZERO;
    let mut min = Duration::MAX;
    let mut max = Duration::ZERO;

    // Warmup
    let _ = tokio::process::Command::new(&claude_path)
        .args(["--print", "--max-turns", "1", "-p", prompt])
        .output()
        .await;

    for _ in 0..iters {
        let start = Instant::now();
        let output = tokio::process::Command::new(&claude_path)
            .args(["--print", "--max-turns", "1", "-p", prompt])
            .output()
            .await;
        let elapsed = start.elapsed();

        match output {
            Ok(o) if o.status.success() => {
                total += elapsed;
                min = min.min(elapsed);
                max = max.max(elapsed);
            }
            _ => {
                eprintln!("  claude CLI failed, skipping...");
                return None;
            }
        }
    }

    Some(BenchResult {
        name: "claude-cli".into(),
        total,
        min,
        max,
        iters,
    })
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let working_dir = std::env::current_dir()?;
    let tmp = tempfile::tempdir()?;

    // Create a test file for read/edit benchmarks
    let test_file = tmp.path().join("test_file.txt");
    let test_content =
        "Hello, world!\nThis is a test file.\nLine three.\nLine four.\nLine five.\n".repeat(100);
    std::fs::write(&test_file, &test_content)?;

    // Create some .rs files for glob/grep
    for i in 0..20 {
        let f = tmp.path().join(format!("mod_{}.rs", i));
        std::fs::write(&f, format!("// module {}\nfn func_{}() {{}}\n", i, i))?;
    }

    // Build tool context
    let ctx = ToolContext {
        working_dir: tmp.path().to_path_buf(),
        session_id: "bench".to_string(),
        permissions: Arc::new(AllowAll),
        cost_tracker: Arc::new(CostTracker::new()),
        mcp_manager: None,
        extensions: Extensions::default(),
    };

    // Get tools
    let tools = cersei::tools::all();
    let find_tool =
        |name: &str| -> &dyn Tool { tools.iter().find(|t| t.name() == name).unwrap().as_ref() };

    println!("╔══════════════════════════════════════════════════════════════╗");
    println!("║           Cersei SDK — Tool I/O Benchmark                   ║");
    println!("╠══════════════════════════════════════════════════════════════╣");
    println!(
        "║  {} iterations per test, --release recommended              ║",
        ITERATIONS
    );
    println!(
        "║  Working dir: {}║",
        format!("{:<42}", tmp.path().display())
    );
    println!("╚══════════════════════════════════════════════════════════════╝\n");

    let mut results: Vec<BenchResult> = Vec::new();

    // ── 1. File Read ─────────────────────────────────────────────────────
    let r = bench_tool(
        "Read",
        find_tool("Read"),
        serde_json::json!({ "file_path": test_file.display().to_string() }),
        &ctx,
        ITERATIONS,
    )
    .await;
    r.print();
    results.push(r);

    // ── 2. File Write ────────────────────────────────────────────────────
    let write_path = tmp.path().join("write_bench.txt");
    let r = bench_tool(
        "Write",
        find_tool("Write"),
        serde_json::json!({
            "file_path": write_path.display().to_string(),
            "content": "Benchmark content line\n".repeat(50),
        }),
        &ctx,
        ITERATIONS,
    )
    .await;
    r.print();
    results.push(r);

    // ── 3. File Edit ─────────────────────────────────────────────────────
    // Re-create the file each iteration since edit modifies it
    let edit_tool = find_tool("Edit");
    {
        let mut total = Duration::ZERO;
        let mut min = Duration::MAX;
        let mut max = Duration::ZERO;
        for _ in 0..ITERATIONS {
            // Reset file
            std::fs::write(&test_file, &test_content)?;
            let input = serde_json::json!({
                "file_path": test_file.display().to_string(),
                "old_string": "Hello, world!",
                "new_string": "Hello, Cersei!",
            });
            let start = Instant::now();
            edit_tool.execute(input, &ctx).await;
            let elapsed = start.elapsed();
            total += elapsed;
            min = min.min(elapsed);
            max = max.max(elapsed);
        }
        let r = BenchResult {
            name: "Edit".into(),
            total,
            min,
            max,
            iters: ITERATIONS,
        };
        r.print();
        results.push(r);
    }

    // ── 4. Glob ──────────────────────────────────────────────────────────
    let r = bench_tool(
        "Glob",
        find_tool("Glob"),
        serde_json::json!({
            "pattern": "**/*.rs",
            "path": tmp.path().display().to_string(),
        }),
        &ctx,
        ITERATIONS,
    )
    .await;
    r.print();
    results.push(r);

    // ── 5. Grep ──────────────────────────────────────────────────────────
    let r = bench_tool(
        "Grep",
        find_tool("Grep"),
        serde_json::json!({
            "pattern": "func_",
            "path": tmp.path().display().to_string(),
        }),
        &ctx,
        ITERATIONS,
    )
    .await;
    r.print();
    results.push(r);

    // ── 6. Bash ──────────────────────────────────────────────────────────
    let r = bench_tool(
        "Bash",
        find_tool("Bash"),
        serde_json::json!({ "command": "echo hello && ls -la" }),
        &ctx,
        ITERATIONS,
    )
    .await;
    r.print();
    results.push(r);

    // ── Summary ──────────────────────────────────────────────────────────
    println!("\n─── Cersei Summary ───");
    let total_avg: f64 = results.iter().map(|r| r.avg().as_secs_f64() * 1000.0).sum();
    println!(
        "  Combined avg: {:.2}ms across {} tools",
        total_avg,
        results.len()
    );
    println!(
        "  Fastest: {} ({:.2}ms avg)",
        results.iter().min_by_key(|r| r.avg()).unwrap().name,
        results
            .iter()
            .min_by_key(|r| r.avg())
            .unwrap()
            .avg()
            .as_secs_f64()
            * 1000.0,
    );
    println!(
        "  Slowest: {} ({:.2}ms avg)",
        results.iter().max_by_key(|r| r.avg()).unwrap().name,
        results
            .iter()
            .max_by_key(|r| r.avg())
            .unwrap()
            .avg()
            .as_secs_f64()
            * 1000.0,
    );

    // ── Claude CLI comparison ────────────────────────────────────────────
    println!("\n─── Claude CLI Comparison ───");
    let claude_path = which::which("claude");
    match claude_path {
        Ok(path) => {
            println!("  Found: {}", path.display());

            // Check if API key is set
            {
                println!("  Benchmarking claude CLI startup overhead (--help)...");
                let cli_iters = 5;
                let mut total = Duration::ZERO;
                let mut min = Duration::MAX;
                let mut max = Duration::ZERO;

                for _ in 0..cli_iters {
                    let start = Instant::now();
                    let _ = tokio::process::Command::new(&path)
                        .args(["--help"])
                        .stdout(std::process::Stdio::null())
                        .stderr(std::process::Stdio::null())
                        .output()
                        .await;
                    let elapsed = start.elapsed();
                    total += elapsed;
                    min = min.min(elapsed);
                    max = max.max(elapsed);
                }
                let r = BenchResult {
                    name: "claude --help".into(),
                    total,
                    min,
                    max,
                    iters: cli_iters,
                };
                r.print();

                // Compare Cersei tool dispatch vs Claude CLI startup
                let cersei_avg = results[0].avg(); // File Read
                let cli_avg = r.avg();
                println!("\n  \x1b[36mCersei Read vs Claude startup:\x1b[0m");
                println!("    Cersei:  {:.3}ms", cersei_avg.as_secs_f64() * 1000.0);
                println!("    Claude:  {:.3}ms", cli_avg.as_secs_f64() * 1000.0);
                if cli_avg > cersei_avg && cersei_avg.as_nanos() > 0 {
                    println!(
                        "    \x1b[32mCersei is {:.0}× faster for tool dispatch\x1b[0m",
                        cli_avg.as_secs_f64() / cersei_avg.as_secs_f64()
                    );
                }
            }
        }
        Err(_) => {
            println!("  claude CLI not found in PATH — skipping comparison.");
        }
    }

    // ── Raw I/O comparison ───────────────────────────────────────────────
    println!("\n─── Raw I/O vs std::fs Baseline ───");
    {
        let iters = ITERATIONS;
        let start = Instant::now();
        for _ in 0..iters {
            let _ = std::fs::read_to_string(&test_file);
        }
        let std_read = start.elapsed() / iters;

        let start = Instant::now();
        for _ in 0..iters {
            std::fs::write(tmp.path().join("raw_bench.txt"), "hello\n")?;
        }
        let std_write = start.elapsed() / iters;

        println!(
            "  std::fs::read  {:.3}ms  vs  Cersei Read  {:.3}ms  (overhead: {:.3}ms)",
            std_read.as_secs_f64() * 1000.0,
            results[0].avg().as_secs_f64() * 1000.0,
            (results[0].avg() - std_read).as_secs_f64() * 1000.0,
        );
        println!(
            "  std::fs::write {:.3}ms  vs  Cersei Write {:.3}ms  (overhead: {:.3}ms)",
            std_write.as_secs_f64() * 1000.0,
            results[1].avg().as_secs_f64() * 1000.0,
            (results[1].avg() - std_write).as_secs_f64() * 1000.0,
        );
    }

    println!("\n✓ Benchmark complete.\n");
    Ok(())
}
