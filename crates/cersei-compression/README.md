# cersei-compression

Structural and command-aware compression for tool outputs in the Cersei SDK.

Sits between a tool's raw `execute()` result and the agent's `cap_tool_result()`
truncation, trimming the 60–90% of tokens in typical tool output that is
comments, ANSI, blank lines, noisy progress messages, or unchanged boilerplate.

## Levels

- `Off`        — passthrough (default). Zero behavior change.
- `Minimal`    — strip ANSI, collapse whitespace, drop comments (code files only).
- `Aggressive` — Minimal plus language-aware stubbing of function bodies, and
                 command-specific TOML rules for common CLIs.

## Dispatch

```
tool_name       path / input hint           filter
─────────────   ─────────────────────────   ──────────────────────
"Bash", "Exec"  first word of .command      toml_rules::apply
"Read", …       file extension of .path     code::filter
"Grep", "Glob"  —                           passthrough
other           —                           passthrough
```

All stages are infallible: on any internal error the raw input is returned
unchanged, so the agent loop never breaks.

## Credits

This crate is a port / adaptation of [**rtk** (Rust Token Killer)](https://github.com/rtk-ai/rtk)
by **Patrick Szymkowiak**, MIT licensed. See [`LICENSE`](LICENSE) for full
attribution.

| cersei-compression module | rtk source                       |
| ------------------------- | -------------------------------- |
| `src/ansi.rs`             | `rtk/src/core/utils.rs`          |
| `src/code.rs`             | `rtk/src/core/filter.rs`         |
| `src/truncate.rs`         | `rtk/src/core/filter.rs`         |
| `src/toml_rules.rs`       | `rtk/src/core/toml_filter.rs`    |
| `src/rules/*.toml`        | `rtk/src/filters/*.toml`         |
