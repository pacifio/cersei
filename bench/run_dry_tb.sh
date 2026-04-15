#!/usr/bin/env bash
# ─── Dry run: Abstract on Terminal-Bench 2.0 (single task smoke test) ──────
#
# Usage:
#   ./bench/run_dry_tb.sh                                        # defaults
#   ./bench/run_dry_tb.sh --model google/gemini-3.1-pro-preview  # Gemini
#   ./bench/run_dry_tb.sh --task build-cython-ext                # different task
#   ./bench/run_dry_tb.sh --local                                # use local Docker instead of Daytona
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
TASK="break-filter-js-from-html"
DATASET="terminal-bench@2.0"
OUTPUT_DIR="$SCRIPT_DIR/tb-results/dry-run"
TIMEOUT_MULT="${TIMEOUT_MULT:-1.0}"
USE_DAYTONA=true

# ─── Parse args ────────────────────────────────────────────────────────────
while [[ $# -gt 0 ]]; do
  case "$1" in
    --model)        MODEL="$2"; shift 2 ;;
    --task)         TASK="$2"; shift 2 ;;
    --timeout-mult) TIMEOUT_MULT="$2"; shift 2 ;;
    --local)        USE_DAYTONA=false; shift ;;
    --help|-h)
      echo "Usage: $0 [--model provider/model] [--task name] [--local]"
      echo ""
      echo "Options:"
      echo "  --model         Model (default: google/gemini-3.1-pro-preview)"
      echo "  --task          Task name (default: break-filter-js-from-html)"
      echo "  --timeout-mult  Timeout multiplier (default: 1.0)"
      echo "  --local         Use local Docker instead of Daytona"
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
echo -e "${BOLD}Abstract × Terminal-Bench 2.0 — Dry Run${RESET}"
echo -e "${DIM}─────────────────────────────────────────${RESET}"

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

echo -e "${DIM}─────────────────────────────────────────${RESET}"
echo -e "  Model:    ${CYAN}$MODEL${RESET}"
echo -e "  Task:     $TASK"
echo -e "  Env:      $($USE_DAYTONA && echo 'Daytona (cloud)' || echo 'Docker (local)')"
echo -e "  Timeout:  ${TIMEOUT_MULT}x"
echo -e "${DIM}─────────────────────────────────────────${RESET}"
echo ""

# ─── Run ───────────────────────────────────────────────────────────────────
info "Running harbor..."
mkdir -p "$OUTPUT_DIR"
cd "$SCRIPT_DIR"

PYTHONPATH="$SCRIPT_DIR${PYTHONPATH:+:$PYTHONPATH}" uv run harbor run \
    --agent-import-path "abstract_tbench:AbstractAgent" \
    --model "$MODEL" \
    --dataset "$DATASET" \
    --include-task-name "$TASK" \
    --n-concurrent 1 \
    --jobs-dir "$OUTPUT_DIR" \
    --timeout-multiplier "$TIMEOUT_MULT" \
    --env-file "$ENV_FILE" \
    $ENV_FLAG \
    -y

EXIT_CODE=$?

echo ""
echo -e "${DIM}─────────────────────────────────────────${RESET}"
if [[ $EXIT_CODE -eq 0 ]]; then
  pass "Dry run completed"
else
  fail "Dry run failed (exit $EXIT_CODE)"
fi
echo -e "  Results: $OUTPUT_DIR"
echo ""

exit $EXIT_CODE
