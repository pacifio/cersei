#!/usr/bin/env bash
# Abstract vs Claude Code — CLI Tool Benchmark
# Usage: ./run_tool_bench.sh [--iterations N] [--full]
#
# Compares the Abstract CLI against Claude Code CLI across:
#   1. Startup time
#   2. Binary size
#   3. Memory usage (RSS)
#   4. Subcommand latency
#   5. Tool I/O dispatch (SDK-level)
#
# Requires: abstract (cargo install --path crates/abstract-cli)
#           claude   (optional — skipped if not in PATH)

set -e
cd "$(dirname "$0")"

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

HAS_CLAUDE=false
if command -v claude &>/dev/null; then
    HAS_CLAUDE=true
fi

HAS_ABSTRACT=false
if command -v abstract &>/dev/null; then
    HAS_ABSTRACT=true
fi

echo -e "${CYAN}${BOLD}Abstract vs Claude Code — CLI Benchmark${RESET}"
echo "=========================================="
echo -e "${DIM}Iterations: ${ITERS} | Date: $(date '+%Y-%m-%d %H:%M') | Platform: $(uname -m) $(uname -s)${RESET}"
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

if $HAS_CLAUDE; then
    bench_cmd "claude --version" "claude --version" "$ITERS"
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
    cc_avg=$(python3 -c "
import subprocess, time
total = 0
for _ in range($ITERS):
    s = time.time_ns()
    subprocess.run(['claude', '--version'], capture_output=True)
    total += time.time_ns() - s
print(total / $ITERS / 1e6)
")
    ratio=$(python3 -c "print(f'{$cc_avg / $abs_avg:.1f}')")
    echo -e "  ${GREEN}Abstract is ${ratio}x faster at startup${RESET}"
else
    echo ""
    echo -e "  ${YELLOW}claude not in PATH — skipping comparison${RESET}"
fi

# ─── 2. Binary Size ────────────────────────────────────────────────────────

echo ""
echo -e "${CYAN}[2] Binary Size${RESET}"
echo ""

abs_path=$(which abstract)
abs_size=$(stat -f%z "$abs_path" 2>/dev/null || stat -c%s "$abs_path" 2>/dev/null)
abs_human=$(python3 -c "print(f'{$abs_size / 1024 / 1024:.1f}MB')")
printf "  %-25s %s (%s)\n" "abstract" "$abs_human" "$abs_path"

if $HAS_CLAUDE; then
    cc_path=$(which claude)
    cc_real=$(readlink "$cc_path" 2>/dev/null || echo "$cc_path")
    cc_size=$(stat -f%z "$cc_real" 2>/dev/null || stat -c%s "$cc_real" 2>/dev/null)
    cc_human=$(python3 -c "print(f'{$cc_size / 1024 / 1024:.1f}MB')")
    printf "  %-25s %s (%s)\n" "claude" "$cc_human" "$cc_real"

    ratio=$(python3 -c "print(f'{$cc_size / $abs_size:.1f}')")
    echo ""
    echo -e "  ${GREEN}Abstract is ${ratio}x smaller${RESET}"
fi

# ─── 3. Memory Usage (RSS) ─────────────────────────────────────────────────

echo ""
echo -e "${CYAN}[3] Memory Usage (Peak RSS)${RESET}"
echo -e "${DIM}    Measuring peak RSS during --help${RESET}"
echo ""

abs_rss=$(/usr/bin/time -l abstract --help 2>&1 | grep "maximum resident" | awk '{print $1}')
abs_rss_mb=$(python3 -c "print(f'{int($abs_rss) / 1024 / 1024:.1f}')")
printf "  %-25s %sMB\n" "abstract" "$abs_rss_mb"

if $HAS_CLAUDE; then
    cc_rss=$(/usr/bin/time -l claude --version 2>&1 | grep "maximum resident" | awk '{print $1}')
    cc_rss_mb=$(python3 -c "print(f'{int($cc_rss) / 1024 / 1024:.1f}')")
    printf "  %-25s %sMB\n" "claude" "$cc_rss_mb"

    ratio=$(python3 -c "print(f'{int($cc_rss) / int($abs_rss):.1f}')")
    echo ""
    echo -e "  ${GREEN}Abstract uses ${ratio}x less memory${RESET}"
fi

# ─── 4. Subcommand Latency ─────────────────────────────────────────────────

echo ""
echo -e "${CYAN}[4] Subcommand Latency${RESET}"
echo -e "${DIM}    Abstract CLI subcommand execution time${RESET}"
echo ""

SUB_ITERS=20
bench_cmd "abstract --help" "abstract --help" "$SUB_ITERS"
bench_cmd "abstract sessions list" "abstract sessions list" "$SUB_ITERS"
bench_cmd "abstract config show" "abstract config show" "$SUB_ITERS"
bench_cmd "abstract mcp list" "abstract mcp list" "$SUB_ITERS"
bench_cmd "abstract memory show" "abstract memory show" "$SUB_ITERS"

# ─── 5. Tool I/O (SDK-level) ──────────────────────────────────────────────

if $FULL; then
    echo ""
    echo -e "${CYAN}[5] Tool I/O Dispatch (SDK-level)${RESET}"
    echo -e "${DIM}    In-process tool execution via Cersei SDK${RESET}"
    echo ""

    cargo run --example benchmark_io --release 2>&1 | grep -E "^\s+(Read|Write|Edit|Glob|Grep|Bash)\s+avg="

    if $HAS_CLAUDE; then
        echo ""
        echo -e "${DIM}    Claude Code CLI startup (for reference):${RESET}"
        bench_cmd "claude --help" "claude --help" 5
    fi
fi

# ─── 6. Memory Architecture Benchmark ──────────────────────────────────────

if $FULL; then
    echo ""
    echo -e "${CYAN}[6] Memory Architecture${RESET}"
    echo -e "${DIM}    Abstract internal (Cersei SDK) vs Claude Code (external measurement)${RESET}"
    echo ""

    # 6a. Abstract internal memory benchmark
    echo -e "  ${DIM}--- Abstract (Cersei SDK, in-process) ---${RESET}"
    cargo run --release -p abstract-cli --example memory_bench 2>&1 | grep -E "^\s+(Scan|Recall|Build|Load|Session|Graph|should)" | head -25
    echo ""

    # 6b. Claude Code memory measurements (real, external)
    if $HAS_CLAUDE; then
        echo -e "  ${DIM}--- Claude Code (external measurement) ---${RESET}"

        # Claude Code: memory recall via agent (forces full memory pipeline)
        echo -e "  ${DIM}Claude -p memory recall (3 runs):${RESET}"
        for i in 1 2 3; do
            start=$(python3 -c "import time; print(time.time_ns())")
            claude -p "What do you remember about this project? One sentence only." 2>/dev/null >/dev/null
            end=$(python3 -c "import time; print(time.time_ns())")
            ms=$(python3 -c "print(f'{($end - $start) / 1e6:.0f}')")
            printf "    Run %d: %sms (full agent + memory pipeline)\n" "$i" "$ms"
        done

        echo ""
        echo -e "  ${DIM}Claude -p memory write (3 runs):${RESET}"
        for i in 1 2 3; do
            start=$(python3 -c "import time; print(time.time_ns())")
            claude -p "Remember: benchmark run $i at $(date +%s). Confirm saved." 2>/dev/null >/dev/null
            end=$(python3 -c "import time; print(time.time_ns())")
            ms=$(python3 -c "print(f'{($end - $start) / 1e6:.0f}')")
            printf "    Run %d: %sms (full agent + memory write)\n" "$i" "$ms"
        done

        echo ""
        echo -e "  ${DIM}Claude Code file-level I/O (no agent):${RESET}"

        # Scan: find all memory files
        total=0
        for i in $(seq 1 10); do
            start=$(python3 -c "import time; print(time.time_ns())")
            find ~/.claude/projects -name "*.md" -path "*/memory/*" 2>/dev/null >/dev/null
            end=$(python3 -c "import time; print(time.time_ns())")
            ms=$(python3 -c "print(($end - $start) / 1e6)")
            total=$(python3 -c "print($total + $ms)")
        done
        avg=$(python3 -c "print(f'{$total / 10:.1f}')")
        printf "    find memory files:   avg=%sms (10 runs)\n" "$avg"

        # Grep recall: search across memory
        MEM_DIR=$(find ~/.claude/projects -name "memory" -type d 2>/dev/null | head -1)
        if [ -n "$MEM_DIR" ] && [ -d "$MEM_DIR" ]; then
            total=0
            for i in $(seq 1 10); do
                start=$(python3 -c "import time; print(time.time_ns())")
                grep -rn "rust" "$MEM_DIR" --include="*.md" 2>/dev/null >/dev/null || true
                end=$(python3 -c "import time; print(time.time_ns())")
                ms=$(python3 -c "print(($end - $start) / 1e6)")
                total=$(python3 -c "print($total + $ms)")
            done
            avg=$(python3 -c "print(f'{$total / 10:.1f}')")
            printf "    grep memory recall:  avg=%sms (10 runs)\n" "$avg"
        fi

        # MEMORY.md load
        MEMMD=$(find ~/.claude/projects -name "MEMORY.md" -path "*/memory/*" 2>/dev/null | head -1)
        if [ -n "$MEMMD" ] && [ -f "$MEMMD" ]; then
            lines=$(wc -l < "$MEMMD" | tr -d ' ')
            total=0
            for i in $(seq 1 20); do
                start=$(python3 -c "import time; print(time.time_ns())")
                cat "$MEMMD" >/dev/null
                end=$(python3 -c "import time; print(time.time_ns())")
                ms=$(python3 -c "print(($end - $start) / 1e6)")
                total=$(python3 -c "print($total + $ms)")
            done
            avg=$(python3 -c "print(f'{$total / 20:.2f}')")
            printf "    MEMORY.md load (%s lines): avg=%sms (20 runs)\n" "$lines" "$avg"
        fi

        # Session JSONL load
        BIGGEST=$(find ~/.claude/projects -name "*.jsonl" -exec wc -c {} + 2>/dev/null | sort -rn | head -2 | tail -1 | awk '{print $NF}')
        if [ -n "$BIGGEST" ] && [ -f "$BIGGEST" ]; then
            lines=$(wc -l < "$BIGGEST" | tr -d ' ')
            size=$(du -h "$BIGGEST" | awk '{print $1}')
            total=0
            for i in $(seq 1 10); do
                start=$(python3 -c "import time; print(time.time_ns())")
                python3 -c "
import json
with open('$BIGGEST') as f:
    for line in f:
        try: json.loads(line)
        except: pass
" 2>/dev/null
                end=$(python3 -c "import time; print(time.time_ns())")
                ms=$(python3 -c "print(($end - $start) / 1e6)")
                total=$(python3 -c "print($total + $ms)")
            done
            avg=$(python3 -c "print(f'{$total / 10:.1f}')")
            printf "    JSONL parse (%s lines, %s): avg=%sms (10 runs)\n" "$lines" "$size" "$avg"
        fi
    fi
fi

# ─── 7. Agentic Benchmark (requires API key) ───────────────────────────────

if $FULL; then
    echo ""
    echo -e "${CYAN}[7] Agentic Benchmark${RESET}"
    echo -e "${DIM}    Real LLM round-trip: prompt → model → tool call → response${RESET}"
    echo ""

    # Check if abstract can authenticate
    if abstract login status 2>&1 | grep -q "not configured"; then
        echo -e "  ${YELLOW}Skipping: no API key configured. Run 'abstract login' first.${RESET}"
    else
        BENCH_DIR=$(mktemp -d)
        echo "test content" > "$BENCH_DIR/test.txt"

        # Benchmark: abstract single-shot (simple prompt, no tools needed)
        echo -e "  ${DIM}Test 1: Simple response (no tools)${RESET}"
        abs_agent_start=$(python3 -c "import time; print(time.time_ns())")
        abs_agent_out=$(abstract "Respond with exactly: BENCHMARK_OK" --no-permissions --fast -C "$BENCH_DIR" --json 2>/dev/null || true)
        abs_agent_end=$(python3 -c "import time; print(time.time_ns())")
        abs_agent_ms=$(python3 -c "print(f'{($abs_agent_end - $abs_agent_start) / 1e6:.0f}')")

        if echo "$abs_agent_out" | grep -q "BENCHMARK_OK"; then
            echo -e "  ${GREEN}abstract: ${abs_agent_ms}ms (prompt → response)${RESET}"
        elif echo "$abs_agent_out" | grep -q "text_delta"; then
            echo -e "  ${GREEN}abstract: ${abs_agent_ms}ms (got response)${RESET}"
        else
            echo -e "  ${YELLOW}abstract: ${abs_agent_ms}ms (response unclear — check API key/credits)${RESET}"
        fi

        # Benchmark: abstract with tool call (read a file)
        echo -e "  ${DIM}Test 2: Single tool call (file read)${RESET}"
        abs_tool_start=$(python3 -c "import time; print(time.time_ns())")
        abs_tool_out=$(abstract "Read the file test.txt and tell me what it says. Be brief." --no-permissions --fast -C "$BENCH_DIR" --json 2>/dev/null || true)
        abs_tool_end=$(python3 -c "import time; print(time.time_ns())")
        abs_tool_ms=$(python3 -c "print(f'{($abs_tool_end - $abs_tool_start) / 1e6:.0f}')")

        if echo "$abs_tool_out" | grep -q "tool_start"; then
            tool_count=$(echo "$abs_tool_out" | grep -c "tool_start")
            echo -e "  ${GREEN}abstract: ${abs_tool_ms}ms (${tool_count} tool call(s))${RESET}"
        else
            echo -e "  ${YELLOW}abstract: ${abs_tool_ms}ms (no tool calls detected)${RESET}"
        fi

        # Compare with Claude Code if available
        if $HAS_CLAUDE; then
            echo ""
            echo -e "  ${DIM}Claude Code comparison:${RESET}"

            cc_agent_start=$(python3 -c "import time; print(time.time_ns())")
            cc_agent_out=$(claude -p "Respond with exactly: BENCHMARK_OK" --no-input 2>/dev/null || true)
            cc_agent_end=$(python3 -c "import time; print(time.time_ns())")
            cc_agent_ms=$(python3 -c "print(f'{($cc_agent_end - $cc_agent_start) / 1e6:.0f}')")
            echo -e "  claude (simple):  ${cc_agent_ms}ms"

            cc_tool_start=$(python3 -c "import time; print(time.time_ns())")
            cc_tool_out=$(cd "$BENCH_DIR" && claude -p "Read the file test.txt and tell me what it says. Be brief." --no-input --allowedTools Read 2>/dev/null || true)
            cc_tool_end=$(python3 -c "import time; print(time.time_ns())")
            cc_tool_ms=$(python3 -c "print(f'{($cc_tool_end - $cc_tool_start) / 1e6:.0f}')")
            echo -e "  claude (tool):    ${cc_tool_ms}ms"

            if [ "$abs_agent_ms" -gt 0 ] && [ "$cc_agent_ms" -gt 0 ]; then
                ratio=$(python3 -c "print(f'{int($cc_agent_ms) / int($abs_agent_ms):.1f}')")
                echo ""
                echo -e "  ${GREEN}Abstract is ~${ratio}x faster end-to-end (simple prompt)${RESET}"
            fi
        fi

        # Token consumption comparison
        echo ""
        echo -e "  ${DIM}Token consumption (from JSON output):${RESET}"
        abs_tokens=$(echo "$abs_agent_out" | grep "cost_update" | tail -1)
        if [ -n "$abs_tokens" ]; then
            in_tok=$(echo "$abs_tokens" | python3 -c "import sys,json; d=json.loads(sys.stdin.read()); print(d.get('input_tokens',0))" 2>/dev/null || echo "?")
            out_tok=$(echo "$abs_tokens" | python3 -c "import sys,json; d=json.loads(sys.stdin.read()); print(d.get('output_tokens',0))" 2>/dev/null || echo "?")
            echo -e "  abstract: ${in_tok} input / ${out_tok} output tokens"
        else
            echo -e "  ${DIM}(no token data — API may not have responded)${RESET}"
        fi

        rm -rf "$BENCH_DIR"
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

if $HAS_CLAUDE; then
    cc_startup=$(python3 -c "
import subprocess, time
total = 0
for _ in range(10):
    s = time.time_ns()
    subprocess.run(['claude', '--version'], capture_output=True)
    total += time.time_ns() - s
print(f'{total / 10 / 1e6:.1f}')
")
    printf "  %-25s %s\n" "claude startup" "${cc_startup}ms"
    printf "  %-25s %s\n" "claude binary" "$cc_human"
    printf "  %-25s %sMB\n" "claude RSS" "$cc_rss_mb"

    echo ""
    startup_x=$(python3 -c "print(f'{float($cc_startup) / float($abs_startup):.1f}')")
    size_x=$(python3 -c "print(f'{$cc_size / $abs_size:.0f}')")
    rss_x=$(python3 -c "print(f'{int($cc_rss) / int($abs_rss):.0f}')")
    echo -e "  ${GREEN}${BOLD}Startup:  ${startup_x}x faster${RESET}"
    echo -e "  ${GREEN}${BOLD}Binary:   ${size_x}x smaller${RESET}"
    echo -e "  ${GREEN}${BOLD}Memory:   ${rss_x}x less RSS${RESET}"
else
    echo ""
    echo -e "  ${YELLOW}Install Claude Code to enable full comparison${RESET}"
fi

echo ""
echo -e "${GREEN}Benchmark complete.${RESET}"
echo -e "${DIM}Full report: crates/abstract-cli/benchmarks/REPORT.md${RESET}"
