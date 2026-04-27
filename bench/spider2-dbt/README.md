# Spider2-DBT Bench

Cersei's run of the [Spider2.0 dbt](https://github.com/xlang-ai/Spider2) 64-task benchmark.
Targets ≥ 65 % pass rate, beating SignalPilot's 51.6 % (Sonnet 4.6 + their MCP gateway)
by switching the answerer to `gemini-3.1-pro-preview` and adding the 0.1.8 cersei
primitives (delegate-based verifier + auditor, pre-loaded skill pack) on top.

## Setup

```bash
./bench/spider2-dbt/setup.sh        # clones xlang-ai/Spider2 if missing
# Make sure .env in the repo root contains GOOGLE_API_KEY (gitignored).
# We use header auth (x-goog-api-key); never put the key in URL query strings.
source .env
```

You also need `dbt` and `duckdb` on `$PATH` for the agent's bash tool to use:

```bash
pipx install 'dbt-core[duckdb]'  # or pip install
brew install duckdb              # or any binary install
```

For Linux runs that need date determinism, install `libfaketime` and pass
`--deterministic-dates` plus `--gold-dates path/to/gold_build_dates.json`. macOS
runs skip libfaketime and accept the date drift.

## Run

```bash
# Smoke (5 tasks, Phase-1 only — ~3 min, ~$2-3)
cargo run --release -p spider2-dbt-bench -- \
  --suite smoke --concurrency 2 \
  --model gemini-3.1-pro-preview

# Full Phase-2 stack (verifier + auditor + 1 repair pass + 2nd-model fallback)
cargo run --release -p spider2-dbt-bench -- \
  --suite full --concurrency 4 \
  --model gemini-3.1-pro-preview \
  --max-turns 80 \
  --verifier --auditor \
  --repair-pass 1 \
  --repair-model gemini-2.5-flash \
  --child-max-turns 60
```

**Phase-2 flags:**
- `--verifier` — spawn a verifier subagent after the main run (7-point checklist).
- `--auditor` — spawn a fan-out / cardinality / surrogate-key auditor in parallel
  with the verifier via `cersei_agent::delegate::run_batch` (depth-2, blocklist).
- `--repair-pass 1` — if the comparator fails after verifier+auditor, re-run the
  primary model with the failure list pinned in context (`max_turns=30`).
- `--repair-model gemini-2.5-flash` — if repair pass also fails, hand the same
  failure list to a cheaper sibling for a fresh attempt.

Inspect-only mode (no agent calls):

```bash
cargo run --release -p spider2-dbt-bench -- --dry-run --suite full
```

## Outputs

Per run, the harness writes `bench/spider2-dbt/results/summary-spider2-dbt.json`
(gitignored). One entry per task with pass/fail, turn count, elapsed ms, and a
truncated comparator trace. Workdirs land under `bench/spider2-dbt/workdirs/`
(also gitignored — they contain the agent's intermediate dbt projects).

## Layout

```
bench/spider2-dbt/
├── Cargo.toml
├── README.md
├── setup.sh                    # clones xlang-ai/Spider2 to $SPIDER2_DBT_DIR
├── prompts/
│   └── system.md               # main-agent system prompt (skills appended)
├── skills/                     # verbatim ports of SignalPilot SKILL.md files
│   ├── dbt-workflow.md
│   ├── dbt-write.md
│   ├── dbt-debugging.md
│   └── duckdb-sql.md
└── src/
    ├── main.rs                 # CLI + dispatch + bounded concurrency
    ├── dataset.rs              # JSONL task loader + eval params
    ├── workdir.rs              # per-task TempDir + reference_snapshot.md
    ├── dates.rs                # libfaketime env injection (Linux)
    ├── runner.rs               # main agent → comparator
    ├── comparator.rs           # verbatim port of SignalPilot evaluator
    └── report.rs               # summary JSON writer
```

## Design notes

- **Comparator parity** with the official Spider2-DBT evaluator is the constant —
  `abs_tol=1e-2`, NaN-aware, ignore-order sort key `(0,0.0,"")`, `fct_↔fact_`
  resolution. Tests under `cargo test -p spider2-dbt-bench` lock this down.
- **No MCP gateway** — the agent uses `bash` + `Read`/`Write`/`Edit`/`Glob`/`Grep`
  to drive `dbt` and `duckdb` directly. The dbt-aware tools other harnesses
  expose (project map, query, schema check) are thin glue around the same shell
  calls; the cersei tool surface covers them without a custom server.
- **Skills pre-loaded** — all four dbt skills are concatenated into the system
  prompt at startup instead of being lazy-loaded mid-turn. Saves ~500 tokens
  per turn over a long agent budget.
- **Phase-2** — verifier + auditor run in parallel via
  `cersei_agent::delegate::run_batch` (depth-2, blocklist, JoinSet). On
  comparator failure, the runner triggers up to one repair pass on the primary
  model and an optional second pass on `--repair-model` before recording the
  verdict. Each phase is flag-gated so we can ablate which lever moves the
  number.
- **Security** — `GOOGLE_API_KEY` flows from gitignored `.env` only. The Gemini
  provider uses `x-goog-api-key` header auth (never URL query string — that's
  the 0.1.7 leak vector we ripped out). Bench artefacts are gitignored before
  any run.
