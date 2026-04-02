#!/usr/bin/env bash
# Abstract vs Codex CLI — Tool Benchmark
# Usage: ./run_tool_bench_codex.sh [--iterations N] [--full]
#
# Compares Abstract CLI against OpenAI Codex CLI across:
#   1. Startup time
#   2. Binary size
#   3. Memory usage (RSS)
#   4. Subcommand latency
#   5. Tool I/O dispatch (SDK-level)
#   6. Memory architecture
#   7. Agentic benchmark
#
# Requires: abstract (cargo install --path crates/abstract-cli)
#           codex   (npm i -g @openai/codex)

set -e
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
cd "$SCRIPT_DIR"

# Codex requires a git repo — create a temp one for benchmarks
CODEX_BENCH_DIR=$(mktemp -d)
git init -q "$CODEX_BENCH_DIR" 2>/dev/null
echo "benchmark" > "$CODEX_BENCH_DIR/README.md"
(cd "$CODEX_BENCH_DIR" && git add . && git commit -m "init" -q 2>/dev/null)
trap "rm -rf '$CODEX_BENCH_DIR'" EXIT

CYAN='\033[36m'
GREEN='\033[32m'
YELLOW='\033[33m'
RED='\033[31m'
DIM='\033[90m'
BOLD='\033[1m'
RESET='\033[0m'

ITERS=50
FULL=false
for arg in "$@"; do
    case $arg in
        --iterations) shift; ITERS=$1 ;;
        --iterations=*) ITERS="${arg#*=}" ;;
        --full) FULL=true ;;
    esac
done

HAS_CODEX=false
if command -v codex &>/dev/null; then
    HAS_CODEX=true
fi

HAS_ABSTRACT=false
if command -v abstract &>/dev/null; then
    HAS_ABSTRACT=true
fi

echo -e "${CYAN}${BOLD}Abstract vs Codex CLI — Benchmark${RESET}"
echo "======================================"
echo -e "${DIM}Iterations: ${ITERS} | Date: $(date '+%Y-%m-%d %H:%M') | Platform: $(uname -m) $(uname -s)${RESET}"
if $HAS_CODEX; then
    echo -e "${DIM}Codex version: $(codex --version 2>&1)${RESET}"
fi
echo ""

if ! $HAS_ABSTRACT; then
    echo -e "${RED}abstract not found in PATH.${RESET}"
    echo "Install with: cargo install --path crates/abstract-cli"
    exit 1
fi

# ─── Helper ─────────────────────────────────────────────────────────────────

bench_cmd() {
    local label="$1"
    local cmd="$2"
    local n="$3"
    local total=0
    local min=999999
    local max=0

    for i in $(seq 1 "$n"); do
        local start end elapsed
        start=$(python3 -c "import time; print(time.time_ns())")
        eval "$cmd" >/dev/null 2>&1
        end=$(python3 -c "import time; print(time.time_ns())")
        elapsed=$(python3 -c "print(($end - $start) / 1_000_000)")
        total=$(python3 -c "print($total + $elapsed)")
        min=$(python3 -c "print(min($min, $elapsed))")
        max=$(python3 -c "print(max($max, $elapsed))")
    done

    local avg
    avg=$(python3 -c "print(f'{$total / $n:.2f}')")
    min=$(python3 -c "print(f'{$min:.2f}')")
    max=$(python3 -c "print(f'{$max:.2f}')")
    printf "  %-25s avg=%7sms  min=%7sms  max=%7sms  (%d iters)\n" "$label" "$avg" "$min" "$max" "$n"
}

# ─── 1. Startup Time ───────────────────────────────────────────────────────

echo -e "${CYAN}[1] Startup Time${RESET}"
echo -e "${DIM}    Measuring --version invocation time${RESET}"
echo ""

bench_cmd "abstract --version" "abstract --version" "$ITERS"

if $HAS_CODEX; then
    bench_cmd "codex --version" "codex --version" "$ITERS"
    echo ""

    abs_avg=$(python3 -c "
import subprocess, time
total = 0
for _ in range($ITERS):
    s = time.time_ns()
    subprocess.run(['abstract', '--version'], capture_output=True)
    total += time.time_ns() - s
print(total / $ITERS / 1e6)
")
    cx_avg=$(python3 -c "
import subprocess, time
total = 0
for _ in range($ITERS):
    s = time.time_ns()
    subprocess.run(['codex', '--version'], capture_output=True)
    total += time.time_ns() - s
print(total / $ITERS / 1e6)
")
    ratio=$(python3 -c "print(f'{$cx_avg / $abs_avg:.1f}')")
    echo -e "  ${GREEN}Abstract is ${ratio}x faster at startup${RESET}"
else
    echo ""
    echo -e "  ${YELLOW}codex not in PATH — skipping comparison${RESET}"
fi

# ─── 2. Binary Size ────────────────────────────────────────────────────────

echo ""
echo -e "${CYAN}[2] Binary Size${RESET}"
echo ""

abs_path=$(which abstract)
abs_size=$(stat -f%z "$abs_path" 2>/dev/null || stat -c%s "$abs_path" 2>/dev/null)
abs_human=$(python3 -c "print(f'{$abs_size / 1024 / 1024:.1f}MB')")
printf "  %-25s %s (%s)\n" "abstract" "$abs_human" "$abs_path"

if $HAS_CODEX; then
    cx_path=$(which codex)
    # codex is likely a symlink or Node.js script — follow it
    cx_real=$(readlink -f "$cx_path" 2>/dev/null || readlink "$cx_path" 2>/dev/null || echo "$cx_path")
    # Get the package directory size (codex is a Node.js app)
    cx_pkg_dir=$(dirname "$(dirname "$cx_real")")
    if [ -d "$cx_pkg_dir/lib/node_modules/@openai/codex" ]; then
        cx_size=$(du -sk "$cx_pkg_dir/lib/node_modules/@openai/codex" 2>/dev/null | awk '{print $1 * 1024}')
        cx_human=$(python3 -c "print(f'{$cx_size / 1024 / 1024:.1f}MB')")
        printf "  %-25s %s (package: %s)\n" "codex" "$cx_human" "$cx_pkg_dir/lib/node_modules/@openai/codex"
    else
        cx_size=$(stat -f%z "$cx_real" 2>/dev/null || stat -c%s "$cx_real" 2>/dev/null || echo "0")
        cx_human=$(python3 -c "print(f'{int($cx_size) / 1024 / 1024:.1f}MB')")
        printf "  %-25s %s (%s)\n" "codex" "$cx_human" "$cx_real"
    fi

    if [ "$cx_size" -gt 0 ] 2>/dev/null; then
        ratio=$(python3 -c "print(f'{int($cx_size) / $abs_size:.1f}')")
        echo ""
        echo -e "  ${GREEN}Abstract is ${ratio}x smaller${RESET}"
    fi
fi

# ─── 3. Memory Usage (RSS) ─────────────────────────────────────────────────

echo ""
echo -e "${CYAN}[3] Memory Usage (Peak RSS)${RESET}"
echo -e "${DIM}    Measuring peak RSS during --help / --version${RESET}"
echo ""

abs_rss=$(/usr/bin/time -l abstract --help 2>&1 | grep "maximum resident" | awk '{print $1}')
abs_rss_mb=$(python3 -c "print(f'{int($abs_rss) / 1024 / 1024:.1f}')")
printf "  %-25s %sMB\n" "abstract" "$abs_rss_mb"

if $HAS_CODEX; then
    cx_rss=$(/usr/bin/time -l codex --version 2>&1 | grep "maximum resident" | awk '{print $1}')
    cx_rss_mb=$(python3 -c "print(f'{int($cx_rss) / 1024 / 1024:.1f}')")
    printf "  %-25s %sMB\n" "codex" "$cx_rss_mb"

    ratio=$(python3 -c "print(f'{int($cx_rss) / int($abs_rss):.1f}')")
    echo ""
    echo -e "  ${GREEN}Abstract uses ${ratio}x less memory${RESET}"
fi

# ─── 4. Subcommand Latency ─────────────────────────────────────────────────

echo ""
echo -e "${CYAN}[4] Subcommand Latency${RESET}"
echo -e "${DIM}    CLI subcommand execution time${RESET}"
echo ""

SUB_ITERS=20
bench_cmd "abstract --help" "abstract --help" "$SUB_ITERS"
bench_cmd "abstract sessions list" "abstract sessions list" "$SUB_ITERS"
bench_cmd "abstract config show" "abstract config show" "$SUB_ITERS"

if $HAS_CODEX; then
    echo ""
    bench_cmd "codex --help" "codex --help" "$SUB_ITERS"
    bench_cmd "codex --version" "codex --version" "$SUB_ITERS"
fi

# ─── 5. Tool I/O (SDK-level) ──────────────────────────────────────────────

if $FULL; then
    echo ""
    echo -e "${CYAN}[5] Tool I/O Dispatch (SDK-level)${RESET}"
    echo -e "${DIM}    In-process tool execution via Cersei SDK${RESET}"
    echo ""

    cd "$SCRIPT_DIR" && cargo run --example benchmark_io --release 2>&1 | grep -E "^\s+(Read|Write|Edit|Glob|Grep|Bash)\s+avg="

    if $HAS_CODEX; then
        echo ""
        echo -e "${DIM}    Codex CLI startup (for reference):${RESET}"
        bench_cmd "codex --help" "codex --help" 5
    fi
fi

# ─── 6. Memory Architecture Benchmark ──────────────────────────────────────

if $FULL; then
    echo ""
    echo -e "${CYAN}[6] Memory Architecture${RESET}"
    echo -e "${DIM}    Abstract internal (Cersei SDK) vs Codex (external measurement)${RESET}"
    echo ""

    echo -e "  ${DIM}--- Abstract (Cersei SDK, in-process) ---${RESET}"
    cd "$SCRIPT_DIR" && cargo run --release -p abstract-cli --example memory_bench 2>&1 | grep -E "^\s+(Scan|Recall|Build|Load|Session|Graph|should)" | head -25
    echo ""

    if $HAS_CODEX; then
        echo -e "  ${DIM}--- Codex (external measurement) ---${RESET}"

        # Codex: agentic memory recall
        echo -e "  ${DIM}Codex exec memory recall (3 runs):${RESET}"
        for i in 1 2 3; do
            start=$(python3 -c "import time; print(time.time_ns())")
            cd "$CODEX_BENCH_DIR" && codex exec "What do you remember about this project? One sentence only." 2>/dev/null >/dev/null || true
            end=$(python3 -c "import time; print(time.time_ns())")
            ms=$(python3 -c "print(f'{($end - $start) / 1e6:.0f}')")
            printf "    Run %d: %sms (full agent + memory pipeline)\n" "$i" "$ms"
        done

        echo ""
        echo -e "  ${DIM}Codex exec memory write (3 runs):${RESET}"
        for i in 1 2 3; do
            start=$(python3 -c "import time; print(time.time_ns())")
            cd "$CODEX_BENCH_DIR" && codex exec "Remember: benchmark run $i at $(date +%s). Confirm saved." 2>/dev/null >/dev/null || true
            end=$(python3 -c "import time; print(time.time_ns())")
            ms=$(python3 -c "print(f'{($end - $start) / 1e6:.0f}')")
            printf "    Run %d: %sms (full agent + memory write)\n" "$i" "$ms"
        done
    fi
fi

# ─── 7. Agentic Benchmark (requires API key) ───────────────────────────────

if $FULL; then
    echo ""
    echo -e "${CYAN}[7] Agentic Benchmark${RESET}"
    echo -e "${DIM}    End-to-end prompt -> response${RESET}"
    echo ""

    # Abstract
    echo -e "  ${DIM}--- Abstract (5 runs) ---${RESET}"
    for i in $(seq 1 5); do
        start=$(python3 -c "import time; print(time.time_ns())")
        result=$(timeout 20 abstract "say OK" --no-permissions --fast 2>/dev/null || true)
        end=$(python3 -c "import time; print(time.time_ns())")
        ms=$(python3 -c "print(f'{($end - $start) / 1e6:.0f}')")
        printf "    Run %d: %sms | '%s'\n" "$i" "$ms" "$result"
    done

    # Codex
    if $HAS_CODEX; then
        echo ""
        echo -e "  ${DIM}--- Codex exec (5 runs) ---${RESET}"
        for i in $(seq 1 5); do
            start=$(python3 -c "import time; print(time.time_ns())")
            result=$(cd "$CODEX_BENCH_DIR" && timeout 30 codex exec "say OK" 2>/dev/null || true)
            end=$(python3 -c "import time; print(time.time_ns())")
            ms=$(python3 -c "print(f'{($end - $start) / 1e6:.0f}')")
            printf "    Run %d: %sms | '%s'\n" "$i" "$ms" "$(echo "$result" | head -c 40)"
        done
    fi

    # Sequential throughput
    echo ""
    echo -e "  ${DIM}--- Sequential throughput (10 prompts) ---${RESET}"

    abs_start=$(python3 -c "import time; print(time.time_ns())")
    for i in $(seq 1 10); do
        timeout 15 abstract "respond with: $i" --no-permissions --fast 2>/dev/null >/dev/null || true
    done
    abs_end=$(python3 -c "import time; print(time.time_ns())")
    abs_total=$(python3 -c "print(f'{($abs_end - $abs_start) / 1e6:.0f}')")
    abs_per=$(python3 -c "print(f'{($abs_end - $abs_start) / 1e6 / 10:.0f}')")
    echo "  abstract: total=${abs_total}ms  avg=${abs_per}ms/req"

    if $HAS_CODEX; then
        cx_start=$(python3 -c "import time; print(time.time_ns())")
        for i in $(seq 1 10); do
            (cd "$CODEX_BENCH_DIR" && timeout 30 codex exec "respond with: $i" 2>/dev/null >/dev/null) || true
        done
        cx_end=$(python3 -c "import time; print(time.time_ns())")
        cx_total=$(python3 -c "print(f'{($cx_end - $cx_start) / 1e6:.0f}')")
        cx_per=$(python3 -c "print(f'{($cx_end - $cx_start) / 1e6 / 10:.0f}')")
        echo "  codex:    total=${cx_total}ms  avg=${cx_per}ms/req"

        ratio=$(python3 -c "print(f'{int($cx_total) / max(int($abs_total), 1):.1f}')")
        echo ""
        echo -e "  ${GREEN}Abstract throughput: ${ratio}x faster${RESET}"
    fi
fi

# ─── Summary ────────────────────────────────────────────────────────────────

echo ""
echo -e "${CYAN}${BOLD}Summary${RESET}"
echo "-------"

abs_startup=$(python3 -c "
import subprocess, time
total = 0
for _ in range(10):
    s = time.time_ns()
    subprocess.run(['abstract', '--version'], capture_output=True)
    total += time.time_ns() - s
print(f'{total / 10 / 1e6:.1f}')
")

printf "  %-25s %s\n" "abstract startup" "${abs_startup}ms"
printf "  %-25s %s\n" "abstract binary" "$abs_human"
printf "  %-25s %sMB\n" "abstract RSS" "$abs_rss_mb"

if $HAS_CODEX; then
    cx_startup=$(python3 -c "
import subprocess, time
total = 0
for _ in range(10):
    s = time.time_ns()
    subprocess.run(['codex', '--version'], capture_output=True)
    total += time.time_ns() - s
print(f'{total / 10 / 1e6:.1f}')
")
    printf "  %-25s %s\n" "codex startup" "${cx_startup}ms"
    if [ -n "$cx_human" ]; then
        printf "  %-25s %s\n" "codex binary/pkg" "$cx_human"
    fi
    printf "  %-25s %sMB\n" "codex RSS" "$cx_rss_mb"

    echo ""
    startup_x=$(python3 -c "print(f'{float($cx_startup) / float($abs_startup):.1f}')")
    rss_x=$(python3 -c "print(f'{int($cx_rss) / int($abs_rss):.0f}')")
    echo -e "  ${GREEN}${BOLD}Startup:  ${startup_x}x faster${RESET}"
    echo -e "  ${GREEN}${BOLD}Memory:   ${rss_x}x less RSS${RESET}"
else
    echo ""
    echo -e "  ${YELLOW}Install Codex CLI to enable full comparison${RESET}"
fi

echo ""
echo -e "${GREEN}Benchmark complete.${RESET}"
echo -e "${DIM}Full report: crates/abstract-cli/benchmarks/REPORT.md${RESET}"
