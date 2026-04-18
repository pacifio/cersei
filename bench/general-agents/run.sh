#!/usr/bin/env bash
# Top-level driver for the general-agent framework benchmark.
#
# Usage:
#   ./run.sh                         # All frameworks, host process via uv
#   ./run.sh --only cersei           # Just Cersei
#   ./run.sh --only agno             # Just one Python framework
#   ./run.sh --docker                # Run Python frameworks in Docker with
#                                    # a memory cap (for max-concurrent enforcement)
#   BENCH_MEM_CAP=4g ./run.sh --docker
#   BENCH_STEPS=100,500,1000,5000 ./run.sh  # Override axis-3 ramp
#
# Requirements:
#   - Rust toolchain (for Cersei)
#   - uv  — `curl -LsSf https://astral.sh/uv/install.sh | sh`
#   - Docker is optional — only needed for --docker mode

set -euo pipefail

cd "$(dirname "$0")"

BENCH_MEM_CAP="${BENCH_MEM_CAP:-4g}"
BENCH_STEPS="${BENCH_STEPS:-100,500,1000}"
ONLY=""
USE_DOCKER=0

while [ $# -gt 0 ]; do
    case "$1" in
        --only) ONLY="$2"; shift 2 ;;
        --docker) USE_DOCKER=1; shift ;;
        -h|--help)
            sed -n '2,16p' "$0"; exit 0 ;;
        *) echo "unknown arg: $1"; exit 1 ;;
    esac
done

mkdir -p results

have() { command -v "$1" >/dev/null 2>&1; }
run_if_matching() {
    [ -z "$ONLY" ] || [ "$ONLY" = "$1" ]
}

# ── Cersei (native, on the host) ──────────────────────────────────────────

if run_if_matching cersei; then
    echo "━━━ cersei ━━━"
    (cd ../.. && cargo run --release -p cersei-agent --example general_agent_bench --features bench-full)
fi

# ── Python frameworks ─────────────────────────────────────────────────────

have uv || {
    echo "error: uv is not installed. Install it with:" >&2
    echo "  curl -LsSf https://astral.sh/uv/install.sh | sh" >&2
    exit 1
}

run_python_uv() {
    local framework="$1"
    echo "━━━ $framework ━━━"
    BENCH_STEPS="$BENCH_STEPS" uv run --extra "$framework" python "bench_${framework}.py"
}

run_python_docker() {
    local framework="$1"
    echo "━━━ $framework (docker, mem=$BENCH_MEM_CAP) ━━━"
    have docker || {
        echo "docker not installed — skipping $framework" >&2; return
    }
    docker run --rm \
        --memory="$BENCH_MEM_CAP" \
        -e BENCH_STEPS="$BENCH_STEPS" \
        -v "$PWD":/bench \
        -w /bench \
        ghcr.io/astral-sh/uv:python3.12-bookworm-slim \
        uv run --extra "$framework" python "bench_${framework}.py"
}

for fw in agno langgraph pydantic_ai crewai; do
    if run_if_matching "$fw"; then
        if [ "$USE_DOCKER" = "1" ]; then
            run_python_docker "$fw"
        else
            run_python_uv "$fw"
        fi
    fi
done

# ── Aggregate ─────────────────────────────────────────────────────────────

echo
echo "━━━ aggregating ━━━"
uv run --no-sync python aggregate.py 2>/dev/null || python3 aggregate.py

echo
echo "Done. Results in $(pwd)/results/  —  summary.json lists every framework."
