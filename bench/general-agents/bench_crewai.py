"""CrewAI benchmark harness — matches Agno's cookbook methodology.

Mirrors Agno's cookbook `crewai_instantiation.py` methodology:
constructs a CrewAI `Agent` with `llm="gpt-4o"` (string form — lazy, no
network) and a `@tool`-decorated weather function.

Install:  uv run --extra crewai python bench_crewai.py
Output:   results/crewai.json
"""
from __future__ import annotations

import asyncio
import os

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


_TOOLS = None


def _make_tools():
    from crewai.tools import tool

    # CrewAI's pydantic schema generator struggles with Literal at runtime
    # inside a benchmark harness (model_json_schema forward-ref resolution),
    # so we use plain `str` — functionally identical for construction perf.
    @tool("Weather")
    def get_weather(city: str) -> str:
        """Use this to get weather information."""
        if city == "nyc":
            return "It might be cloudy in nyc"
        elif city == "sf":
            return "It's always sunny in sf"
        return "Unknown city"

    return [get_weather]


def _build_agent():
    global _TOOLS
    from crewai.agent import Agent
    if _TOOLS is None:
        _TOOLS = _make_tools()
    return Agent(
        llm="gpt-4o",
        role="Test Agent",
        goal="Be concise, reply with one sentence.",
        tools=_TOOLS,
        backstory="Test",
    )


async def main():
    import crewai  # noqa: F401
    version = package_version("crewai")
    print(f"\ncrewai {version}  — general-agent benchmark")

    print(f"\n[axis 1] instantiation ({INSTANTIATION_ITERS} samples)...")
    axis_1 = bench_instantiation(_build_agent, iters=INSTANTIATION_ITERS, warmup=100)
    print(f"  p50={axis_1.p50:.2f}us  p95={axis_1.p95:.2f}us  p99={axis_1.p99:.2f}us  mean={axis_1.mean:.2f}us")

    print(f"\n[axis 2] per-agent memory ({MEMORY_ITERS} held live)...")
    axis_2 = bench_per_agent_memory(_build_agent, iters=MEMORY_ITERS)
    print(f"  mean_bytes={axis_2['mean_bytes']} ({axis_2['allocator']})")

    print(f"\n[axis 3] concurrent construction (ramp: {DEFAULT_STEPS})...")
    axis_3 = await bench_max_concurrent(_build_agent, steps=DEFAULT_STEPS)

    path = write_report("crewai", version, axis_1, axis_2, axis_3)
    print(f"\nReport written: {path}")


if __name__ == "__main__":
    asyncio.run(main())
