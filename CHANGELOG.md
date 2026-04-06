# Changelog

## [0.1.4] - 2026-04-06

### Added

- **Tool primitives** (`tool_primitives` module). 6 sub-modules exposing the low-level building blocks that all 34 built-in tools use: `diff` (unified diffs, line diffs, patch application via `similar`), `fs` (async read/write/edit/diff/patch/metadata), `process` (async command execution with shell selection), `http` (GET/POST/fetch_html), `search` (structured grep with ripgrep + glob), `git` (async status/diff/log/branch detection). 26 new tests.
- **Built-in tools reference** documentation page with complete input schemas for all 34 tools using TypeTable.
- **Tool primitives documentation** — overview page, full API reference, and cookbook with DiffTool, deploy verifier, research agent, and git-aware code reviewer examples.
- **Providers documentation** page covering all 13 providers with env vars, models, context windows, and usage examples.

### Changed

- `file_read.rs`, `file_write.rs`, `file_edit.rs` refactored to delegate to `tool_primitives::fs`.
- `bash.rs` refactored to delegate to `tool_primitives::process::exec` (ShellState preserved).
- `web_fetch.rs` refactored to delegate to `tool_primitives::http::fetch_html`.
- `grep_tool.rs` refactored to delegate to `tool_primitives::search::grep` (structured `SearchMatch` results).
- `glob_tool.rs` refactored to delegate to `tool_primitives::search::glob`.

## [0.1.3] - 2026-04-05

### Added

- **Session auto-fork.** When a session file exceeds 50MB, writes automatically fork to a new part file (`session_part2.jsonl`, `_part3.jsonl`, etc.). Loading stitches all parts together transparently. Tombstones apply across parts. Total session limit across all parts is 200MB.
- **Multi-part session helpers** — `all_part_paths()` and `total_session_size()` for inspecting session files programmatically.
- **5 new session tests** — multi-part load, tombstones across parts, auto-fork path resolution, total size calculation.
- **Sessions & Tasks documentation** — two new doc pages (`sessions.mdx`, `background-tasks.mdx`) covering the full session lifecycle, auto-compact, memory extraction, auto-dream consolidation, task orchestration with programmatic code samples, cron scheduling, and git worktree isolation.

### Changed

- `load_transcript()` now loads from all part files and applies tombstones across the combined set.
- `abstract sessions rm` now removes all part files, not just the base.

### Fixed

- Sessions that exceeded 50MB became unloadable. Now they auto-fork before hitting the limit.

## [0.1.2] - 2026-04-04

### Added

- **Multi-provider model router** (`registry.rs`, `router.rs`). 13 providers supported out of the box via `provider/model` string format. Most providers reuse the existing OpenAI-compatible client with a different base URL — zero new SSE parsing per provider.
- **Provider registry** with API base URLs, env var names, default models, context windows, and capabilities for: Anthropic, OpenAI, Google (Gemini), Mistral, Groq, DeepSeek, xAI (Grok), Together, Fireworks, Perplexity, Cerebras, Ollama, OpenRouter.
- **`from_model_string()`** top-level function on `cersei-provider` — parses `"groq/llama-3.1-70b-versatile"` into a configured `Box<dyn Provider>`.
- **Auto-detection** from bare model names — `"gpt-4o"` routes to OpenAI, `"claude-sonnet-4-6"` to Anthropic, `"gemini-2.0-flash"` to Google.
- **Model aliases** in Abstract CLI — `--model llama`, `--model deepseek`, `--model grok`, `--model gemini`, `--model mistral`, `--model 4o`.
- **`abstract login status`** now shows all 13 providers with auth detection from environment variables.
- **Providers documentation page** (`docs/content/docs/providers.mdx`) covering every provider with env vars, models, context windows, and usage examples.

- **Provider continuity** — interactive model switching on provider errors. On rate limit or outage, the REPL shows options to retry, switch to a fallback model, wait, or skip. Conversation history transfers across provider switches via `AgentBuilder::with_messages()`.
- **`--fallback` CLI flag** — configure fallback models for provider switching (e.g., `--fallback groq/llama-3.1-70b-versatile,google/gemini-2.0-flash`).
- **`fallback_models`** config field and `ABSTRACT_FALLBACK_MODELS` environment variable.
- **OpenAI tool calling** — full streaming tool call support for OpenAI-compatible providers. Accumulates `delta.tool_calls` chunks across SSE events, emits proper `ContentBlockStart`/`InputJsonDelta`/`ContentBlockStop` events. Previously tool calls were silently dropped.
- **OpenAI message serialization** — assistant messages with tool calls now serialize as `tool_calls` array. Tool results serialize as `role: "tool"` with `tool_call_id`. Previously all messages were flattened to text, breaking the tool call loop.
- **`AgentBuilder::with_messages()`** — pre-populate conversation history when building an agent. Used for provider switching mid-session.
- **Session load guard** — runner skips loading session history from memory when messages are pre-populated via `with_messages()`, preventing duplicates.

### Changed

- Abstract CLI provider resolution replaced with the model router. The `provider` config field is now optional — the model string is the source of truth.
- Default model format changed to `provider/model` (e.g., `anthropic/claude-sonnet-4-6`).
- REPL now owns the Agent (instead of borrowing), enabling mid-session provider swaps.
- `build_agent()` extracted as a standalone reusable function in `app.rs`, called on both startup and provider switch.

### Fixed

- Grafeo graph database dependency now uses crates.io (`grafeo = "0.5"`) instead of local filesystem paths. `cargo install` no longer fails on machines without the author's directory layout.
- OpenAI tool calling loop — the agent no longer repeats the same tool call indefinitely. Tool results are now correctly serialized in OpenAI's expected format (`role: "tool"`, `tool_call_id`), allowing the model to see results and proceed.

### Removed

- `resolve_provider()` and `resolve_provider_name()` from Abstract CLI — replaced by `cersei_provider::from_model_string()`.
- Hardcoded local filesystem paths from all `Cargo.toml` files.

## [0.1.1] - 2026-04-03

### Added

- **Schema versioning and migration engine** (`graph_migrate.rs`). Graph databases now store a `(:SchemaVersion)` node. On open, the code checks the version and runs sequential migrations automatically. Each migration is idempotent.
- **Confidence decay**. Memory nodes track `last_validated_at` and `decay_rate`. `effective_confidence()` computes time-decayed confidence at read time — old memories lose weight without manual cleanup.
- **Embedding readiness**. Memory nodes include `embedding_model_version` (empty by default) preparing for future vector-based semantic recall.
- **`revalidate_memory(id)`** resets the decay clock on a memory node.
- **`schema_version()`** on `GraphMemory` for inspecting the current graph version.
- **Centralized GQL queries**. All 15+ scattered query strings in `graph.rs` extracted into a `mod gql` block.

### Changed

- `GraphMemory::open()` and `open_in_memory()` now auto-detect schema version and migrate on startup. No API change — MemoryManager callers are unaffected.
- `store_memory()` writes v2 fields (`last_validated_at`, `decay_rate`, `embedding_model_version`) on new nodes. Public signature unchanged.
- Default provider in Abstract CLI changed from `anthropic` to `auto` (detects from environment variables).
- `README.md` updated with Abstract CLI section, three-way benchmark table, and docs link.

### Fixed

- Empty `ANTHROPIC_API_KEY` environment variable no longer treated as valid auth.
- `run_tool_bench_claude.sh` (renamed from `run_tool_bench.sh`) — `grep` recall measurement no longer fails silently on no match.

## [0.1.0] - 2026-04-02

Initial release.

### Core SDK

- **cersei-types**: Provider-agnostic types — `Message`, `ContentBlock`, `Usage`, `StopReason`, `StreamEvent`, `CerseiError`.
- **cersei-provider**: `Provider` trait with Anthropic and OpenAI implementations. SSE streaming, token counting, prompt caching, extended thinking, OAuth support.
- **cersei-tools**: `Tool` trait, 34 built-in tools across 7 categories (filesystem, shell, web, planning, scheduling, orchestration, other). `#[derive(Tool)]` proc macro. Permission system with 6 levels. Bash command safety classifier. Skill discovery (Claude Code + OpenCode formats). Shell state persistence across invocations.
- **cersei-agent**: `Agent` builder with 20+ configuration options. Agentic loop with tool dispatch and multi-turn conversations. 26-variant event system (`AgentEvent`). `AgentStream` with bidirectional control. Auto-compact at configurable context threshold. Effort levels (Low/Medium/High/Max). Sub-agent orchestration. Coordinator mode. Auto-dream background consolidation. Session memory extraction.
- **cersei-memory**: `Memory` trait with `JsonlMemory` and `InMemory` backends. `MemoryManager` composing 3 tiers: Grafeo graph DB, flat files (memdir), CLAUDE.md hierarchy. Session storage via append-only JSONL with tombstone soft-delete. Auto-dream 3-gate consolidation system.
- **cersei-hooks**: `Hook` trait for lifecycle middleware. Pre/post tool use, model turn events. `ShellHook` for external command integration.
- **cersei-mcp**: MCP client over JSON-RPC 2.0 stdio transport. Tool discovery, resource enumeration, environment variable expansion.
- **cersei**: Facade crate re-exporting all sub-crates via `prelude::*`.

### Graph Memory

- Grafeo embedded graph database with 3 node types (`Memory`, `Session`, `Topic`) and 2 edge types (`RELATES_TO`, `TAGGED`).
- Graph recall in 98 microseconds (indexed lookup vs file-by-file text scan).
- Graph ON adds zero overhead to scan and context building, 92.5% faster recall.

### Benchmarks

- Tool dispatch: Edit 0.02ms, Read 0.09ms, Grep 6ms, Bash 17ms.
- Memory scan: 1.2ms for 100 files.
- Session I/O: 27us write, 268us load (100 entries).
- Context build: 45us (CLAUDE.md + MEMORY.md).

### Examples

- `simple_agent`, `custom_tools`, `streaming_events`, `multi_listener`, `resumable_session`, `custom_provider`, `hooks_middleware`, `benchmark_io`, `usage_report`, `coding_agent`, `oauth_login`.
- 5 stress test suites: core infrastructure, tools, orchestration, skills, memory. 160 unit tests, 262 stress checks.

### Documentation

- 10 markdown guides covering getting started, providers, tools, agent lifecycle, events, memory, hooks, permissions, architecture, benchmarks.
