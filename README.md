# Cersei

The complete Rust SDK for building coding agents.

Cersei gives you every building block of a production coding agent — tool execution, LLM streaming, sub-agent orchestration, persistent memory, skills, MCP integration — as composable library functions. Build a Claude Code replacement, embed an agent in your app, or create something entirely new.

```rust
use cersei::prelude::*;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let output = Agent::builder()
        .provider(Anthropic::from_env()?)
        .tools(cersei::tools::coding())
        .permission_policy(AllowAll)
        .run_with("Fix the failing tests in src/")
        .await?;

    println!("{}", output.text());
    Ok(())
}
```

**MIT License** | Built by [Adib Mohsin](https://github.com/pacifio) | [Docs](https://cersei.pacifio.dev/docs) | [GitHub](https://github.com/pacifio/cersei)

---

## Why Cersei

| | Claude Code | OpenCode | **Cersei SDK** | **Abstract CLI** |
|---|---|---|---|---|
| Form factor | CLI app | CLI app | **Library** | **CLI app** |
| Embeddable | No | No | **Yes** | No (uses SDK) |
| Provider | Anthropic only | Multi-provider | **Multi-provider** | **Multi-provider** |
| Language | TypeScript | TypeScript | **Rust** | **Rust** |
| Custom tools | Plugins | Plugins | **`impl Tool` / `#[derive(Tool)]`** | Via SDK |
| Startup | ~269ms | ~300ms | N/A (library) | **~34ms** |
| Binary / RSS | 174MB / 330MB | — | N/A | **5.8MB / 4.9MB** |
| Memory | File-based | SQLite | **File + Graph** | **File + Graph** |
| Skills | `.claude/commands/` | `.claude/skills/` | **Both formats** | **Both formats** |

Cersei is built from the architecture of Claude Code (reverse-engineered Rust port) and designed so that anyone can build a complete, drop-in replacement for Claude Code, OpenCode, or any coding agent — as a library call.

---

## Abstract — The CLI

**Abstract** is a complete CLI coding agent built on Cersei. One binary, zero runtime dependencies, graph memory by default.

```bash
# Install
cargo install --path crates/abstract-cli

# Use
abstract                           # Interactive REPL
abstract "fix the failing tests"   # Single-shot
abstract --resume                  # Resume last session
abstract --model opus --max        # Opus with max thinking
abstract --no-permissions --json   # CI mode with NDJSON output
```

### Abstract vs Claude Code

All numbers from `run_tool_bench.sh --full`.

| Metric | Abstract | Claude Code | Winner |
|--------|----------|-------------|--------|
| Startup (warm) | **32ms** | 266ms | Abstract (8.2x) |
| Binary size | **6.0 MB** | 174 MB | Abstract (29x) |
| Memory (RSS) | **4.9 MB** | 333 MB | Abstract (68x) |
| Tool dispatch | **0.02-17ms** | 5-265ms+ | Abstract |
| Memory recall | **98us** (graph) | 7,545ms (LLM) | Abstract (77,000x) |
| Memory write | **30us** (graph) | 20,687ms (agent) | Abstract (689,000x) |
| MEMORY.md load | **9.6us** | 17.1ms | Abstract (1,781x) |
| Sequential throughput | **906ms/req** | 12,079ms/req | Abstract (13.3x) |
| System prompt tokens | **~2,200** | ~8,000+ | Abstract (3.6x fewer) |
| LLM call for recall | **Not needed** | Required (Sonnet) | Abstract |

> Claude Code's memory recall calls Sonnet every turn to rank the top 5 files by relevance (7.5s measured).
> Abstract's graph does indexed lookups in 98 microseconds — same capability, no LLM call, no API cost.

Full benchmark: [`crates/abstract-cli/benchmarks/REPORT.md`](crates/abstract-cli/benchmarks/REPORT.md)

### Features

- 34 built-in tools (file, shell, web, planning, orchestration, scheduling)
- Multi-provider: Anthropic + OpenAI (+ Ollama, Azure, vLLM)
- Graph memory (Grafeo) on by default
- Auto-compact, auto-dream, effort levels (Low/Medium/High/Max)
- MCP server support
- Session persistence (Claude Code-compatible JSONL)
- Interactive permissions with session caching
- 12 slash commands (`/help`, `/commit`, `/review`, `/memory`, `/model`, `/diff`, etc.)
- Streaming markdown rendering with syntax highlighting
- TOML config: `~/.abstract/config.toml` + `.abstract/config.toml`
- JSON output mode for piping (`--json`)

---

## Install

```toml
[dependencies]
cersei = { git = "https://github.com/pacifio/cersei" }
tokio = { version = "1", features = ["full"] }
anyhow = "1"
```

For graph-backed memory (optional):
```toml
cersei-memory = { git = "https://github.com/pacifio/cersei", features = ["graph"] }
```

---

## Architecture

```
cersei                    Facade crate — use cersei::prelude::*;
  cersei-types            Provider-agnostic messages, errors, stream events
  cersei-provider         Provider trait + Anthropic/OpenAI implementations
  cersei-tools            30+ tools, permissions, bash classifier, skills, git utils
  cersei-tools-derive     #[derive(Tool)] proc macro
  cersei-agent            Agent builder, agentic loop, compact, coordinator, effort
  cersei-memory           Memory trait, memdir, CLAUDE.md, sessions, Grafeo graph
  cersei-hooks            Hook/middleware system
  cersei-mcp              MCP client (JSON-RPC 2.0, stdio transport)
abstract-cli              CLI coding agent ("abstract") — REPL, commands, config, permissions
```

---

## Core Concepts

### Provider

Any LLM backend. Built-in: Anthropic (with OAuth), OpenAI (compatible with Ollama, Azure, vLLM).

```rust
Agent::builder().provider(Anthropic::from_env()?)           // Anthropic API key
Agent::builder().provider(OpenAi::builder()
    .base_url("http://localhost:11434/v1")                   // Ollama
    .model("llama3.1:70b").api_key("ollama").build()?)
Agent::builder().provider(MyCustomProvider)                  // impl Provider
```

### Tools (30+)

Every tool a coding agent needs, organized into sets:

```rust
cersei::tools::all()           // 30+ tools
cersei::tools::coding()        // filesystem + shell + web
cersei::tools::filesystem()    // Read, Write, Edit, Glob, Grep, NotebookEdit
cersei::tools::shell()         // Bash, PowerShell
cersei::tools::web()           // WebFetch, WebSearch
cersei::tools::planning()      // EnterPlanMode, ExitPlanMode, TodoWrite
cersei::tools::scheduling()    // CronCreate/List/Delete, Sleep, RemoteTrigger
cersei::tools::orchestration() // SendMessage, Tasks (6 tools), Worktree
```

Custom tools in 10 lines:

```rust
#[derive(Tool)]
#[tool(name = "search", description = "Search docs", permission = "read_only")]
struct SearchTool;

#[async_trait]
impl ToolExecute for SearchTool {
    type Input = SearchInput; // derives Deserialize + JsonSchema
    async fn run(&self, input: SearchInput, ctx: &ToolContext) -> ToolResult {
        ToolResult::success(format!("Found: {}", input.query))
    }
}
```

### Sub-Agent Orchestration

Spawn parallel workers, coordinate tasks, pass messages between agents:

```rust
// AgentTool — model spawns sub-agents autonomously
Agent::builder()
    .tool(AgentTool::new(|| Box::new(Anthropic::from_env()?), cersei::tools::coding()))

// Coordinator mode — orchestrate parallel workers
Agent::builder()
    .tools(cersei::tools::all())  // includes Agent, Tasks, SendMessage
    // Workers get filtered tools (no Agent — prevents recursion)

// Task system
// TaskCreate → TaskUpdate → TaskGet → TaskList → TaskStop → TaskOutput
```

### Memory (Three-Tier)

```rust
use cersei::memory::manager::MemoryManager;

let mm = MemoryManager::new(project_root)
    .with_graph(Path::new("./memory.grafeo"))?;  // optional graph layer

// Tier 1: Flat files (~/.claude/projects/<root>/memory/)
let metas = mm.scan();                    // scan .md files with frontmatter
let content = mm.build_context();         // build system prompt injection

// Tier 2: CLAUDE.md hierarchy (managed > user > project > local)
// Automatically merged into build_context()

// Tier 3: Graph memory (Grafeo, optional)
let id = mm.store_memory("User prefers Rust", MemoryType::User, 0.9)?;
mm.tag_memory(&id, "preferences");
let results = mm.recall("Rust", 5);       // graph query with fallback to text match

// Session persistence (JSONL, append-only, tombstone soft-delete)
mm.write_user_message("session-1", Message::user("Hello"))?;
let messages = mm.load_session_messages("session-1")?;
```

### Skills (Claude Code + OpenCode Compatible)

```rust
// Auto-discovers skills from:
//   .claude/commands/*.md      (Claude Code format)
//   .claude/skills/*/SKILL.md  (OpenCode format)
//   ~/.claude/commands/*.md    (user-level)
//   Bundled skills             (simplify, debug, commit, verify, stuck, remember, loop)

let skill_tool = SkillTool::new().with_project_root(".");
// skill="list" → lists all available skills
// skill="debug" args="tests are flaky" → expands $ARGUMENTS template
```

### Realtime Events

Three observation mechanisms:

```rust
// 1. Callback
Agent::builder().on_event(|e| match e {
    AgentEvent::TextDelta(t) => print!("{}", t),
    AgentEvent::ToolStart { name, .. } => eprintln!("[{}]", name),
    _ => {}
})

// 2. Broadcast (multi-consumer)
let agent = Agent::builder().enable_broadcast(256).build()?;
let mut rx = agent.subscribe().unwrap();
tokio::spawn(async move { while let Ok(e) = rx.recv().await { /* ... */ } });

// 3. Stream (bidirectional control)
let mut stream = agent.run_stream("Deploy");
while let Some(e) = stream.next().await {
    if let AgentEvent::PermissionRequired(req) = e {
        stream.respond_permission(req.id, PermissionDecision::Allow);
    }
}
```

### Context Management

```rust
Agent::builder()
    .auto_compact(true)          // summarize old messages at 90% context usage
    .compact_threshold(0.9)      // trigger threshold
    .tool_result_budget(50_000)  // truncate oldest tool results above 50K chars
    .thinking_budget(8192)       // extended thinking tokens
    .effort(EffortLevel::High)   // Low/Medium/High/Max
```

### MCP (Model Context Protocol)

```rust
let mcp = McpManager::connect(&[
    McpServerConfig::stdio("db", "npx", &["-y", "@my/db-mcp"]),
    McpServerConfig::sse("docs", "https://mcp.example.com"),
]).await?;

Agent::builder().tools(mcp.tool_definitions().await)
```

### OAuth (Anthropic Native)

```rust
// Opens browser, PKCE flow, token storage, refresh
cargo run --example oauth_login
```

---

## Agent Builder — Complete API

```rust
Agent::builder()
    // Provider (required)
    .provider(Anthropic::from_env()?)

    // Tools
    .tool(MyTool)
    .tools(cersei::tools::coding())

    // Model & generation
    .model("claude-sonnet-4-6")
    .max_turns(10)
    .max_tokens(16384)
    .temperature(0.7)
    .thinking_budget(8192)

    // Prompt
    .system_prompt("You are a helpful assistant.")
    .append_system_prompt("Extra context.")

    // Environment
    .working_dir("./my-project")
    .permission_policy(AllowAll)          // or AllowReadOnly, DenyAll, RuleBased, Interactive

    // Memory
    .memory(JsonlMemory::new("./sessions"))
    .session_id("my-session")

    // Hooks & events
    .hook(CostGuard { max_usd: 5.0 })
    .on_event(|e| { /* ... */ })
    .enable_broadcast(256)
    .reporter(ConsoleReporter { verbose: true })

    // Context management
    .auto_compact(true)
    .compact_threshold(0.9)
    .tool_result_budget(50_000)

    // Execute
    .build()?                             // -> Agent
    .run_with("Fix the tests")            // -> AgentOutput (shorthand)
```

---

## Benchmarks

Measured on Apple Silicon, release build, 100 iterations with 3 warmup runs.

### Tool I/O

| Tool | Avg | Min | Max |
|------|-----|-----|-----|
| Edit | 0.04ms | 0.02ms | 0.05ms |
| Glob | 0.05ms | 0.05ms | 0.07ms |
| Write | 0.09ms | 0.07ms | 0.11ms |
| Read | 0.09ms | 0.08ms | 0.11ms |
| Grep | 5.85ms | 5.34ms | 8.51ms |
| Bash | 15.64ms | 14.50ms | 16.19ms |

### vs Claude Code CLI

> **Note:** Cersei is a library — tool dispatch happens in-process. Claude Code is a CLI where
> each sub-agent fork pays full startup cost. These are different layers; the comparison below
> shows the gap between in-process dispatch and CLI process overhead.

| Metric | Cersei (SDK) | Claude Code (CLI) | Notes |
|--------|-------------|-------------------|-------|
| Tool dispatch (Read) | 0.09ms | ~5-15ms (est.) | In-process vs Node.js fs |
| CLI startup | N/A (library) | 269ms | Claude `--version` warm avg |
| Sub-agent spawn | ~1ms (in-process) | ~300ms (fork) | Agent tool overhead |

For an apples-to-apples CLI comparison, see [Abstract CLI benchmarks](crates/abstract-cli/benchmarks/REPORT.md).

### Memory I/O

| Operation | Abstract (Cersei) | Claude Code (measured) | Ratio |
|-----------|------------------|----------------------|-------|
| Scan 100 files | **1.2ms** | 26.6ms (`find`) | 22x |
| Load MEMORY.md | **9.6μs** | 17.1ms | 1,781x |
| Memory recall (graph) | **98μs** | 7,545ms (LLM call) | 77,000x |
| Memory recall (text) | **1.3ms** | 17.5ms (`grep`) | 13x |
| Session write | **27μs/entry** | N/A | — |
| Session load (100) | **268μs** | N/A | — |
| Graph store | **30μs/node** | N/A (no graph) | — |
| Topic query | **77μs** | N/A (no graph) | — |

### Run Benchmarks

```bash
# Tool I/O benchmark
cargo run --example benchmark_io --release

# Memory architecture benchmark (graph ON vs OFF + Claude Code comparison)
cargo run --release -p abstract-cli --example memory_bench

# Full CLI comparison (abstract vs claude, all categories)
./run_tool_bench.sh --iterations 20 --full

# Standalone benchmark suite (with Markdown output)
cd examples/benchmark && cargo run --release
```

---

## Stress Tests

```bash
cargo run --example stress_core_infrastructure --release  # system prompt, compact, context, bash classifier
cargo run --example stress_tools --release                 # all 30+ tools, registry, performance
cargo run --example stress_orchestration --release         # sub-agents, coordinator, tasks, messaging
cargo run --example stress_skills --release                # bundled + disk skills, Claude Code + OpenCode format
cargo run --example stress_memory --release                # memdir, CLAUDE.md, sessions, extraction, auto-dream
```

---

## Examples

| Example | Description |
|---------|-------------|
| [`simple_agent`](examples/simple_agent.rs) | Minimal agent in 3 lines |
| [`custom_tools`](examples/custom_tools.rs) | Define and register custom tools |
| [`streaming_events`](examples/streaming_events.rs) | Real-time `run_stream()` with colored output |
| [`multi_listener`](examples/multi_listener.rs) | Broadcast channel with multiple consumers |
| [`resumable_session`](examples/resumable_session.rs) | Persist and resume with `JsonlMemory` |
| [`custom_provider`](examples/custom_provider.rs) | Echo provider + OpenAI-compatible endpoints |
| [`hooks_middleware`](examples/hooks_middleware.rs) | Cost guard + audit logger + tool blocker |
| [`benchmark_io`](examples/benchmark_io.rs) | Full I/O benchmark suite |
| [`usage_report`](examples/usage_report.rs) | Token/cost tracking and billing estimates |
| [`coding_agent`](examples/coding_agent.rs) | Build a Python todo CLI (end-to-end) |
| [`oauth_login`](examples/oauth_login.rs) | Anthropic OAuth PKCE login flow |

```bash
cargo run --example simple_agent --release
```

---

## Test Suite

```bash
# Run all 160 unit tests
cargo test --workspace

# Run with graph memory (requires grafeo)
cargo test --workspace --features graph

# Run specific crate
cargo test -p cersei-tools
cargo test -p cersei-agent
cargo test -p cersei-memory
cargo test -p cersei-mcp
```

**160 unit tests** | **262 stress checks** | **0 failures** | **Zero I/O regression**

---

## Extension Points

| What | How | Example |
|------|-----|---------|
| Custom provider | `impl Provider` | Local LLM, Azure, Bedrock |
| Custom tool | `#[derive(Tool)]` or `impl Tool` | DB query, deploy, search |
| Custom permissions | `impl PermissionPolicy` | RBAC, OAuth-scoped |
| Custom memory | `impl Memory` | PostgreSQL, Redis, S3 |
| Custom hooks | `impl Hook` | Cost gating, audit logging |
| Custom reporters | `impl Reporter` | Dashboards, WebSocket relay |
| MCP servers | `McpServerConfig` via builder | Any MCP-compatible server |
| Skills | `.claude/commands/*.md` | Custom prompt templates |
| Graph memory | `features = ["graph"]` | Grafeo relationship tracking |

---

## Documentation

**[cersei.pacifio.dev/docs](https://cersei.pacifio.dev/docs)** — full docs with API reference, architecture, cookbooks, benchmarks, and llms.txt support.

| Section | Content |
|---------|---------|
| [Quick Start](https://cersei.pacifio.dev/docs/quick-start) | First agent in 10 lines |
| [API Reference](https://cersei.pacifio.dev/docs/api-agent) | Agent, Provider, Tools, Memory, Hooks, MCP |
| [Architecture](https://cersei.pacifio.dev/docs/architecture) | Crate map, data flow, design principles |
| [Cookbooks](https://cersei.pacifio.dev/docs/cookbook-custom-tools) | Custom tools, deployment, embedding |
| [Abstract CLI](https://cersei.pacifio.dev/docs/abstract) | Reference CLI built on Cersei |
| [Benchmarks](https://cersei.pacifio.dev/docs/bench-vs-claude-code) | vs Claude Code vs Codex |

---

## License

MIT License

Copyright (c) 2025 Adib Mohsin

Permission is hereby granted, free of charge, to any person obtaining a copy of this software and associated documentation files (the "Software"), to deal in the Software without restriction, including without limitation the rights to use, copy, modify, merge, publish, distribute, sublicense, and/or sell copies of the Software, and to permit persons to whom the Software is furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY, FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM, OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE SOFTWARE.
