"""Agno benchmark harness — matches Agno's own cookbook methodology.

Axes 1 & 2 mirror `cookbook/09_evals/performance/instantiate_agent_with_tool.py`
from the Agno source tree exactly: construct `Agent(model=OpenAIChat(id=...), tools=[...])`
1000 times, measure per-construction latency and held-live memory.

Axis 3 extends the same pattern to concurrent construction (see _common.py
for why we don't invoke the agent).

Install:  uv run --extra agno python bench_agno.py
Output:   results/agno.json
"""
from __future__ import annotations

import asyncio
import os
from typing import Literal

# Agno's OpenAI client reads OPENAI_API_KEY in its constructor; we never
# actually make an API call but we need *something* there.
os.environ.setdefault("OPENAI_API_KEY", "sk-benchmark-not-real")

from _common import (
    DEFAULT_STEPS,
    INSTANTIATION_ITERS,
    MEMORY_ITERS,
    bench_instantiation,
    bench_max_concurrent,
    bench_per_agent_memory,
    package_version,
    write_report,
)


# ── Benchmark tool (copied from Agno's cookbook for fidelity) ─────────────

def get_weather(city: Literal["nyc", "sf"]):
    """Use this to get weather information."""
    if city == "nyc":
        return "It might be cloudy in nyc"
    elif city == "sf":
        return "It's always sunny in sf"


def _build_agent():
    from agno.agent import Agent
    from agno.models.openai import OpenAIChat
    return Agent(model=OpenAIChat(id="gpt-4o"), tools=[get_weather])


async def main():
    import agno  # noqa: F401
    version = package_version("agno")
    print(f"\nagno {version}  — general-agent benchmark")

    print(f"\n[axis 1] instantiation ({INSTANTIATION_ITERS} samples)...")
    axis_1 = bench_instantiation(_build_agent, iters=INSTANTIATION_ITERS, warmup=100)
    print(f"  p50={axis_1.p50:.2f}us  p95={axis_1.p95:.2f}us  p99={axis_1.p99:.2f}us  mean={axis_1.mean:.2f}us")

    print(f"\n[axis 2] per-agent memory ({MEMORY_ITERS} held live)...")
    axis_2 = bench_per_agent_memory(_build_agent, iters=MEMORY_ITERS)
    print(f"  mean_bytes={axis_2['mean_bytes']} ({axis_2['allocator']})")

    print(f"\n[axis 3] concurrent construction (ramp: {DEFAULT_STEPS})...")
    axis_3 = await bench_max_concurrent(_build_agent, steps=DEFAULT_STEPS)

    path = write_report("agno", version, axis_1, axis_2, axis_3)
    print(f"\nReport written: {path}")


if __name__ == "__main__":
    asyncio.run(main())
