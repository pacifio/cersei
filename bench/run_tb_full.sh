#!/usr/bin/env bash
# ─── Full run: Abstract on Terminal-Bench 2.0 ─────────────────────────────
#
# Usage:
#   ./bench/run_tb_full.sh                                        # Gemini + embeddings + Daytona
#   ./bench/run_tb_full.sh --model openai/gpt-5.4-2026-03-05      # OpenAI
#   ./bench/run_tb_full.sh --no-embedding                          # disable embedding search
#   ./bench/run_tb_full.sh --local                                 # local Docker
#
# Prerequisites:
#   - uv installed
#   - bench/.env with DAYTONA_API_KEY and model API keys

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ENV_FILE="$SCRIPT_DIR/.env"

# ─── Load .env ─────────────────────────────────────────────────────────────
if [[ -f "$ENV_FILE" ]]; then
  set -a
  source "$ENV_FILE"
  set +a
fi

# ─── Defaults ──────────────────────────────────────────────────────────────
MODEL="${MODEL:-google/gemini-3.1-pro-preview}"
CONCURRENT="${CONCURRENT:-20}"
DATASET="terminal-bench@2.0"
OUTPUT_DIR="$SCRIPT_DIR/tb-results"
JOB_NAME="abstract-$(date +%Y%m%d-%H%M%S)"
TIMEOUT_MULT="${TIMEOUT_MULT:-1.0}"
ATTEMPTS="${ATTEMPTS:-1}"  # Use --attempts 5 for leaderboard submission
USE_DAYTONA=true
ENABLE_EMBEDDING=true  # ON by default
EXTRA_ARGS=()

# ─── Parse args ────────────────────────────────────────────────────────────
while [[ $# -gt 0 ]]; do
  case "$1" in
    --model)            MODEL="$2"; shift 2 ;;
    --concurrent)       CONCURRENT="$2"; shift 2 ;;
    --dataset)          DATASET="$2"; shift 2 ;;
    --output)           OUTPUT_DIR="$2"; shift 2 ;;
    --timeout-mult)     TIMEOUT_MULT="$2"; shift 2 ;;
    --local)            USE_DAYTONA=false; shift ;;
    --no-embedding)     ENABLE_EMBEDDING=false; shift ;;
    --include)          EXTRA_ARGS+=("--include-task-name" "$2"); shift 2 ;;
    --exclude)          EXTRA_ARGS+=("--exclude-task-name" "$2"); shift 2 ;;
    --task)             EXTRA_ARGS+=("--include-task-name" "$2"); shift 2 ;;
    --attempts)         ATTEMPTS="$2"; shift 2 ;;
    --debug)            EXTRA_ARGS+=("--debug"); shift ;;
    --help|-h)
      echo "Usage: $0 [OPTIONS]"
      echo ""
      echo "Options:"
      echo "  --model <provider/model>  Model (default: google/gemini-3.1-pro-preview)"
      echo "  --concurrent <N>          Parallel tasks (default: 20)"
      echo "  --timeout-mult <N>        Timeout multiplier (default: 1.5)"
      echo "  --no-embedding            Disable embedding-enhanced CodeSearch"
      echo "  --local                   Use local Docker instead of Daytona"
      echo "  --include <pattern>       Include task name pattern"
      echo "  --exclude <pattern>       Exclude task name pattern"
      echo "  --task <name>             Run single task"
      echo "  --attempts <N>            Attempts per task (for pass@k)"
      echo "  --debug                   Enable debug logging"
      exit 0 ;;
    *) echo "Unknown arg: $1"; exit 1 ;;
  esac
done

# ─── Colors ────────────────────────────────────────────────────────────────
CYAN='\033[0;36m'
GREEN='\033[0;32m'
RED='\033[0;31m'
YELLOW='\033[0;33m'
DIM='\033[0;90m'
BOLD='\033[1m'
RESET='\033[0m'

info()  { echo -e "${CYAN}▸${RESET} $*"; }
pass()  { echo -e "${GREEN}✓${RESET} $*"; }
fail()  { echo -e "${RED}✗${RESET} $*"; }
warn()  { echo -e "${YELLOW}!${RESET} $*"; }

# ─── Preflight ─────────────────────────────────────────────────────────────
echo ""
echo -e "${BOLD}Abstract × Terminal-Bench 2.0${RESET}"
echo -e "${DIM}═══════════════════════════════════════════════════${RESET}"

if ! command -v uv &>/dev/null; then fail "'uv' not found."; exit 1; fi
pass "uv"

[[ -f "$SCRIPT_DIR/abstract_tbench.py" ]] && pass "Agent module" || { fail "Agent module not found"; exit 1; }
[[ -f "$SCRIPT_DIR/abstract-linux-amd64" || -f "$SCRIPT_DIR/abstract-linux-arm64" ]] && pass "Linux binary" || { fail "No binary found"; exit 1; }

ENV_FLAG=""
if $USE_DAYTONA; then
  [[ -n "${DAYTONA_API_KEY:-}" ]] && { ENV_FLAG="--env daytona"; pass "Daytona"; } || { fail "DAYTONA_API_KEY not set"; exit 1; }
else
  docker info &>/dev/null && { ENV_FLAG="--env docker"; pass "Docker"; } || { fail "Docker not running"; exit 1; }
fi

PROVIDER="${MODEL%%/*}"
case "$PROVIDER" in
  openai)    KEY_ORDER=(OPENAI_API_KEY) ;;
  google)    KEY_ORDER=(GOOGLE_API_KEY GEMINI_API_KEY) ;;
  anthropic) KEY_ORDER=(ANTHROPIC_API_KEY) ;;
  *)         KEY_ORDER=(OPENAI_API_KEY GOOGLE_API_KEY ANTHROPIC_API_KEY) ;;
esac
HAS_KEY=false; KEY_NAME=""
for key in "${KEY_ORDER[@]}"; do
  [[ -n "${!key:-}" ]] && { HAS_KEY=true; KEY_NAME="$key"; break; }
done
$HAS_KEY || { fail "No API key for '$PROVIDER'"; exit 1; }
pass "API key ($KEY_NAME)"

echo ""
echo -e "${DIM}───────────────────────────────────────────────────${RESET}"
echo -e "  ${DIM}Model:${RESET}       ${CYAN}$MODEL${RESET}"
echo -e "  ${DIM}Dataset:${RESET}     $DATASET"
echo -e "  ${DIM}Concurrent:${RESET}  $CONCURRENT"
echo -e "  ${DIM}Env:${RESET}         $($USE_DAYTONA && echo 'Daytona' || echo 'Docker')"
echo -e "  ${DIM}Timeout:${RESET}     ${TIMEOUT_MULT}x"
echo -e "  ${DIM}Embedding:${RESET}   $($ENABLE_EMBEDDING && echo 'ON (USearch + Gemini)' || echo 'off')"
echo -e "  ${DIM}Attempts:${RESET}    ${ATTEMPTS} per task"
echo -e "  ${DIM}Retries:${RESET}     2 (on agent crash)"
echo -e "  ${DIM}Job:${RESET}         $JOB_NAME"
echo -e "${DIM}───────────────────────────────────────────────────${RESET}"
echo ""

# ─── Agent kwargs ──────────────────────────────────────────────────────────
if $ENABLE_EMBEDDING; then
  EXTRA_ARGS+=("--agent-kwarg" "enable_embedding=true")
fi

# ─── Run ───────────────────────────────────────────────────────────────────
info "Starting benchmark..."
mkdir -p "$OUTPUT_DIR"
START_TIME=$(date +%s)
cd "$SCRIPT_DIR"

PYTHONPATH="$SCRIPT_DIR${PYTHONPATH:+:$PYTHONPATH}" uv run harbor run \
    --agent-import-path "abstract_tbench:AbstractAgent" \
    --model "$MODEL" \
    --dataset "$DATASET" \
    --n-concurrent "$CONCURRENT" \
    --jobs-dir "$OUTPUT_DIR" \
    --job-name "$JOB_NAME" \
    --timeout-multiplier "$TIMEOUT_MULT" \
    -k "$ATTEMPTS" \
    --max-retries 2 \
    --retry-include "NonZeroAgentExitCodeError" \
    --env-file "$ENV_FILE" \
    $ENV_FLAG \
    -y \
    ${EXTRA_ARGS[@]+"${EXTRA_ARGS[@]}"}

EXIT_CODE=$?
ELAPSED=$(( $(date +%s) - START_TIME ))
ELAPSED_MIN=$(( ELAPSED / 60 ))

echo ""
echo -e "${DIM}═══════════════════════════════════════════════════${RESET}"
if [[ $EXIT_CODE -eq 0 ]]; then
  pass "${BOLD}Completed in ${ELAPSED_MIN}m${RESET}"
else
  fail "${BOLD}Failed (exit $EXIT_CODE) after ${ELAPSED_MIN}m${RESET}"
fi
echo -e "  Results: $OUTPUT_DIR/$JOB_NAME"
echo ""

exit $EXIT_CODE
