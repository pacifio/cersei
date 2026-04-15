#!/usr/bin/env bash
# ─── Full run: Abstract on Terminal-Bench 2.0 (leaderboard) ───────────────
#
# Usage:
#   ./bench/run_tb_full.sh                                        # defaults (Daytona + Gemini)
#   ./bench/run_tb_full.sh --model openai/gpt-5.4-2026-03-05      # OpenAI
#   ./bench/run_tb_full.sh --concurrent 20                         # more parallel
#   ./bench/run_tb_full.sh --local                                 # use local Docker
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
CONCURRENT=20
DATASET="terminal-bench@2.0"
OUTPUT_DIR="$SCRIPT_DIR/tb-results"
JOB_NAME="abstract-$(date +%Y%m%d-%H%M%S)"
TIMEOUT_MULT="${TIMEOUT_MULT:-1.0}"
USE_DAYTONA=true
EXTRA_ARGS=()

# ─── Parse args ────────────────────────────────────────────────────────────
while [[ $# -gt 0 ]]; do
  case "$1" in
    --model)        MODEL="$2"; shift 2 ;;
    --concurrent)   CONCURRENT="$2"; shift 2 ;;
    --dataset)      DATASET="$2"; shift 2 ;;
    --output)       OUTPUT_DIR="$2"; shift 2 ;;
    --timeout-mult) TIMEOUT_MULT="$2"; shift 2 ;;
    --local)        USE_DAYTONA=false; shift ;;
    --include)      EXTRA_ARGS+=("--include-task-name" "$2"); shift 2 ;;
    --exclude)      EXTRA_ARGS+=("--exclude-task-name" "$2"); shift 2 ;;
    --task)         EXTRA_ARGS+=("--include-task-name" "$2"); shift 2 ;;
    --attempts)     EXTRA_ARGS+=("--n-attempts" "$2"); shift 2 ;;
    --debug)        EXTRA_ARGS+=("--debug"); shift ;;
    --help|-h)
      echo "Usage: $0 [OPTIONS]"
      echo ""
      echo "Run Abstract on the full terminal-bench 2.0 dataset."
      echo ""
      echo "Options:"
      echo "  --model <provider/model>  Model (default: google/gemini-3.1-pro-preview)"
      echo "  --concurrent <N>          Parallel tasks (default: 20)"
      echo "  --dataset <name@ver>      Dataset (default: terminal-bench@2.0)"
      echo "  --output <dir>            Output directory"
      echo "  --timeout-mult <N>        Timeout multiplier (default: 1.0)"
      echo "  --local                   Use local Docker instead of Daytona"
      echo "  --include <pattern>       Include task name pattern (glob)"
      echo "  --exclude <pattern>       Exclude task name pattern (glob)"
      echo "  --task <name>             Run single task by name"
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
echo -e "${BOLD}Abstract × Terminal-Bench 2.0 — Full Run${RESET}"
echo -e "${DIM}═══════════════════════════════════════════════════${RESET}"

if ! command -v uv &>/dev/null; then
  fail "'uv' not found. Install: curl -LsSf https://astral.sh/uv/install.sh | sh"
  exit 1
fi
pass "uv"

if [[ ! -f "$SCRIPT_DIR/abstract_tbench.py" ]]; then
  fail "Agent module not found"
  exit 1
fi
pass "Agent module"

if [[ ! -f "$SCRIPT_DIR/abstract-linux-arm64" ]]; then
  fail "Linux binary not found. See TERMINAL_BENCH.md"
  exit 1
fi
pass "Linux binary"

# Environment check
ENV_FLAG=""
if $USE_DAYTONA; then
  if [[ -z "${DAYTONA_API_KEY:-}" ]]; then
    fail "DAYTONA_API_KEY not set. Add it to bench/.env or use --local"
    exit 1
  fi
  ENV_FLAG="--env daytona"
  pass "Daytona"
else
  if ! docker info &>/dev/null; then
    fail "Docker is not running"
    exit 1
  fi
  ENV_FLAG="--env docker"
  pass "Docker (local)"
fi

# API key check
PROVIDER="${MODEL%%/*}"
case "$PROVIDER" in
  openai)    KEY_ORDER=(OPENAI_API_KEY) ;;
  google)    KEY_ORDER=(GOOGLE_API_KEY GEMINI_API_KEY) ;;
  anthropic) KEY_ORDER=(ANTHROPIC_API_KEY) ;;
  *)         KEY_ORDER=(OPENAI_API_KEY GOOGLE_API_KEY ANTHROPIC_API_KEY) ;;
esac

HAS_KEY=false
KEY_NAME=""
for key in "${KEY_ORDER[@]}"; do
  if [[ -n "${!key:-}" ]]; then
    HAS_KEY=true
    KEY_NAME="$key"
    break
  fi
done

if ! $HAS_KEY; then
  fail "No API key for provider '$PROVIDER'. Add ${KEY_ORDER[0]} to bench/.env"
  exit 1
fi
pass "API key ($KEY_NAME)"

echo ""
echo -e "${DIM}───────────────────────────────────────────────────${RESET}"
echo -e "  ${DIM}Model:${RESET}       ${CYAN}$MODEL${RESET}"
echo -e "  ${DIM}Dataset:${RESET}     $DATASET"
echo -e "  ${DIM}Concurrent:${RESET}  $CONCURRENT"
echo -e "  ${DIM}Env:${RESET}         $($USE_DAYTONA && echo 'Daytona (cloud)' || echo 'Docker (local)')"
echo -e "  ${DIM}Timeout:${RESET}     ${TIMEOUT_MULT}x"
echo -e "  ${DIM}Job:${RESET}         $JOB_NAME"
echo -e "  ${DIM}Output:${RESET}      $OUTPUT_DIR"
echo -e "${DIM}───────────────────────────────────────────────────${RESET}"
echo ""

# ─── Run ───────────────────────────────────────────────────────────────────
info "Starting full benchmark..."
if $USE_DAYTONA; then
  echo -e "${DIM}  Running on Daytona cloud with $CONCURRENT concurrent containers${RESET}"
else
  echo -e "${DIM}  Running locally — estimated 2-4 hours${RESET}"
fi
echo ""

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
