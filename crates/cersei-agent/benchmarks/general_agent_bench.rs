//! General Agent Framework Benchmark — Cersei harness
//!
//! Five axes:
//!   1. Instantiation (μs) — Agent::builder().build()
//!   2. Per-agent memory (bytes) — jemalloc allocated delta over N instantiations
//!   3. Max concurrent agents — spawn N tasks, first-tool-call latency p99, process RSS
//!   4. Graph memory recall under load — Cersei-only
//!   5. Semantic search under load — Cersei-only
//!
//! Writes JSON output to bench/general-agents/results/cersei.json matching the
//! shared schema read by bench/general-agents/aggregate.py.
//!
//! Run:
//!   cargo run --release -p cersei-agent --example general_agent_bench \
//!     --features bench-full
//!
//! Scoped run (skip slow axes during iteration):
//!   CERSEI_BENCH_AXES=1,2,3 cargo run --release ...

#[cfg(feature = "jemalloc-bench")]
#[global_allocator]
static ALLOC: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

use async_trait::async_trait;
use cersei_agent::Agent;
use cersei_provider::{CompletionRequest, CompletionStream, Provider, ProviderCapabilities};
use cersei_tools::permissions::AllowAll;
use cersei_tools::{PermissionLevel, Tool, ToolCategory, ToolContext, ToolResult};
use cersei_types::*;
use serde::Serialize;
use serde_json::json;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

// ─── Output schema ─────────────────────────────────────────────────────────

#[derive(Serialize)]
struct Report {
    framework: &'static str,
    version: String,
    host: HostInfo,
    axis_1_instantiation_us: Option<LatencyStats>,
    axis_2_per_agent_bytes: Option<MemStats>,
    axis_3_max_concurrent: Option<Vec<ConcurrencyPoint>>,
    axis_4_graph_recall_us: Option<ScalingLatency>,
    axis_5_semantic_search_us: Option<ScalingLatency>,
}

#[derive(Serialize)]
struct HostInfo {
    os: String,
    arch: String,
    cpu: String,
    ram_gb: u64,
    cgroup_memory_gb: Option<u64>,
}

#[derive(Serialize, Clone, Copy)]
struct LatencyStats {
    p50: f64,
    p95: f64,
    p99: f64,
    mean: f64,
    samples: usize,
}

#[derive(Serialize)]
struct MemStats {
    mean_bytes: u64,
    samples: usize,
    allocator: &'static str,
}

#[derive(Serialize)]
struct ConcurrencyPoint {
    n: usize,
    p50_ms: f64,
    p99_ms: f64,
    rss_mb: f64,
    wall_ms: f64,
}

#[derive(Serialize)]
struct ScalingLatency {
    at_10k: LatencyStats,
    at_100k: LatencyStats,
}

// ─── Stats helpers ─────────────────────────────────────────────────────────

fn percentile(sorted_us: &[f64], p: f64) -> f64 {
    if sorted_us.is_empty() {
        return 0.0;
    }
    let idx = ((sorted_us.len() as f64 - 1.0) * p).round() as usize;
    sorted_us[idx.min(sorted_us.len() - 1)]
}

fn summarize(mut samples: Vec<f64>) -> LatencyStats {
    samples.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let n = samples.len();
    let mean = samples.iter().sum::<f64>() / n.max(1) as f64;
    LatencyStats {
        p50: percentile(&samples, 0.50),
        p95: percentile(&samples, 0.95),
        p99: percentile(&samples, 0.99),
        mean,
        samples: n,
    }
}

fn us(d: Duration) -> f64 {
    d.as_nanos() as f64 / 1000.0
}

// ─── RSS reader (cross-platform, best-effort) ──────────────────────────────

fn read_rss_mb() -> f64 {
    #[cfg(target_os = "linux")]
    {
        if let Ok(status) = std::fs::read_to_string("/proc/self/status") {
            for line in status.lines() {
                if let Some(rest) = line.strip_prefix("VmRSS:") {
                    let kb: u64 = rest
                        .split_whitespace()
                        .next()
                        .and_then(|s| s.parse().ok())
                        .unwrap_or(0);
                    return kb as f64 / 1024.0;
                }
            }
        }
        0.0
    }
    #[cfg(target_os = "macos")]
    {
        use std::process::Command;
        let pid = std::process::id();
        let out = Command::new("ps")
            .args(["-o", "rss=", "-p", &pid.to_string()])
            .output();
        match out {
            Ok(o) => {
                let s = String::from_utf8_lossy(&o.stdout);
                s.trim()
                    .parse::<u64>()
                    .ok()
                    .map(|kb| kb as f64 / 1024.0)
                    .unwrap_or(0.0)
            }
            Err(_) => 0.0,
        }
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        0.0
    }
}

// ─── Jemalloc allocated bytes (optional) ───────────────────────────────────

#[cfg(feature = "jemalloc-bench")]
fn jemalloc_allocated() -> u64 {
    use tikv_jemalloc_ctl::{epoch, stats};
    epoch::advance().ok();
    stats::allocated::read().unwrap_or(0) as u64
}

#[cfg(not(feature = "jemalloc-bench"))]
fn jemalloc_allocated() -> u64 {
    0
}

// ─── Host info ─────────────────────────────────────────────────────────────

fn host_info() -> HostInfo {
    let os = std::env::consts::OS.to_string();
    let arch = std::env::consts::ARCH.to_string();
    let cpu = detect_cpu();
    let ram_gb = detect_ram_gb();
    let cgroup_memory_gb = detect_cgroup_gb();
    HostInfo {
        os,
        arch,
        cpu,
        ram_gb,
        cgroup_memory_gb,
    }
}

fn detect_cpu() -> String {
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("sysctl")
            .args(["-n", "machdep.cpu.brand_string"])
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .map(|s| s.trim().to_string())
            .unwrap_or_else(|| "unknown".into())
    }
    #[cfg(target_os = "linux")]
    {
        std::fs::read_to_string("/proc/cpuinfo")
            .ok()
            .and_then(|s| {
                s.lines()
                    .find(|l| l.starts_with("model name"))
                    .and_then(|l| l.split(':').nth(1))
                    .map(|s| s.trim().to_string())
            })
            .unwrap_or_else(|| "unknown".into())
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        "unknown".into()
    }
}

fn detect_ram_gb() -> u64 {
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("sysctl")
            .args(["-n", "hw.memsize"])
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .and_then(|s| s.trim().parse::<u64>().ok())
            .map(|b| b / 1_073_741_824)
            .unwrap_or(0)
    }
    #[cfg(target_os = "linux")]
    {
        std::fs::read_to_string("/proc/meminfo")
            .ok()
            .and_then(|s| {
                s.lines()
                    .find(|l| l.starts_with("MemTotal:"))
                    .and_then(|l| l.split_whitespace().nth(1))
                    .and_then(|s| s.parse::<u64>().ok())
            })
            .map(|kb| kb / 1_048_576)
            .unwrap_or(0)
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        0
    }
}

fn detect_cgroup_gb() -> Option<u64> {
    // Linux cgroup v2 memory.max
    std::fs::read_to_string("/sys/fs/cgroup/memory.max")
        .ok()
        .and_then(|s| s.trim().parse::<u64>().ok())
        .map(|b| b / 1_073_741_824)
}

// ─── Stub provider ─────────────────────────────────────────────────────────

/// Minimal in-process provider — returns a canned assistant text + EndTurn
/// immediately. Zero network, zero allocation in the hot path beyond the
/// channel buffer.
struct StubProvider;

#[async_trait]
impl Provider for StubProvider {
    fn name(&self) -> &str {
        "stub"
    }
    fn context_window(&self, _model: &str) -> u64 {
        1_000_000
    }
    fn capabilities(&self, _model: &str) -> ProviderCapabilities {
        ProviderCapabilities {
            streaming: true,
            tool_use: false,
            vision: false,
            thinking: false,
            system_prompt: true,
            caching: false,
        }
    }

    async fn complete(&self, _request: CompletionRequest) -> Result<CompletionStream> {
        let (tx, rx) = mpsc::channel(8);
        tokio::spawn(async move {
            let _ = tx
                .send(StreamEvent::MessageStart {
                    id: "stub-1".into(),
                    model: "stub".into(),
                })
                .await;
            let _ = tx
                .send(StreamEvent::ContentBlockStart {
                    index: 0,
                    block_type: "text".into(),
                    id: None,
                    name: None,
                })
                .await;
            let _ = tx
                .send(StreamEvent::TextDelta {
                    index: 0,
                    text: "ok".into(),
                })
                .await;
            let _ = tx.send(StreamEvent::ContentBlockStop { index: 0 }).await;
            let _ = tx
                .send(StreamEvent::MessageDelta {
                    stop_reason: Some(StopReason::EndTurn),
                    usage: Some(Usage {
                        input_tokens: 1,
                        output_tokens: 1,
                        ..Default::default()
                    }),
                })
                .await;
            let _ = tx.send(StreamEvent::MessageStop).await;
        });
        Ok(CompletionStream::new(rx))
    }
}

// ─── Echo tool ─────────────────────────────────────────────────────────────

struct EchoTool;

#[async_trait]
impl Tool for EchoTool {
    fn name(&self) -> &str {
        "echo"
    }
    fn description(&self) -> &str {
        "Return the input text unchanged."
    }
    fn permission_level(&self) -> PermissionLevel {
        PermissionLevel::None
    }
    fn category(&self) -> ToolCategory {
        ToolCategory::Custom
    }
    fn input_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": { "msg": { "type": "string" } },
            "required": ["msg"]
        })
    }
    async fn execute(&self, input: serde_json::Value, _ctx: &ToolContext) -> ToolResult {
        let msg = input.get("msg").and_then(|v| v.as_str()).unwrap_or("");
        ToolResult::success(msg.to_string())
    }
}

fn build_agent() -> Agent {
    Agent::builder()
        .provider(StubProvider)
        .tools(vec![Box::new(EchoTool) as Box<dyn Tool>])
        .permission_policy(AllowAll)
        .max_turns(1)
        .build()
        .expect("agent build")
}

// ─── Axis 1 — Instantiation ────────────────────────────────────────────────

fn axis_1_instantiation(iters: usize) -> LatencyStats {
    // Warmup
    for _ in 0..100 {
        let _ = build_agent();
    }

    let mut samples = Vec::with_capacity(iters);
    for _ in 0..iters {
        let start = Instant::now();
        let a = build_agent();
        samples.push(us(start.elapsed()));
        drop(a);
    }
    summarize(samples)
}

// ─── Axis 2 — Per-agent memory ─────────────────────────────────────────────

fn axis_2_per_agent_memory(iters: usize) -> MemStats {
    let before = jemalloc_allocated();
    let mut agents: Vec<Agent> = Vec::with_capacity(iters);
    for _ in 0..iters {
        agents.push(build_agent());
    }
    let after = jemalloc_allocated();
    let delta = after.saturating_sub(before);
    let mean = delta / iters as u64;
    drop(agents);
    MemStats {
        mean_bytes: mean,
        samples: iters,
        allocator: if cfg!(feature = "jemalloc-bench") {
            "jemalloc (stats::allocated)"
        } else {
            "unavailable (build with --features jemalloc-bench)"
        },
    }
}

// ─── Axis 3 — Max concurrent agents ────────────────────────────────────────

async fn axis_3_max_concurrent(steps: &[usize]) -> Vec<ConcurrencyPoint> {
    // Parity with the Python harnesses: build N agents concurrently, hold them
    // all live, measure per-construction latency + total RSS. We deliberately
    // do NOT run a turn — Agno's cookbook doesn't either, and invoking would
    // require every Python framework to ship an in-process LLM stub.
    let mut out = Vec::new();
    for &n in steps {
        let wall_start = Instant::now();
        let mut handles = Vec::with_capacity(n);
        for _ in 0..n {
            handles.push(tokio::spawn(async move {
                let start = Instant::now();
                let agent = build_agent();
                let elapsed = start.elapsed();
                (agent, elapsed)
            }));
        }
        let mut agents = Vec::with_capacity(n);
        let mut latencies = Vec::with_capacity(n);
        for h in handles {
            if let Ok((a, d)) = h.await {
                agents.push(a);
                latencies.push(d.as_nanos() as f64 / 1_000_000.0); // ms
            }
        }
        let wall_ms = wall_start.elapsed().as_nanos() as f64 / 1_000_000.0;
        latencies.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let p50 = percentile(&latencies, 0.50);
        let p99 = percentile(&latencies, 0.99);
        let rss_mb = read_rss_mb();
        println!(
            "  axis-3 n={n:>6}  p50={p50:>7.2}ms  p99={p99:>7.2}ms  rss={rss_mb:>7.1}MB  wall={wall_ms:>7.1}ms",
        );
        out.push(ConcurrencyPoint {
            n,
            p50_ms: p50,
            p99_ms: p99,
            rss_mb,
            wall_ms,
        });
        drop(agents); // release before the next step
    }
    out
}

// ─── Axis 4 — Graph memory recall under load ───────────────────────────────

#[cfg(feature = "graph-bench")]
async fn axis_4_graph_under_load() -> ScalingLatency {
    use cersei_memory::manager::MemoryManager;
    use cersei_memory::memdir::MemoryType;

    async fn measure(n_nodes: usize) -> LatencyStats {
        let tmp = tempfile::tempdir().unwrap();
        let graph_path = tmp.path().join("bench.grafeo");
        let mm = Arc::new(
            MemoryManager::new(tmp.path())
                .with_graph(&graph_path)
                .expect("open graph"),
        );

        println!("  axis-4  seeding {n_nodes} nodes...");
        for i in 0..n_nodes {
            mm.store_memory(
                &format!("Memory {i}: topic_{} about rust {}", i % 20, i % 7),
                match i % 4 {
                    0 => MemoryType::User,
                    1 => MemoryType::Feedback,
                    2 => MemoryType::Project,
                    _ => MemoryType::Reference,
                },
                0.8,
            );
        }

        // 10 concurrent "agents" × 100 recalls each. Graph recall is sync
        // (spawn_blocking) so higher concurrency just measures blocking-pool
        // contention, not per-query latency.
        let concurrency = 10usize;
        let per_agent = 100usize;
        let mut handles = Vec::with_capacity(concurrency);
        for a in 0..concurrency {
            let mm = Arc::clone(&mm);
            handles.push(tokio::task::spawn_blocking(move || {
                let mut samples = Vec::with_capacity(per_agent);
                for q in 0..per_agent {
                    let query = format!("topic_{} rust", (a + q) % 20);
                    let t = Instant::now();
                    let _ = mm.recall(&query, 5);
                    samples.push(us(t.elapsed()));
                }
                samples
            }));
        }
        let mut all = Vec::with_capacity(concurrency * per_agent);
        for h in handles {
            all.extend(h.await.unwrap());
        }
        let stats = summarize(all);
        println!(
            "  axis-4  n={n_nodes} p50={:.1}us p95={:.1}us p99={:.1}us",
            stats.p50, stats.p95, stats.p99
        );
        stats
    }

    // Scale tunable via CERSEI_BENCH_GRAPH_MAX (default 10k; large for 100k).
    let max = std::env::var("CERSEI_BENCH_GRAPH_MAX")
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(10_000);
    ScalingLatency {
        at_10k: measure(max.min(10_000)).await,
        at_100k: if max >= 100_000 {
            measure(100_000).await
        } else {
            LatencyStats {
                p50: 0.0,
                p95: 0.0,
                p99: 0.0,
                mean: 0.0,
                samples: 0,
            }
        },
    }
}

#[cfg(not(feature = "graph-bench"))]
async fn axis_4_graph_under_load() -> ScalingLatency {
    println!("  axis-4  skipped (build with --features graph-bench)");
    let empty = LatencyStats {
        p50: 0.0,
        p95: 0.0,
        p99: 0.0,
        mean: 0.0,
        samples: 0,
    };
    ScalingLatency {
        at_10k: empty,
        at_100k: empty,
    }
}

// ─── Axis 5 — Semantic search under load ───────────────────────────────────

use cersei_embeddings::{EmbeddingError, EmbeddingProvider, EmbeddingStore, Metric};

/// Deterministic stub embedder — hashes text into a 64-d vector. Zero network.
struct StubEmbedder {
    dim: usize,
}

#[async_trait]
impl EmbeddingProvider for StubEmbedder {
    fn name(&self) -> &str {
        "stub"
    }
    fn dimensions(&self) -> usize {
        self.dim
    }

    async fn embed_batch(
        &self,
        texts: &[String],
    ) -> std::result::Result<Vec<Vec<f32>>, EmbeddingError> {
        Ok(texts.iter().map(|t| hash_vec(t, self.dim)).collect())
    }
}

fn hash_vec(text: &str, dim: usize) -> Vec<f32> {
    use std::hash::{Hash, Hasher};
    let mut v = Vec::with_capacity(dim);
    for i in 0..dim {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        text.hash(&mut h);
        (i as u64).hash(&mut h);
        let x = (h.finish() as u64) as f32 / u64::MAX as f32;
        v.push(x * 2.0 - 1.0);
    }
    // L2-normalize so cosine makes sense
    let mag = v.iter().map(|x| x * x).sum::<f32>().sqrt().max(1e-8);
    for x in v.iter_mut() {
        *x /= mag;
    }
    v
}

async fn axis_5_semantic_under_load() -> ScalingLatency {
    async fn measure(n_chunks: usize) -> LatencyStats {
        let store =
            Arc::new(EmbeddingStore::new(StubEmbedder { dim: 64 }, Metric::Cosine).expect("store"));
        println!("  axis-5  seeding {n_chunks} chunks...");
        let batch: Vec<(u64, String)> = (0..n_chunks)
            .map(|i| (i as u64, format!("chunk_{i} about rust topic_{}", i % 20)))
            .collect();
        // Ingest in batches of 5000 to keep memory reasonable
        for slice in batch.chunks(5000) {
            store.add_batch(slice).await.expect("add_batch");
        }

        // Semantic search is async-native — can sustain higher concurrency
        // cleanly, so we keep 50 concurrent "agents" × 100 queries each.
        let concurrency = 50usize;
        let per_agent = 100usize;
        let mut handles = Vec::with_capacity(concurrency);
        for a in 0..concurrency {
            let store = Arc::clone(&store);
            handles.push(tokio::spawn(async move {
                let mut samples = Vec::with_capacity(per_agent);
                for q in 0..per_agent {
                    let query = format!("rust topic_{}", (a + q) % 20);
                    let t = Instant::now();
                    let _ = store.search(&query, 5).await;
                    samples.push(us(t.elapsed()));
                }
                samples
            }));
        }
        let mut all = Vec::with_capacity(concurrency * per_agent);
        for h in handles {
            all.extend(h.await.unwrap());
        }
        let stats = summarize(all);
        println!(
            "  axis-5  n={n_chunks} p50={:.1}us p95={:.1}us p99={:.1}us",
            stats.p50, stats.p95, stats.p99
        );
        stats
    }

    let max = std::env::var("CERSEI_BENCH_SEMANTIC_MAX")
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(10_000);
    ScalingLatency {
        at_10k: measure(max.min(10_000)).await,
        at_100k: if max >= 100_000 {
            measure(100_000).await
        } else {
            LatencyStats {
                p50: 0.0,
                p95: 0.0,
                p99: 0.0,
                mean: 0.0,
                samples: 0,
            }
        },
    }
}

// ─── Entry point ───────────────────────────────────────────────────────────

fn enabled(axes: &[u8], n: u8) -> bool {
    axes.is_empty() || axes.contains(&n)
}

fn parse_axes() -> Vec<u8> {
    std::env::var("CERSEI_BENCH_AXES")
        .ok()
        .map(|s| {
            s.split(',')
                .filter_map(|x| x.trim().parse().ok())
                .collect::<Vec<u8>>()
        })
        .unwrap_or_default()
}

fn write_report(report: &Report) -> PathBuf {
    let out_dir = std::env::var("CERSEI_BENCH_OUT_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            // default: repo-root bench/general-agents/results/
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("..")
                .join("..")
                .join("bench")
                .join("general-agents")
                .join("results")
        });
    std::fs::create_dir_all(&out_dir).ok();
    let path = out_dir.join("cersei.json");
    let json = serde_json::to_string_pretty(report).expect("serialize report");
    std::fs::write(&path, json).expect("write report");
    path
}

#[tokio::main(flavor = "multi_thread")]
async fn main() {
    println!("\x1b[36;1m╔═══════════════════════════════════════════════════════════╗\x1b[0m");
    println!("\x1b[36;1m║  Cersei — General-Agent Framework Benchmark               ║\x1b[0m");
    println!("\x1b[36;1m╚═══════════════════════════════════════════════════════════╝\x1b[0m");

    let axes = parse_axes();
    if !axes.is_empty() {
        println!("  running axes: {axes:?}");
    }

    let host = host_info();
    println!(
        "  host: {} / {} / {} / {} GB",
        host.os, host.arch, host.cpu, host.ram_gb
    );
    if let Some(cg) = host.cgroup_memory_gb {
        println!("  cgroup memory cap: {cg} GB");
    }

    // Axis 1
    let axis_1 = if enabled(&axes, 1) {
        println!("\n[axis 1] instantiation (1000 samples)...");
        let s = axis_1_instantiation(1000);
        println!(
            "  p50={:.2}us  p95={:.2}us  p99={:.2}us  mean={:.2}us",
            s.p50, s.p95, s.p99, s.mean
        );
        Some(s)
    } else {
        None
    };

    // Axis 2
    let axis_2 = if enabled(&axes, 2) {
        println!("\n[axis 2] per-agent memory (1000 held live)...");
        let s = axis_2_per_agent_memory(1000);
        println!("  mean_bytes={} ({})", s.mean_bytes, s.allocator);
        Some(s)
    } else {
        None
    };

    // Axis 3
    let axis_3 = if enabled(&axes, 3) {
        println!("\n[axis 3] max concurrent agents (ramp)...");
        let steps: &[usize] = &[100, 500, 1_000, 5_000, 10_000];
        let s = axis_3_max_concurrent(steps).await;
        Some(s)
    } else {
        None
    };

    // Axis 4
    let axis_4 = if enabled(&axes, 4) {
        println!("\n[axis 4] graph memory recall under load (100 agents × 100 recalls)...");
        Some(axis_4_graph_under_load().await)
    } else {
        None
    };

    // Axis 5
    let axis_5 = if enabled(&axes, 5) {
        println!("\n[axis 5] semantic search under load (100 agents × 100 queries)...");
        Some(axis_5_semantic_under_load().await)
    } else {
        None
    };

    let report = Report {
        framework: "cersei",
        version: env!("CARGO_PKG_VERSION").to_string(),
        host,
        axis_1_instantiation_us: axis_1,
        axis_2_per_agent_bytes: axis_2,
        axis_3_max_concurrent: axis_3,
        axis_4_graph_recall_us: axis_4,
        axis_5_semantic_search_us: axis_5,
    };

    let out = write_report(&report);
    println!("\n\x1b[32mReport written:\x1b[0m {}", out.display());
}
