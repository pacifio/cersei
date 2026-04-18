# General Agent Framework Benchmark

How many live agent instances can each framework handle on one host?

This suite benchmarks **Cersei** (Rust) against four Python frameworks — **Agno**,
**LangGraph**, **CrewAI**, **PydanticAI** — on five axes:

| # | Axis | What it answers |
|---|---|---|
| 1 | Instantiation time | How fast can you construct an Agent? |
| 2 | Per-agent memory | How much RAM does one agent cost at rest? |
| 3 | **Max concurrent agents** | How many can you hold live + turn through before p99 > 100 ms? |
| 4 | Graph memory recall under load | (Cersei-only) how fast is recall at 10k / 100k nodes under 100 concurrent agents? |
| 5 | Semantic search under load | (Cersei-only) how fast is HNSW search at 10k / 100k chunks under 100 concurrent agents? |

The first three axes are measured for every framework. The last two are
reported as Cersei-only reference points — the Python frameworks don't ship
an in-process graph or vector index at all, which is itself the point.

## Ground rules (methodology honesty)

- **No real LLM calls.** Inference latency is identical across frameworks and
  masks overhead. Every harness uses a stub provider that returns a canned
  response immediately.
- **One trivial tool** (`echo(msg: str) -> str`) implemented identically.
- **One workload per agent**: `build → 1 turn → shutdown`.
- **Host profile** published with every run (cpu / ram / cgroup cap).
- All results land in `results/<framework>.json` matching a shared schema.

## Running it

Python dependencies are managed with [**uv**](https://docs.astral.sh/uv/)
(install via `curl -LsSf https://astral.sh/uv/install.sh | sh`). `uv` is
~10× faster than `pip` and creates one dedicated venv per framework-extra,
so Agno's deps never collide with LangGraph's.

```bash
# All frameworks (host processes via uv — default)
./run.sh

# Single framework — isolated venv, handled by uv under the hood
./run.sh --only cersei
./run.sh --only agno

# Per-extra manually, no wrapper:
uv run --extra agno        python bench_agno.py
uv run --extra langgraph   python bench_langgraph.py
uv run --extra pydantic_ai python bench_pydantic_ai.py
uv run --extra crewai      python bench_crewai.py

# Opt-in: run Python harnesses in Docker with a cgroup memory cap (useful
# for reproducing the "max concurrent agents under 4 GB" number exactly)
./run.sh --docker
BENCH_MEM_CAP=16g ./run.sh --docker

# Override axis-3 ramp (Python defaults to [100, 500, 1000])
BENCH_STEPS=100,500,1000,5000 ./run.sh
```

`run.sh` runs each harness, writes `results/<framework>.json`, then
`aggregate.py` merges everything into `results/summary.json`.

### Why smaller scale for Python

Python frameworks hit asyncio / GIL scaling walls around 1–5k concurrent
coroutines, so axis-3 defaults cap there. Cersei's harness sweeps to 10k+
because Rust + tokio actually gets there without thrashing. The difference
*is* the story — the scale caps aren't "hiding" anything.

## Re-running just Cersei

From the repo root:

```bash
cargo run --release -p cersei-agent --example general_agent_bench \
  --features bench-full
```

Output: `bench/general-agents/results/cersei.json`.

Axis selection via env var:

```bash
CERSEI_BENCH_AXES=1,2,3 cargo run ...
```

## Shared output schema

See `results/cersei.json` for the canonical shape. Every harness emits:

```json
{
  "framework": "<id>",
  "version": "<framework version>",
  "host": { "os": "...", "arch": "...", "cpu": "...", "ram_gb": N, "cgroup_memory_gb": N },
  "axis_1_instantiation_us":   { "p50": ..., "p95": ..., "p99": ..., "mean": ..., "samples": ... },
  "axis_2_per_agent_bytes":    { "mean_bytes": ..., "samples": ..., "allocator": "..." },
  "axis_3_max_concurrent":     [ { "n": ..., "p50_ms": ..., "p99_ms": ..., "rss_mb": ..., "wall_ms": ... }, ... ],
  "axis_4_graph_recall_us":    { "at_10k": {...}, "at_100k": {...} } | null,
  "axis_5_semantic_search_us": { "at_10k": {...}, "at_100k": {...} } | null
}
```

## Contributing a new framework

1. Add a new `bench_<framework>.py` script that implements axes 1–3 and writes
   the same JSON shape to `results/<framework>.json`. (The `bench_` prefix is
   required — naming the file after the package shadows its imports.)
2. Pin it in `pyproject.toml` or add its own `requirements.txt`.
3. Add a service in `docker-compose.yml` mirroring the others.
4. Open a PR.

## Why we don't publish "vendor numbers" as our own

Our docs page cites Agno's published numbers until we re-run their harness
in a matched environment ourselves. The `run.sh` script does exactly that;
the results/summary.json produced by a reader should reproduce what we publish.

See the [docs page](../../docs/content/docs/bench-vs-agents.mdx) for the rendered
comparison and charts.
