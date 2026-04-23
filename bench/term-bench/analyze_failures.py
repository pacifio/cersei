#!/usr/bin/env python3
"""Analyze terminal-bench failures and generate learned patterns for the prompt.

Usage:
    python3 analyze_failures.py <result.json> [<result2.json> ...]

Outputs failure_patterns.txt with learned anti-patterns.
"""

import json
import sys
from pathlib import Path
from collections import defaultdict

def analyze_run(result_path: Path) -> dict:
    """Extract failure details from a harbor result.json."""
    data = json.load(open(result_path))

    failures = {}
    for eval_data in data.get("stats", {}).get("evals", {}).values():
        rewards = eval_data.get("reward_stats", {}).get("reward", {})
        for task_ref in rewards.get("0.0", []):
            task_name = task_ref.split("__")[0]
            failures[task_name] = "wrong_output"

        for exc_type, tasks in eval_data.get("exception_stats", {}).items():
            for task_ref in tasks:
                task_name = task_ref.split("__")[0]
                failures[task_name] = exc_type

    return failures

def main():
    if len(sys.argv) < 2:
        print("Usage: python3 analyze_failures.py <result.json> [...]")
        sys.exit(1)

    # Aggregate failures across runs
    all_failures = defaultdict(list)
    for path in sys.argv[1:]:
        failures = analyze_run(Path(path))
        for task, reason in failures.items():
            all_failures[task].append(reason)

    # Tasks that fail consistently across runs
    consistent_failures = {
        task: reasons for task, reasons in all_failures.items()
        if len(reasons) >= 2  # Failed in 2+ runs
    }

    # Generate patterns file
    output = [
        "# Auto-generated failure patterns from previous terminal-bench runs.",
        "# Injected into the agent's system prompt to avoid repeating mistakes.",
        f"# Based on {len(sys.argv)-1} run(s), {len(consistent_failures)} consistently failing tasks.",
        "",
        "# --- Consistent failures (failed 2+ times) ---",
    ]

    for task, reasons in sorted(consistent_failures.items()):
        reason_str = ", ".join(set(reasons))
        output.append(f"# {task}: failed {len(reasons)}x ({reason_str})")

    output.extend([
        "",
        "# --- General anti-patterns ---",
        "When writing to a file, always verify the file exists and has correct content afterward.",
        "When implementing algorithms, test with the examples given in the instruction before finishing.",
        "If a command fails, read the FULL error output before retrying.",
        "Do NOT run the same failing command more than twice — try a different approach.",
        "Do NOT assume file formats — read and verify first.",
        "Always check for existing code/data in /app before writing new files.",
        "",
    ])

    # Timeout-specific patterns
    timeout_tasks = [t for t, reasons in all_failures.items() if "AgentTimeoutError" in reasons]
    if timeout_tasks:
        output.append("# --- Tasks that tend to timeout ---")
        for task in sorted(timeout_tasks):
            output.append(f"# {task}: tends to timeout — work efficiently, avoid unnecessary exploration")
        output.append("")

    # Write output
    patterns_path = Path(__file__).parent / "failure_patterns.txt"
    patterns_path.write_text("\n".join(output))
    print(f"Written {len(output)} lines to {patterns_path}")
    print(f"Consistent failures: {len(consistent_failures)}")
    print(f"Timeout-prone tasks: {len(timeout_tasks)}")

if __name__ == "__main__":
    main()
