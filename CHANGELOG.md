# Changelog

## [Unreleased]

### Fixed

- **`Agent::run_stream` — cancelling a run no longer retires the agent.** `Agent::cancel()` cancelled a `tokio_util::sync::CancellationToken` held for the agent's whole lifetime, and the runner re-checks that token at the top of every turn. A `CancellationToken` never un-cancels, so the *first* cancel was permanent: every later `run()`/`run_stream()` returned `Cancelled` without ever reaching the provider. The CLI hit this on every Ctrl+C. Every run now mints its own token, so cancellation ends the run in flight and nothing more — a fresh token, never the previous run's, because an `AgentStream` cancels on drop and a shared token would let a stale handle kill a run started after it. `AgentBuilder::cancel_token` is now explicitly a *process-shutdown* signal: each run's token is a child of it, so firing it stops the current run and every later one, while cancelling a run leaves it (and its other children) untouched. New `Agent::cancel_token()` accessor exposes the live run token for observation.
- **`AgentStream`'s control channel is now read.** `run_agent_streaming` bound its control receiver as `_control_rx` and never touched it, so every message an `AgentStream` sent died in an undrained buffer. All three documented control methods were silent no-ops:
  - `AgentStream::cancel()` now cancels the run (directly on the run's token, so it lands even if the control buffer is full or the pump has yet to be scheduled).
  - `AgentStream::inject_message()` now appends the message to the conversation at the next turn boundary.
  - `AgentStream::respond_permission()` now decides the request it names. The runner emits `AgentEvent::PermissionRequired` and waits for the answer whenever the permission policy opts into stream deferral, decided before dispatch so concurrent tool calls cannot interleave prompts. Previously the event was never emitted anywhere and `StreamDeferredPolicy` silently allowed everything; with no stream to ask (the non-streaming `run()` path) it still falls back to allowing, now with a warning.
- **Streamed runs emit their terminal event to the agent's own listeners.** `run()` emitted `Complete`/`Error` via `Agent::emit`; `run_stream()` sent them only down the event channel. `on_event` handlers, `Reporter`s and broadcast subscribers therefore saw every event of a streamed run except the one saying it had ended.
- **Dropping an `AgentStream` cancels its run.** The run is driven by a spawned task owning an `Arc<Agent>`; dropping the handle only closed the receiver, and every send site swallows that failure, so the agent kept calling the provider — and spending — with nobody listening.
- **`Agent::run()` no longer deadlocks on responses over ~512 events.** `run_agent` built a 512-slot event channel and bound the receiver as `_event_rx` — an underscore-prefixed binding lives to the end of scope, unlike a bare `_` — so the channel stayed open with nobody draining it and `event_tx.send().await` blocked forever once it filled. Any non-streaming run emitting more than 512 events (a few hundred text deltas) hung. The receiver is now dropped up front, making each send a no-op on that path.
- **`abstract` CLI:** Ctrl+C during a run now cancels that run — through its `AgentStream` in the TUI, and through the active agent in the REPL/JSON path — instead of firing the process-wide token, so the next prompt works. The process-wide token now does what a shutdown signal should: it quits the TUI, and is fired on the exit path. `signals::fresh_cancel_token`, an unused `#[allow(dead_code)]` helper left over from an earlier attempt at this fix, is gone — the agent resets its own token per run.
- Documented streaming snippet in `quick-start` that could not compile (`run_stream` needs `&Arc<Agent>`).

### Added

- **`AgentBuilder::stream_with(prompt)`** — the streaming twin of `run_with`: build and start streaming in one call, no `Arc` wrapping.
- **`Agent::into_stream(prompt)`** — start a streaming run from an owned `Agent`.
- **`AgentBuilder::build_shared()`** — build straight to `Arc<Agent>` for multi-turn streaming.
- **`AgentStream::detach()`** — opt out of drop-cancellation and let a run outlive its handle.
- **`Agent::cancel_token()`** — observe the current run's cancellation token.
- **`PermissionPolicy::defers_to_stream()`** — defaulted to `false`, so existing policies are unaffected; `StreamDeferredPolicy` overrides it to opt into `AgentEvent::PermissionRequired`.
- `AgentEvent` is now re-exported from the `cersei_agent` crate root.

## [0.2.5] - 2026-06-29

### Changed

- **`Grep` is now native and in-process — no external `rg`/`grep` binary required.** The tool previously shelled out to ripgrep if `which` found it on `PATH`, else fell back to system `grep` (losing gitignore-awareness and speed). Since `rg` is rarely preinstalled, models that reached for it — directly or via the tool — failed on stock machines. `Grep` (and the `tool_primitives::search::grep` primitive) now use ripgrep's own library crates: [`ignore`](https://docs.rs/ignore) for a lock-free **parallel, gitignore-aware** recursive directory walk and [`grep`](https://docs.rs/grep) for the regex matcher/searcher. Behavior is now identical on every machine, and the tool description steers models to use `Grep` instead of running `rg` in Bash.
  - **Respects `.gitignore`/`.ignore` and skips hidden + binary files by default** (gitignore is honored even outside a checked-out git repo). Output is sorted by `(file, line)` for **deterministic** results.
  - **New `GrepOptions` fields** (`no_ignore`, `hidden`), both defaulting to today's behavior so `GrepOptions::default()` is unchanged. The `Grep` tool exposes `case_insensitive` and `hidden` in its input schema; `pattern`/`path`/`glob` and the 250-match cap are unchanged.
  - New workspace dependencies: `ignore` and `grep` (both Unlicense OR MIT). MSRV stays satisfied (these track Rust 1.85).
- Workspace bumped to **0.2.5** across every crate via `version.workspace = true`.

## [0.2.4] - 2026-06-28

### Added

- **New `MultiEdit` tool — apply several string replacements to one file atomically.** Weaker models bungle refactors that need many separate `Edit` calls (e.g. a variable rename touching several lines): each call re-reads stale context and the sequence drifts. `MultiEdit` takes an ordered `edits: [{old_string, new_string, replace_all?}]` list, applies them **sequentially in memory** (each edit sees the previous edit's result), and writes **all-or-nothing** — if any edit fails to match, the file is left untouched and the failing edit is named (`edit N of M`) with a corrective message. Every edit routes through the same tolerant replacer ladder as `Edit`, so it inherits whitespace/indentation tolerance and the destructive-match guard, plus the same input coercion (aliased field names, stringified booleans, missing `new_string` ⇒ deletion). Registered in `cersei_tools::filesystem()` (and thus `all()`/`coding()`).
  - For a single-pattern mass replace (rename `foo`→`bar` everywhere), `Edit`/`MultiEdit` with `replace_all: true` already does it in one call; `MultiEdit` adds the case of *several distinct* edits in one atomic call.

### Changed

- Workspace bumped to **0.2.4** across every crate via `version.workspace = true`.

## [0.2.3] - 2026-06-28

### Changed

- **The `Edit` tool now tolerates whitespace/indentation drift instead of requiring a byte-exact `old_string`.** Exact matching made edits brittle: weaker BYOK models (Qwen, DeepSeek, Gemini Flash) routinely drift on leading whitespace and indentation, so an otherwise-correct `old_string` failed with `old_string not found` and the model fell back to slow, error-prone `sed`/`cat` edits. `Edit` (and the `tool_primitives::fs::edit_file` primitive) now run a **replacer ladder**: exact match first, then line-trimmed, block-anchor, whitespace-normalized, and indentation-flexible strategies. Every strategy only ever yields a candidate that *actually exists in the file*, so a fuzzy match relaxes *how* the text is located, never *what* gets written.
  - **Destructive-match guard.** The line-based strategies require every line to match (after normalization); the one anchor-only strategy (block-anchor) is additionally gated by a similarity threshold so a coincidental first/last-line pair can't rewrite an unrelated block. Exact matches are still tried first, so genuine duplicates report `AmbiguousMatch` rather than being silently fuzzed.
  - **Input coercion + corrective errors.** The tool now accepts common near-miss field names (`path`/`old`/`new`), stringified booleans (`replace_all: "true"`), and a missing `new_string` (treated as a deletion). Failure messages now guide the model toward a fix (re-read the file; add context to disambiguate; set `replace_all`) instead of a bare error.
  - New primitive: `tool_primitives::replace::replace`.
- Workspace bumped to **0.2.3** across every crate via `version.workspace = true`.

## [0.2.2] - 2026-06-24

### Added

- **Multimodal input across the SDK.** Messages can now carry images, video, audio, and documents (PDF/text) — not just text — and the three first-party providers serialize them to each backend's native wire format. The provider-agnostic blocks (`ContentBlock::Image` / `ContentBlock::Document`) already existed; this release adds the high-level constructors to build them and teaches every provider to send them. Docs: [Multimodal Input](https://cersei.pacifio.dev/docs/multimodal).
  - **High-level constructors** in `cersei-types` (re-exported through `cersei::prelude`):
    - `ContentBlock::from_path(path)` — reads a local file, **auto-detects the MIME type** from magic bytes (PNG/JPEG/GIF/WebP/PDF/MP4/MOV/WebM/MP3/WAV/Ogg…) with an extension fallback, base64-encodes it, and routes to an `Image` block (image/video/audio) or a `Document` block (PDF/text).
    - `ContentBlock::image_bytes` / `image_base64` / `image_url`, and the `document_*` equivalents, plus `media_bytes(mime, bytes)` when you want to set the type explicitly.
    - `Message::user_with_files("caption", &paths)` and `Message::user_with_media(text, blocks)` for one-call multimodal user messages.
    - `MediaKind` + `detect_mime(bytes, path)` exported for callers that want the classifier directly.
  - **Provider serialization.** OpenAI now emits a typed `parts` array (`image_url` data-URLs for images, `file` parts for PDFs; video/audio are dropped since Chat Completions can't accept them). Gemini emits `inlineData` for base64 and `fileData`/`fileUri` for URLs — the same path for images, **video**, audio, and PDFs. Anthropic already round-tripped images and PDFs via direct serde (now covered by a wire-shape test).
  - **`Gemini` is now exported from the `cersei` facade** (`cersei::Gemini`, in the prelude) — previously only reachable via `cersei_provider::gemini::Gemini`.
  - **Examples.** `cersei/examples/multimodal.rs` (provider-agnostic) and `cersei/examples/gemini_vision_test.rs` (a live image-analysis smoke test).

### Changed

- Workspace bumped to **0.2.2** across every crate via `version.workspace = true`.
- **Gemini `thinking_budget` is now configurable** via `ProviderOptions` (mirrors the Anthropic provider). `gemini-2.5`+ models spend dynamic-thinking tokens out of `maxOutputTokens`, which silently truncates a small-budget response; `options.set("thinking_budget", 0)` disables thinking so the full budget goes to the visible answer. Surfaces as `generationConfig.thinkingConfig.thinkingBudget`.

## [0.2.1] - 2026-06-23

### Added

- **New crate: `cersei-workflows`.** A first-party workflow engine designed so an entire workflow round-trips to and from a visual builder (React + xyflow), powering the Atlas workflow UI. Feature-gated behind `workflows`; mirrors the Mastra DX (`createStep`/`createWorkflow`, `.then`, input/output JSON schemas, `.start()`/`.stream()`, shared state, workflows-as-steps, suspend/resume, status-discriminated run results). Docs: [Overview](https://cersei.pacifio.dev/docs/workflows-overview) · [API](https://cersei.pacifio.dev/docs/workflows-api) · [Cookbook](https://cersei.pacifio.dev/docs/workflows-cookbook).
  - **`ir`** — the serializable `WorkflowDef` (`nodes` + `edges`) is the single source of truth; the programmatic `WorkflowBuilder` and the UI emit the *same* IR (one IR, two front-ends), compiled once by `Workflow::compile`. Control flow covers sequential (`then`), parallel fan-out/`Join`, conditional `Branch`, `Loop` (`dowhile`/`dountil`/`foreach`), and `Map` reshape nodes, each with a clean xyflow mapping. `UiHints` carry x/y/label and are ignored at execution time, keeping the IR lossless across the React Flow boundary.
  - **`step`** — a `Step` trait mirroring `cersei_tools::Tool` (id + input/output JSON schema + async `execute`), plus first-party `FnStep`, `AgentStep` (wraps a `cersei_agent::Agent`), `ToolStep` (wraps any `Tool`), and `WorkflowStep` (nested workflows).
  - **`registry`** — a `StepRegistry` (mirrors AgentRL's `ToolRegistry`) maps step-ids to executable steps; UI JSON carries only references, the host supplies implementations, and `catalog()` feeds the builder palette.
  - **`condition`** — a small serde predicate enum over JSON-pointer paths (UI-editable, side-effect-free) instead of an opaque expression language, so branches render and edit structurally in the UI.
  - **`executor` / `events` / `store`** — a memoized recursive DAG walker; a `Serialize`-able `WorkflowEvent`/`WorkflowStream` for live UI status over SSE/WebSocket; and an in-memory `RunStore` backing snapshot-driven suspend/resume.
- The `workflows` feature is opt-in on the `cersei` facade (`cersei = { version = "0.2.1", features = ["workflows"] }`), re-exported as `cersei::workflows` with the common types in `cersei::prelude`.

### Changed

- Workspace bumped to **0.2.1** across every crate via `version.workspace = true`; the layout now includes `cersei-workflows`.

## [0.2.0] - 2026-06-19

### Added

- **New crate: `cersei-agentrl`.** AgentRL — a self-evolving orchestration layer on top of the Cersei agent SDK. Governs the run → fail → trace → plan → sandbox → promote → register loop via an `ExecutionGraph` and a dynamic `ToolRegistry`. Docs: [Overview](https://cersei.pacifio.dev/docs/agentrl-overview) · [API](https://cersei.pacifio.dev/docs/agentrl-api) · [Template DSL](https://cersei.pacifio.dev/docs/agentrl-template) · [Cookbook](https://cersei.pacifio.dev/docs/agentrl-cookbook).
  - **`graph` / `graph_reporter`** — build a failure-tracing DAG from agent events.
  - **`registry`** — a persisted, searchable database of agent-built tools.
  - **`orchestrator`** — the RL loop, written against the `AgentRlRunner` trait.
  - **`scrub`** — enforces the hard rule that no secrets land in persisted artifacts (traces, planned tools, registry rows).
- **New crate: `cersei-agentlang`.** The AgentTemplate DSL — a small functional language that LLMs and agents author and that executes on top of the Cersei runtime. Pipeline: `parse` → `Program` → `run_program` over an `EvalCtx`. Builtins (`io.*`, `net.*`, `agent.*`, `kv.*`) dispatch by tool name through the `ToolDispatch` trait, so the language is decoupled from the concrete tool set. Ships an LLM-facing `AGENTLANG_SPEC`.
- **New crate: `cersei-tbench`.** A purpose-built Terminal-Bench coding agent on the Cersei SDK + AgentRL, with its own task prompt.
- **`--agentrl` mode in abstract-cli** (`agentrl_run.rs`). Solves a single task with the self-evolving AgentRL orchestrator instead of a one-shot agent. Designed for headless / terminal-bench runs: resolves a provider from `--model`, builds a verifier that prefers a task-provided `run-tests.sh` and falls back to accepting the agent's result, runs the `Orchestrator` in the working directory, and prints a machine-readable result line compatible with the harbor adapter.
- **Anthropic via Google Vertex AI provider** (`cersei-provider::AnthropicVertex`, `vertex` registry id). Reuses the direct-Anthropic request body + SSE parser; differences are the Vertex `:streamRawPredict` URL, `Authorization: Bearer <gcp token>` auth, and the `"anthropic_version": "vertex-2023-10-16"` body field. Auth resolves in order from a self-refreshing service-account JSON (via `gcp_auth`, required for long runs since GCP tokens expire ~1h), a pre-minted `VERTEX_ACCESS_TOKEN`, or `gcloud auth print-access-token`.
- **First-class custom OpenAI-compatible endpoints.** Any OpenAI-compatible provider's base URL can now be overridden with `<PROVIDER>_BASE_URL` (e.g. `OPENAI_BASE_URL`, `DEEPSEEK_BASE_URL`, `OPENROUTER_BASE_URL`); `OPENAI_API_BASE` is accepted as a legacy alias for `openai`. `ProviderEntry::resolved_api_base()` performs the lookup. When an override is in effect, a redacted final-target diagnostic (`provider=… host=… model=… key_present=…`) is printed before the first request so a misrouted endpoint is obvious instead of surfacing as an opaque `401`. Closes [#17](https://github.com/pacifio/cersei/issues/17) (item 2).
- **`deepseek-reasoner`** added to the DeepSeek provider registry (64K context, thinking capabilities) alongside `deepseek-chat` and `deepseek-coder`. Docs updated with a minimal `--provider deepseek` working example and the `<PROVIDER>_BASE_URL` override table.

### Changed

- Workspace bumped to **0.2.0** across every crate via `version.workspace = true` (18-crate layout now includes `cersei-agentrl`, `cersei-agentlang`, `cersei-tbench`).
- `ProviderEntry::api_key_from_env()` now **trims** the env value before use — launchers that reconstruct a key via command substitution (`KEY="$(…)"`) commonly leave a trailing newline, which previously produced a confusing `401` instead of a clean error. Whitespace-only values are treated as absent. Addresses [#17](https://github.com/pacifio/cersei/issues/17) (item 1).
- The OpenAI-compatible router path resolves its base URL through `resolved_api_base()` instead of the hardcoded registry default, so `--provider deepseek` always targets `api.deepseek.com` and never silently falls back to `api.openai.com`.

### Fixed

- **UTF-8 panic on multibyte input in the TUI** (`assertion failed: self.is_char_boundary(idx)`). The text-input cursor in `tui/event_loop.rs` tracked `cursor_pos` as a byte index but advanced it by one *character* per keypress, so typing a multibyte character (Chinese / Japanese / Korean, etc.) and pressing a key landed the cursor mid-character and panicked on the next insert. Char insert, Backspace, Left, and Right now move by the char's UTF-8 byte length. Fixes [#14](https://github.com/pacifio/cersei/issues/14).
- **UTF-8 panic on accented characters** (`byte index … is not a char boundary; it is inside 'à'`). Six byte-slice truncators across the CLI and agent runner (`render::truncate`, `event_loop::truncate`, `graph::truncate_label`, `messages::wrap_text`, `widgets/input` wrap, and `runner` content head/tail cuts) could split a multibyte UTF-8 sequence. The `truncate*` helpers are now char-based; the word-wrap and content-cut paths floor/ceil their slice points to char boundaries with a forward-progress guard so a single wide char can't loop. Fixes [#17](https://github.com/pacifio/cersei/issues/17) (item 3).

## [0.1.9] - 2026-05-17

### Added

- **New crate: `cersei-vms`.** Sandbox & VM isolation layer for coding agents. Lets the agent run shell commands and edit files inside isolated environments instead of on the host, and lets multiple parallel agents run in separate sandboxes that share state through host-mediated primitives. 15th workspace crate. Docs: [VMs Overview](https://cersei.pacifio.dev/docs/vms-overview) · [API](https://cersei.pacifio.dev/docs/vms-api) · [Cookbook](https://cersei.pacifio.dev/docs/vms-cookbook).
  - **Core traits** in `cersei_vms::runtime` — `SandboxRuntime`, `Sandbox`, plus `Commands` and `Filesystem` per-sandbox surfaces. Mirrors E2B's `Sandbox` / `Commands` / `Filesystem` shape almost 1:1, so existing E2B-based mental models port directly.
  - **Backends.** `LocalProcessRuntime` (always-on, no isolation — for tests and `--sandbox local`) and `DockerRuntime` (real container isolation via the local `docker` CLI; feature `backend-docker`, default-on). Phase 1 ships without `bollard` / HTTP-over-UDS — we shell out, which works identically on macOS, Linux, and Windows wherever Docker is installed.
  - **Cross-sandbox primitives** in `cersei_vms::primitives`. `Volume` registry (host-side dirs bind-mounted into N sandboxes), `Mailbox` (broadcast pub/sub by topic, backed by `tokio::sync::broadcast`), `KvStore` (DashMap + optional journal file, versioned CAS via `cas(key, expected_version, value)`). All three are reachable from sandboxes through a host-side broker; sandboxes never need direct network links between each other.
  - **First-party snapshots.** `Sandbox::snapshot() -> SnapshotId` and `SandboxRuntime::restore(&SnapshotId) -> SandboxHandle`. Docker implementation = `docker commit` for FS state + a JSON `SnapshotManifest` (env, mounts, mailbox topics, KV checkpoint) in `~/.cersei/vms/snapshots/`. Local implementation = directory copy. Snapshots survive process restart.
  - **`cersei-envd` binary** (feature `envd`, default-on). Tiny Rust JSON-RPC 2.0 daemon meant to be baked into container images for richer in-VM ops; talks to the host over a bind-mounted Unix socket at `/run/cersei-envd.sock`. Methods: `process.run`, `fs.{read,write,list,stat,mkdir,remove}`, `ping`, `info`. Reuses the JSON-RPC wire shape from `cersei-mcp/src/jsonrpc.rs`.
  - **Reference image.** `crates/cersei-vms/docker/Dockerfile` produces `cersei/sandbox-base:latest` — Alpine 3.20 + `bash` + `git` + `cersei-envd`, ~8 MB.
  - **Tests.** 9 passing — 4 LocalProcessRuntime (incl. snapshot round-trip preserving FS + KV state), 2 mailbox (cross-sandbox pub/sub + topic isolation), 3 KvStore (concurrent writes, CAS rejecting stale writers, journal-survives-reopen).
- **cersei-tools integration** behind a new `vms` feature.
  - **Transparent routing in `BashTool`.** When `ctx.extensions.get::<Arc<dyn cersei_vms::Sandbox>>()` is `Some`, the bash tool routes `RunRequest` through the sandbox instead of running on the host. Falls back to the existing local `pproc::exec()` path otherwise — fully backward-compatible for users not on the feature.
  - **New agent-facing tools** in `cersei_tools::vm_tools`: `SendVmMessage` / `RecvVmMessage` (cross-sandbox pub/sub with bounded wait timeout), `SharedStateGet` / `SharedStateSet` (KV with optional CAS), `SandboxSnapshot`. All gated by the existing `PermissionLevel` system (`Write` / `ReadOnly`).
- **`cersei` facade** — `cersei::vms` re-export behind a new default-on `vms` feature, so `use cersei::prelude::*` gains the sandbox surface without an extra import.

### Changed

- Workspace bumped to **0.1.9** (15-crate layout now includes `cersei-vms`).
- `Cargo.toml` workspace `members` and `[workspace.dependencies]` updated to register `cersei-vms` alongside the existing 14 crates.

### Deferred to 0.1.10

- **Agent-builder injection.** `AgentBuilder::with_sandbox(handle)` / `with_sandbox_allocator(alloc)` so each `Agent` (or each child of a delegate batch) lands inside its own sandbox automatically, without callers hand-stuffing `extensions`. Phase 1 leaves this as a call-site responsibility while we land the right type-shape (needs a generic extension-provider so `cersei-agent` doesn't permanently depend on `cersei-vms`).
- **`cersei-agent::delegate` per-task allocator.** Wire `Option<Arc<dyn SandboxAllocator>>` into `DelegateConfig` so each parallel worker gets a fresh sandbox + shared Volume/Mailbox.
- **abstract-cli surface.** `--sandbox <local|docker>` flag and `/vm` slash command (list / inspect / kill sandboxes attached to the current session).
- **Other backends.** `FirecrackerRuntime` (Linux-only microVMs), `E2bRuntime` (remote E2B API), `VercelSandboxRuntime`.
- **Phase 2 lifecycle.** `docker pause` / `unpause` on `DockerRuntime`, incremental snapshots, snapshot garbage collection, bi-directional stdin streaming on `Commands::stream`.

## [0.1.8] - 2026-04-25

### Added

- **Workstream A — Memory++ (partial).** Targeted retrieval improvements aimed at LongMemEval multi-session / temporal weak spots, on top of the 0.1.7 85.7 % hybrid baseline.
  - **Omega-style per-question-type RAG prompts** (`bench/long-mem/src/omega_prompts.rs`) — verbatim ports of Omega-memory's 5 answerer templates (vanilla, enhanced-confident, multi-session-aggregation, temporal, preference), routed by `QuestionType`. Stacks on top of Mastra's `OBSERVATION_CONTEXT_INSTRUCTIONS` in the system prompt, Omega template in the user turn.
  - **LLM-driven query expansion** (`bench/long-mem/src/query_expand.rs`) — Omega's verbatim `OMEGA_EXPANSION_SYSTEM` prompt producing `{lex, vec, hyde}` variants. HyDE gated by a `looks_vague` heuristic (≥ 80 chars or synthesis keyword). Tolerant JSON parser (fenced / chatty). Variants fused into RRF at `EXPANSION_WEIGHT = 0.8`.
  - **Abstention floors** (`VEC_MIN_SIM = 0.35`, `GRAPH_MIN_SCORE = 0.30`) in `HybridConfig::retrieve` — drops low-confidence hits before RRF.
  - **Semantic dedup at ingest** — normalized Jaccard ≥ 0.85 collapses near-duplicate facts to keep the retrieval set crisp.
  - **500Q `longmemeval_s` numbers on `gemini-2.5-flash` (2026-04-25):**
    - a-baseline-jsonl: **87.56 %** overall
    - b-embed-only: **86.56 %**
    - d-hybrid-embed-graph: **86.28 %** overall, **81.0 % multi-session (+2.5 pp vs 0.1.7 hybrid)**, 82.7 % temporal-reasoning, 90.3 % knowledge-update
    - A2 FTS5 channel and A3 three-way RRF deferred to 0.1.9 — that pair is what closes the remaining gap to the 90 % stretch target.
- **Workstream B — Skills primitive.** New leaf crate `cersei-skills` for loading [agentskills.io](https://agentskills.io/)-compatible SKILL.md files.
  - **`SkillRegistry`** — loads from `~/.cersei/skills/` (global) and `.cersei/skills/` (project); project-over-global merge; progressive-disclosure `list()` / `view()` / `by_tag()` surface.
  - **`Skill` parser** — YAML frontmatter (name ≤ 64 ASCII, description ≤ 1024, version, license, platforms, prerequisites) + markdown body. 100 KB size cap.
  - **Security scanner** — `SkillSecurityIssue::{PromptInjection, DestructiveCommand, CredentialExfil, InvisibleUnicode, SudoersOrSetuid}`; skills refuse to load until reviewed.
  - **`HookEvent::TurnsElapsed`** added to `cersei-hooks`; `cersei-agent::runner` emits it every `turns_elapsed_cadence` turns (default 10, 0 disables). Intended for background skill review nudges without blocking the agent loop. `AgentBuilder::turns_elapsed_cadence(n)`.
- **Workstream C — Delegation / parallel subagents.** Port of hermes-agent's `delegate_tool.py`.
  - **`cersei_agent::delegate`** module — `DelegateConfig`, `DelegateTask`, `DelegateResult`, `run_batch`. Bounded parallelism via `tokio::task::JoinSet` + semaphore (`DEFAULT_MAX_CONCURRENT = 3`). Depth cap `MAX_DEPTH = 2` (parent = 0, child = 1, no grandchildren). Default blocklist `{delegate, clarify, memory, memory_write, send_message, ask_user, AskUserQuestion}` stripped from child toolsets at depth ≥ 1. Child system prompt is a verbatim port of `_inspirations/hermes-agent/tools/delegate_tool.py::_build_child_system_prompt`.
  - **`cersei_agent::delegate_tool::DelegateTool`** — `impl Tool` wrapper. Accepts either a single `goal` or a `tasks` array. Fresh provider + toolset per child via `ProviderFactory` / `ToolsetFactory` `Arc<dyn Fn() -> Box<...>>` closures (the non-cloneable trait-object workaround).
  - **`AgentBuilder::provider_boxed(Box<dyn Provider>)`** — builder extension needed by the delegation pathway.
  - Best-effort semantics: one child failure doesn't abort the batch; the parent gets a per-task error summary.

### Changed

- `bench/long-mem/src/runner.rs` — answerer system prompt now splices `ANSWERER_BASE_SYSTEM` + Mastra `OBSERVATION_CONTEXT_INSTRUCTIONS`; user turn uses the Omega per-type template via `omega_prompts::prompt_for(question_type)`.
- `HybridConfig::retrieve` rewritten to perform multi-query RRF over `{primary, lex, vec, hyde}` with abstention floors; variants contribute at `EXPANSION_WEIGHT = 0.8`.

### Deferred to 0.1.9

- **Workstream A2/A3** — FTS5 channel on `cersei-memory` (behind new `fts` feature) and three-way RRF with per-type `RetrievalProfile` TOML. The biggest lever still unused.
- **Python RPC code-exec tool** — collapses multi-step tool chains into one LLM turn (terminal-bench unlock). Requires Rust-side UDS server + Python stub generator.
- **Workstream D** — `cersei-session-search` (FTS5), `HonchoMemoryProvider`, `cersei-cron`.

## [LongMemEval Benchmark] - 2026-04-24

### Added

- **LongMemEval head-to-head benchmark (`bench/long-mem/`).** Runs the 500-question [LongMemEval](https://arxiv.org/abs/2410.10813) (ICLR 2025) dataset — the same benchmark [Mastra](https://mastra.ai/research/observational-memory), [Zep](https://arxiv.org/abs/2501.13956), Supermemory, Hindsight, and EmergenceMem report on — against four Cersei memory configurations: full-context baseline, usearch-HNSW semantic (`EmbeddingMemory`), grafeo graph substring+rerank (`GraphMemory::recall_top_k`), and a hybrid config combining an Observer LLM pass + embedding + graph + RRF fusion. Judge rubric, observer rubric, and context-injection prompts are **verbatim ports** from Mastra's `@mastra/memory` so numbers land on the same public leaderboard. Docs: [Memory Benchmark](https://cersei.pacifio.dev/docs/bench-memory).
  - **Headline result on `longmemeval_s` (2026-04-24, all on `gemini-2.5-flash`):**
    - Baseline (full-context `JsonlMemory`): **84.6 %** overall, 86.7 % abstention, 422/500, 53.16 M input tokens.
    - Embed (`EmbeddingMemory`, usearch HNSW + `gemini-embedding-001`): **84.2 %** overall, 86.7 % abstention, 429/500, **2.68 M input tokens (20× fewer than baseline)**.
    - Graph substring (`GraphMemory`, grafeo): 6.6 % overall (honest floor — substring can't paraphrase-match), 100 % abstention.
    - **Hybrid (Observer + embed + graph + RRF):** **85.7 % overall, 93.3 % abstention, 432/500, 1.58 M input tokens (34× fewer than baseline).** Best config; wins outright on `knowledge-update` (94.4 %).
  - **Leaderboard position:** Cersei Hybrid 85.7 % beats Supermemory / `gemini-3-pro-preview` (85.2 %), Supermemory / `gpt-5` (84.6 %), Mastra OM / `gpt-4o` (84.23 %), Mastra RAG (80.05 %), Zep (71.2 %), and lands within 0.3 pp of EmergenceMem Internal on `gpt-4o` (86.0 %). Remaining gap to Mastra OM / `gemini-3-flash-preview` (89.2 %) and `gpt-5-mini` (94.87 %) is concentrated on answerer-model tier, not algorithm.
- **Mastra prompt port** (`bench/long-mem/src/mastra_prompts.rs`) — verbatim ports of `OBSERVER_EXTRACTION_INSTRUCTIONS`, `OBSERVER_OUTPUT_FORMAT`, `OBSERVER_GUIDELINES`, `OBSERVATION_CONTEXT_PROMPT`, `OBSERVATION_CONTEXT_INSTRUCTIONS` from `_inspirations/mastra/packages/memory/src/processors/observational-memory/`. Unit tests assert the required sections round-trip.
- **`cersei-memory::embedding_memory::EmbeddingMemory`** — thin adapter bridging `cersei-embeddings::EmbeddingStore` into the `Memory` trait. Behind the new optional `embed` feature so consumers opt in. Exposes `add`, `add_batch`, and standard `Memory::{store, search, delete}` with relevance-scored `MemoryEntry` return values.
- **`cersei-memory::graph::GraphMemory::recall_top_k(query, limit)`** — returns `Vec<(String, f32)>` where the score is the fraction of query words found in each memory. Additive; does not change the existing `recall` signature.
- **`cersei-embeddings::GeminiEmbeddings`** rewritten for `gemini-embedding-001` (3072-d native, Matryoshka `outputDimensionality` supported). Uses the `embedContent` endpoint with `futures::stream::buffered(20)` concurrency; retries 429 + 5xx + transport errors with exponential backoff (6 attempts, ~30 s window).

### Fixed

- **`cersei-embeddings::OpenAiEmbeddings::embed_batch`** no longer panics on multi-byte UTF-8 input. The truncation logic used raw byte slicing at index 2000, which panicked on any text containing non-ASCII characters (Spanish diacritics, emoji, smart quotes) when the character straddled the slice boundary. Now walks back to the nearest char boundary. Caught while running the LongMemEval bench against Spanish session content.

### Security

- **Gemini API keys moved from URL query string (`?key=…`) to the `x-goog-api-key` header** in both `cersei-provider::Gemini` and `cersei-embeddings::GeminiEmbeddings`. The URL now contains no secret, so `reqwest::Error` Display (which prints the URL) cannot leak the key. Prior code would expose the key in every transport error, and those errors rode into tracked `.log` files and per-question `rows-*.json` artifacts.
- **`redact_url_key` helper** in `cersei-embeddings/src/gemini.rs` — belt-and-braces scrubber applied to any error body that might still reference a URL carrying `key=…` (e.g. historic error strings, upstream error bodies).
- **`.gitignore` hardened** to cover `bench/**/*.log`, `bench/**/results*/`, `bench/**/runner-*.sh`, `bench/**/abstract-output.jsonl`, `bench/**/tb-results/`, and any `.env*` / `*.secret*` / `credentials*.json`.
- **`bench/term-bench/runner-google.sh`** no longer hardcodes a key; it fails fast unless `GOOGLE_API_KEY` is already in env.
- **38 previously-tracked bench artifacts** (logs, per-question rows, terminal-bench JSONL) removed from the git index. Numbers are reproducible by rerunning the bench.
- Pre-commit sanity check: `git ls-files | xargs grep -l -E "AIza[A-Za-z0-9_-]{35}|sk-[A-Za-z0-9_-]{30,}"` must return zero tracked files.

## [0.1.7] - 2026-04-20

### Added

- **New crate: `cersei-compression`.** Structural and command-aware compression for tool outputs, sitting between a tool's `execute()` result and the agent's existing `cap_tool_result()` truncation. Trims the tokens that typical tool output wastes on ANSI, comments, blank lines, and boilerplate. Three levels: `Off` (default — zero behaviour change), `Minimal` (ANSI + comment stripping, whitespace collapse), `Aggressive` (adds language-aware body stubbing for source files and declarative TOML rules for common CLIs: `git`, `cargo`, `npm`, `pnpm`, `pytest`, `docker`, plus a generic catch-all). Docs: [Overview](https://cersei.pacifio.dev/docs/compression-overview) · [Benchmarks](https://cersei.pacifio.dev/docs/compression-benchmarks).
  - **Credit:** this crate is a port of [**rtk** (Rust Token Killer)](https://github.com/rtk-ai/rtk) by **Patrick Szymkowiak**, MIT licensed. See `crates/cersei-compression/LICENSE` for full attribution and the per-module mapping table.
  - **`AgentBuilder::compression_level(level)`** — set at build time.
  - **`Agent::set_compression_level(level)` / `agent.compression_level()`** — change or inspect at runtime (shared-mutex, takes effect on the next tool call).
  - **Observability** — every call emits a structured `tracing::info!` event on target `cersei_compression` with `tool`, `level`, `strategy`, `detail` (matched rule or detected Language), `before_bytes`, `after_bytes`, `before_lines`, `after_lines`, and `savings_pct`. Subscribe with `RUST_LOG=cersei_compression=info`.
- **`abstract-cli` compression controls.**
  - **`--compress <off|minimal|aggressive>`** CLI flag, `ABSTRACT_COMPRESSION` env var, and `compression_level` in `~/.abstract/config.toml` / `.abstract/config.toml`.
  - **`/compression [on|off|minimal|aggressive]`** slash command flips the active agent's level mid-session. `/compression` with no argument reports the current level.
- **Live-provider savings benchmark** (`crates/cersei-agent/tests/e2e_openai_compression.rs`, `#[ignore]`). Same prompt, same fixture, same tool — only `CompressionLevel` changes between runs. Token counts are provider-reported, not our estimate.
  - **OpenAI `gpt-4o-mini`** — 11,576 → 8,202 input tokens (**−29.1%**, Δ 3,374 tokens; 15 → 13 tool calls).
  - **Google Gemini `gemini-2.5-flash`** — 4,490 → 1,700 input tokens (**−62.1%**, Δ 2,790 tokens; 1 → 1 tool call).
  - **Synthetic floors** (`crates/cersei-compression/tests/savings.rs`) — `git log` ≥ 30% at Minimal, `cargo test` ≥ 25% at Minimal, Off is byte-for-byte identity.
  - **Reproduce** — full commands and per-call log dumps on [Compression Benchmarks](https://cersei.pacifio.dev/docs/compression-benchmarks).

### Changed

- **Workspace version** — every crate (`cersei`, `cersei-agent`, `cersei-compression`, `cersei-embeddings`, `cersei-hooks`, `cersei-lsp`, `cersei-mcp`, `cersei-memory`, `cersei-provider`, `cersei-tools`, `cersei-tools-derive`, `cersei-types`, `abstract-cli`) bumped to **0.1.7** via `version.workspace = true`.
- **`cersei-agent::Agent` + `AgentBuilder`** gained a `compression_level` field wired through to the runner at `crates/cersei-agent/src/runner.rs:708`. Default is `CompressionLevel::Off` — existing users see no behavioural change without opting in.

## [0.1.6-patch.2] - 2026-04-18

### Added

- **New crate: `cersei-embeddings`.** Provider-agnostic text embeddings with a pluggable `usearch`-backed vector index, extracted from the inline embedding logic in `CodeSearch`. Ships with built-in `GeminiEmbeddings` (Google `text-embedding-004`, 768-d) and `OpenAiEmbeddings` (OpenAI `text-embedding-3-small`, 1536-d, base-URL overridable for Azure / Ollama).
  - **`EmbeddingProvider` trait** — `embed`, `embed_batch`, `dimensions`, `name`. Implement once, compose with everything below.
  - **`VectorIndex`** — thin `usearch` wrapper exposing `new`, `from_vectors`, `reserve`, `add`, `search`, `len`. Cosine / L2 / InnerProduct metrics with automatic similarity conversion.
  - **`EmbeddingStore<P>`** — provider + index bundled for the add-text / search-by-text flow. `new`, `add_batch`, `search`.
  - **`auto_from_model(&str)`** factory — picks OpenAI or Gemini based on an LLM model string and reads the appropriate env var.
  - **Leaf dependency** — the crate has zero dependencies on other `cersei-*` crates, usable standalone for RAG, semantic search, clustering, and custom tools.
  - **Docs** — [Overview](https://cersei.pacifio.dev/docs/embeddings-overview), [API Reference](https://cersei.pacifio.dev/docs/embeddings-api), [Cookbook](https://cersei.pacifio.dev/docs/embeddings-cookbook).
- **General-Agent Framework Benchmark.** First-party, end-to-end measured showdown against the Python stack — Agno 2.5.17, PydanticAI 1.22.0, LangGraph 1.1.8, CrewAI 1.14.2. Everything measured on Apple M1 Pro via the same harness suite, methodology mirroring Agno's own `cookbook/09_evals/performance/` (real agent constructors, no LLM invocation, no stub models). Three new chart components (`AgentInstantiationChart`, `PerAgentMemoryChart`, `MaxConcurrentChart`) now render on three pages.
  - **Headline numbers** — Cersei **704 B per agent** (8× smaller than Agno's 5.8 KiB, 44× smaller than LangGraph's 30 KiB). Cersei builds 500 agents concurrently in **4.4 ms / 8.5 MB** vs CrewAI's **50,697 ms / 1,739 MB** at the same N — **11,500× faster wall time, 204× less memory**. Cersei sweeps to 10,000 concurrent agents held live in 87 ms on 22 MB total RSS.
  - **Details & charts** — [cersei.pacifio.dev/docs/bench-vs-agents](https://cersei.pacifio.dev/docs/bench-vs-agents) (dedicated deep-dive page with all five axes, reproduction instructions, and caveats).
  - **General comparisons page** — [cersei.pacifio.dev/docs/comparisons](https://cersei.pacifio.dev/docs/comparisons#cersei-vs-general-agent-frameworks-agno-pydanticai-langgraph-crewai) (now includes side-by-side against all four Python frameworks + "when to choose which" guidance).
  - **Landing page performance section** — [cersei.pacifio.dev/docs](https://cersei.pacifio.dev/docs#vs-general-agent-frameworks) (the new "vs General Agent Frameworks" sub-section under Performance at a Glance).
  - **Harness source** — Rust: `crates/cersei-agent/benchmarks/general_agent_bench.rs` (opt-in via `bench-full` feature). `uv`-managed Python harnesses + `run.sh` at `bench/general-agents/` ([GitHub](https://github.com/pacifio/cersei/tree/main/bench/general-agents)). Reproduce end-to-end on your own machine with `./run.sh`.

### Changed

- **`cersei-tools::code_search`** refactored to delegate to `cersei-embeddings`. Inline `gemini_embeddings` / `openai_embeddings` / raw `usearch::Index` handling removed. `CodeSearchTool::with_embeddings` now takes `Arc<dyn EmbeddingProvider>` instead of `(provider_string, api_key_string)`.
- **`abstract-cli`** constructs its embedding provider via `cersei_embeddings::auto_from_model(&resolved_model)` — the model → provider detection and env-var lookup moved into the new crate. End-user behaviour of `--embedding-api` is unchanged.
- **`cersei-lsp`** `Cargo.toml` now uses `version.workspace = true` (was hardcoded to `0.1.6`) so workspace version bumps propagate.
- **Google provider default model** upgraded from `gemini-2.0-flash` to `gemini-3.1-pro-preview` (2M context). Affects `abstract --model gemini`, the `auto` fallback when `GOOGLE_API_KEY` is the only configured key, and `Gemini::new()` / `Gemini::builder()` when `.model(...)` is omitted.
- **`abstract login <provider>`** now accepts any provider registered in the `cersei-provider` registry (Google, Groq, DeepSeek, xAI, Mistral, Together, Fireworks, Perplexity, Cerebras, OpenRouter, Cohere, SambaNova, …), not just `anthropic` and `openai`. Saved keys are stored in a generic `provider_keys` map in `~/.abstract/credentials.json` and exported as the provider's first env var at startup so downstream registry lookups see them transparently. Local providers (Ollama) report "no login needed".

### Fixed

- **Auto-default no longer silently picks Ollama.** Two changes: (1) `cersei_provider::registry::available()` now TCP-probes local providers (those with no `env_keys`) via a 200ms check against their `api_base`, so `abstract login status` distinguishes `available (local)` from `not running`. (2) `from_model_string("auto")` now skips local providers entirely and only considers keyed providers (Anthropic, OpenAI, Google, Groq, …). Ollama and other local providers must be selected explicitly via `--model ollama/<model>`. When no API keys are set, the CLI errors out with a helpful message instead of silently defaulting to `llama3.1`.
- **`abstract login google`** (and every other provider known to the registry) is no longer rejected as "Unknown provider".

## [0.1.6-patch.1] - 2026-04-13

### Added

- **VibeProxy support.** Abstract CLI can now route requests through VibeProxy or any compatible local proxy, enabling use of existing AI subscriptions (Claude Pro/Max, ChatGPT Plus) instead of API keys.
  - **`--proxy` CLI flag** — force proxy usage even when API keys are set.
  - **`--proxy-url URL` flag** — specify a custom proxy URL (default: `http://localhost:8317/v1`).
  - **Auto-detection** — when no API keys are set and VibeProxy is running on `localhost:8317`, Abstract automatically routes through the proxy.
  - **`/proxy` slash command** — shows proxy status, URL, and authenticated accounts from `~/.cli-proxy-api/`.
  - **`[proxy]` config section** in `.abstract/config.toml` — `enabled`, `force`, `url` fields.
  - Header shows `model via proxy` when proxy is active.
- **Channel-based TUI permission system.** Permissions now flow through `tokio::sync::mpsc` + `oneshot` channels instead of reading from stdin directly. Fixes permission overlay freezing the TUI.
- **Virtualized message list.** `VirtualList` renders only visible items (O(viewport_height) per frame instead of O(total_lines)). Pre-built committed items are cached; only streaming content is rebuilt per frame. Buffer cleared before render to prevent stale content bleed-through.
- **Inline diff viewer.** File edit tools (Edit, Write, ApplyPatch) now show syntax-highlighted unified diffs inline in the conversation with `┌─ diff` / `│ +/-` / `└─` borders.
- **Edit tool captures diffs.** `file_edit.rs` reads file before and after edit, computes unified diff, includes it in the tool result.
- **Multi-line input.** Textarea with word wrapping, dynamic height (1-10 lines), and newline insertion via Option+Enter (macOS), Ctrl+J, or Shift+Enter (kitty protocol terminals).
- **Kitty keyboard protocol.** `PushKeyboardEnhancementFlags(DISAMBIGUATE_ESCAPE_CODES)` enabled at startup for terminals that support it. Shift+Enter works in iTerm2, Kitty, WezTerm, Alacritty.
- **4 new cookbook pages** — ML Coding Agent, Research Agent, General Agent (memory + skills + MCP), Graph Memory Deep Dive.
- **Comparisons page** — Cersei vs Claude Code SDK, Cersei vs Pydantic AI / LangChain with feature matrix.
- **Code & AST Intelligence docs** — full API breakdown for `cersei-lsp` and tree-sitter modules.

### Changed

- **Permission system rewritten** from stdin-based to channel-based for TUI mode. `TuiPermissionPolicy` sends requests via `mpsc`, TUI renders overlay, user decision sent back via `oneshot`. No more stdin race condition.
- Permission overlay enlarged to 75% x 55% with padding and `Wrap { trim: true }`.
- Recovery overlay also enlarged.
- Help overlay updated with all 17+ commands, keybindings, and permission modes.
- File tree in side panel now shows compact directory view with `▸ dir/ (count)` format instead of fully expanded tree.
- Git diff panel shows `git status --short` with human-readable labels (untracked/modified/added/deleted) instead of raw `??`/`M`/`A` codes.
- Side panel supports focus mode: Ctrl+B focuses panel, j/k scroll, Tab switches tabs, Esc returns to input. Yellow border when focused.
- Tool call output preview: file tools get 12 lines (up from 5), diff rendering for Edit/Write/ApplyPatch.

### Fixed

- **TUI permission freeze** — permission overlay no longer blocks input. Root cause: old `CliPermissionPolicy` called `crossterm::event::read()` and `enable_raw_mode()`/`disable_raw_mode()` while TUI was already in raw mode, causing stdin race condition and raw mode corruption.
- **Stale content in scrolled messages** — VirtualList now clears buffer area with `cell.reset()` before rendering visible items.
- **Resize crash in Ghostty** — kitty keyboard protocol only enabled if `supports_keyboard_enhancement()` returns true. Event drain capped at 50 per tick. Protocol re-pushed on resize.
- **Cost display $0** — CostUpdate and TurnComplete handlers now estimate cost from model pricing when provider reports $0.
- `/memory`, `/sessions`, `/model` no longer return "unknown command".

## [0.1.6] - 2026-04-12

### Added

#### New Crate: `cersei-lsp`
- **Language Server Protocol client** (`cersei-lsp`). On-demand LSP server management with JSON-RPC 2.0 over stdio. 5 operations: hover, definition, references, document symbols, diagnostics. Built-in configs for 13 language servers (rust-analyzer, pyright, typescript-language-server, gopls, clangd, ruby-lsp, phpactor, lua-language-server, bash-language-server, sourcekit-lsp, omnisharp, jdtls, zls). Auto-detection by file extension, lazy server startup.
- **LSP tool** in `cersei-tools` — agents can query language servers via `LSP` tool with action/file/line/column.

#### Tree-sitter Code Intelligence
- **Multi-language tree-sitter parsing** (Rust, TypeScript/JavaScript, Python, Go). Extracts imports and symbols (functions, structs, classes, interfaces, enums, modules, types).
- **Bash command safety analysis** (`bash_safety`) — tree-sitter AST-based risk classification (Safe/Moderate/High/Forbidden). Detects command substitution, process substitution, redirections, pipelines, privilege escalation. 8 tests.
- **Dependency-ranked project intelligence** (`code_intel`) — `scan_project()` discovers source files, parses with tree-sitter, scores by importance (entry points, import frequency, symbol count). Injected into system prompt as `project_intel`.

#### Production TUI
- **ratatui TUI** with alternate screen, `tokio::select!` event loop at 62 FPS.
- **Side panel** (Ctrl+B) — 38% right width, tabbed: Git Diff (status + colored diff) and File Tree (compact with file counts).
- **Permission modes** (Shift+Tab) — Auto, Plan, Editor, Bypass, Bypass+Alert.
- **Enterprise theme** — AMOLED black (`#000`), monochromatic, `#ffff00` accent, derived from Zed Enterprise theme. Default theme.
- **3 themes** — Enterprise, Light, Solarized.
- **Markdown rendering** with pulldown-cmark + syntect. Text wrapping at terminal width.
- **Graph visualization** (`/graph`) — memory node graph overlay.
- **16 slash commands** — `/help`, `/clear`, `/cost`, `/model`, `/memory`, `/sessions`, `/diff`, `/files`, `/panel`, `/graph`, `/undo`, `/rewind`, `/compact`, `/exit`.
- **Scrolling** — PageUp/Down, Home/End, Ctrl+Up/Down for side panel.
- **Bracketed paste** support. Native text selection (mouse capture disabled).

#### Agent Loop
- **Parallel tool execution** via `futures::future::join_all()`.
- **Automatic retry with exponential backoff** — 5 retries on 429/529, 1s→16s delays + jitter.
- **LLM-based context compaction** — provider-based summarization at 90% context, snip-compact fallback.
- **Todo nudge injection** — reminds model about incomplete tasks on turns > 2.
- **Depth nudge** — forces deeper exploration on early EndTurn after tool calls.
- **MaxTokens recovery** — 3 retries on output token limit.

#### File Operations
- **File snapshot/undo** (`file_snapshot`) — before/after content per tool call, `/undo` command.
- **ApplyPatch tool** — unified diff format patching.
- **Shell state persistence** — sentinel-based cwd capture across bash calls.

#### Provider Updates
- **GPT-5.x models** — `gpt-5.3-chat-latest` (default), `gpt-5.3-chat`, `gpt-5-chat`, `o3-pro`. 1M context windows.
- **`max_completion_tokens`** for GPT-5.x/o-series. **`stream_options: include_usage`** for token tracking.
- **Per-message cost estimation** — `estimate_cost()` with pricing for 15 models.

#### Memory & Config
- **AGENTS.md / CLAUDE.md hierarchy** — walks up directory tree collecting instruction files.
- **File watching** (`file_watcher`) — `notify` crate for project file change detection.
- **max_turns** increased from 20 to 50.

### Changed

- Default OpenAI model: `gpt-4o` → `gpt-5.3-chat-latest`.
- Default theme: generic dark → Enterprise (AMOLED black).
- `Agent::run_stream()` takes `Arc<Self>` instead of unsafe pointer cast.
- System prompt rewritten for deep exploration enforcement.
- Gemini tool results use actual function name instead of `tool_use_id`.
- Glob capped at 200 results. Per-result cap at 30KB.

### Fixed

- TUI streaming via `tokio::select!` (no longer blocks event loop).
- Mid-stream cancellation via `cancel_token`.
- OpenAI `max_tokens` → `max_completion_tokens` for GPT-5.x.
- Token stats and cost display.
- Git diff panel shows untracked files with readable labels.
- Markdown text wrapping. Paste handling. `cap_tool_result` bounds.

## [0.1.5] - 2026-04-07

### Added

- **`/sessions` and `/ls` slash commands** — list all sessions directly from the REPL. Addresses [#9](https://github.com/pacifio/cersei/issues/9).
- **Expanded `/help` output** — now shows CLI subcommands (`abstract sessions list`, `abstract --resume`, `abstract login status`) alongside REPL slash commands. Model aliases updated to include `4o`, `gemini`, `llama`.
- **Conditional system prompt components.** System prompt refactored from 6 static sections to 23 components (8 conditional). New sections: output efficiency, tool result summarization, sub-agent guidance (when Agent tool available), skills guidance (when Skill tool available), memory guidance (when memory configured), context management warning (when auto-compact on), git status snapshot (structured: branch, user, status lines, recent commits), MCP server instructions, language preference.
- **`GitSnapshot` struct** for structured git context in the system prompt (branch, user, status lines, recent commits).
- **New `SystemPromptOptions` fields** — `tools_available`, `has_memory`, `has_auto_compact`, `git_status`, `mcp_instructions`, `language`.
- 11 new system prompt tests covering all conditional components.

### Changed

- Abstract CLI `prompt.rs` now populates `tools_available`, `has_memory`, `has_auto_compact`, and `git_status` from config and the working directory. Git info is a structured `GitSnapshot` instead of a one-line string.
- System prompt includes output efficiency and tool result summarization by default (previously missing).

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
