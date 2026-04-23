# LongMemEval — Cersei memory head-to-head

Runs the 500-question [LongMemEval benchmark](https://arxiv.org/abs/2410.10813)
(ICLR 2025) against four memory configurations built on Cersei primitives, so
we can publish numbers line-for-line comparable to Mastra's
[*Observational Memory*](https://mastra.ai/research/observational-memory)
research, the Zep Graphiti paper, and Supermemory's claims.

## Quick start

```bash
# 1. Download the datasets (oracle: 15 MB, small: 265 MB, medium: 2.6 GB)
./setup.sh                     # pulls all three variants; ~1–2 min on LFS
./setup.sh oracle              # or one at a time
./setup.sh s

# 2. Export your OpenAI key (used for embeddings + judge + answerer + extractor)
export OPENAI_API_KEY=sk-...

# 3. Smoke: 10 questions × 4 configs on the small oracle set (~30 seconds, <$0.05)
cargo run --release -p longmem-bench -- \
  --dataset oracle --config all --limit 10

# 4. Full run: 500 questions × 4 configs on longmemeval_s (~30–60 min, ~$15–20)
cargo run --release -p longmem-bench -- \
  --dataset s --config all --concurrency 8
```

Outputs land in `./results/` as `<config>-<dataset>.json` summaries plus
`<config>-rows-<dataset>.json` per-question traces, and one roll-up
`summary-<dataset>.json`.

## Configurations

| Config | Backend | What it tests |
|---|---|---|
| **A. `baseline`** | `JsonlMemory` full haystack in prompt | Control lower bound — how well does the answerer do with perfect recall? |
| **B. `embed`** | `EmbeddingMemory` (usearch HNSW, cosine) | Pure semantic retrieval. Direct comparison to Mastra's RAG mode. |
| **C. `graph`** | `GraphMemory` (grafeo) substring+rank | Weakest Cersei config — the honest floor. |
| **D. `hybrid`** | LLM fact extractor → EmbeddingMemory + GraphMemory → RRF fusion | Head-to-head with Mastra's *observational memory* and Zep's Graphiti. |

## Where the numbers come from

- **Dataset**: [`xiaowu0162/longmemeval`](https://huggingface.co/datasets/xiaowu0162/longmemeval), unchanged. Three variants ship: `longmemeval_s` (~115 k tokens / Q, the headline variant), `longmemeval_m` (~1.5 M tokens / Q), `longmemeval_oracle` (evidence-only control).
- **Judge**: `gpt-4o-mini`, temperature 0, with the six question-type rubrics **ported verbatim** from Mastra's TypeScript harness (which in turn copies from the official Python evaluator). See `src/judge.rs` — no prompt is modified.
- **Answerer / extractor**: `gpt-4o-mini` by default. Override with `--answerer-model` / `--extractor-model`.
- **Metric**: `overall_accuracy` is the **macro average** across question types excluding abstention (matching Mastra's `cli.ts:99`). Abstention is reported as a separate `abstention_accuracy` field.

## Credits

- Benchmark & rubric prompts: [Di Wu et al., ICLR 2025](https://arxiv.org/abs/2410.10813) · [official repo](https://github.com/xiaowu0162/LongMemEval).
- Harness shape, question-type handling, abstention detection (`question_id.ends_with("_abs")`): adapted from Mastra's [`@mastra/longmemeval`](https://github.com/mastra-ai/mastra/tree/main/explorations/longmemeval).

## What this benchmark is **not**

- It's not a general "does Cersei beat Mastra at everything" test. It measures long-term conversational recall under a specific rubric.
- It's not apples-to-apples against systems that let the agent call tools during answering (LongMemEval is a single-shot recall benchmark — the retrieval is fixed before the answerer LLM runs).
- It's not cheap to run at scale — each full pass on `longmemeval_s` is ~$15–20 in OpenAI API cost.
