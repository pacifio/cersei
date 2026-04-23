#!/usr/bin/env bash
# ─── Abstract Benchmark Report Generator ───────────────────────────────────
# Usage:
#   ./bench/report.sh              # Report from latest run
#   ./bench/report.sh 20260413-...  # Report from specific run ID

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
RESULTS_DIR="$SCRIPT_DIR/results"

# ─── Find run ──────────────────────────────────────────────────────────────
if [[ $# -ge 1 ]]; then
  RUN_ID="$1"
else
  RUN_ID=$(ls -1t "$RESULTS_DIR" 2>/dev/null | head -1)
fi

if [[ -z "$RUN_ID" || ! -d "$RESULTS_DIR/$RUN_ID" ]]; then
  echo "No benchmark results found."
  echo "Run: ./bench/run.sh first"
  exit 1
fi

RUN_DIR="$RESULTS_DIR/$RUN_ID"

# ─── Colors ────────────────────────────────────────────────────────────────
GREEN='\033[0;32m'
RED='\033[0;31m'
YELLOW='\033[0;33m'
CYAN='\033[0;36m'
DIM='\033[0;90m'
BOLD='\033[1m'
RESET='\033[0m'

# ─── Parse metadata ───────────────────────────────────────────────────────
MODEL=$(jq -r '.model // "unknown"' "$RUN_DIR/meta.json" 2>/dev/null || echo "unknown")
TIMESTAMP=$(jq -r '.timestamp // "unknown"' "$RUN_DIR/meta.json" 2>/dev/null || echo "unknown")
VERSION=$(jq -r '.abstract_version // "unknown"' "$RUN_DIR/meta.json" 2>/dev/null || echo "unknown")

# ─── Aggregate results using jq ───────────────────────────────────────────
# Merge all task results into a single JSON array
RESULTS_JSON=$(jq -s '.' "$RUN_DIR"/*.json 2>/dev/null | jq '[.[] | select(.task_id != null)]')

TOTAL=$(echo "$RESULTS_JSON" | jq 'length')
PASSED=$(echo "$RESULTS_JSON" | jq '[.[] | select(.status == "passed")] | length')
FAILED=$(echo "$RESULTS_JSON" | jq '[.[] | select(.status == "failed")] | length')
ERRORS=$(echo "$RESULTS_JSON" | jq '[.[] | select(.status == "timeout" or .status == "error")] | length')
TOTAL_TIME=$(echo "$RESULTS_JSON" | jq '[.[].agent_time_secs // 0] | add')
TOTAL_INPUT=$(echo "$RESULTS_JSON" | jq '[.[].input_tokens // 0] | add')
TOTAL_OUTPUT=$(echo "$RESULTS_JSON" | jq '[.[].output_tokens // 0] | add')
TOTAL_COST=$(echo "$RESULTS_JSON" | jq '[.[].cost_usd // 0] | add')

if [[ $TOTAL -eq 0 ]]; then
  echo "No task results found in $RUN_DIR"
  exit 1
fi

ACCURACY=$(echo "scale=1; $PASSED * 100 / $TOTAL" | bc)

# ─── Print report ─────────────────────────────────────────────────────────
echo ""
echo -e "${BOLD}╔══════════════════════════════════════════════════╗${RESET}"
echo -e "${BOLD}║       Abstract Benchmark Report                  ║${RESET}"
echo -e "${BOLD}╚══════════════════════════════════════════════════╝${RESET}"
echo ""
echo -e "  ${DIM}Run ID:${RESET}     $RUN_ID"
echo -e "  ${DIM}Model:${RESET}      ${CYAN}$MODEL${RESET}"
echo -e "  ${DIM}Version:${RESET}    $VERSION"
echo -e "  ${DIM}Timestamp:${RESET}  $TIMESTAMP"
echo ""
echo -e "${BOLD}  Overall Score${RESET}"
echo -e "  ─────────────────────────────────"
echo -e "  ${GREEN}$PASSED${RESET} passed / ${RED}$FAILED${RESET} failed / ${YELLOW}$ERRORS${RESET} errors / $TOTAL total"
echo -e "  ${BOLD}Accuracy: ${GREEN}${ACCURACY}%${RESET}"
echo ""

# By category
echo -e "${BOLD}  By Category${RESET}"
echo -e "  ─────────────────────────────────"
echo "$RESULTS_JSON" | jq -r '
  group_by(.category) | .[] |
  { cat: .[0].category, total: length, passed: ([.[] | select(.status == "passed")] | length) } |
  "\(.cat)\t\(.passed)/\(.total)\t\(.passed * 100 / .total | floor)%"
' | while IFS=$'\t' read -r cat score pct; do
  printf "  %-20s %s  (%s)\n" "$cat" "$score" "$pct"
done
echo ""

# By difficulty
echo -e "${BOLD}  By Difficulty${RESET}"
echo -e "  ─────────────────────────────────"
echo "$RESULTS_JSON" | jq -r '
  group_by(.difficulty) | sort_by(
    if .[0].difficulty == "trivial" then 0
    elif .[0].difficulty == "easy" then 1
    elif .[0].difficulty == "medium" then 2
    elif .[0].difficulty == "hard" then 3
    else 4 end
  ) | .[] |
  { diff: .[0].difficulty, total: length, passed: ([.[] | select(.status == "passed")] | length) } |
  "\(.diff)\t\(.passed)/\(.total)\t\(.passed * 100 / .total | floor)%"
' | while IFS=$'\t' read -r diff score pct; do
  printf "  %-20s %s  (%s)\n" "$diff" "$score" "$pct"
done
echo ""

# Resource usage
echo -e "${BOLD}  Resource Usage${RESET}"
echo -e "  ─────────────────────────────────"
echo -e "  Total time:     ${TOTAL_TIME}s"
echo -e "  Input tokens:   $TOTAL_INPUT"
echo -e "  Output tokens:  $TOTAL_OUTPUT"
echo -e "  Total cost:     \$${TOTAL_COST}"
if [[ $TOTAL -gt 0 ]]; then
  AVG_TIME=$(echo "scale=1; $TOTAL_TIME / $TOTAL" | bc)
  echo -e "  Avg time/task:  ${AVG_TIME}s"
fi
echo ""

# Per-task details
echo -e "${BOLD}  Task Details${RESET}"
echo -e "  ─────────────────────────────────"
printf "  ${DIM}%-20s %-10s %-8s %-10s${RESET}\n" "TASK" "STATUS" "TIME" "TOKENS"
echo "$RESULTS_JSON" | jq -r '.[] | "\(.task_id)\t\(.status)\t\(.agent_time_secs)s\t\((.input_tokens // 0) + (.output_tokens // 0))"' | while IFS=$'\t' read -r task_id status time_s tokens; do
  case "$status" in
    passed)  status_colored="${GREEN}passed${RESET}" ;;
    failed)  status_colored="${RED}failed${RESET}" ;;
    timeout) status_colored="${YELLOW}timeout${RESET}" ;;
    *)       status_colored="${DIM}$status${RESET}" ;;
  esac
  printf "  %-20s %-18b %-8s %-10s\n" "$task_id" "$status_colored" "$time_s" "$tokens"
done
echo ""

# ─── Save JSON summary ────────────────────────────────────────────────────
cat > "$RUN_DIR/summary.json" << EOF
{
  "run_id": "$RUN_ID",
  "model": "$MODEL",
  "timestamp": "$TIMESTAMP",
  "total": $TOTAL,
  "passed": $PASSED,
  "failed": $FAILED,
  "errors": $ERRORS,
  "accuracy_pct": $ACCURACY,
  "total_time_secs": $TOTAL_TIME,
  "total_input_tokens": $TOTAL_INPUT,
  "total_output_tokens": $TOTAL_OUTPUT,
  "total_cost_usd": $TOTAL_COST
}
EOF

echo -e "${DIM}  JSON summary: $RUN_DIR/summary.json${RESET}"
echo ""
