# Cersei Benchmark Suite

Measures raw tool I/O performance, memory operations, and token consumption for the Cersei SDK. Compares against native Claude Code CLI.

## Run

```bash
# Standalone benchmark (100 iterations, Markdown output)
cd examples/benchmark
cargo run --release

# Quick benchmark via example (50 iterations)
cd src-cersei
cargo run --example benchmark_io --release

# Token/cost usage tracking
cargo run --example usage_report --release
```

## What It Measures

### Tool I/O (per-tool dispatch speed)

| Test | Description |
|------|-------------|
| **Read** | Read a 50KB file with line-number formatting |
| **Write** | Write a 1KB file to disk |
| **Edit** | String replacement in a file (read + find + replace + write) |
| **Glob** | Find `**/*.rs` files across 20 files |
| **Grep** | Regex search across 20 files via `rg` or `grep` |
| **Bash** | Spawn `sh -c "echo hello && ls -la"` and capture output |

### Memory I/O

| Test | Description |
|------|-------------|
| **MEMORY.md load** | Load and parse the memory index file |
| **Scan 100 files** | Recursive .md scan with frontmatter parsing |
| **Session write** | Append one JSONL entry to transcript |
| **Session load** | Two-pass load of 100 entries with tombstone filtering |
| **Recall** | Text-match search across 100 memory files |

### Baselines

| Test | Description |
|------|-------------|
| **std::fs::read** | Raw stdlib file read (no JSON, no formatting) |
| **std::fs::write** | Raw stdlib file write |
| **claude --help** | Claude Code CLI startup overhead |

## Latest Results

**Machine:** Apple Silicon (macOS), release build

### Tool I/O

| Tool | Avg | Min | Max |
|------|-----|-----|-----|
| Edit | 0.04ms | 0.02ms | 0.05ms |
| Glob | 0.05ms | 0.05ms | 0.07ms |
| Write | 0.09ms | 0.07ms | 0.11ms |
| Read | 0.09ms | 0.08ms | 0.11ms |
| Grep | 5.85ms | 5.34ms | 8.51ms |
| Bash | 15.64ms | 14.50ms | 16.19ms |

### Memory I/O

| Operation | Speed |
|-----------|-------|
| MEMORY.md load | 12μs |
| Session write | 51μs/entry |
| Session load (100 entries) | 155μs |
| Scan 100 files | 1,378μs |
| Recall from 100 files | 1,482μs |

### vs Claude Code CLI

| | Cersei | Claude CLI | Ratio |
|---|--------|------------|-------|
| Tool dispatch | 0.09ms | — | — |
| CLI startup | — | 323ms | — |
| **Comparison** | **0.09ms** | **323ms** | **3,589x faster** |

### Token Consumption (simulated 4-turn session)

| Model | Input | Output | Cost |
|-------|-------|--------|------|
| Sonnet 4.6 | 11,901 | 2,110 | $0.067 |
| Opus 4.6 | 11,901 | 2,110 | $0.337 |
| Haiku 4.5 | 11,901 | 2,110 | $0.018 |

## Output

The benchmark outputs both human-readable tables and a copy-pasteable Markdown table section for documentation.
