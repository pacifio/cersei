//! Memory Architecture Benchmark — Abstract (Cersei)
//!
//! Measures every memory operation with precise timing, both graph-ON and graph-OFF.
//! Run: cargo run --release --manifest-path crates/abstract-cli/Cargo.toml --example memory_bench

use cersei_agent::auto_dream::AutoDream;
use cersei_agent::session_memory;
use cersei_memory::manager::MemoryManager;
use cersei_memory::memdir::{self, MemoryType};
use cersei_memory::session_storage;
use cersei_types::*;
use std::path::Path;
use std::time::{Duration, Instant};

// ─── Benchmark harness ─────────────────────────────────────────────────────

struct BenchResult {
    name: String,
    avg: Duration,
    min: Duration,
    max: Duration,
    iters: usize,
}

fn bench<F: FnMut()>(name: &str, iters: usize, mut f: F) -> BenchResult {
    // Warmup
    for _ in 0..3 {
        f();
    }

    let mut times = Vec::with_capacity(iters);
    for _ in 0..iters {
        let start = Instant::now();
        f();
        times.push(start.elapsed());
    }

    let total: Duration = times.iter().sum();
    let avg = total / iters as u32;
    let min = *times.iter().min().unwrap();
    let max = *times.iter().max().unwrap();

    BenchResult {
        name: name.to_string(),
        avg,
        min,
        max,
        iters,
    }
}

fn print_result(r: &BenchResult) {
    println!(
        "  {:<40} avg={:>8.1}us  min={:>8.1}us  max={:>8.1}us  ({} iters)",
        r.name,
        r.avg.as_nanos() as f64 / 1000.0,
        r.min.as_nanos() as f64 / 1000.0,
        r.max.as_nanos() as f64 / 1000.0,
        r.iters,
    );
}

fn section(title: &str) {
    println!("\n\x1b[36m--- {} ---\x1b[0m", title);
}

// ─── Data generators ───────────────────────────────────────────────────────

fn create_memory_files(dir: &Path, count: usize) {
    std::fs::create_dir_all(dir).unwrap();
    // Create MEMORY.md index
    let mut index = String::new();
    for i in 0..count.min(200) {
        index.push_str(&format!("- [mem_{i}](mem_{i}.md) — memory entry {i}\n"));
    }
    std::fs::write(dir.join("MEMORY.md"), &index).unwrap();

    // Create individual memory files
    for i in 0..count {
        let mem_type = match i % 4 {
            0 => "user",
            1 => "feedback",
            2 => "project",
            _ => "reference",
        };
        let content = format!(
            "---\nname: Memory {i}\ndescription: Test memory entry number {i}\ntype: {mem_type}\n---\n\n\
            This is memory entry {i}. It contains information about topic_{} and relates to area_{}.\n\
            Keywords: rust, testing, performance, benchmark, memory_{i}\n",
            i % 10,
            i % 5,
        );
        std::fs::write(dir.join(format!("mem_{i}.md")), &content).unwrap();
    }
}

fn create_claude_md(root: &Path) {
    let content = "# Project Rules\n\n\
        - Use Rust for all new code\n\
        - Run `cargo test` before committing\n\
        - Follow the builder pattern for configuration\n\
        - Keep functions under 50 lines\n";
    std::fs::write(root.join("CLAUDE.md"), content).unwrap();

    let local_dir = root.join(".claude");
    std::fs::create_dir_all(&local_dir).unwrap();
    std::fs::write(
        local_dir.join("CLAUDE.md"),
        "# Local Overrides\n\n- Debug mode enabled\n- Extra verbose logging\n",
    )
    .unwrap();
}

fn create_session_entries(path: &Path, count: usize) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    for i in 0..count {
        let msg = if i % 2 == 0 {
            Message::user(&format!("User message {i}"))
        } else {
            Message::assistant(&format!("Assistant response {i}"))
        };
        let entry = if i % 2 == 0 {
            session_storage::TranscriptEntry::User(session_storage::TranscriptMessage {
                uuid: uuid::Uuid::new_v4().to_string(),
                parent_uuid: None,
                timestamp: chrono::Utc::now().to_rfc3339(),
                session_id: "bench-session".to_string(),
                cwd: "/tmp".to_string(),
                message: msg,
                is_sidechain: false,
                extra: Default::default(),
            })
        } else {
            session_storage::TranscriptEntry::Assistant(session_storage::TranscriptMessage {
                uuid: uuid::Uuid::new_v4().to_string(),
                parent_uuid: None,
                timestamp: chrono::Utc::now().to_rfc3339(),
                session_id: "bench-session".to_string(),
                cwd: "/tmp".to_string(),
                message: msg,
                is_sidechain: false,
                extra: Default::default(),
            })
        };
        session_storage::write_transcript_entry(path, &entry).unwrap();
    }
}

// ─── Main ──────────────────────────────────────────────────────────────────

fn main() {
    println!("\x1b[36;1m╔══════════════════════════════════════════════════════════════╗\x1b[0m");
    println!("\x1b[36;1m║  Abstract — Memory Architecture Benchmark                    ║\x1b[0m");
    println!("\x1b[36;1m╚══════════════════════════════════════════════════════════════╝\x1b[0m");

    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();

    // ═══ SECTION 1: MEMDIR SCAN ══════════════════════════════════════════

    section("1. Memory Directory Scan");

    for &count in &[10, 100, 200, 500] {
        let mem_dir = root.join(format!("scan_{count}"));
        create_memory_files(&mem_dir, count);

        let r = bench(&format!("Scan {} files", count), 100, || {
            let _ = memdir::scan_memory_dir(&mem_dir);
        });
        print_result(&r);
    }

    // ═══ SECTION 2: RECALL / QUERY ═══════════════════════════════════════

    section("2. Recall / Query (text matching, no graph)");

    for &count in &[10, 100, 500] {
        let mem_dir = root.join(format!("recall_{count}"));
        create_memory_files(&mem_dir, count);

        let mm = MemoryManager::new(root).with_memory_dir(mem_dir);

        // Recall hit (keyword exists)
        let r = bench(&format!("Recall hit ({} files)", count), 50, || {
            let _ = mm.recall("rust", 5);
        });
        print_result(&r);

        // Recall miss (keyword doesn't exist)
        let r = bench(&format!("Recall miss ({} files)", count), 50, || {
            let _ = mm.recall("nonexistent_keyword_xyz", 5);
        });
        print_result(&r);
    }

    // ═══ SECTION 3: CONTEXT BUILDING ═════════════════════════════════════

    section("3. Context Building (CLAUDE.md + MEMORY.md)");

    // Setup: CLAUDE.md + memory files
    let ctx_root = root.join("context_test");
    std::fs::create_dir_all(&ctx_root).unwrap();
    create_claude_md(&ctx_root);
    let ctx_mem = ctx_root.join("memory");
    create_memory_files(&ctx_mem, 100);

    let mm = MemoryManager::new(&ctx_root).with_memory_dir(ctx_mem);

    let r = bench("Build full context (100 files)", 100, || {
        let _ = mm.build_context();
    });
    print_result(&r);

    // Load MEMORY.md only (various sizes)
    for &lines in &[50, 100, 200] {
        let idx_dir = root.join(format!("idx_{lines}"));
        std::fs::create_dir_all(&idx_dir).unwrap();
        let mut content = String::new();
        for i in 0..lines {
            content.push_str(&format!("- [item_{i}](item_{i}.md) — description {i}\n"));
        }
        std::fs::write(idx_dir.join("MEMORY.md"), &content).unwrap();

        let r = bench(&format!("Load MEMORY.md ({} lines)", lines), 200, || {
            let _ = memdir::load_memory_index(&idx_dir);
        });
        print_result(&r);
    }

    // ═══ SECTION 4: SESSION I/O ══════════════════════════════════════════

    section("4. Session I/O (JSONL)");

    // Write benchmarks
    {
        let session_dir = root.join("session_write");
        std::fs::create_dir_all(&session_dir).unwrap();
        let mm = MemoryManager::new(root).with_sessions_dir(session_dir.clone());

        let r = bench("Session write (single entry)", 200, || {
            let _ = mm.write_user_message("bench-w", Message::user("test message"));
        });
        print_result(&r);

        // Burst write: 100 entries
        let r = bench("Session write (100 burst)", 10, || {
            let sid = format!("burst-{}", uuid::Uuid::new_v4());
            for j in 0..100 {
                let _ = mm.write_user_message(&sid, Message::user(&format!("msg {j}")));
            }
        });
        print_result(&r);
    }

    // Load benchmarks
    for &count in &[10, 100, 500, 1000] {
        let session_file = root.join(format!("session_load_{count}.jsonl"));
        create_session_entries(&session_file, count);

        let r = bench(&format!("Session load ({} entries)", count), 50, || {
            let entries = session_storage::load_transcript(&session_file).unwrap();
            let _ = session_storage::messages_from_transcript(&entries);
        });
        print_result(&r);
    }

    // ═══ SECTION 5: GRAPH MEMORY ═════════════════════════════════════════

    section("5. Graph Memory (Grafeo)");

    #[cfg(feature = "graph")]
    {
        let graph_dir = root.join("graph_bench");
        std::fs::create_dir_all(&graph_dir).unwrap();
        let graph_path = graph_dir.join("bench.grafeo");

        let mm = MemoryManager::new(&graph_dir)
            .with_graph(&graph_path)
            .expect("Failed to open graph");

        // Store single
        let r = bench("Graph store (single node)", 100, || {
            let _ = mm.store_memory(
                "Test memory about Rust performance",
                MemoryType::Project,
                0.8,
            );
        });
        print_result(&r);

        // Store bulk
        let r = bench("Graph store (100 nodes bulk)", 10, || {
            for j in 0..100 {
                let _ = mm.store_memory(
                    &format!("Bulk memory {j} about topic_{}", j % 10),
                    MemoryType::User,
                    0.7,
                );
            }
        });
        print_result(&r);

        // Pre-populate for query benchmarks
        let mut ids: Vec<String> = Vec::new();
        for j in 0..100 {
            if let Some(id) = mm.store_memory(
                &format!("Indexed memory {j}: topic_{} area_{}", j % 10, j % 5),
                match j % 4 {
                    0 => MemoryType::User,
                    1 => MemoryType::Feedback,
                    2 => MemoryType::Project,
                    _ => MemoryType::Reference,
                },
                0.9,
            ) {
                if j < 20 {
                    mm.tag_memory(&id, &format!("topic_{}", j % 5));
                }
                ids.push(id);
            }
        }

        // Link some memories
        for i in 0..ids.len().min(50).saturating_sub(1) {
            mm.link_memories(&ids[i], &ids[i + 1], "relates_to");
        }

        // Tag
        let sample_id = ids.first().cloned().unwrap_or_default();
        let r = bench("Graph tag memory", 100, || {
            mm.tag_memory(&sample_id, "benchmark_topic");
        });
        print_result(&r);

        // Link
        let id_a = ids.get(0).cloned().unwrap_or_default();
        let id_b = ids.get(1).cloned().unwrap_or_default();
        let r = bench("Graph link memories", 100, || {
            mm.link_memories(&id_a, &id_b, "benchmark_link");
        });
        print_result(&r);

        // Query by type
        let r = bench("Graph query by type (User)", 100, || {
            let _ = mm.by_type(MemoryType::User);
        });
        print_result(&r);

        // Query by topic
        let r = bench("Graph query by topic", 100, || {
            let _ = mm.by_topic("topic_1");
        });
        print_result(&r);

        // Recall (graph path)
        let r = bench("Graph recall (hit)", 50, || {
            let _ = mm.recall("Indexed memory", 5);
        });
        print_result(&r);

        let r = bench("Graph recall (miss)", 50, || {
            let _ = mm.recall("nonexistent_xyzzy", 5);
        });
        print_result(&r);

        // Stats
        let r = bench("Graph stats", 200, || {
            let _ = mm.graph_stats();
        });
        print_result(&r);

        let stats = mm.graph_stats();
        println!(
            "\n  Graph contents: {} memories, {} topics, {} relationships, {} sessions",
            stats.memory_count, stats.topic_count, stats.relationship_count, stats.session_count,
        );
    }

    #[cfg(not(feature = "graph"))]
    {
        println!("  \x1b[33mGraph feature not enabled. Run with --features graph\x1b[0m");
    }

    // ═══ SECTION 6: AUTO-DREAM GATES ═════════════════════════════════════

    section("6. Auto-Dream Gate Evaluation");

    {
        let dream_mem = root.join("dream_mem");
        let dream_conv = root.join("dream_conv");
        std::fs::create_dir_all(&dream_mem).unwrap();
        std::fs::create_dir_all(&dream_conv).unwrap();

        let dreamer = AutoDream::new(dream_mem.clone(), dream_conv.clone());

        let r = bench("should_consolidate() check", 500, || {
            let _ = dreamer.should_consolidate();
        });
        print_result(&r);

        let r = bench("load_state()", 500, || {
            let _ = dreamer.load_state();
        });
        print_result(&r);
    }

    // ═══ SECTION 7: SESSION MEMORY EXTRACTION ════════════════════════════

    section("7. Session Memory Extraction Gate");

    {
        // Build a conversation of 30 messages (above threshold)
        let messages: Vec<Message> = (0..30)
            .map(|i| {
                if i % 2 == 0 {
                    Message::user(&format!("Question {i}"))
                } else {
                    Message::assistant(&format!("Answer {i}"))
                }
            })
            .collect();

        let state = session_memory::SessionMemoryState::default();

        let r = bench("should_extract() (30 msgs)", 1000, || {
            let _ = session_memory::should_extract(&messages, &state);
        });
        print_result(&r);

        // Parse extraction output
        let sample_output = "MEMORY: preference | 8 | User prefers dark mode\n\
            MEMORY: project | 9 | API uses REST with JSON\n\
            MEMORY: pattern | 6 | Uses builder pattern for config\n";

        let r = bench("parse_extraction_output()", 500, || {
            let _ = session_memory::parse_extraction_output(sample_output);
        });
        print_result(&r);
    }

    // ═══ SECTION 8: GRAPH ON vs OFF COMPARISON ═══════════════════════════

    section("8. Graph ON vs OFF (same operations)");

    let cmp_mem_dir = root.join("cmp_mem");
    create_memory_files(&cmp_mem_dir, 100);

    // Graph OFF
    let mm_off = MemoryManager::new(root).with_memory_dir(cmp_mem_dir.clone());

    let r_off_scan = bench("Scan 100 (graph OFF)", 100, || {
        let _ = mm_off.scan();
    });
    print_result(&r_off_scan);

    let r_off_recall = bench("Recall 100 (graph OFF)", 50, || {
        let _ = mm_off.recall("rust", 5);
    });
    print_result(&r_off_recall);

    let r_off_ctx = bench("Build context (graph OFF)", 100, || {
        let _ = mm_off.build_context();
    });
    print_result(&r_off_ctx);

    // Graph ON
    #[cfg(feature = "graph")]
    {
        let graph_cmp = root.join("cmp_graph.grafeo");
        let mm_on = MemoryManager::new(root)
            .with_memory_dir(cmp_mem_dir.clone())
            .with_graph(&graph_cmp)
            .unwrap();

        // Pre-populate graph
        for i in 0..100 {
            mm_on.store_memory(
                &format!("Graph memory {i} about rust and testing"),
                MemoryType::Project,
                0.8,
            );
        }

        let r_on_scan = bench("Scan 100 (graph ON)", 100, || {
            let _ = mm_on.scan();
        });
        print_result(&r_on_scan);

        let r_on_recall = bench("Recall 100 (graph ON)", 50, || {
            let _ = mm_on.recall("rust", 5);
        });
        print_result(&r_on_recall);

        let r_on_ctx = bench("Build context (graph ON)", 100, || {
            let _ = mm_on.build_context();
        });
        print_result(&r_on_ctx);

        // Print comparison
        println!("\n  \x1b[36mComparison:\x1b[0m");
        let scan_delta =
            (r_on_scan.avg.as_nanos() as f64 / r_off_scan.avg.as_nanos() as f64 - 1.0) * 100.0;
        let recall_delta =
            (r_on_recall.avg.as_nanos() as f64 / r_off_recall.avg.as_nanos() as f64 - 1.0) * 100.0;
        let ctx_delta =
            (r_on_ctx.avg.as_nanos() as f64 / r_off_ctx.avg.as_nanos() as f64 - 1.0) * 100.0;
        println!("    Scan:    graph ON is {:+.1}% vs OFF", scan_delta);
        println!("    Recall:  graph ON is {:+.1}% vs OFF", recall_delta);
        println!("    Context: graph ON is {:+.1}% vs OFF", ctx_delta);
    }

    // ═══ SECTION 9: SCHEMA VERSION & MIGRATION ════════════════════════

    section("9. Schema Version & Migration");

    #[cfg(feature = "graph")]
    {
        use cersei_memory::graph::{effective_confidence, VersionCheck, CURRENT_SCHEMA_VERSION};
        use cersei_memory::graph_migrate;

        // Version check on already-migrated graph
        let ver_dir = root.join("ver_bench");
        std::fs::create_dir_all(&ver_dir).unwrap();
        let ver_path = ver_dir.join("ver.grafeo");
        {
            // Open once to trigger migration
            let _ = cersei_memory::graph::GraphMemory::open(&ver_path);
        }

        let r = bench("Version check (migrated graph)", 200, || {
            let db = grafeo::GrafeoDB::open(&ver_path).unwrap();
            let _ = graph_migrate::check_version(&db);
        });
        print_result(&r);

        // Migration on fresh graph (v0→v2)
        let r = bench("Migration v0→v2 (fresh graph)", 50, || {
            let db = grafeo::GrafeoDB::new_in_memory();
            let _ = graph_migrate::run_migrations(&db, 0, 2);
        });
        print_result(&r);

        // Migration with 100 existing nodes
        let r = bench("Migration v0→v2 (100 nodes)", 10, || {
            let db = grafeo::GrafeoDB::new_in_memory();
            let session = db.session();
            for i in 0..100 {
                let _ = session.execute(&format!(
                    "INSERT (:Memory {{id: 'bench-{i}', content: 'test {i}', mem_type: 'User', \
                     confidence: 0.8, created_at: '2024-01-01T00:00:00Z', updated_at: '2024-01-01T00:00:00Z'}})"
                ));
            }
            let _ = graph_migrate::run_migrations(&db, 0, 2);
        });
        print_result(&r);

        // Idempotent re-run
        let r = bench("Migration re-run (idempotent)", 100, || {
            let db = grafeo::GrafeoDB::new_in_memory();
            let _ = graph_migrate::run_migrations(&db, 0, 2);
            let _ = graph_migrate::run_migrations(&db, 0, 2); // second run
        });
        print_result(&r);

        // Effective confidence calculation
        let now = chrono::Utc::now().to_rfc3339();
        let r = bench("effective_confidence() calc", 1000, || {
            let _ = effective_confidence(0.9, 0.01, &now);
        });
        print_result(&r);

        let old = "2024-01-01T00:00:00Z";
        let r = bench("effective_confidence() (old)", 1000, || {
            let _ = effective_confidence(0.9, 0.01, old);
        });
        print_result(&r);

        println!("\n  Schema version: v{}", CURRENT_SCHEMA_VERSION);
    }

    #[cfg(not(feature = "graph"))]
    {
        println!("  \x1b[33mGraph feature not enabled — skipping migration benchmarks\x1b[0m");
    }

    // ═══ SUMMARY ═════════════════════════════════════════════════════════

    println!("\n\x1b[32;1m╔══════════════════════════════════════════════════════════════╗\x1b[0m");
    println!("\x1b[32;1m║  Benchmark Complete                                          ║\x1b[0m");
    println!("\x1b[32;1m╚══════════════════════════════════════════════════════════════╝\x1b[0m");
}
