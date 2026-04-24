#!/usr/bin/env bash
# Thin wrapper around run_dry_tb.sh that uses Google models.
#
# SECURITY: the API key MUST come from the environment — never hardcode it
# here. If the key is in your shell history, `source` an env file instead:
#
#     source ~/.abstract-bench.env && ./bench/term-bench/runner-google.sh
#
# where the env file contains a line like `export GOOGLE_API_KEY=AIza...`.

set -euo pipefail

if [[ -z "${GOOGLE_API_KEY:-}" ]]; then
  echo "error: GOOGLE_API_KEY is not set." >&2
  echo "       source ~/.abstract-bench.env or export it manually before running." >&2
  exit 1
fi

exec bash "$(dirname "${BASH_SOURCE[0]}")/run_dry_tb.sh" --model google/gemini-3.1-pro-preview "$@"
