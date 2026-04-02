# Architecture

## Crate Map

```
src-cersei/
├── Cargo.toml                    workspace manifest
├── README.md                     project README with benchmark results
├── docs/                         this documentation
├── examples/
│   └── benchmark/                standalone benchmark binary
│       ├── Cargo.toml
│       ├── src/main.rs
│       └── README.md
└── crates/
    ├── cersei/                   facade — re-exports everything
    │   ├── src/lib.rs            prelude, convenience re-exports
    │   └── examples/             8 runnable examples
    │
    ├── cersei-types/             provider-agnostic types
    │   └── src/lib.rs            Message, ContentBlock, Usage, CerseiError, StreamEvent
    │
    ├── cersei-provider/          LLM provider abstraction
    │   └── src/
    │       ├── lib.rs            Provider trait, Auth, CompletionRequest/Stream
    │       ├── stream.rs         StreamAccumulator
    │       ├── anthropic.rs      Anthropic provider (SSE streaming, OAuth)
    │       └── openai.rs         OpenAI-compatible provider
    │
    ├── cersei-tools/             tool system
    │   └── src/
    │       ├── lib.rs            Tool trait, ToolContext, ToolResult, built-in sets
    │       ├── permissions.rs    PermissionPolicy trait + built-in policies
    │       ├── bash.rs           Bash tool (shell state persistence)
    │       ├── file_read.rs      Read tool
    │       ├── file_write.rs     Write tool
    │       ├── file_edit.rs      Edit tool (string replacement)
    │       ├── glob_tool.rs      Glob tool
    │       └── grep_tool.rs      Grep tool (rg/grep)
    │
    ├── cersei-tools-derive/      proc-macro crate
    │   └── src/lib.rs            #[derive(Tool)] implementation
    │
    ├── cersei-agent/             agent builder and runtime
    │   └── src/
    │       ├── lib.rs            Agent, AgentBuilder, AgentOutput
    │       ├── events.rs         AgentEvent (26 variants), AgentStream, AgentControl
    │       ├── reporters.rs      Reporter trait + Console/Json/Collector/Metrics
    │       └── runner.rs         Agentic loop implementation
    │
    ├── cersei-memory/            session persistence
    │   └── src/lib.rs            Memory trait, JsonlMemory, InMemory
    │
    ├── cersei-hooks/             middleware system
    │   └── src/lib.rs            Hook trait, ShellHook, run_hooks()
    │
    └── cersei-mcp/               Model Context Protocol
        └── src/lib.rs            McpManager, McpServerConfig
```

## Dependency Flow

```
cersei-types          zero LLM-specific concepts
    ↑                 Message, ContentBlock, Usage, CerseiError, StreamEvent
    │
cersei-provider       abstracts LLM communication
    ↑                 Provider trait, Auth, CompletionRequest/Stream
    │
cersei-tools          what agents can do
    ↑                 Tool trait, ToolContext, permissions, built-in tools
    │
cersei-mcp            external tool servers
    ↑                 McpManager, McpServerConfig
    │
cersei-hooks          lifecycle interception
    ↑                 Hook, HookEvent, HookAction, ShellHook
    │
cersei-memory         conversation persistence
    ↑                 Memory trait, JsonlMemory, InMemory
    │
cersei-agent          the runtime
    ↑                 Agent, AgentBuilder, agentic loop, events, reporters
    │
cersei                facade + prelude
```

Each layer depends only on the layers below it. The `cersei` facade crate re-exports everything for convenience.

## Data Flow

```
User prompt
    │
    ▼
Agent::run("prompt")
    │
    ├── Load session from Memory (if configured)
    │
    ▼
┌─ AGENTIC LOOP ──────────────────────────────────┐
│                                                   │
│  Build CompletionRequest                          │
│    │                                              │
│    ▼                                              │
│  Provider.complete(request) → CompletionStream    │
│    │                                              │
│    ▼                                              │
│  StreamAccumulator collects events                │
│    │  ├── TextDelta → emit to listeners           │
│    │  ├── ThinkingDelta → emit                    │
│    │  └── ToolUse → queue for dispatch            │
│    │                                              │
│    ▼                                              │
│  CompletionResponse                               │
│    │                                              │
│    ├── PostModelTurn hooks                        │
│    │                                              │
│    ▼                                              │
│  Stop reason?                                     │
│    ├── EndTurn → BREAK                            │
│    ├── ToolUse → dispatch tools ──┐               │
│    │                              │               │
│    │   For each tool:             │               │
│    │     PreToolUse hooks         │               │
│    │     PermissionPolicy check   │               │
│    │     Tool.execute()           │               │
│    │     PostToolUse hooks        │               │
│    │     Record ToolCallRecord    │               │
│    │                              │               │
│    │   Append results ────────────┘               │
│    │   CONTINUE loop                              │
│    │                                              │
│    └── MaxTokens → inject continuation, CONTINUE  │
│                                                   │
└───────────────────────────────────────────────────┘
    │
    ├── Save session to Memory (if configured)
    │
    ▼
AgentOutput { message, usage, stop_reason, turns, tool_calls }
```

## Event Distribution

```
Agent loop emits AgentEvent
    │
    ├── on_event callback (if set)     — synchronous, in-loop
    │
    ├── broadcast channel (if enabled) — async, multi-consumer
    │   ├── subscriber 1
    │   ├── subscriber 2
    │   └── subscriber N
    │
    └── reporters (Vec<Arc<dyn Reporter>>)
        ├── ConsoleReporter
        ├── JsonReporter
        └── MetricsReporter
```

## Design Principles

1. **Trait-based extensibility** — every major component is a trait (`Provider`, `Tool`, `Memory`, `Hook`, `PermissionPolicy`, `Reporter`). Swap any component by implementing the trait.

2. **Builder pattern** — `AgentBuilder` with `self -> Self` chaining. Validates at `build()` time, not at method call time.

3. **No global state** — except `ShellState` registry (keyed by session_id, cleaned up on session end). All configuration flows through the builder.

4. **Streaming-first** — providers emit `StreamEvent`s. The accumulator is a separate concern. Consumers (callback, broadcast, stream) get events as they arrive.

5. **Headless-first** — no TUI dependency in the SDK. The TUI is an application-layer concern that consumes Cersei via events.

6. **Zero-cost defaults** — `AllowAll` permission policy, no hooks, no memory, no broadcast. You only pay for what you configure.
