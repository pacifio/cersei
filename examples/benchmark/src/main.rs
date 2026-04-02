//! Cersei SDK — Tool I/O Benchmark Suite
//!
//! Measures raw tool execution speed (no LLM calls) for all built-in
//! coding agent tools and compares against native Claude Code CLI.
//!
//! Run:
//!   cargo run --release
//!
//! What it tests:
//!   1. File Read   — read a 50KB file with line numbering
//!   2. File Write  — write a 1KB file to disk
//!   3. File Edit   — string replacement in a file
//!   4. Glob        — find *.rs files across 20 files
//!   5. Grep        — regex search across 20 files (via rg or grep)
//!   6. Bash        — spawn sh -c and capture output
//!   7. std::fs     — raw stdlib baseline for read/write
//!   8. Claude CLI  — startup overhead of native claude binary

use cersei::prelude::*;
use std::sync::Arc;
use std::time::{Duration, Instant};

// ─── Bench harness ───────────────────────────────────────────────────────────

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

    fn avg_ms(&self) -> f64 {
        self.avg().as_secs_f64() * 1000.0
    }

    fn print(&self) {
        println!(
            "  {:<14} avg={:>8.3}ms  min={:>8.3}ms  max={:>8.3}ms  ({} iters)",
            self.name,
            self.avg_ms(),
            self.min.as_secs_f64() * 1000.0,
            self.max.as_secs_f64() * 1000.0,
            self.iters,
        );
    }

    fn print_markdown(&self) {
        println!(
            "| {} | {:.3}ms | {:.3}ms | {:.3}ms |",
            self.name,
            self.avg_ms(),
            self.min.as_secs_f64() * 1000.0,
            self.max.as_secs_f64() * 1000.0,
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

    // Warmup (3 runs)
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

async fn bench_command(
    name: &str,
    cmd: &str,
    args: &[&str],
    iters: u32,
) -> BenchResult {
    let mut total = Duration::ZERO;
    let mut min = Duration::MAX;
    let mut max = Duration::ZERO;

    // Warmup
    let _ = tokio::process::Command::new(cmd)
        .args(args)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .await;

    for _ in 0..iters {
        let start = Instant::now();
        let _ = tokio::process::Command::new(cmd)
            .args(args)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .output()
            .await;
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

fn bench_sync(name: &str, iters: u32, f: impl Fn()) -> BenchResult {
    let mut total = Duration::ZERO;
    let mut min = Duration::MAX;
    let mut max = Duration::ZERO;

    // Warmup
    for _ in 0..3 {
        f();
    }

    for _ in 0..iters {
        let start = Instant::now();
        f();
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

// ─── Main ────────────────────────────────────────────────────────────────────

const ITERS: u32 = 100;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let tmp = tempfile::tempdir()?;

    // ── Setup test fixtures ──────────────────────────────────────────────
    let test_file = tmp.path().join("test_file.txt");
    let test_content =
        "Hello, world!\nThis is a test file.\nLine three.\nLine four.\nLine five.\n".repeat(100);
    std::fs::write(&test_file, &test_content)?;

    for i in 0..20 {
        let f = tmp.path().join(format!("mod_{}.rs", i));
        std::fs::write(&f, format!("// module {i}\nfn func_{i}() {{}}\npub struct S{i};\n"))?;
    }

    let ctx = ToolContext {
        working_dir: tmp.path().to_path_buf(),
        session_id: "bench".to_string(),
        permissions: Arc::new(AllowAll),
        cost_tracker: Arc::new(CostTracker::new()),
        mcp_manager: None,
        extensions: Extensions::default(),
    };

    let tools = cersei::tools::all();
    let find_tool = |name: &str| -> &dyn Tool {
        tools.iter().find(|t| t.name() == name).unwrap().as_ref()
    };

    // ── Header ───────────────────────────────────────────────────────────
    println!();
    println!("  Cersei SDK — Tool I/O Benchmark");
    println!("  ================================");
    println!("  Iterations : {ITERS}");
    println!("  Warmup     : 3");
    println!("  Profile    : release (optimized)");
    println!("  Tmpdir     : {}", tmp.path().display());
    println!("  Platform   : {} {}", std::env::consts::OS, std::env::consts::ARCH);
    println!();

    let mut results: Vec<BenchResult> = Vec::new();

    // ── Tool benchmarks ──────────────────────────────────────────────────
    println!("  Tool I/O");
    println!("  --------");

    let r = bench_tool(
        "Read",
        find_tool("Read"),
        serde_json::json!({ "file_path": test_file.display().to_string() }),
        &ctx,
        ITERS,
    )
    .await;
    r.print();
    results.push(r);

    let write_path = tmp.path().join("write_bench.txt");
    let r = bench_tool(
        "Write",
        find_tool("Write"),
        serde_json::json!({
            "file_path": write_path.display().to_string(),
            "content": "Benchmark content line\n".repeat(50),
        }),
        &ctx,
        ITERS,
    )
    .await;
    r.print();
    results.push(r);

    let edit_tool = find_tool("Edit");
    {
        let mut total = Duration::ZERO;
        let mut min = Duration::MAX;
        let mut max = Duration::ZERO;
        for _ in 0..3 {
            std::fs::write(&test_file, &test_content)?;
            edit_tool
                .execute(
                    serde_json::json!({
                        "file_path": test_file.display().to_string(),
                        "old_string": "Hello, world!",
                        "new_string": "Hello, Cersei!",
                    }),
                    &ctx,
                )
                .await;
        }
        for _ in 0..ITERS {
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
            iters: ITERS,
        };
        r.print();
        results.push(r);
    }

    let r = bench_tool(
        "Glob",
        find_tool("Glob"),
        serde_json::json!({
            "pattern": "**/*.rs",
            "path": tmp.path().display().to_string(),
        }),
        &ctx,
        ITERS,
    )
    .await;
    r.print();
    results.push(r);

    let r = bench_tool(
        "Grep",
        find_tool("Grep"),
        serde_json::json!({
            "pattern": "func_",
            "path": tmp.path().display().to_string(),
        }),
        &ctx,
        ITERS,
    )
    .await;
    r.print();
    results.push(r);

    let r = bench_tool(
        "Bash",
        find_tool("Bash"),
        serde_json::json!({ "command": "echo hello && ls -la" }),
        &ctx,
        ITERS,
    )
    .await;
    r.print();
    results.push(r);

    // ── Raw stdlib baseline ──────────────────────────────────────────────
    println!();
    println!("  std::fs Baseline");
    println!("  ----------------");

    let tf = test_file.clone();
    let r_std_read = bench_sync("std::fs::read", ITERS, move || {
        let _ = std::fs::read_to_string(&tf);
    });
    r_std_read.print();

    let wp = tmp.path().join("raw_bench.txt");
    let r_std_write = bench_sync("std::fs::write", ITERS, move || {
        let _ = std::fs::write(&wp, "hello\n");
    });
    r_std_write.print();

    // ── Claude CLI startup ───────────────────────────────────────────────
    println!();
    println!("  Claude CLI");
    println!("  ----------");

    if let Ok(claude) = which::which("claude") {
        println!("  Found: {}", claude.display());
        let r_cli =
            bench_command("claude --help", claude.to_str().unwrap(), &["--help"], 10).await;
        r_cli.print();

        // Version
        if let Ok(out) = tokio::process::Command::new(&claude)
            .args(["--version"])
            .output()
            .await
        {
            let ver = String::from_utf8_lossy(&out.stdout);
            println!("  Version: {}", ver.trim());
        }

        // ── Comparison table ─────────────────────────────────────────────
        println!();
        println!("  Comparison");
        println!("  ----------");
        let cersei_read = results[0].avg_ms();
        let cli_startup = r_cli.avg_ms();
        let ratio = cli_startup / cersei_read;
        println!("  Cersei tool dispatch (Read): {:.3}ms", cersei_read);
        println!("  Claude CLI startup:          {:.1}ms", cli_startup);
        println!("  Ratio:                       Cersei is {:.0}x faster", ratio);

        println!();
        println!("  Cersei overhead vs std::fs:");
        println!(
            "    Read:  +{:.3}ms  ({:.1}x std::fs)",
            results[0].avg_ms() - r_std_read.avg_ms(),
            results[0].avg_ms() / r_std_read.avg_ms().max(0.001),
        );
        println!(
            "    Write: +{:.3}ms  ({:.1}x std::fs)",
            results[1].avg_ms() - r_std_write.avg_ms(),
            results[1].avg_ms() / r_std_write.avg_ms().max(0.001),
        );
    } else {
        println!("  claude CLI not found in PATH — skipping.");
    }

    // ── Markdown output ──────────────────────────────────────────────────
    println!();
    println!("  Markdown Table (copy-paste)");
    println!("  --------------------------");
    println!("| Tool | Avg | Min | Max |");
    println!("|------|-----|-----|-----|");
    for r in &results {
        r.print_markdown();
    }
    r_std_read.print_markdown();
    r_std_write.print_markdown();

    println!();
    println!("  Done.");
    println!();
    Ok(())
}
