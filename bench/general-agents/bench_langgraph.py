"""LangGraph benchmark harness — matches Agno's cookbook methodology.

Mirrors `_inspirations/agno/cookbook/09_evals/performance/comparison/langgraph_instantiation.py`:
uses `langgraph.prebuilt.create_react_agent` with a real `ChatOpenAI` model
(lazy, no network on construction) plus a `@tool`-decorated weather function.

Install:  uv run --extra langgraph python bench_langgraph.py
Output:   results/langgraph.json
"""
from __future__ import annotations

import asyncio
import os
from typing import Literal

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


# ── Tool (Agno cookbook pattern) ───────────────────────────────────────────

def _make_tool():
    from langchain_core.tools import tool

    @tool
    def get_weather(city: Literal["nyc", "sf"]):
        """Use this to get weather information."""
        if city == "nyc":
            return "It might be cloudy in nyc"
        elif city == "sf":
            return "It's always sunny in sf"
        else:
            raise AssertionError("Unknown city")

    return get_weather


# Build tool once; LangGraph tools are stateless so reuse is safe.
_TOOLS = None


def _build_agent():
    global _TOOLS
    from langchain_openai import ChatOpenAI
    from langgraph.prebuilt import create_react_agent
    if _TOOLS is None:
        _TOOLS = [_make_tool()]
    return create_react_agent(model=ChatOpenAI(model="gpt-4o"), tools=_TOOLS)


async def main():
    import langgraph  # noqa: F401
    version = package_version("langgraph")
    print(f"\nlanggraph {version}  — general-agent benchmark")

    print(f"\n[axis 1] instantiation ({INSTANTIATION_ITERS} samples)...")
    axis_1 = bench_instantiation(_build_agent, iters=INSTANTIATION_ITERS, warmup=100)
    print(f"  p50={axis_1.p50:.2f}us  p95={axis_1.p95:.2f}us  p99={axis_1.p99:.2f}us  mean={axis_1.mean:.2f}us")

    print(f"\n[axis 2] per-agent memory ({MEMORY_ITERS} held live)...")
    axis_2 = bench_per_agent_memory(_build_agent, iters=MEMORY_ITERS)
    print(f"  mean_bytes={axis_2['mean_bytes']} ({axis_2['allocator']})")

    print(f"\n[axis 3] concurrent construction (ramp: {DEFAULT_STEPS})...")
    axis_3 = await bench_max_concurrent(_build_agent, steps=DEFAULT_STEPS)

    path = write_report("langgraph", version, axis_1, axis_2, axis_3)
    print(f"\nReport written: {path}")


if __name__ == "__main__":
    asyncio.run(main())
