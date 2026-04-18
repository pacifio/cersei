"""Shared utilities for the Python harnesses — measurement helpers + JSON writer."""
from __future__ import annotations

import gc
import importlib.metadata
import json
import os
import platform
import resource
import statistics
import time
import tracemalloc
from dataclasses import asdict, dataclass
from pathlib import Path
from typing import Any, Callable, Optional

RESULTS_DIR = Path(__file__).parent / "results"
RESULTS_DIR.mkdir(exist_ok=True)

# Python frameworks hit asyncio scaling walls long before Rust does. Capping
# at 1k by default keeps each harness under 3 minutes wall-clock on a laptop.
# Override via BENCH_STEPS="100,500,1000,5000" if you want to push higher.
DEFAULT_STEPS = [int(x) for x in os.environ.get("BENCH_STEPS", "100,500,1000").split(",")]
INSTANTIATION_ITERS = int(os.environ.get("BENCH_INSTANTIATION_ITERS", "1000"))
MEMORY_ITERS        = int(os.environ.get("BENCH_MEMORY_ITERS", "500"))


@dataclass
class LatencyStats:
    p50: float
    p95: float
    p99: float
    mean: float
    samples: int


def percentile(sorted_vals: list[float], p: float) -> float:
    if not sorted_vals:
        return 0.0
    idx = round((len(sorted_vals) - 1) * p)
    return sorted_vals[min(idx, len(sorted_vals) - 1)]


def summarize(samples_us: list[float]) -> LatencyStats:
    s = sorted(samples_us)
    return LatencyStats(
        p50=percentile(s, 0.50),
        p95=percentile(s, 0.95),
        p99=percentile(s, 0.99),
        mean=statistics.fmean(s) if s else 0.0,
        samples=len(s),
    )


def package_version(package: str) -> str:
    """Look up a package's version via importlib.metadata, falling back to
    the module's __version__ attribute, then to 'unknown'.

    Needed because many frameworks (agno, some langgraph releases) don't
    export __version__ on the top-level module.
    """
    try:
        return importlib.metadata.version(package)
    except importlib.metadata.PackageNotFoundError:
        pass
    try:
        mod = __import__(package.replace("-", "_"))
        return getattr(mod, "__version__", "unknown")
    except ImportError:
        return "unknown"


def host_info() -> dict[str, Any]:
    try:
        ram_gb = os.sysconf("SC_PAGE_SIZE") * os.sysconf("SC_PHYS_PAGES") // (1024**3)
    except (ValueError, OSError):
        ram_gb = 0

    cgroup = None
    cgroup_path = Path("/sys/fs/cgroup/memory.max")
    if cgroup_path.exists():
        try:
            raw = cgroup_path.read_text().strip()
            if raw != "max":
                cgroup = int(raw) // (1024**3)
        except (ValueError, OSError):
            pass

    return {
        "os": platform.system().lower(),
        "arch": platform.machine(),
        "cpu": platform.processor() or "unknown",
        "ram_gb": ram_gb,
        "cgroup_memory_gb": cgroup,
    }


def read_rss_mb() -> float:
    """Per-process RSS in MB. macOS reports bytes; Linux reports KB."""
    rss = resource.getrusage(resource.RUSAGE_SELF).ru_maxrss
    if platform.system() == "Darwin":
        return rss / (1024 * 1024)
    return rss / 1024


# ── Axis 1 — instantiation ─────────────────────────────────────────────────

def bench_instantiation(build: Callable[[], Any], iters: int = 1000, warmup: int = 100) -> LatencyStats:
    for _ in range(warmup):
        build()
    samples: list[float] = []
    for _ in range(iters):
        t = time.perf_counter_ns()
        agent = build()
        samples.append((time.perf_counter_ns() - t) / 1000.0)  # μs
        del agent
    return summarize(samples)


# ── Axis 2 — per-agent memory via tracemalloc ──────────────────────────────

def bench_per_agent_memory(build: Callable[[], Any], iters: int = 1000) -> dict[str, Any]:
    gc.collect()
    tracemalloc.start()
    baseline_snap = tracemalloc.take_snapshot()
    agents = [build() for _ in range(iters)]
    after_snap = tracemalloc.take_snapshot()
    diff = after_snap.compare_to(baseline_snap, "filename")
    total = sum(stat.size_diff for stat in diff if stat.size_diff > 0)
    tracemalloc.stop()
    mean_bytes = total // iters if iters else 0
    # hold a reference to agents until after we've taken the snapshot
    _ = len(agents)
    return {"mean_bytes": int(mean_bytes), "samples": iters, "allocator": "tracemalloc (size_diff)"}


# ── Axis 3 — concurrent construction + hold ───────────────────────────────
#
# We deliberately do NOT invoke the agent here: Agno's own cookbook doesn't
# either (`cookbook/09_evals/performance/`), and doing so would require each
# framework to ship an in-process LLM stub — which they don't — or force a
# network call that bottlenecks on HTTP latency instead of framework overhead.
#
# Instead we measure the pure concurrent-construction scalability question:
# how fast can you build N live agents, and how much RSS do N of them hold?
# That IS the question "can I handle 10k customer sessions in one process?"

async def bench_max_concurrent(
    build: Callable[[], Any],    # sync factory returning one agent
    steps: list[int],
) -> list[dict[str, Any]]:
    import asyncio

    out = []
    for n in steps:
        gc.collect()
        wall_start = time.perf_counter_ns()

        async def one():
            t = time.perf_counter_ns()
            # Run construction off the event loop so GIL contention surfaces.
            agent = await asyncio.to_thread(build)
            return agent, (time.perf_counter_ns() - t) / 1_000_000.0  # ms

        results = await asyncio.gather(*[one() for _ in range(n)])
        wall_ms = (time.perf_counter_ns() - wall_start) / 1_000_000.0
        agents, latencies = zip(*results)
        latencies = sorted(latencies)
        p50 = percentile(latencies, 0.50)
        p99 = percentile(latencies, 0.99)
        rss_mb = read_rss_mb()
        print(f"  axis-3 n={n:>6}  p50={p50:>7.2f}ms  p99={p99:>7.2f}ms  rss={rss_mb:>7.1f}MB  wall={wall_ms:>7.1f}ms")
        out.append({"n": n, "p50_ms": p50, "p99_ms": p99, "rss_mb": rss_mb, "wall_ms": wall_ms})
        # Explicitly drop before the next step so RSS readings don't accumulate
        del agents
        del results
        gc.collect()
    return out


# ── Report writer ──────────────────────────────────────────────────────────

def write_report(
    framework: str,
    version: str,
    axis_1: Optional[LatencyStats],
    axis_2: Optional[dict[str, Any]],
    axis_3: Optional[list[dict[str, Any]]],
) -> Path:
    report = {
        "framework": framework,
        "version": version,
        "host": host_info(),
        "axis_1_instantiation_us": asdict(axis_1) if axis_1 else None,
        "axis_2_per_agent_bytes": axis_2,
        "axis_3_max_concurrent": axis_3,
        "axis_4_graph_recall_us": None,       # not applicable to Python frameworks
        "axis_5_semantic_search_us": None,    # not applicable to Python frameworks
    }
    path = RESULTS_DIR / f"{framework}.json"
    path.write_text(json.dumps(report, indent=2))
    return path
