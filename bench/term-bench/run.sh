#!/usr/bin/env bash
# ─── Abstract Benchmark Runner (terminal-bench) ────────────────────────────
# Usage:
#   ./bench/term-bench/run.sh                          # Run all tasks with default model
#   ./bench/term-bench/run.sh --model google/gemini-3.1-pro-preview
#   ./bench/term-bench/run.sh --task fibonacci         # Run single task
#   ./bench/term-bench/run.sh --dry-run                # Print commands without executing
#   ./bench/term-bench/run.sh --report                 # Generate report from last results
#
# Environment:
#   GOOGLE_API_KEY / GEMINI_API_KEY   — for Gemini models
#   ANTHROPIC_API_KEY                 — for Claude models
#   ABSTRACT_BIN                      — path to abstract binary (auto-detected)

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"
TASKS_DIR="$SCRIPT_DIR/tasks"
RESULTS_DIR="$SCRIPT_DIR/results"

# ─── Defaults ──────────────────────────────────────────────────────────────
MODEL="${MODEL:-google/gemini-3.1-pro-preview}"
ABSTRACT_BIN="${ABSTRACT_BIN:-$REPO_DIR/target/release/abstract}"
TASK_FILTER=""
DRY_RUN=false
REPORT_ONLY=false
TIMEOUT_MULTIPLIER=1
RUN_ID="$(date +%Y%m%d-%H%M%S)"
WORKDIR=""

# ─── Parse args ────────────────────────────────────────────────────────────
while [[ $# -gt 0 ]]; do
  case "$1" in
    --model)      MODEL="$2"; shift 2 ;;
    --task)       TASK_FILTER="$2"; shift 2 ;;
    --dry-run)    DRY_RUN=true; shift ;;
    --report)     REPORT_ONLY=true; shift ;;
    --timeout-mult) TIMEOUT_MULTIPLIER="$2"; shift 2 ;;
    --bin)        ABSTRACT_BIN="$2"; shift 2 ;;
    --help|-h)
      head -10 "$0" | tail -8
      exit 0 ;;
    *) echo "Unknown arg: $1"; exit 1 ;;
  esac
done

# ─── Colors ────────────────────────────────────────────────────────────────
GREEN='\033[0;32m'
RED='\033[0;31m'
YELLOW='\033[0;33m'
CYAN='\033[0;36m'
DIM='\033[0;90m'
BOLD='\033[1m'
RESET='\033[0m'

info()  { echo -e "${CYAN}▸${RESET} $*"; }
pass()  { echo -e "${GREEN}✓${RESET} $*"; }
fail()  { echo -e "${RED}✗${RESET} $*"; }
warn()  { echo -e "${YELLOW}!${RESET} $*"; }

# ─── Report mode ───────────────────────────────────────────────────────────
if $REPORT_ONLY; then
  exec "$SCRIPT_DIR/report.sh"
fi

# ─── Validate ──────────────────────────────────────────────────────────────
if [[ ! -x "$ABSTRACT_BIN" ]]; then
  warn "Binary not found at $ABSTRACT_BIN"
  info "Building release binary..."
  (cd "$REPO_DIR" && cargo build --release)
fi

if [[ ! -d "$TASKS_DIR" ]]; then
  echo "No tasks directory at $TASKS_DIR"
  exit 1
fi

# ─── Collect tasks ─────────────────────────────────────────────────────────
TASKS=()
for task_file in "$TASKS_DIR"/*.json; do
  task_id=$(jq -r '.id' "$task_file")
  if [[ -n "$TASK_FILTER" && "$task_id" != "$TASK_FILTER" ]]; then
    continue
  fi
  TASKS+=("$task_file")
done

if [[ ${#TASKS[@]} -eq 0 ]]; then
  echo "No tasks found${TASK_FILTER:+ matching '$TASK_FILTER'}"
  exit 1
fi

# ─── Run header ────────────────────────────────────────────────────────────
RUN_RESULTS_DIR="$RESULTS_DIR/$RUN_ID"
mkdir -p "$RUN_RESULTS_DIR"

echo ""
echo -e "${BOLD}Abstract Benchmark${RESET}"
echo -e "${DIM}─────────────────────────────────────────${RESET}"
echo -e "  Model:   ${CYAN}$MODEL${RESET}"
echo -e "  Tasks:   ${#TASKS[@]}"
echo -e "  Run ID:  $RUN_ID"
echo -e "  Binary:  $ABSTRACT_BIN"
echo -e "${DIM}─────────────────────────────────────────${RESET}"
echo ""

# Save run metadata
cat > "$RUN_RESULTS_DIR/meta.json" << EOF
{
  "run_id": "$RUN_ID",
  "model": "$MODEL",
  "task_count": ${#TASKS[@]},
  "timestamp": "$(date -u +%Y-%m-%dT%H:%M:%SZ)",
  "abstract_version": "$("$ABSTRACT_BIN" --version 2>/dev/null || echo 'unknown')",
  "hostname": "$(hostname)"
}
EOF

# ─── Run each task ─────────────────────────────────────────────────────────
PASSED=0
FAILED=0
ERRORS=0
TOTAL=${#TASKS[@]}

for task_file in "${TASKS[@]}"; do
  task_id=$(jq -r '.id' "$task_file")
  instruction=$(jq -r '.instruction' "$task_file")
  test_cmd=$(jq -r '.test_command' "$task_file")
  timeout_agent=$(jq -r '.timeout_agent // 120' "$task_file")
  timeout_test=$(jq -r '.timeout_test // 30' "$task_file")
  difficulty=$(jq -r '.difficulty // "unknown"' "$task_file")
  category=$(jq -r '.category // "unknown"' "$task_file")

  # Apply timeout multiplier
  timeout_agent=$(echo "$timeout_agent * $TIMEOUT_MULTIPLIER" | bc | cut -d. -f1)

  # Create isolated working directory for this task
  WORKDIR=$(mktemp -d)
  mkdir -p "$WORKDIR/app" "$WORKDIR/test"

  info "${BOLD}$task_id${RESET} ${DIM}[$category/$difficulty]${RESET}"

  # Rewrite /app and /test references to use the actual workdir (for local runs)
  # Use sed for robust replacement (handles special chars in $WORKDIR)
  local_instruction=$(echo "$instruction" | sed "s|/app/|$WORKDIR/app/|g; s|/app |$WORKDIR/app |g")
  local_test_cmd=$(echo "$test_cmd" | sed "s|'/app'|'$WORKDIR/app'|g; s|/app/|$WORKDIR/app/|g; s|/app |$WORKDIR/app |g; s|/test/|$WORKDIR/test/|g")

  # Build the abstract command
  AGENT_CMD="$ABSTRACT_BIN -p \"$local_instruction\" --model $MODEL --no-permissions --headless -C $WORKDIR/app --output-format stream-json"

  if $DRY_RUN; then
    echo -e "  ${DIM}cmd: $AGENT_CMD${RESET}"
    echo -e "  ${DIM}test: $local_test_cmd${RESET}"
    echo ""
    continue
  fi

  # Run agent with timeout
  TASK_RESULT_FILE="$RUN_RESULTS_DIR/$task_id.json"
  AGENT_LOG="$RUN_RESULTS_DIR/$task_id.log"
  START_TIME=$(date +%s)

  set +e
  timeout "${timeout_agent}s" bash -c "$AGENT_CMD" > "$AGENT_LOG" 2>&1
  AGENT_EXIT=$?
  set -e

  AGENT_TIME=$(($(date +%s) - START_TIME))

  # Run tests
  TEST_OUTPUT=""
  TEST_EXIT=1
  if [[ $AGENT_EXIT -eq 0 || $AGENT_EXIT -ne 124 ]]; then
    set +e
    TEST_OUTPUT=$(cd "$WORKDIR" && timeout "${timeout_test}s" bash -c "$local_test_cmd" 2>&1)
    TEST_EXIT=$?
    set -e
  fi

  # Determine result
  if [[ $AGENT_EXIT -eq 124 ]]; then
    STATUS="timeout"
    fail "$task_id — agent timed out (${timeout_agent}s)"
    ((ERRORS++))
  elif [[ $TEST_EXIT -eq 0 ]]; then
    STATUS="passed"
    pass "$task_id — ${AGENT_TIME}s"
    ((PASSED++))
  else
    STATUS="failed"
    fail "$task_id — tests failed (${AGENT_TIME}s)"
    ((FAILED++))
  fi

  # Parse NDJSON log for cost/token info
  INPUT_TOKENS=$(grep '"type":"cost_update"' "$AGENT_LOG" 2>/dev/null | tail -1 | jq -r '.input_tokens // 0' 2>/dev/null || echo 0)
  OUTPUT_TOKENS=$(grep '"type":"cost_update"' "$AGENT_LOG" 2>/dev/null | tail -1 | jq -r '.output_tokens // 0' 2>/dev/null || echo 0)
  COST=$(grep '"type":"cost_update"' "$AGENT_LOG" 2>/dev/null | tail -1 | jq -r '.cumulative_cost // 0' 2>/dev/null || echo 0)

  # Save task result
  cat > "$TASK_RESULT_FILE" << TASKEOF
{
  "task_id": "$task_id",
  "category": "$category",
  "difficulty": "$difficulty",
  "status": "$STATUS",
  "agent_exit_code": $AGENT_EXIT,
  "test_exit_code": $TEST_EXIT,
  "agent_time_secs": $AGENT_TIME,
  "input_tokens": $INPUT_TOKENS,
  "output_tokens": $OUTPUT_TOKENS,
  "cost_usd": $COST,
  "test_output": $(echo "$TEST_OUTPUT" | head -20 | jq -Rs .)
}
TASKEOF

  # Cleanup workdir
  rm -rf "$WORKDIR"
done

if $DRY_RUN; then
  echo -e "${DIM}Dry run complete. No tasks were executed.${RESET}"
  exit 0
fi

# ─── Summary ───────────────────────────────────────────────────────────────
echo ""
echo -e "${DIM}─────────────────────────────────────────${RESET}"
echo -e "${BOLD}Results:${RESET} ${GREEN}$PASSED passed${RESET} / ${RED}$FAILED failed${RESET} / ${YELLOW}$ERRORS errors${RESET} / $TOTAL total"

if [[ $TOTAL -gt 0 ]]; then
  ACCURACY=$(echo "scale=1; $PASSED * 100 / $TOTAL" | bc)
  echo -e "${BOLD}Accuracy:${RESET} ${ACCURACY}%"
fi

echo -e "${DIM}Results saved to: $RUN_RESULTS_DIR/${RESET}"
echo ""

# Auto-generate report
"$SCRIPT_DIR/report.sh" "$RUN_ID" 2>/dev/null || true
