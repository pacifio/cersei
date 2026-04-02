#!/usr/bin/env bash
# Cersei — Run benchmarks
# Usage: ./run_benchmark.sh [--full] [--memory] [--usage]

set -e
cd "$(dirname "$0")"

CYAN='\033[36m'
GREEN='\033[32m'
RESET='\033[0m'

echo -e "${CYAN}Cersei — Benchmark Suite${RESET}"
echo "========================="

FULL=false
MEMORY=false
USAGE=false
for arg in "$@"; do
    case $arg in
        --full)   FULL=true ;;
        --memory) MEMORY=true ;;
        --usage)  USAGE=true ;;
    esac
done

# Tool I/O benchmark (always runs)
echo -e "\n${CYAN}[1] Tool I/O Benchmark${RESET}"
cargo run --example benchmark_io --release 2>&1 | grep -E "avg=|Combined|Fastest|Slowest|Cersei.*faster|std::fs"

# Memory benchmark (from stress test)
if $MEMORY || $FULL; then
    echo -e "\n${CYAN}[2] Memory I/O Benchmark${RESET}"
    cargo run --example stress_memory --release 2>&1 | grep -E "μs|Performance"
fi

# Token/cost usage
if $USAGE || $FULL; then
    echo -e "\n${CYAN}[3] Token Usage Report${RESET}"
    cargo run --example usage_report --release 2>&1 | grep -E "Token|Cost|Billing|tokens|cost|\$"
fi

# Full standalone benchmark
if $FULL; then
    echo -e "\n${CYAN}[4] Standalone Benchmark Suite${RESET}"
    cd examples/benchmark
    cargo run --release 2>&1 | grep -E "avg|Comparison|Markdown|Tool|std::fs"
    cd ../..
fi

echo -e "\n${GREEN}Benchmark complete.${RESET}"
