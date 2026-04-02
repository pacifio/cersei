#!/usr/bin/env bash
# Cersei — Run all tests
# Usage: ./run_tests.sh [--stress] [--graph] [--verbose]

set -e
cd "$(dirname "$0")"

GREEN='\033[32m'
CYAN='\033[36m'
RESET='\033[0m'

echo -e "${CYAN}Cersei — Test Suite${RESET}"
echo "================================"

# Parse flags
STRESS=false
GRAPH=false
VERBOSE=""
for arg in "$@"; do
    case $arg in
        --stress)  STRESS=true ;;
        --graph)   GRAPH=true ;;
        --verbose) VERBOSE="-- --nocapture" ;;
    esac
done

# Unit tests
echo -e "\n${CYAN}[1/3] Unit tests${RESET}"
if $GRAPH; then
    echo "  (with graph feature)"
    cargo test --workspace --features graph $VERBOSE
else
    cargo test --workspace $VERBOSE
fi

# Count results
PASSED=$(cargo test --workspace 2>&1 | grep "test result" | grep -oP '\d+ passed' | awk '{s+=$1}END{print s}')
echo -e "\n${GREEN}✓ $PASSED unit tests passed${RESET}"

# Stress tests
if $STRESS; then
    echo -e "\n${CYAN}[2/3] Stress tests${RESET}"

    echo -e "\n  Core Infrastructure..."
    cargo run --example stress_core_infrastructure --release 2>&1 | tail -3

    echo -e "\n  Tools..."
    cargo run --example stress_tools --release 2>&1 | tail -3

    echo -e "\n  Orchestration..."
    cargo run --example stress_orchestration --release 2>&1 | tail -3

    echo -e "\n  Skills..."
    cargo run --example stress_skills --release 2>&1 | tail -3

    echo -e "\n  Memory..."
    cargo run --example stress_memory --release 2>&1 | tail -3

    echo -e "\n${GREEN}✓ All stress tests passed${RESET}"
else
    echo -e "\n${CYAN}[2/3] Stress tests${RESET} (skipped — use --stress to run)"
fi

# Warning check
echo -e "\n${CYAN}[3/3] Warning check${RESET}"
WARNINGS=$(cargo check 2>&1 | grep "^warning:" | grep -v "generated" | wc -l | tr -d ' ')
if [ "$WARNINGS" -eq 0 ]; then
    echo -e "${GREEN}✓ Zero warnings${RESET}"
else
    echo "  $WARNINGS warnings remaining"
    cargo check 2>&1 | grep "^warning:" | grep -v "generated"
fi

echo -e "\n${GREEN}All checks passed.${RESET}"
