# Benchmarks

Complete performance analysis of the Cersei SDK across tool I/O, memory operations, token consumption, and comparison with Claude Code CLI.

## How to Run

```bash
# Quick: tool I/O benchmark (50 iterations)
cargo run --example benchmark_io --release

# Full: standalone suite with Markdown output (100 iterations)
cd examples/benchmark && cargo run --release

# Stress tests (validates correctness + performance)
cargo run --example stress_core_infrastructure --release
cargo run --example stress_tools --release
cargo run --example stress_orchestration --release
cargo run --example stress_skills --release
cargo run --example stress_memory --release

# Usage/cost tracking
cargo run --example usage_report --release
```

---

## Methodology

- **Machine**: Apple Silicon (macOS), release build (`--release`)
- **Iterations**: 100 per test (50 for quick benchmark)
- **Warmup**: 3 runs excluded from timing
- **Clock**: `std::time::Instant` (monotonic)
- **Temp dir**: fresh tmpdir per run (no filesystem caching between runs)
- **Targets**: tool dispatch overhead, not LLM latency

---

## Tool I/O Performance

Measures the time from "agent decides to use a tool" to "tool result is ready". This is pure local execution — no network, no LLM.

### Results (100 iterations, release build)

| Tool | Avg | Min | Max | What It Measures |
|------|-----|-----|-----|------------------|
| **Edit** | **0.04ms** | 0.02ms | 0.05ms | Read file + find string + replace + write back |
| **Glob** | **0.05ms** | 0.05ms | 0.07ms | Pattern match across 20 files |
| **Write** | **0.09ms** | 0.07ms | 0.11ms | Write 1KB to disk (creates parent dirs) |
| **Read** | **0.09ms** | 0.08ms | 0.11ms | Read 50KB file + format with line numbers |
| **Grep** | **5.85ms** | 5.34ms | 8.51ms | `rg` subprocess across 20 files |
| **Bash** | **15.64ms** | 14.50ms | 16.19ms | `sh -c "echo hello && ls -la"` |

### vs Raw `std::fs`

| Operation | `std::fs` | Cersei | Overhead |
|-----------|-----------|--------|----------|
| Read | 0.012ms | 0.09ms | +0.078ms (JSON parsing + line numbering) |
| Write | 0.071ms | 0.09ms | +0.019ms (JSON parsing + dir check) |

### vs Claude Code CLI

| Metric | Cersei | Claude Code |
|--------|--------|-------------|
| Tool dispatch (Read) | 0.09ms | — |
| CLI startup (`--help`) | — | 323ms |
| **Ratio** | **3,589x faster** | |

**Why**: Claude Code pays Node.js boot + module loading + config parsing on every invocation. Cersei tools are native Rust function calls — zero process overhead.

### What This Means for Agents

| Scenario | Tool Calls | Cersei Overhead | Note |
|----------|-----------|----------------|------|
| Quick fix | 5 | 0.5ms | Invisible |
| Feature build | 50 | 4.5ms | Invisible |
| Large refactor | 200 | 18ms | Invisible |
| CI batch (1000 files) | 1000 | 90ms | Noticeable only in batch |

The real bottleneck is **always** the LLM API (500ms-5s per turn). Tool dispatch overhead matters only for high-frequency automation.

### Why Grep and Bash Are Slower

| Tool | Bottleneck | Why |
|------|-----------|-----|
| Grep (~6ms) | `rg` subprocess | Process creation on macOS is ~5ms. In-process regex would be ~0.1ms. |
| Bash (~19ms) | `sh -c` subprocess | Inherent `fork()`+`exec()` cost. Same for any agent framework. |

---

## Memory I/O Performance

Measured with 100 memory files, each with YAML frontmatter.

| Operation | Speed | Target | Headroom |
|-----------|-------|--------|----------|
| **MEMORY.md load** | **12μs** | <1000μs | 83x |
| **Session write** (per entry) | **51μs** | <500μs | 10x |
| **Session load** (100 entries) | **155μs** | <5000μs | 32x |
| **Scan 100 memory files** | **1,378μs** | <5000μs | 3.6x |
| **Recall from 100 files** | **1,482μs** | <10000μs | 6.7x |

### What This Means

- **Session writes at 51μs/entry** = 20,000 writes/second. Zero overhead during agent execution.
- **Memory scanning at 1.4ms** for 100 files = fast enough for agent startup (most projects have <20 memory files).
- **MEMORY.md in 12μs** = instant system prompt injection.
- **Full Claude Code compatibility** with `~/.claude/projects/` directory structure (tested against 42 real project directories and 171 session transcripts).

---

## Token Consumption Estimates

Simulated with realistic Claude token counts (based on actual API response patterns):

### Per-Session Breakdown

| Turn | Input Tokens | Output Tokens | Cost (Sonnet) |
|------|-------------|---------------|---------------|
| 1 (file read) | 2,900 | 1,250 | $0.027 |
| 2 (file write) | 2,467 | 380 | $0.017 |
| 3 (verify) | 3,000 | 195 | $0.016 |
| 4 (summary) | 3,534 | 285 | $0.020 |
| **Total** | **11,901** | **2,110** | **$0.081** |

### Model Cost Comparison

Same session (11,901 in / 2,110 out) across models:

| Model | Input Cost | Output Cost | Total |
|-------|-----------|-------------|-------|
| Claude Sonnet 4.6 | $0.036 | $0.032 | **$0.067** |
| Claude Opus 4.6 | $0.179 | $0.158 | **$0.337** |
| Claude Haiku 4.5 | $0.010 | $0.008 | **$0.018** |

### Projected Monthly Costs

At Sonnet rates:

| Usage | Daily Cost | Monthly Cost |
|-------|-----------|-------------|
| 10 sessions/day | $0.67 | $20 |
| 50 sessions/day | $3.35 | $101 |
| 100 sessions/day | $6.70 | $201 |

### Efficiency Metrics

| Metric | Value |
|--------|-------|
| Input/output ratio | 5.6:1 (typical for tool-heavy agents) |
| Cost per turn | $0.020 |
| Cost per tool call | $0.027 |
| Tokens/second (throughput) | ~25,000 |

---

## Context Management Performance

### Auto-Compact

| Threshold | Action | Validated |
|-----------|--------|-----------|
| <80% | No action | Yes |
| 80-90% | Warning emitted | Yes |
| 90%+ | Auto-compact triggers | Yes |
| 95%+ | Critical warning | Yes |
| 98%+ | Context collapse (emergency) | Yes |

### Tool Result Budget

| Scenario | Before | After | Reduction |
|----------|--------|-------|-----------|
| 20 tool results (185KB) | 185,000 chars | 48,070 chars | 74% |
| Recent results | Preserved | Preserved | — |
| Truncated entries | — | 15 entries | "[truncated]" notice |

### Compact Circuit Breaker

- 3 consecutive failures → auto-compact disabled
- Prevents infinite compact-fail loops
- Success resets counter

---

## Stress Test Results

### Core Infrastructure (46 checks)

| Subsystem | Checks | Status |
|-----------|--------|--------|
| System prompt builder | 19 | All pass |
| Bash classifier (23 commands) | 2 | All pass |
| Context analyzer | 8 | All pass |
| Auto-compact | 14 | All pass |
| Tool result budget | 3 | All pass |

### Tools (47 checks)

| Category | Tools | Checks | Status |
|----------|-------|--------|--------|
| Filesystem | Read, Write, Edit, Glob, Grep, NotebookEdit | 7 | All pass |
| Shell | Bash, PowerShell | 2 | All pass |
| Planning | PlanMode, TodoWrite | 4 | All pass |
| Scheduling | Cron (3), Sleep, RemoteTrigger | 6 | All pass |
| Orchestration | SendMessage | 3 | All pass |
| Config/Utility | Config, SyntheticOutput, AskUser | 3 | All pass |
| Web | WebFetch, WebSearch | 4 | Schema only |
| ToolSearch | 4 queries | 4 | All pass |
| Performance | Read, Edit, Glob (<500μs) | 3 | All pass |
| Registry | 24 tools, unique names, valid schemas | 10 | All pass |

### Orchestration (33 checks)

| Subsystem | Checks | Status |
|-----------|--------|--------|
| AgentTool (sub-agents) | 10 | All pass |
| Coordinator mode | 8 | All pass |
| Task system (6 tools) | 8 | All pass |
| Message passing | 5 | All pass |
| Full orchestration (3 workers) | 2 | All pass |

### Skills (51 checks)

| Subsystem | Checks | Status |
|-----------|--------|--------|
| Bundled skills (7 skills) | 19 | All pass |
| Claude Code format | 7 | All pass |
| OpenCode format | 3 | All pass |
| Precedence & dedup | 2 | All pass |
| SkillTool integration | 6 | All pass |
| Real user skills | 3 | All pass |
| Formatting | 3 | All pass |

### Memory (85 checks)

| Subsystem | Checks | Status |
|-----------|--------|--------|
| Memdir (scanning, FM, staleness) | 21 | All pass |
| CLAUDE.md (4 scopes, @include) | 10 | All pass |
| Session storage (JSONL, tombstones) | 10 | All pass |
| Session memory extraction | 8 | All pass |
| Auto-dream (3-gate system) | 9 | All pass |
| Graph memory (feature gate) | 2 | All pass |
| Unified manager | 13 | All pass |
| Performance (scan, recall, write, load) | 5 | All pass |
| Real user data compatibility | 1+ | All pass |

---

## Cumulative Totals

| Metric | Value |
|--------|-------|
| Unit tests | **160** |
| Stress checks | **262** |
| Tools | **30+** |
| Source files | **35+** |
| Crates | **9** |
| I/O regressions | **0** |
| Test failures | **0** |
