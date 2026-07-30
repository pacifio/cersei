# Cersei tool-calling reliability: multi-tier audit

**Scope:** tool schema out → parsed tool result back, across all four provider paths.
**Tree:** branch `graphify-current-project`, HEAD `2c13628`, workspace v0.2.6.
**Date:** 2026-07-28. Every line cite below was opened during this audit; stale anchors from
prior reviews were re-derived, not carried forward.

> **STATUS UPDATE — 2026-07-30.** All 15 P0 items are implemented (uncommitted, on branch
> `graphify-current-project`). Gate: `cargo build --workspace` clean, **509 passed /
> 0 failed / 14 ignored**. F-01's Half B — the manual-thinking 400 that §H3 marked
> `Unverified` — is now **Confirmed from primary docs** (§10.2). A full P0 **mutation
> audit** was then run: each fix was reverted one at a time to check whether any test
> notices. **15 of 22 fix sites are test-bound; 7 revert green.** The audit, the resulting
> fix backlog, and the still-open items are in **§10** at the end of this file. Roadmap
> tick marks are in §8. Line numbers cited in §2–§9 predate the fixes and may have drifted.
>
> **SECOND FOLLOW-UP — 2026-07-30, branch `runtime-fix`.** The §10.4 backlog is closed:
> 13 new tests, no production-code changes. All 7 formerly-unbound sites were re-mutated
> and every mutant is now killed — **22/22 fix sites test-bound**. Gate:
> `cargo test --workspace` **522 / 0 / 14**. Details in §10.7.

---

## 1. Executive summary

**Root cause, one sentence:** Cersei has no seam between "what the agent wants to say" and
"what this particular model/provider can actually accept" — so every provider gets
byte-identical tool schemas, every model gets a byte-identical prompt, and every failure is
reported to the model as an undiagnosable string, or not reported at all.

**Top 3 findings:**

1. **The error message Cersei returns halves weak-model recovery — measured.** A malformed
   call becomes `Value::Null` (`stream.rs:85`), which the model sees as
   `Invalid input: invalid type: null, expected struct Input` — a Rust type name, no field, no
   schema. Two 7-8B models, same bad call, n=12: **50% recovery with Cersei's message, 100%
   with a message that echoes the args and the schema** (§7.0). Under Cersei's message
   llama-3.1-8b emitted *malformed JSON on 4 of 6 attempts* — the bad error actively degrades
   output. The fix is ~20 lines and needs no abstraction. (F-05)
2. **Failures are systematically laundered into "success."** Four independent paths report a
   broken turn as a clean `StopReason::EndTurn`: stream EOF without `[DONE]`
   (`openai.rs:338`), swallowed `StreamEvent::Error` (`stream.rs:118`), absent stop_reason
   (`stream.rs:142`), and Gemini's `MALFORMED_FUNCTION_CALL` (`gemini.rs:488`). The agent loop
   exits normally and returns a confident, empty answer. (F-03, F-09)
3. **The runtime can neither retry nor help the model recover.** `ProviderStatus` and
   `RateLimit` have **zero construction sites**, so `is_retryable()` never returns true and one
   429 ends the session (F-02). When a call fails, the model is told
   `Invalid input: invalid type: null, expected struct Input` — a Rust type name, no field, no
   schema (F-05) — under a "N attempts remaining" warning that is a **bluff**, since
   `MAX_TOOL_ERRORS_PER_TOOL` is never enforced (F-06). And the one guard that does fire
   runs *after* the write lands, so complying with its advice destroys the file (F-11).

**Architectural recommendation: do NOT build `ModelProfile`.** Build a ~150-line
**tool-serialization seam** at the three sites where `ToolDefinition` becomes provider JSON,
plus a 4-field `ProviderQuirks` for the incompatibilities that are genuinely forced by
provider APIs. Rationale in §6 — the decisive argument is empirical: Cersei *already has* a
per-model capability table, and it is 100% dead code (F-23). A second, larger table is
unlikely to fare better.

**Expected gain and confidence:** The P0 list is 9 bugs, mostly `S` effort, that convert
silent total failures into either working calls or loud errors.

Seven experiments were run (§7.0, total spend <$0.01). What they settled:

| Claim | Result |
|---|---|
| F-05 error-message quality | **50% → 100%** weak-model recovery |
| F-04 compaction orphans | **50%** of lengths; proposed fix → **0/30** |
| F-03 / F-A2 / F-A3 / H2c | all four **reproduced against the real provider** |
| Gemini rejects schemars output | **confirmed**, and the `adapt_tools` spec corrected |
| F-10 trigger rate, F-08 fix | **not established** — two harness errors of mine |

So: confidence in the *bugs* is high — each has a line cite, and the highest-severity ones now
have a reproduction. Confidence in the *aggregate magnitude of gain* remains low, because no
end-to-end task-success benchmark was run and no usable baseline exists to run one against
(§7.1). Two P0 items (F-01, F-11) are argued but not reproduced.

**On the brief's thesis (§1.3):** partly confirmed, with an important correction. The runtime
does amplify weak tool-calling. But the largest defects found are **not tier-differential** —
F-01 through F-04 break every tier equally. They have survived because the one configuration
that mostly works (direct Anthropic + frontier + short sessions) is the development path. The
thesis under-predicted the damage by looking only for tier-dependent bugs.

---

## 2. How tool calling flows through Cersei today

### 2.1 The path, per provider

| Stage | Anthropic | Anthropic/Vertex | OpenAI (+ **Ollama, all OpenAI-compat**) | Gemini |
|---|---|---|---|---|
| Tool set built | `cersei-tools/src/lib.rs:266-357` — identical for all | ← | ← | ← |
| Schema emitted | `input_schema()` per tool, hand-written `json!` | ← | ← | ← |
| **Schema adapted** | **none** | **none** | **none** | **none** |
| Serialized | `anthropic.rs:181` `"input_schema"` | shared fn | `openai.rs:270` `"parameters"` | `gemini.rs:272` `"parameters"` |
| System prompt | `system_prompt.rs:170-327` — **no model/provider branch** | ← | ← | ← |
| Request extras | `cache_control` on last tool (`anthropic.rs:185`); `thinking` (`:197`) | same, no beta header | `reasoning_effort` (`:256`) | none |
| Stream decode | `anthropic.rs:292-306` → `StreamAccumulator` | ← | `openai.rs:328-499` → accumulator | `gemini.rs:380-490` |
| Tool args parsed | `stream.rs:85` `unwrap_or(Value::Null)` | ← | ← | ← |
| Dispatch | `runner.rs:650` name lookup → `:692` `execute(raw_value)` — **no validation** | ← | ← | ← |
| Result → message | `runner.rs:793-797` `ToolResult` block | ← | `openai.rs:94-104` (drops `is_error`) | `gemini.rs:160` `functionResponse` |

### 2.2 The divergences that matter

Only **four** things differ across providers, and none of them is schema- or
capability-aware:

1. The JSON key name for the schema (`input_schema` vs `parameters`).
2. Anthropic adds `cache_control` + `thinking`; OpenAI adds `reasoning_effort`.
3. Gemini alone destructures `ToolResultContent::Blocks` (`gemini.rs:148`); OpenAI does not,
   which is a latent 400 the moment any tool returns an image (F-A6).
4. Stop-reason mapping, which is wrong in a different way on each path.

**The single most important structural fact in this document:** there are exactly three
tool-serialization sites, and all three interpolate the identical `t.input_schema` value with
zero transformation. Verified by exhaustive grep — `$ref`, `oneOf`, `anyOf`, `allOf`,
`definitions`, `$defs`, `additionalProperties` return **zero hits across every `.rs` file in
the workspace**. No code in Cersei knows those keywords exist. That absence is both the
central defect and, conveniently, the natural place to fix it (§6).

### 2.3 Measured tool-surface cost

Measured with a standalone probe (`joy/schema-probe`, path-dep on `cersei-tools`, cl100k
tokenizer; the repo tree was not modified):

| set | tools | bytes (JSON) | cl100k tokens |
|---|---|---|---|
| `coding()` | 14 | 7,224 | **1,566** |
| `all()` | 34 | 11,520 | **2,544** |

Most expensive tools in `coding()`: `ExaSearch` 349 tok (11 params), `MultiEdit` 191,
`Grep` 158, `CodeSearch` 125. Cheapest: `Write` 57, `Bash` 62, `Read` 65.

Schema-hazard scan across all 34 tools: **zero** occurrences of `$ref`, `oneOf`, `anyOf`,
`definitions`, `$defs`, `$schema`, or `additionalProperties`. The shipped surface is clean
flat JSON Schema. This **refutes** the schemars-0.8 half of H8 for shipped tools (see §4).

---

## 3. Findings

Ordered by (weak-tier impact) ÷ (blast radius + effort). Nine full findings; the remainder is
in Appendix A so this section stays actionable (§8.6).

---

### F-01 · Vertex's default model rejects every request Cersei sends

```yaml
id: F-01
taxonomy: [T1]
locus: runtime-side
confidence: Confirmed
tiers_affected: [low, mid, frontier]
severity: P0
effort: S
evidence:
  - crates/cersei-provider/src/anthropic.rs:197-199
  - crates/cersei-provider/src/registry.rs:190
  - crates/abstract-cli/src/app.rs:283-284
  - crates/cersei-agent/src/lib.rs:253
  - "https://platform.claude.com/docs/en/build-with-claude/extended-thinking (fetched this session)"
falsifier: "If Anthropic still accepted thinking.type:'enabled' on Opus 4.8, or if the Vertex
            default were a 4.5/4.6-era model, this would be a deprecation note, not a bug."
refutation_attempted: yes
```

`anthropic.rs:197-199` emits the manual-thinking form:

```rust
if let Some(budget) = thinking_budget {
    body["thinking"] = serde_json::json!({ "type": "enabled", "budget_tokens": budget });
}
```

Anthropic's current docs state plainly: *"Claude 4.7 and later models do not support it and
reject requests that use it, returning a 400 error"*, listing **Claude Opus 4.8** explicitly.
`registry.rs:190` sets the Vertex provider's `default_model: "claude-opus-4-8"`, and
`abstract-cli/src/app.rs:283-284` calls `builder.thinking_budget(budget)` **unconditionally**
for every effort level — there is no capability gate anywhere on this path.

**I went looking for the refutation and found a partial one, which sharpens the finding.**
Direct Anthropic's default is `claude-sonnet-4-6` (`registry.rs:163`), where the docs say the
deprecated form *"still succeed[s]"*. And the SDK defaults `thinking_budget: None`
(`cersei-agent/src/lib.rs:253`). So the blast radius is **Vertex CLI users today**, and
**every Anthropic CLI user the moment that default is bumped to 4.7+**.

**Failure scenario** — `abstract --provider anthropic-vertex` with any prompt → HTTP 400 →
`CerseiError::Provider` → not retryable (F-02) → session dies before turn 1. Zero tool calls
ever execute.

**Impact by tier** — low/mid/frontier: identical, total. This is not a tier bug.

**Fix sketch** — gate on model generation and migrate to adaptive thinking:

```rust
// resolve once, at router time
match thinking_mode_for(model) {
    ThinkingMode::Adaptive => {
        body["thinking"] = json!({"type": "adaptive"});
        body["output_config"] = json!({"effort": effort_str});
    }
    ThinkingMode::Manual(budget) => {
        // budget MUST be < max_tokens (docs: "Less than max_tokens")
        body["thinking"] = json!({"type":"enabled","budget_tokens": budget.min(max_tokens - 1)});
    }
    ThinkingMode::Unsupported => {}
}
```

The same gate fixes the `--effort max` bug: `effort.rs:26` requests a 32768 budget while
`max_tokens` defaults to 16384 (`cersei-provider/src/lib.rs:146`), violating the documented
*"Less than `max_tokens`"* rule. Of four effort levels, Medium and High are the only two that
produce a valid Anthropic request.

---

### F-02 · The retry ladder is unreachable; one 429 kills the session

```yaml
id: F-02
taxonomy: [T6]
locus: runtime-side
confidence: Confirmed
tiers_affected: [low, mid, frontier]
severity: P0
effort: S
evidence:
  - crates/cersei-types/src/lib.rs:382-389
  - crates/cersei-types/src/lib.rs:342,354
  - crates/cersei-provider/src/openai.rs:301-308
  - "rg -n 'ProviderStatus|RateLimit' crates/ → 5 hits, all declarations or match arms, zero constructions"
falsifier: "A single `CerseiError::ProviderStatus { .. }` or `RateLimit { .. }` constructed
            anywhere in the workspace would disprove this."
refutation_attempted: yes
```

```rust
// crates/cersei-types/src/lib.rs:382-389
pub fn is_retryable(&self) -> bool {
    matches!(
        self,
        CerseiError::RateLimit { .. }
            | CerseiError::ProviderStatus { status: 429, .. }
            | CerseiError::ProviderStatus { status: 529, .. }
    )
}
```

I grepped the whole workspace for both variants. Five hits: two enum declarations
(`:342`, `:354`) and three match arms (`:385-387`). **Neither variant is ever constructed.**

Compounding it, `openai.rs:301-308` turns HTTP status failures into a *stream event* rather
than a typed error, so `complete()` returns `Ok(stream)` even for a 429 — the backoff loop
never observes it — and the status is then stringified into `CerseiError::Provider(String)`,
which `is_retryable()` does not match.

**Failure scenario** — a 429 during a 40-minute agent run. Expected: back off, retry, continue.
Actual: `runner.rs` returns `Err` and the session ends, losing all context.

**Impact by tier** — low: worst, because free/local endpoints rate-limit most aggressively.
mid/frontier: any transient blip is fatal.

**Fix sketch** — construct the typed variant at the HTTP boundary in all four providers, and
route status failures through the `Result` rather than the stream:

```rust
if !response.status().is_success() {
    return Err(CerseiError::ProviderStatus { status, message: body });
}
```

---

### F-03 · Stream EOF without `[DONE]` silently discards every tool call and reports success

```yaml
id: F-03
taxonomy: [T1, T5]
locus: runtime-side
confidence: Confirmed
tiers_affected: [low, mid, frontier]
severity: P0
effort: S
evidence:
  - crates/cersei-provider/src/openai.rs:338-340
  - crates/cersei-provider/src/stream.rs:118
  - crates/cersei-provider/src/stream.rs:142
falsifier: "A flush after the read loop, or a StopReason of anything but EndTurn on an
            unterminated stream, would disprove it."
refutation_attempted: yes — I read to the end of the spawn fn; nothing follows the loop.
```

The accumulated tool-call map is drained in exactly one place — inside `if data == "[DONE]"`
(`openai.rs:338`) — and nothing follows the `while let Some(chunk)` loop. Three separate
laundering steps then convert the loss into apparent success:

```rust
// stream.rs:118 — mid-stream provider errors are discarded
StreamEvent::Error { .. } => {}

// stream.rs:142 — no stop_reason ever arrived → report a clean turn
stop_reason: self.stop_reason.unwrap_or(StopReason::EndTurn),
```

**Failure scenario** — a local llama.cpp server closes the SSE body after emitting three
valid tool calls but before `[DONE]`. All three are dropped. The accumulator reports
`EndTurn`. `runner.rs` treats it as the model finishing, breaks the loop, and returns whatever
prose preceded the calls. No error, no log, no retry.

**Impact by tier** — low: high frequency; this path serves Ollama, llama.cpp, LiteLLM and every
OpenAI-compat shim, which are exactly the servers most likely to close untidily. mid: occasional.
frontier: rare, but a dropped connection produces the same silent wrong answer.

**Fix sketch** — flush after the loop, and treat an unterminated stream as an error:

```rust
} // end read loop
if !tool_calls.is_empty() { emit_accumulated(&tool_calls, &tx).await; }
if !saw_done {
    let _ = tx.send(StreamEvent::Error { message: "stream ended without [DONE]".into() }).await;
}
```
…and make `StreamEvent::Error` actually terminate the accumulator rather than being ignored.

---

### F-04 · Compaction orphans tool_use/tool_result pairs on ~half of all cuts

```yaml
id: F-04
taxonomy: [T4]
locus: runtime-side
confidence: Confirmed
tiers_affected: [low, mid, frontier]
severity: P0
effort: M
evidence:
  - crates/cersei-agent/src/compact.rs:14
  - crates/cersei-agent/src/runner.rs:919-922
  - crates/cersei-agent/src/compact.rs:214-222
  - crates/cersei-agent/src/runner.rs:212,433,804
  - "rg -niE 'orphan|dangling|unmatched|reconcil|pair.?integrity' crates/ → zero hits"
falsifier: "Any pair-integrity scan between the compaction slice and the next request would
            disprove it. I grepped for eight spellings of the concept and found none."
refutation_attempted: yes
```

```rust
// runner.rs:919-922
let split_idx = msgs.len().saturating_sub(compact::KEEP_RECENT_MESSAGES);
let recent = msgs[split_idx..].to_vec();
*msgs = vec![Message::user(&result.summary)];
msgs.extend(recent);
```

`KEEP_RECENT_MESSAGES = 10` (`compact.rs:14`) — an **even** number. The runner builds a
strictly alternating history: user prompt (`:212`), assistant tool_use (`:433`), user
tool_result (`:804`). Because the keep-count is even, `parity(split_idx) == parity(len)`, so
whenever the message count is even the slice begins on a tool_result whose matching
`tool_use` was just discarded.

There is no repair pass. I grepped for eight spellings of the concept across `crates/` and got
zero hits; the providers don't sanitize either (`anthropic.rs:147` filters only on role).

**Failure scenario** — a long session crosses the compaction threshold at an even message
count. Next request contains a `tool_result` referencing a `tool_use_id` that no longer
exists → Anthropic 400 `unexpected tool_use_id` / OpenAI 400 `tool message without preceding
tool_calls`. Not retryable (F-02) → session dies. **The compaction that was supposed to save
the session ends it.**

There is a second defect on the same lines: `*msgs = vec![Message::user(summary)]` followed by
`extend(recent)` produces **two consecutive user messages** when `split_idx` is even, which
Gemini and most local chat templates reject or silently merge.

**Impact by tier** — low: worst. Weak models take more turns per task and emit more failed
calls, so they reach compaction sooner and roll this coin more often. mid/frontier: same
mechanism, less frequently.

**Fix sketch** — walk backward from `split_idx` to the nearest safe boundary:

```rust
fn safe_split(msgs: &[Message], mut idx: usize) -> usize {
    // never begin on a message containing a ToolResult block
    while idx > 0 && starts_with_tool_result(&msgs[idx]) { idx -= 1; }
    idx
}
```
Add a debug-assert before every request that every `tool_result.tool_use_id` has a preceding
`tool_use` with that id. That assertion would have caught this, F-A6, and the empty-`tool_calls`
loop (F-A2).

---

### F-05 · Malformed tool JSON becomes `null`, and the model is told nothing it can act on

```yaml
id: F-05
taxonomy: [T3, T6]
locus: runtime-side
confidence: Confirmed
tiers_affected: [low, mid, frontier]
severity: P0
effort: S
evidence:
  - crates/cersei-provider/src/stream.rs:83-85
  - crates/cersei-tools/src/file_read.rs:44-47
  - crates/cersei-tools-derive/src/lib.rs:102-107
  - crates/cersei-agent/src/runner.rs:692
falsifier: "If the dispatch layer re-validated args against input_schema() before use, the
            null would surface as a typed schema error naming the bad field."
refutation_attempted: yes — searched for any validate/coerce/repair at the boundary; only two
                      per-tool coerce_input fns exist, both post-dispatch (F-A4).
```

```rust
// stream.rs:83-85
let json_str = self.partial_json.remove(&index).unwrap_or_default();
let input = serde_json::from_str(&json_str).unwrap_or(serde_json::Value::Null);
```

The parse error — position, unexpected token, truncation point — is discarded. So is the
original string. Note the second trigger: `unwrap_or_default()` yields `""` when a tool_use
block carried no deltas, and `from_str("")` also fails, so a **no-argument tool call becomes
`Null` rather than `{}`**.

Downstream, `runner.rs:692` hands the raw value to `execute()`, and the tool's hand-written
deserialization produces:

```rust
// file_read.rs:44-47 — the shape 34 of the 42 tools use
Err(e) => return ToolResult::error(format!("Invalid input: {}", e)),
```

The struct is literally named `Input`, so the model receives verbatim:

> `Invalid input: invalid type: null, expected struct Input`

No field name. No valid parameter list. No echo of what it sent. A Rust type name that
appears nowhere in its tool schema. Every tool has an `input_schema()` — it is never
referenced from any error path.

**Failure scenario** — a 7B model emits `{'file_path': '/x/y.rs'}` (single quotes). Parse
fails → `Null` → the message above → the model has no signal that its *quoting* was the
problem → it re-emits the same call.

**Impact by tier** — low: this is the dominant failure loop; weak models malform JSON often and
this guarantees they cannot recover. mid: occasional truncated args, same dead end.
frontier: rare, but a truncated stream produces the same undiagnosable error.

**Fix sketch** — carry both the raw text and the parse error forward:

```rust
Err(e) => ContentBlock::ToolUse {
    id, name,
    input: json!({ "__parse_error": e.to_string(), "__raw": json_str }),
},
```
and at the dispatch boundary, on any deserialization failure, return the tool's own schema:

```rust
ToolResult::error(format!(
    "Invalid arguments for '{name}': {e}\nYou sent: {raw}\nExpected schema: {}",
    serde_json::to_string(&tool.input_schema()).unwrap()
))
```
This is the single highest-leverage change in the document: it costs ~20 lines, needs no
abstraction, and converts the most common weak-tier failure from unrecoverable to recoverable.

---

### F-06 · The "N attempts remaining" warning is a bluff — the limit is never enforced

```yaml
id: F-06
taxonomy: [T6]
locus: runtime-side
confidence: Confirmed
tiers_affected: [low, mid]
severity: P0
effort: S
evidence:
  - crates/cersei-agent/src/runner.rs:231
  - crates/cersei-agent/src/runner.rs:738-748
  - "rg -n 'MAX_TOOL_ERRORS_PER_TOOL' crates/ → exactly 2 hits: the declaration and one saturating_sub"
falsifier: "Any comparison of the counter against the constant, anywhere."
refutation_attempted: yes — the grep is exhaustive; there are only two occurrences.
```

```rust
// runner.rs:741-745
let remaining = MAX_TOOL_ERRORS_PER_TOOL.saturating_sub(*count);
result.content = format!(
    "{}\n\n[Tool '{}' failed {} time(s). {} attempts remaining. Analyze the error and try a different approach.]",
    result.content, tool_name, count, remaining
);
```

The constant is declared at `:231` and used in exactly one arithmetic expression at `:741`.
It is never compared to anything. There is no `if *count >= MAX { break }`.

**Failure scenario** — a model fails `Edit` four times. `saturating_sub` floors at zero, so
from the 4th failure onward it is told **"0 attempts remaining"** on every subsequent call,
forever, while dispatch continues unchanged. A well-behaved model that believes the message
and gives up is penalized; one that ignores it burns turns to the `max_turns` cap (50 in the
CLI, `config.rs:59`).

**Impact by tier** — low: severe — weak models fail more and are more literal about system
text. mid: wasted turns. frontier: mostly ignores it, minor.

**Fix sketch** — either enforce it (`break` and report the tool as unavailable for the rest of
the turn) or stop claiming it. Enforcing is better; the message is already written.

---

### F-07 · Error tool results bypass truncation entirely

```yaml
id: F-07
taxonomy: [T5]
locus: runtime-side
confidence: Confirmed
tiers_affected: [low, mid, frontier]
severity: P0
effort: S
evidence:
  - crates/cersei-agent/src/runner.rs:752-764
  - crates/cersei-agent/src/runner.rs:33,38
  - crates/cersei-agent/src/runner.rs:114-115
falsifier: "A cap_tool_result call on the is_error branch."
refutation_attempted: yes — the branch returns result.content.clone() unmodified.
```

```rust
// runner.rs:752-764
let (capped_content, compression) = if result.is_error {
    (result.content.clone(), None)          // <-- neither compressed NOR capped
} else {
    ...
    (cap_tool_result(&compressed), Some(stats))
};
```

`cap_tool_result` is otherwise good — head+tail with an explicit omission marker and a
recovery hint, correctly handling UTF-8 boundaries (`runner.rs:38-74`). It simply is not
called on the path that needs it most. The only backstop, `apply_tool_result_budget`, runs at
the *top of the next turn* and explicitly skips the last 6 messages (`:114-115`).

**Failure scenario** — a failing `cargo build` emits 500KB of diagnostics. It enters history
verbatim, lands inside the 6-message protected zone, and is ~125k tokens — instantly over the
window for every model except gpt-5-class. It then triggers compaction, which runs the
pair-orphaning slice of F-04. The two defects compound into session death.

**Impact by tier** — low: worst, both because weak models fail tools more often and because
their windows are smallest. frontier: survivable but expensive.

**Fix sketch** — one-line: apply `cap_tool_result` on both branches. Skip compression for
errors if desired; do not skip capping.

---

### F-08 · A prose-only turn ends the run with no nudge — the top weak-model failure is unhandled

```yaml
id: F-08
taxonomy: [T1]
locus: runtime-side
confidence: Confirmed
tiers_affected: [low, mid]
severity: P1
effort: S
evidence:
  - crates/cersei-agent/src/runner.rs:220,594-601,605
  - crates/cersei-agent/src/runner.rs:490,530
  - crates/cersei-agent/src/system_prompt.rs:362
falsifier: "Any non-benchmark-gated retry on an EndTurn with zero tool calls."
refutation_attempted: yes — all three nudges are gated on benchmark_mode or had_tool_use.
```

```rust
// runner.rs:594-601
if had_tool_use && turn <= 4 && !depth_nudge_sent {
    ...
    continue; // Don't break — force another round
}
break;
```

`had_tool_use` is set only inside the `ToolUse` arm (`:605`). On a turn-1 prose answer it is
`false`, so the guard fails and control falls to `break`. The other two retry paths (`:490`,
`:530`) are gated on `agent.benchmark_mode`.

**Failure scenario** — a 7B model answers "The authentication logic is in `src/auth.rs` and
uses JWT" from parametric knowledge, touching zero files. The runtime accepts it and returns
it as the final answer. Meanwhile `system_prompt.rs:362` instructs *"ALWAYS verify information
about the codebase using tools before answering"* — a rule the runtime never enforces.

**Impact by tier** — low: this is *the* characteristic weak-model failure and it is completely
unhandled outside benchmark mode. mid: occasional. frontier: rare.

**Fix sketch** — ungate the nudge for the zero-tool-call case, once per session:

```rust
if !had_tool_use && turn == 1 && !no_tool_nudge_sent { /* nudge + continue */ }
```
Better, where the provider supports it: `tool_choice: "required"` / Gemini
`functionCallingConfig.mode: "ANY"` on the first turn. Neither is currently sent by any
provider (F-A1).

---

### F-09 · Ollama gets a 200k context budget against a real 4k window

```yaml
id: F-09
taxonomy: [T1, T3]
locus: runtime-side
confidence: Confirmed (mechanism) / Likely (truncation consequence)
tiers_affected: [low]
severity: P1
effort: M
evidence:
  - crates/cersei-agent/src/compact.rs:98
  - crates/cersei-agent/src/runner.rs:874,902
  - crates/cersei-provider/src/registry.rs:116
  - crates/cersei-provider/src/openai.rs:240
  - "rg 'num_ctx|keep_alive' → zero hits repo-wide"
falsifier: "Sending options.num_ctx to Ollama, or any consumer of the real model window."
refutation_attempted: yes — traced both context-window functions to their call sites.
```

Two separate context-window functions exist and both are wrong:

- `registry.rs:116` `unwrap_or(128_000)` — **dead code**, zero non-test callers. The brief's H4
  targeted this one.
- `compact.rs:98` `context_window_for_model()` — the live path (`runner.rs:874`), whose
  catch-all is **`_ => 200_000`**.

An unknown Ollama tag like `qwen2.5-coder:7b` matches no arm and gets 200,000. Compaction
(`runner.rs:902`) therefore never fires. And `num_ctx` is never sent — the complete Ollama
request body (`openai.rs:240`) has no `options` object, and the string appears nowhere in the
repo.

**Why I label the consequence `Likely`, not `Confirmed`:** the measured payload is
1,566 tokens of tool defs (§2.3) plus ~1,700 tokens of system prompt (F-A7) ≈ 3.3k, which
*fits* in a 4096 window with ~800 tokens to spare. The truncation is therefore not immediate —
it is triggered by the first substantial tool result, and `cap_tool_result` permits **20,000
chars ≈ 5,000 tokens for a single result** (`runner.rs:33`), which alone exceeds the entire
real window. I have not observed the truncation on a live server; that is a Phase-6 experiment
(§7). This corrects my own Phase-0 prior, which assumed tool-definition bulk was the cause.

**Impact by tier** — low: only tier affected, but severely. mid/frontier: none.

**Fix sketch** — send `options: { num_ctx: N }` on the Ollama path and make the catch-all
conservative (`_ => 8_192`) rather than optimistic. A wrong-low guess costs an unnecessary
compaction; a wrong-high guess costs silent truncation with no error.

---

### F-10 · Unknown parameters are silently dropped, producing confidently wrong results

```yaml
id: F-10
taxonomy: [T3, T5]
locus: runtime-side
confidence: Confirmed (mechanism) / Unverified (trigger rate — see §7.0)
tiers_affected: [low, mid, frontier]
severity: P1                 # downgraded from P0: the drop is certain, the trigger rate is not
effort: S
evidence:
  - crates/cersei-tools/src/grep_tool.rs:41-50
  - crates/cersei-tools/src/grep_tool.rs:57-59
  - crates/cersei-tools/src/file_edit.rs:129
  - "rg -n 'deny_unknown_fields' crates/ → only cersei-compression/src/toml_rules.rs; zero in cersei-tools"
falsifier: "A serde(deny_unknown_fields) on any tool Input struct, or a required `path`."
refutation_attempted: yes — checked all search tools; none rejects unknown keys.
```

No tool `Input` struct in `cersei-tools` carries `#[serde(deny_unknown_fields)]`. Combined
with the `file_path`/`path` split measured in §2.3, this converts a naming mistake into
**silent data corruption rather than an error**:

```rust
// grep_tool.rs:41-50 — no deny_unknown_fields
struct Input { pattern: String, path: Option<String>, glob: Option<String>, .. }
// grep_tool.rs:57-59
let search_path = input.path.unwrap_or_else(|| ctx.working_dir.display().to_string());
```

**Failure scenario** — the model calls `Read(file_path="/repo/src/auth.rs")`, then
`Grep(pattern="login", file_path="/repo/src/auth.rs")`. `file_path` is **silently discarded**,
`path` is `None`, so the search falls back to the entire working directory. The model receives
up to 250 matches from across the whole repo and believes every one came from `auth.rs`. **No
error is emitted at any layer.** Identical shape for `Glob` and `CodeSearch`.

This is worse than F-05: F-05 at least fails loudly. Here the model reasons confidently from
corrupted evidence, and no amount of recovery logic can help because there is nothing to
recover from.

It is made worse by *partial* leniency: `file_edit.rs:129` accepts `path` as an alias for
`file_path`, so `Edit(path=X, ...)` **succeeds**. The model learns from that success that
`path` is correct, then carries the hypothesis into `Read` (hard fail) and `Grep`/`Glob`
(silent mis-scope). Coercion exists on 2 of 42 tools and actively teaches the wrong schema.

**Impact by tier** — low: constant, and invisible. mid: frequent. frontier: rarer, but the
failure is undetectable when it happens, which makes it arguably worst here.

**Fix sketch** — two lines of policy: add `#[serde(deny_unknown_fields)]` to every tool
`Input`, **or** hoist `file_edit.rs`'s `coerce_input` alias approach to a shared helper applied
uniformly. Do one or the other, not the current mixture.

---

### F-11 · The read-before-edit guard runs after the write and manufactures a second failure

```yaml
id: F-11
taxonomy: [T6]
locus: runtime-side
confidence: Confirmed
tiers_affected: [low, mid]
severity: P0
effort: S
evidence:
  - crates/cersei-agent/src/runner.rs:692
  - crates/cersei-agent/src/runner.rs:709
  - crates/cersei-agent/src/runner.rs:722-735
  - crates/cersei-tools/src/file_edit.rs:70-77
falsifier: "The guard evaluated before tool.execute(), or covering Write/MultiEdit."
refutation_attempted: yes — traced execute (:692) → join_all (:709) → guard (:722). Ordering confirmed.
```

```rust
// runner.rs:722 — runs AFTER execute(:692) and join_all(:709) have completed
if (tool_name == "Edit" || tool_name == "edit") && !result.is_error {
    ...
    result = ToolResult::error(format!("You must Read '{}' before editing it. ..."));
}
```

The guard is gated on `!result.is_error` — i.e. it fires only when the edit **already
succeeded** and the bytes are already on disk. It then overwrites the success with an error
telling the model the edit did not happen.

**Failure scenario** — model edits without reading. The write lands. The model is told to Read
first and retry. It complies: Reads, re-issues the identical Edit — and now gets
`old_string not found in <file>` (`file_edit.rs:70`), because **its own first edit already
replaced that text**. A weak model reading "old_string not found" immediately after being
instructed to re-read typically concludes the file is corrupt and rewrites it wholesale with
`Write` — which the guard does not cover at all, since it matches `tool_name == "Edit"` only.
The guard converts one recoverable mistake into data loss.

Two further gaps: it reads `tool_input["file_path"]`, so an Edit using the `path` alias
(accepted by `file_edit.rs:129`, per F-10) bypasses it entirely; and `Write`, `MultiEdit`,
`NotebookEdit` and `ApplyPatch` are uncovered.

**Impact by tier** — low/mid: severe; this is a trap that punishes compliance. frontier: usually
recovers, but pays two wasted turns.

**Fix sketch** — move the check **before** dispatch, extend the tool-name match, and read the
path through the same alias resolution the tools use:

```rust
// before tool.execute(...)
if is_write_tool(&tool_name) && !files_read.contains(resolve_path(&tool_input)?) && exists {
    return ToolResult::error("You must Read '{path}' before editing it.");  // nothing written
}
```

---

## 4. Hypothesis adjudication (H1–H9)

| H | Verdict | Evidence and correction |
|---|---|---|
| **H1** | **CONFIRMED**, downstream consequence now traced | `stream.rs:85` coerces to `Null`; `file_read.rs:44-47` renders it as `Invalid input: invalid type: null, expected struct Input`. The brief asked me to verify the downstream half — done, and it is worse than described: the struct is literally named `Input`, and `input_schema()` is never used in any error path. → **F-05** |
| **H2** | **3 of 4 CONFIRMED, 1 REFUTED** | (a) `[DONE]`-gated flush, EOF drops all calls: **CONFIRMED** (`openai.rs:338`) → F-03. (b) HashMap order scrambling: **REFUTED** for the assembled message — `stream.rs:106` writes positionally by index — but **CONFIRMED** for live event order (`openai.rs:340`). (c) `finish_reason:"length"`: **CONFIRMED unreachable** — the mapping is nested inside the `usage` block (`openai.rs:454`) and `[DONE]` overwrites it afterward; `StopReason::MaxTokens` cannot occur on the OpenAI path, making the continuation logic dead. (d) empty `name`/`id`: **CONFIRMED** → F-A3. **Fresh, worse than (b):** servers omitting `index` collapse all parallel calls into one via `unwrap_or(0)` and *concatenate* their JSON → F-A2. |
| **H3** | **PARTIAL — OpenAI half FIXED as stated; Anthropic half CONFIRMED but for a different reason** | `reasoning_effort` works (`openai.rs:256`, tests at `:646`); minor gap — `starts_with("o1")/("o3")` misses `o4-mini` and provider-prefixed ids. **`signature_delta`: CONFIRMED** — `StreamEvent` has no such variant (`cersei-types/src/lib.rs:293-332`), so it is structurally unparseable; `stream.rs:94` hardcodes `signature: String::new()`, and the field has `#[serde(default)]` but **no `skip_serializing_if`** (unlike `is_error` and `title` beside it), so it always emits `"signature": ""`. **The thinking/temperature 400 claim I am marking `Unverified`** — I could not source it in current docs and will not pass through a subagent's confidence on provider behavior (§4.5). What I *did* source is bigger: the whole manual-thinking API is rejected on 4.7+ → **F-01**. |
| **H4** | **Mechanism REFUTED, conclusion CONFIRMED and worse** | The `registry.rs` 128k is **dead code** (zero non-test callers). The live budget is `compact.rs:98` with a **200_000** catch-all. → **F-09** |
| **H5** | **CONFIRMED, with a quantified trigger rate** | Line anchors had indeed drifted; re-derived. `KEEP_RECENT_MESSAGES = 10` is **even**, and the history strictly alternates, so parity makes ~half of all cuts orphan a pair. Zero repair logic exists (8-spelling grep). Plus a second defect: two consecutive user messages. → **F-04** |
| **H6** | **CONFIRMED, and the dead-code half is total** | Zero occurrences of `strict`, `additionalProperties`, `tool_choice`, `parallel_tool_calls` in any provider. `capabilities()` has **exactly one call site** — a blanket `Box<dyn Provider>` forwarder that forwards to nothing. All six fields dead. `ProviderCapabilities` also hardcodes `thinking: true` for every model (`anthropic.rs:87-96` discards its `model` argument), so the type *cannot express* the gate F-01 needs. → **F-A1, F-23** |
| **H7** | **CONFIRMED on all four sub-claims, one number corrected** | No per-model/provider prompt branching exists (`SystemPromptOptions` has no model field). Measured base prompt **5,368 chars ≈ 1,342 tokens**; typical CLI config **6,801 chars ≈ 1,700 tokens** — close to the brief's ~1.8k estimate. **Not XML-tagged** as the brief assumed: the static body is plain Markdown; XML appears only on injected wrappers. CLAUDE.md **is** double-injected, and the two copies land on *opposite sides* of the cache boundary, defeating the caching the boundary exists for. Parallel mandates appear in **four** places, one a hard `MUST`. Unregistered tools **confirmed** — `Agent` is advertised (`system_prompt.rs:447`) and not registered. → **F-A7–F-A10** |
| **H8** | **SPLIT: parameter inconsistency CONFIRMED; schemars claim REFUTED for shipped tools** | Measured: `file_path` (Read/Write/Edit/MultiEdit/NotebookEdit) vs `path` (Glob/Grep/CodeSearch/EnterWorktree/ExitWorktree) — five tools each. `ToolSearch` **confirmed unregistered**. **But the schemars-0.8 `$ref` claim does not hold for the shipped surface:** zero shipped tools use `#[derive(Tool)]` (all 42 `impl Tool` blocks are hand-written), and my probe found **zero** `$ref`/`oneOf`/`definitions` hazards across all 34 tools. The hazard is real but scoped to *SDK users'* custom tools and MCP passthrough. → **F-A5, F-A11** |
| **H9** | **Evaluated and NOT recommended** | See §6. The decisive argument is drawn from H6: a per-model capability table already exists in this codebase and is 100% dead. |

---

## 5. External baseline

**Declared scope reduction (§8.7):** I ran the provider-documentation half of Phase 4 at full
depth because §4.5 requires it and because the design recommendation depends on it. I
**compressed** the "what comparable runtimes do" survey to the paragraph below rather than
skipping it silently. Reason: it does not move any of the brief's three decisions, and the
budget was better spent on Phase 3.

**Verified this session (primary source):** Anthropic's extended-thinking docs — manual
`thinking: {type:"enabled"}` is deprecated on 4.6 and **returns 400 on 4.7+**, migration is to
`{type:"adaptive"}` + `output_config.effort`; `budget_tokens` must be **≥1024 and < max_tokens**.
Both facts are load-bearing for F-01.

**Not verified this session, flagged rather than asserted:** OpenAI strict-mode requirements
(`additionalProperties: false`, all fields required, subset of JSON Schema), Gemini's
function-calling schema subset, and Ollama's default `num_ctx`. My design recommendation is
written so that it does not *depend* on the specifics — the seam in §6 is where those rules
get encoded once they're confirmed, and the probe already proves the shipped schemas are flat
enough to be strict-compatible.

**Techniques from the literature, filtered for a Rust SDK that does not control the decoder:**

| Technique | Available to Cersei? |
|---|---|
| Constrained/grammar decoding (GBNF, outlines) | **No** for hosted APIs. Possible for Ollama via `format`, which Cersei never sends. |
| Provider strict function calling | **Yes** — a request-body flag plus a schema shape. The cheapest real win. |
| `tool_choice` / `functionCallingConfig: ANY` | **Yes**, unused everywhere. Directly addresses F-08. |
| Validate-before-dispatch + repair loop | **Yes**, fully local, no provider support needed. Addresses F-05. |
| Progressive tool disclosure / tool search | **Yes** — `ToolSearch` is already written and unregistered. |
| Tool-name namespacing | **Yes**, relevant to MCP collisions (not fully audited, §9). |
| Few-shot exemplars in tool descriptions | **Yes**, cheap; the 2,873-char `CORE_CAPABILITIES` prose could be traded for them. |

---

## 6. Design: three options, one recommendation

### The null option — fix P0s, add no abstraction
Nine bugs, mostly `S`. **All of §3 is reachable this way.** No new concepts for SDK users, no
maintenance surface. Weakness: leaves the Gemini/MCP schema hazard (F-A11) and the missing
`strict`/`tool_choice` levers unaddressed, and nothing prevents the next provider divergence.

### Option A — `ModelProfile` (H9)
A per-family record (prompt style, strict mode, tool trimming, temperature policy, context
truth, retry policy) resolved at router time.

**Rejected.** Three reasons, in descending force:

1. **The empirical one.** Cersei *already built* a per-model capability table —
   `ModelEntry.capabilities`, written 40+ times across `registry.rs`. It has **one** call site,
   a blanket forwarder to nothing (H6). A per-model table in this codebase has already been
   demonstrated to rot into dead code within a release cycle, and `ModelProfile` is strictly
   larger. Before adding a second table, the honest move is to make the first one load-bearing.
2. **It doesn't fix the bugs.** F-01 through F-08 are unconditional defects. A profile
   abstraction would sit above them and change nothing. Shipping it first would create the
   impression of a fix.
3. **Maintenance scales with the model catalogue**, which is the fastest-moving input in the
   system — `registry.rs` already carries 15 providers.

### Option B — tool-serialization seam + minimal `ProviderQuirks` — **RECOMMENDED**

Two narrow pieces, both resolving at points that already exist.

**B1. The schema seam.** §2.2 established that there are exactly three sites where a
`ToolDefinition` becomes provider JSON. Insert one function at all three:

```rust
pub enum SchemaDialect { AnthropicNative, OpenAiStrict, OpenAiLoose, GeminiSubset }

/// Normalize once, at the only three places schemas cross the provider boundary.
pub fn adapt_tools(tools: &[ToolDefinition], dialect: SchemaDialect) -> Vec<Value>;
```

Responsibilities, **as measured in Exp 3 (§7.0) rather than assumed**:

| Dialect | Transform |
|---|---|
| `GeminiSubset` | strip `$schema`; strip `definitions`; inline `$ref`; **strip `additionalProperties`**. Leave `oneOf`/`anyOf`/nesting/`enum`/`format`/`default` alone — Gemini accepts all of them. |
| `OpenAiStrict` | inline `$ref`; **add `additionalProperties: false`**; all properties required |
| `OpenAiLoose` / `AnthropicNative` | inline `$ref`, strip `$schema` (harmless but noisy) |

Plus, for all dialects: sanitize names to `^[a-zA-Z0-9_-]{1,64}$` and dedupe collisions.

Note the load-bearing measured fact: **`additionalProperties: false` is required by OpenAI
strict and rejected by Gemini.** The two targets are irreconcilable, so this cannot be one
normalization pass — the per-dialect enum is forced by the providers, not a design preference.
That is the strongest argument for the seam, and I only have it because Exp 3 was run.

Why this and not a profile: it is **~150 lines, has one input and one output, and is fully
testable offline with zero API keys** — I demonstrated exactly this kind of offline schema
analysis with `joy/schema-probe` in a few minutes. It fixes the MCP and custom-tool hazard
(F-A11) at the choke point rather than per-provider, and it is the prerequisite for ever
turning on `strict: true`.

**B2. `ProviderQuirks` — four fields, not twenty.** Only for incompatibilities the provider
API *forces*:

```rust
pub struct ProviderQuirks {
    pub thinking: ThinkingMode,        // Adaptive | Manual{max_budget} | Unsupported  → F-01
    pub temperature: TemperaturePolicy,// Free | Forbidden | PinnedTo(f32)             → F-A12
    pub context_window: u64,           // real, conservative                            → F-09
    pub dialect: SchemaDialect,        //                                               → B1
}
```

Resolved in `router.rs::build_provider`, which already branches on `entry.api_format` and is
the natural home. The discipline that keeps this from becoming Option A: **a field may only be
added when omitting it produces a provider error.** Preferences do not qualify.

**What I am explicitly not recommending:** per-family prompt variants. The prompt problems
(F-A7–F-A10) are quality defects — a bluffing error counter, an unregistered tool, a
double-injected file — that should be *fixed*, not *forked per family*. Forking the prompt
multiplies the maintenance cost of every future prompt fix by the number of families.

**Sequencing:** P0 bugs → B1 → B2. If only the first lands, that is still most of the value.

---

## 7. Measurement plan

### 7.0 Measured results (run 2026-07-28, total spend $0.0011)

Two experiments were run. **One confirmed a finding decisively; one failed to reproduce my
own prediction.** Both are reported.

**Exp 1 — Gemini schema rejection. CONFIRMED.** Identical request to
`gemini-flash-lite-latest`, only the schema shape varying:

| Schema | Result |
|---|---|
| Flat (the shape all 34 shipped tools hand-write) | **ACCEPTED** — returned a correct `functionCall` |
| schemars 0.8 `schema_for!` output | **REJECTED** — `INVALID_ARGUMENT` |

Verbatim error: `Unknown name "$schema" ... Cannot find field. Unknown name "$ref" at
'tools[0].function_declarations[0].parameters.properties[1].value' ... Unknown name
"definitions"`. All three constructs rejected, and the rejection kills the **entire request** —
every tool in the turn, not just the offending one. F-A11 is real; §6's `adapt_tools()` seam is
now empirically justified rather than merely argued.

**Exp 2 — weak-model error recovery. CONFIRMED, with a clean before/after.** Two 7-8B models,
Cersei's real `Read` schema, a bad call (`{"path": …}` instead of `{"file_path": …}`), n=6 per
cell. Only the error message text varies:

| Error message returned to the model | llama-3.1-8b | qwen-2.5-7b | total |
|---|---|---|---|
| Cersei's actual: `Invalid input: invalid type: null, expected struct Input` | 2/6 | 4/6 | **6/12 (50%)** |
| Proposed fix: names the tool, echoes what was sent, includes `input_schema()` | 6/6 | 6/6 | **12/12 (100%)** |

Same models, same bad call — **50% → 100% recovery on message text alone.** Note the failure
*mode* under Cersei's message: llama emitted **malformed JSON on 4 of its 6 attempts**, i.e.
the uninformative error actively degraded output quality rather than leaving it flat. That is
F-05's amplification loop, observed.

**Exp 2, Test A — my F-10 prediction did NOT reproduce.** I predicted weak models would carry
`file_path` from `Read` into `Grep` (whose param is `path`), triggering the silent drop.
Observed: **0/12 silent drops.** But the test is confounded and I am not claiming it as
evidence *against* F-10 either — 10 of 12 trials ended in `no_call_turn2`, because my stub
`Read` result already contained the string the task asked to search for, so stopping was
arguably correct. **The experiment was badly designed and measured nothing about F-10.** What
it did incidentally show is consistent with F-08: weak models stop emitting tool calls readily,
and Cersei has no nudge for that. F-10's *mechanism* remains Confirmed from code; its
*trigger rate* is now explicitly Unverified and needs a non-confounded rerun.

**Exp 3 — Gemini's real schema dialect. This CORRECTS §6's spec.** Twelve single-construct
probes against `gemini-flash-lite-latest`:

| Rejected (4) | Accepted (8) |
|---|---|
| `$schema`, `$ref`, `definitions`, **`additionalProperties`** | nested objects, arrays of objects, `enum`, **`oneOf`**, **`anyOf`**, `title`/`description`, `format`, `default`, `minimum` |

Two corrections to what I wrote in §6:
1. I specified "drop or lower `oneOf`/`anyOf` for `GeminiSubset`". **Wrong — Gemini accepts
   both.** The transform is far simpler than I claimed: strip `$schema`, strip `definitions`,
   inline `$ref`, strip `additionalProperties`. Four keys.
2. **`additionalProperties: false` is rejected by Gemini but *required* by OpenAI strict
   mode.** That is a direct, irreconcilable conflict between two targets — which means
   `adapt_tools` genuinely cannot be a single normalization pass and must be per-dialect. This
   *strengthens* the case for the seam while shrinking its implementation.

**Exp 4 — compaction orphan rate. CONFIRMED at exactly the predicted rate, and the fix
validated.** Offline probe replicating `runner.rs:919-922` and `snip_compact` against a
synthetic runner-shaped history, over 30 conversation lengths (12..=41):

| Path | Orphaned pairs |
|---|---|
| LLM-compaction (`runner.rs:919-922`) | **15/30 (50%)** |
| snip fallback (`compact.rs:214-222`) | **15/30 (50%)** |
| Consecutive-user-message defect | 15/30 (50%) |
| **After the §3 F-04 fix (back off to a safe boundary)** | **0/30** |

The pattern is exactly the parity argument: every even length orphans, every odd length is
clean. This was the claim I flagged in §9 as most likely to be wrong. It holds, and the
proposed fix takes it to zero — so F-04's remediation is validated, not just its diagnosis.

**Exp 5 — `tool_choice: "required"` as the F-08 fix. VOID, no conclusion.** OpenRouter returned
`502 Upstream error from Phala` for every `tool_choice` request, and my harness counted the
failures as "model made no call," producing a fake 0/6. **This is the second time in this
session I let a harness swallow errors as negative results** (the first ruined F-10's
measurement). F-08's *fix* is therefore still unvalidated — its diagnosis stands on code, but
whether forcing tool calls actually helps a 7B model is untested. Retest against a direct
provider, not an aggregator.

**Exp 6 — wire pathologies against the real provider. FOUR CONFIRMED.** A fake
OpenAI-compatible SSE server (`joy/fake_sse_server.py`) driven through the **actual**
`cersei-provider` OpenAI client (`joy/sse-probe/`) — `openai.rs` and `stream.rs` execute
exactly as in production; nothing on the Cersei side is stubbed. The control case passes,
which validates the harness:

| Scenario | Expected | Actual |
|---|---|---|
| **control** — 2 calls, correct `index`, `[DONE]` | 2 calls, `ToolUse` | ✅ 2 calls, `ToolUse` |
| **F-03** — identical stream, EOF without `[DONE]` | 2 calls | ❌ **0 calls, `EndTurn`** |
| **F-A2** — 2 calls, `index` absent | 2 calls | ❌ **1 call, `id="call_b"`, `input=null`** |
| **F-A3** — tool call with empty id and name | rejected/flagged | ❌ **emitted as `id="" name=""`** |
| **H2c** — truncated call + `finish_reason:"length"` | `MaxTokens` | ❌ **`ToolUse`, `input=null`** |

Three things this settles that code reading alone could not:

1. **F-03 is exactly as severe as claimed.** The only difference between the control and the
   failing case is the four-byte `[DONE]` sentinel. Two perfectly valid tool calls are
   discarded and the turn is reported as a clean `EndTurn`. Total silent loss.
2. **F-A2 chains into F-05.** The collapse isn't merely a lost call — the two argument bodies
   concatenate into `{"file_path":"/a.rs"}{"file_path":"/b.rs"}`, which fails to parse and
   becomes `Null` at `stream.rs:85`. So the model receives the undiagnosable
   `expected struct Input` error for a call it constructed perfectly. Two P0s compound.
3. **H2c confirmed end-to-end.** `StopReason::MaxTokens` is unreachable on the OpenAI path, so
   the runner's continuation logic (`runner.rs:857-866`) is dead code for every
   OpenAI-compatible model — and a turn truncated mid-tool-call is reported as normal
   `ToolUse` with `null` args.

**Exp 7 — the actual Ollama-path wire body. F-09 half-confirmed empirically.** Captured the
literal request `cersei-provider` emits for `model=llama3.1` by pointing it at a recording
server. Complete body: `max_tokens`, `messages`, `model`, `stream`, `stream_options`, `tools`.
**No `options` object, so no `num_ctx`** — confirming by observation, not just by grep, that
Ollama is left at its server-side default window for the life of the session while
`compact.rs:98` budgets 200,000 tokens against it.

The remaining half of F-09 — that Ollama *silently truncates* the prompt front rather than
erroring — is a fact about Ollama, not about Cersei, and should be closed with a citation to
Ollama's `num_ctx` documentation rather than an experiment. **Not done.**

**No other number in this document is measured.**

### 7.1 tbench gap analysis — and a warning about historical results

`cersei-tbench` is **an agent binary, not a measurement harness**. It solves one task from
argv/stdin and exits 0 unconditionally. It has no fixtures (`resolve_task`,
`main.rs:73-86`, is the entire "fixture system"), no model matrix (`--model` is a single
`String`), no repeats (`--samples` is a retry-until-first-pass loop that short-circuits on
success and defaults to 1), and no cost tracking. Its complete machine-readable output is
three keys: `{type, solved, how}`.

**It cannot distinguish any of T1–T6 today.** The data partially exists in the agentrl graph
but is never scored, and args are truncated at 1200 bytes before storage.

**The warning, verified this session:**

```rust
// cersei-tbench/src/main.rs:128-131
ChainVerifier::new(vec![Arc::new(TestScriptVerifier::default_candidates())])
    .with_default(true)
// cersei-agentrl/src/verify.rs:200-203 — when no verifier is applicable
VerifyResult { passed: self.default_passed, detail: "no applicable verifier in chain".into() }
```

When no `run-tests.sh` is on disk — the normal case during an agent run, since graders mount
it at grade time — `solved` is **unconditionally `true`**. Any historical `solved=true` from
tbench means "the process exited", not "the task succeeded". **Do not cite past tbench numbers
as a baseline.**

### 7.2 The cheap path to a real T1/T3 measurement

Do **not** instrument the Orchestrator — `SolveOutcome` has no usage field and the graph loses
args. Bypass it: `AgentOutput.tool_calls: Vec<ToolCallRecord>`
(`cersei-agent/src/lib.rs:56-63`) already carries **untruncated** `input: serde_json::Value`,
`is_error`, and `duration`, and `AgentOutput.usage` carries tokens. Replace
`orchestrator.solve()` (`main.rs:177`) with a direct `Agent::run()` — the ~15 lines to copy
are already written at `cersei-agentrl/src/runner.rs:157-172` — then:

- **T1** = `tool_calls.len() > 0` AND no record whose result starts with `"Unknown tool:"`
  (that literal is at `cersei-agent/src/runner.rs:700`).
- **T3** = validate each `record.input` against that tool's `input_schema()`. Needs a
  JSON Schema validator added to `cersei-tbench/Cargo.toml`; none exists in the workspace.

Only the fixture loader and the scorer (~40 lines) are genuinely new code.

### 7.3 The matrix a real harness needs

**What a real harness needs**, per sub-capability:

| Cap | Fixture shape | Pass criterion |
|---|---|---|
| T1 emission | A task that cannot be answered without a tool | ≥1 syntactically valid tool_use block |
| T2 selection | A task where `Grep` is right and `Bash`/`CodeSearch` are wrong | correct tool name on first call |
| T3 args | A task requiring `file_path` on Read then `path` on Glob | both calls schema-valid (targets F-A5) |
| T4 sequencing | Read-then-Edit with a dependency | Edit follows Read, no redundant re-reads |
| T5 result interp | Tool returns a specific value the answer must use | value appears in final answer |
| T6 recovery | Inject a deliberately malformed first call | second call is *different* and valid (targets F-05) |

**Matrix:** 3 tiers × ≥5 repeats (tool-calling variance is high; fewer than 5 will not separate
a fix from noise) × 6 fixtures. **Cost:** not estimated — depends on fixture length.

**The two cheap experiments worth running first**, given the available Gemini key and $1 of
OpenRouter credit:

1. **Gemini schema rejection (settles F-A11).** Send one `functionDeclarations` payload
   containing a `$ref` (as MCP would produce) and observe the response. Cost: one call.
   Converts the highest-uncertainty finding to `Confirmed` or kills it.
2. **The F-05 loop, live.** Run a 7B OpenRouter model on a task requiring `Read`, capture the
   transcript, and count how many turns it spends re-emitting the same malformed call after
   receiving `expected struct Input`. This directly measures the T6 lever and gives a
   before/after number for the fix.

Both are single-digit-cent experiments. Neither was run.

---

## 8. Roadmap

Each item is phrased to be filed verbatim.

### P0 — correctness, blocks v0.3 — **ALL DONE 2026-07-30**, and **all 22 fix sites test-bound** as of the same day (branch `runtime-fix`, §10.7)
- [x] **F-01** Gate thinking on model generation; migrate to adaptive; clamp `budget_tokens < max_tokens`. *(S)* — Phase 7. Half B confirmed from primary docs; gate + clamp in `build_anthropic_body`, 15 unit tests, 10-mutation suite. **Bound.**
- [x] **F-02** Construct `ProviderStatus`/`RateLimit` at the HTTP boundary in all four providers so `is_retryable()` can fire. *(S)* — Phase 5. Status checked inside `complete()`; `StreamEvent::HttpError` deleted; Ctrl-C preserved via `tokio::select!`. **Bound (all 3 providers).**
- [x] **F-03** Flush accumulated tool calls after the read loop; treat EOF-without-`[DONE]` as an error; stop swallowing `StreamEvent::Error`. *(S)* — Phase 1. openai.rs path bound via `sse_pathologies`; the `stream.rs` accumulator half **bound 2026-07-30** (5 `stream::tests`, §10.7).
- [x] **F-04** Make the compaction split pair-aware; add a pre-request tool-pair assertion. *(M)* — Phase 6. `pair_aware_split` + `find_orphaned_tool_results`. Core bound (5 tests); both runner call sites **bound 2026-07-30** (`p0_wiring` + `orphan_check_logging`, §10.7).
- [x] **F-05** Preserve raw args + parse error through `stream.rs`; return `input_schema()` on deserialization failure. *(S)* — Phases 1–2. Parse-error half bound; the `{}`-not-`null` no-arg half **bound 2026-07-30** (3 `stream::tests`, §10.7).
- [x] **F-06** Enforce `MAX_TOOL_ERRORS_PER_TOOL` or remove the claim from the message. *(S)* — Phase 4. Enforced, with honest advice text. **Bound.**
- [x] **F-07** Call `cap_tool_result` on the `is_error` branch. *(S)* — Phase 4. Helper bound; the branch selection **bound 2026-07-30** (`p0_wiring::oversized_error_result_is_capped_before_entering_history`, §10.7).
- [x] **F-10** Add `#[serde(deny_unknown_fields)]` to every tool `Input`, or hoist `file_edit.rs`'s alias coercion into a shared helper applied uniformly. Stop doing both. *(S)* — Phase 3. 35 inputs; Edit's `path`/`old`/`new` aliases removed, scalar type-coercion kept. **Bound** (policy test sweeps every tool).
- [x] **F-11** Move the read-before-edit guard before dispatch; extend it to Write/MultiEdit/NotebookEdit/ApplyPatch; resolve the path through the same aliases the tools accept. *(S)* — Phase 4. `refusals_for_batch` bound (12 guard_tests); its wiring into dispatch **bound 2026-07-30** (`p0_wiring::edit_of_unread_file_is_refused_and_the_file_is_untouched`, asserting on bytes-on-disk, §10.7).
- [x] **F-A2** Reject/segregate tool-call deltas with a missing `index` instead of collapsing to 0. *(S)* — Phase 1. **Bound** (`sse_pathologies::no_index`).
- [x] **F-A3** Validate tool-call `id`/`name` are non-empty before emitting. *(S)* — Phase 1. **Bound** (`sse_pathologies::empty_id`).
- [x] **F-A13** Surface `total_lines`/`lines_returned` in Read's result. *(S)* — Phase 2. `tool_feedback::window_notice`. **Bound (4 tests).**
- [x] **F-A14** Attach the tool name to the 31 hand-written `"Invalid input: {}"` sites. *(S)* — Phase 2. `tool_feedback::invalid_input` (name + echo + schema + near-miss hints), 42 call sites. **Bound (8 tests).**
- [x] **F-A15** Append the valid-value list to every "not found". *(S)* — Phase 2. `tool_feedback::not_found` (+ `closest()` did-you-mean). **Bound (5 tests).**

### P1 — profile-or-schema work
- [ ] **B1** `adapt_tools()` seam at the three serialization sites. *(M)*
- [ ] **F-08** Ungate the no-tool-call nudge; add `tool_choice`/`functionCallingConfig` support. *(M)*
- [ ] **F-09** Send Ollama `num_ctx`; make the context catch-all conservative. *(M)*
- [ ] **B2** Four-field `ProviderQuirks` resolved in `router.rs::build_provider`. *(M)*
- [ ] **F-23** Make `capabilities()` load-bearing or delete it. *(S)* — note: F-01 was deliberately gated in `build_anthropic_body`, *not* via `ProviderCapabilities`; when F-23 lands, `thinking_mode()` in `anthropic.rs` is the logic to absorb.

### P2 — prompt & tool surface
- [ ] **F-A5** Unify `file_path`/`path`; extend alias coercion beyond Edit/MultiEdit. *(S)* — partially superseded: F-10 removed Edit's aliases entirely; what remains is the naming unification.
- [ ] **F-A9** Remove the `Agent` advertisement or register the tool. *(S)*
- [ ] **F-A8** De-duplicate CLAUDE.md injection. *(S)*
- [ ] **F-A10** Soften the four parallel-tool-call mandates. *(S)*
- [ ] Register `ToolSearch`; add per-tool guidance for the 25 unexplained tools. *(M)*

### P3
- [ ] Cache-breakpoint stability; usage/cache-token accounting; Vertex beta-header parity.

---

## 9. Open questions and what I could not verify

**A correction that changes the severity of one of my own findings.** MCP is **100% dead
code**. `AgentBuilder::build()` hard-codes `mcp_manager: None, // TODO: connect MCP servers`
(`cersei-agent/src/lib.rs:478`), and I confirmed by grep that **every** `mcp_manager` site in
the workspace is `None` — there is no `Some(McpManager)` construction anywhere. The tool
request is built solely from `agent.tools` (`runner.rs:269-270`). `abstract-cli mcp add`
writes config with no runtime effect.

Consequences for this document:
- **F-A11 (Gemini receives unsanitized MCP schemas) is LATENT, not live.** The only live
  exposure is SDK users' `#[derive(Tool)]` tools. I have downgraded it accordingly. Had I
  shipped this document an hour earlier I would have overstated it.
- The MCP defects found are real but **pre-wiring**: raw un-namespaced tool names, no
  collision detection (`flat_map` with no dedup; dispatch picks the first match in
  *randomized* HashMap order), no `^[a-zA-Z0-9_-]{1,64}$` sanitization, no tool-count cap, no
  re-fetch or `list_changed` handling, and `.ok().unwrap_or_default()` at
  `cersei-mcp/src/lib.rs:163-168` which silently converts one malformed tool entry into an
  empty tool list for the whole server. **Wiring MCP up without fixing these ships all six at
  once** — that is the actionable form of this finding.

**Verified as unresolved:**
- **The thinking × temperature 400.** Widely believed, asserted by one subagent, but I could
  not source it in current docs. Marked `Unverified` in H3. One API call settles it.
- **F-09's truncation consequence.** Mechanism confirmed; the actual server-side truncation is
  inferred, not observed.
- **OpenAI strict-mode and Gemini schema-subset specifics.** Deliberately not asserted (§5).
- **Nothing in this document is benchmarked.** Per §7.1, no usable baseline exists to compare
  against either — the prior harness's only output bit defaults to `true`.

**What I would still want measured even though the code is clear:**
- The **real** malformed-JSON rate by tier. F-05's severity is proportional to it, and I am
  assuming rather than knowing that it is high on 7B models.
- Whether F-04's parity argument holds in practice — it is sound given strict alternation, but
  hook-injected or restored messages could break the alternation and change the rate.
- Whether fixing F-05 alone measurably improves weak-tier success, or whether weak models fail
  to use the schema even when handed it. **This is the load-bearing assumption of the entire
  recommendation**, and it is untested.

**Where I think I am most likely wrong:** the §6 rejection of `ModelProfile`. My argument rests
heavily on the dead-capability-table precedent, which is evidence about *this codebase's
history*, not about the abstraction's merit. If you intend to make `capabilities()`
load-bearing anyway, the marginal cost of a fuller profile drops a lot and Option A gets more
attractive than I have credited.

---

## 10. Implementation report and P0 mutation audit (added 2026-07-30)

All 15 P0 items were implemented across seven phases on branch `graphify-current-project`
(uncommitted). This section records what landed, the F-01 evidence that resolved §H3's
`Unverified` marker, the full-P0 mutation audit, and the fix backlog that audit produced.

### 10.1 What landed, per phase

| Phase | Items | Where |
|---|---|---|
| 1 | F-05a, F-A2, F-A3, F-03 | `cersei-provider/{stream,openai}.rs` + `tests/sse_pathologies.rs` (7 cases, replaces `joy/sse-probe`) |
| 2 | F-05b, F-A14, F-A15, F-A13 | `cersei-tools/src/tool_feedback.rs` (new) + 42 call sites |
| 3 | F-10 | `serde(deny_unknown_fields)` on 35 tool inputs; Edit's `path`/`old`/`new` aliases removed, scalar type coercion kept |
| 4 | F-11, F-06, F-07 | read-before-edit guard moved **before** dispatch (`refusals_for_batch`); error budget enforced; error results capped |
| 5 | F-02 | status checked inside `complete()` in all 4 providers → typed `Err(from_http_status(..))`; `StreamEvent::HttpError` deleted; the hoisted await and backoff sleep wrapped in `tokio::select!` on `cancel_token` (Ctrl-C preserved); `tests/retry_on_429.rs` (4) |
| 6 | F-04 | `compact::pair_aware_split()` + `find_orphaned_tool_results()`; `snip_compact` prepends a truncation notice (first message must be `user`); `tests/compact_pairing.rs` (7) |
| 7 | F-01 | thinking gate + budget clamp in `build_anthropic_body` (details below); 15 unit tests |

Gate after Phase 7: `cargo build --workspace` clean; `cargo test --workspace` **509 / 0 / 14**;
`sse_pathologies` 7/7; `compact_pairing` 7/7; `retry_on_429` 4/4.

### 10.2 F-01 resolution — §H3's `Unverified` is now Confirmed

> **CONFIRMED AGAINST THE LIVE API — 2026-07-31.** The docs-only basis below has been
> re-verified with real calls (`cersei-provider::anthropic::tests::live_*`, 4 ignored tests,
> run with a key on `claude-sonnet-5`). Results:
>
> | Body sent | HTTP | API message |
> |---|---|---|
> | What the gate builds (`{type:"adaptive",display:"summarized"}`) | **200** | — |
> | Pre-gate manual form (`{type:"enabled",budget_tokens:N}`) | **400** | `"thinking.type.enabled" is not supported for this model. Use "thinking.type.adaptive" and "output_config.effort" to control thinking behavior.` |
> | Gate output + `temperature: 0.3` | **400** | `` `temperature` may only be set to 1 when thinking is enabled or in adaptive mode. `` |
>
> F-01 is therefore **Confirmed from the API**, not merely from documentation, and the fix is
> confirmed to produce an accepted request. The 400 text also names the exact migration the
> gate implements, which is as direct a corroboration as this finding can get.
>
> **The third message opened a new question — see §10.5 #10.** *"…when thinking is enabled or
> in adaptive mode"* covers the **manual** form too, but `accepts_sampling_params()` returns
> `true` for `Manual`, so Cersei still sends a budget and a caller temperature together on
> 4.6-era models. `live_manual_thinking_plus_temperature_is_the_open_question` settles it;
> it is written to **fail** if that combination 400s.

Primary-source facts (Anthropic docs via the `claude-api` reference; docs-only at the time of writing — since superseded by the live results above):

- `thinking:{type:"enabled",budget_tokens:N}` → **400** on Opus 4.7/4.8, Sonnet 5, Fable 5;
  deprecated-but-functional on Opus/Sonnet 4.6; the only form on 4.5-era and older.
- `temperature`/`top_p`/`top_k` → **400** on that same set.
- Fable 5/Mythos 5: **any** explicit `thinking` value 400s, `{type:"disabled"}` included — omit the key.
- Adaptive default `display:"omitted"` streams **empty** thinking text; `display:"summarized"` opted in.
- Manual budget legal range: `1024 <= budget_tokens < max_tokens`.

Implementation choices (all deliberate):

1. **Gate lives in `build_anthropic_body`** (covers direct + Vertex), *not* `ProviderCapabilities` — per the §6 constraint; F-23 stays open.
2. **`thinking_mode()` classifier enumerates the models that REJECT the manual form** and defaults everything else — unknown Claude ids, dated Claude 4.0 snapshots, Vertex `@`-versions, dotted spellings, and all `ANTHROPIC_BASE_URL` gateway ids — to the legacy manual shape. The default direction matters: an id the build has never seen keeps exactly its pre-gate behaviour, so the gate can only fix requests, never break working ones. (The first draft defaulted to Adaptive; an adversarial review caught the gateway/4.0-snapshot regression and it was inverted.)
3. **Clamp:** `budget.clamp(1024, max_tokens - max_tokens/4)`, logged via `tracing::warn!`; a window too small for the 1024 floor omits thinking entirely. Both bounds enforced (the first draft enforced only the ceiling — caught by the same review).
4. **`thinking_budget = 0` disables thinking in every mode** (this codebase's Gemini-side sentinel, honoured on the Anthropic path too).
5. `--effort max` (32768 vs default `max_tokens` 16384) now clamps to 12288 instead of 400ing; effort semantics on adaptive models are deferred (§10.5 #6).

Verification discipline: RED-first (6 genuine assertion failures before the fix), then a
**10-mutation suite** — clamp removed, gate removed, floor dropped, zero-budget honoured,
sampling gate inverted both ways, AlwaysOn emitting a key, ceiling at `max_tokens-1`,
display opt-in dropped, unknown-id default flipped — every mutant killed by the semantically
correct test. The serde_json f32/f64 equality trap was probed live (`json!(0.3)` ≠ an
f32-widened body value; tests compare `json!(0.3f32)`).

### 10.3 P0 mutation audit — is each fix actually test-bound?

Method: revert each P0 fix site one at a time (22 mutations across 15 items); run
`cargo test --workspace --no-fail-fast`; record whether anything fails. **UNBOUND** = the fix
can be silently reverted with the suite fully green. Tree restored and re-verified at
509/0/14 after every mutation and at the end.

| Item | Fix site reverted | Result |
|---|---|---|
| F-01a | budget clamp | **BOUND** (3 tests) |
| F-01b | model-generation gate | **BOUND** (6) |
| F-02 | anthropic `from_http_status` → untyped | **BOUND** (1) |
| F-02 | openai — same | **BOUND** (4) |
| F-02 | gemini — same | **BOUND** (1) |
| F-03b | swallow `StreamEvent::Error` in accumulator | ⚠️ was UNBOUND → **BOUND 2026-07-30** (§10.7) |
| F-03c | terminal-less EOF → clean `EndTurn` | ⚠️ was UNBOUND → **BOUND 2026-07-30** (§10.7) |
| F-05a | empty args → `null` instead of `{}` | ⚠️ was UNBOUND → **BOUND 2026-07-30** (§10.7) |
| F-05b | drop parse error + raw text | **BOUND** (1) |
| F-A2 | missing `index` collapses to 0 | **BOUND** (1) |
| F-A3 | emit empty id/name | **BOUND** (2) |
| F-04a | compaction call site → naive `len - KEEP` split | ⚠️ was UNBOUND → **BOUND 2026-07-30** (§10.7) |
| F-04b | pre-request orphan check → always empty | ⚠️ was UNBOUND → **BOUND 2026-07-30** (§10.7) |
| F-04c | `pair_aware_split` core | **BOUND** (5) |
| F-06 | error-budget enforcement | **BOUND** (1) |
| F-07 | error branch skips `cap_tool_result` | ⚠️ was UNBOUND → **BOUND 2026-07-30** (§10.7) |
| F-10 | drop `deny_unknown_fields` | **BOUND** (1) |
| F-11 | guard wiring → never refuses | ⚠️ was UNBOUND → **BOUND 2026-07-30** (§10.7) |
| F-A13 | `window_notice` → `None` | **BOUND** (4) |
| F-A14 | strip tool name/schema from errors | **BOUND** (8) |
| F-A15 | strip valid-value list | **BOUND** (5) |

**Score: 15/22 bound.** Fully bound items: F-01, F-02, F-06, F-10, F-A2, F-A3, F-A13,
F-A14, F-A15. Items with unbound halves: **F-03, F-04, F-05, F-07, F-11.**
*(Superseded 2026-07-30, branch `runtime-fix`: all 7 gaps closed, **22/22 bound** — §10.7.)*

**The systemic pattern:** in every gap, the *helper* is unit-tested and the *call site* is
not — and the call site was the original bug. F-11's defect was guard placement; 12 tests
exercise `refusals_for_batch` directly, yet replacing its call with an empty map fails
nothing. F-04's defect was the naive split at the call sites; the split function has 5 tests,
both call sites revert green. F-07's defect was the branch exempting errors; only the helper
is called in tests. F-03 is a variant: it is implemented twice, and only the openai.rs copy
is covered (`sse_pathologies` passes with the `stream.rs` accumulator fixes reverted, because
Anthropic/Gemini's shared accumulator path has no test driving it). The suite validates the
code that was never broken and skips the code that was.

### 10.4 Fix backlog from the audit — new work, priority order — **ALL DONE 2026-07-30, branch `runtime-fix`** (verification in §10.7)

1. **F-11 wiring test** *(highest — silent data-corruption class)*. Drive dispatch with an
   Edit on an unread file; assert (a) the ToolResult is a refusal AND **(b) the file on disk
   is unmodified**. (b) is what fails if the guard ever moves back after dispatch.
   — done: `p0_wiring::edit_of_unread_file_is_refused_and_the_file_is_untouched`.
2. **F-07 branch test.** Push an oversized `is_error` result through dispatch; assert the
   message entering history is capped.
   — done: `p0_wiring::oversized_error_result_is_capped_before_entering_history`.
3. **F-04 call-site tests.** Compact a history whose naive split lands mid-pair; assert no
   orphaned `tool_result` in the result. Separately assert `find_orphaned_tool_results` is
   consulted pre-request (e.g. via the `tracing::error!` it emits).
   — done: `p0_wiring::compaction_through_the_runner_never_orphans_tool_results` (asserts on
   the post-compaction **request body**, the artifact the provider judges) and
   `orphan_check_logging::a_request_carrying_an_orphaned_tool_result_is_reported_before_send`.
4. **F-03 accumulator tests.** Unit-test `StreamAccumulator` directly:
   `apply(StreamEvent::Error)` → `into_response()` is `Err`; terminal-less stream → `Err`.
   Then consider collapsing the two F-03 implementations into one.
   — done: 5 tests in `stream::tests` (error wins over a later terminal event; first error
   beats cascade noise; terminal-less → `Err`; two terminal-signal controls). Collapsing the
   two implementations is deliberately **not** done here — that is a refactor, not a binding.
5. **F-05a test.** Empty and literal-`null` argument payloads both yield `{}`.
   — done: 3 tests in `stream::tests` (no deltas, literal `null`, whitespace-only), plus a
   4th binding F-05b's `__parse_error`/`__raw` preservation at the same seam.

### 10.5 Known gaps — confirmed still open, deliberately not fixed in P0

1. `is_retryable()` covers 429/529 only; Gemini's 503 and transport errors are fatal.
2. `from_http_status` drops the 429 body — `insufficient_quota` is retried 5× as if transient.
3. `find_orphaned_tool_results` checks `tool_result`→`tool_use` only, not the mirror rule.
4. F-02's end-to-end retry test covers OpenAI only; anthropic/gemini are covered at the `complete()` boundary.
5. No test binds runner.rs to `pair_aware_split` (this is §10.4 #3).
6. ~~**`output_config.effort` is never sent**, so `--effort` is inert on adaptive models…~~ **FIXED 2026-07-31.** `build_anthropic_body` now translates the requested thinking budget into `output_config.effort` on Adaptive and AlwaysOn models (the four canonical CLI budgets map 1024→low, 4096→medium, 8192→high, 32768→max; off-canonical budgets land on the nearest level). No cross-crate plumbing was needed — the budget was already the one signal on this wire, and it is injective per effort level. Manual models keep `budget_tokens` and get no `output_config`. Bound by 6 tests; reverting the emission fails 3. Note the deliberate behavior change: a CLI run at the default effort (Medium) now runs adaptive models at `medium` instead of silently at the API default `high` — that is the fix working, not a regression.
7. **Turn-2 hazard on newly-unblocked models:** `cersei-types` serializes `signature: ""` on thinking blocks (`#[serde(default)]` but no `skip_serializing_if`), and the stream accumulator hardcodes an empty signature — echoed history may 400 on adaptive models. F-01 fixed turn 1; turn 2 on `claude-opus-4-8` is **not proven**. Related: §H3's `signature_delta` finding.
8. Opus/Sonnet 4.6 stay on manual+clamp rather than migrating to adaptive (docs: deprecated but functional; migrating is untestable risk on the one working direct path).
9. `display:"summarized"` applies only when a budget is requested; a library caller with `thinking_budget: None` on an adaptive model gets server-side thinking with empty streamed text.
10. ~~The thinking × temperature interaction on *manual* models … could not be sourced in primary docs … One live API call settles it.~~ **PARTLY SETTLED 2026-07-31 — and the review lens looks right.** The live call was made (§10.2). On *adaptive* models `temperature` is a hard **400**, and the gate already drops it, so that half is closed and now test-bound. The API's own wording is `` `temperature` may only be set to 1 when thinking is enabled **or** in adaptive mode `` — and "thinking is enabled" is the **manual** `{type:"enabled"}` form, which is precisely the claim §4.5 discipline had refused to code against for want of a source. It now has one. **CLOSED, and the hole was real.** The live call on `claude-sonnet-4-6` — a `Manual` model — returned **400**: `` `temperature` may only be set to 1 when thinking is enabled. `` So the first F-01 gate fixed the adaptive path and left every `--effort`-driven run on 4.6-era models sending an illegal body. That is the *direct-Anthropic default model*, i.e. the development path §1 already identified as the one configuration that "mostly works". **Fixed 2026-07-31:** `build_anthropic_body` now resolves `thinking` *before* `temperature` and `accepts_sampling_params(thinking_enabled)` takes the request into account, so temperature is dropped whenever a `thinking` key is emitted on a Claude id. The ban is deliberately scoped to Claude ids: non-Claude `ANTHROPIC_BASE_URL` gateway models keep their pre-gate behaviour, on the same reasoning that makes `thinking_mode` default them to the legacy shape. Bound by 2 offline tests (reverting the fix fails both) plus a live pair. **Live-verified 2026-07-31, 5/5:** the corrected manual body returns **200**, and putting the temperature back returns **400** — so the drop is confirmed *required*, not merely cautious.

**Method note worth keeping.** This bug was invisible to code reading, to the docs, and to a 44-test offline suite that *asserted the buggy behaviour as correct*. It surfaced only because a live test printed the API's own error text, and that text described a rule broader than the one being tested. §4.5's discipline — don't code against unsourced provider behavior — was right to defer it, but deferral is not closure: the item sat as "one live API call settles it" through an entire P0 cycle while shipping a guaranteed 400.

### 10.6 Verdict

The P0 list is done and the headline gate is green, but the mutation audit shows the green is
load-bearing for only ~70% of the fix surface. The five unbound halves share one shape —
tested helper, untested wiring — and the wiring was the bug in four of the five cases. §10.4
is therefore the highest-value next increment: five tests, no production code, and it
converts the P0 suite from "the helpers work" to "reverting any P0 fix fails CI."

*Update 2026-07-30: that increment has landed and been re-audited — every P0 fix now fails
CI when reverted. See §10.7.*

### 10.7 Second mutation audit (2026-07-30, branch `runtime-fix`) — the backlog is closed

The §10.4 backlog landed as 13 new tests and zero production-code changes:
`stream.rs::tests` (9, on the shared accumulator the Anthropic/Gemini path streams
through), `cersei-agent/tests/p0_wiring.rs` (3, real runner against a scripted
OpenAI-compatible SSE socket), and `cersei-agent/tests/orphan_check_logging.rs` (1, its own
binary because it installs a global tracing subscriber).

Each of the 7 formerly-unbound sites was then re-reverted, one at a time, exactly as in
§10.3:

| Mutation (from §10.3) | Was | Now |
|---|---|---|
| F-03b · swallow `StreamEvent::Error` in accumulator | ⚠️ UNBOUND | **KILLED** (2 tests) |
| F-03c · terminal-less EOF → clean `EndTurn` | ⚠️ UNBOUND | **KILLED** (1) |
| F-05a · empty args → `null` instead of `{}` | ⚠️ UNBOUND | **KILLED** (4) |
| F-04a · compaction call site → naive `len - KEEP` split | ⚠️ UNBOUND | **KILLED** (1 — fails naming the orphaned `seed_t5` id in the post-compaction request) |
| F-04b · pre-request orphan check → always empty | ⚠️ UNBOUND | **KILLED** (1) |
| F-07 · error branch skips `cap_tool_result` | ⚠️ UNBOUND | **KILLED** (1) |
| F-11 · guard wiring → never refuses | ⚠️ UNBOUND | **KILLED** (1 — fails on the bytes-on-disk assertion, the data-loss half) |

**Score: 22/22 fix sites bound.** Tree restored after every mutation; final gate
`cargo test --workspace`: **522 passed / 0 failed / 14 ignored** (509 prior + 13 new).
Reverting any P0 fix now fails CI. Still open, unchanged: §10.5's known gaps, including
collapsing the two F-03 implementations into one (a refactor the new accumulator tests make
safe to attempt).
