#!/usr/bin/env bash
# Download the LongMemEval datasets from HuggingFace.
# Usage:
#   ./setup.sh            # pulls all three: oracle, s, m
#   ./setup.sh oracle     # just oracle (15 MB)
#   ./setup.sh s          # just small (265 MB)
#   ./setup.sh m          # just medium (2.6 GB — skip unless you're running phase 2)

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
DATA_DIR="$SCRIPT_DIR/data"
mkdir -p "$DATA_DIR"

VARIANTS=("${@:-oracle s}") # default: oracle + s (skip m unless asked)

fetch() {
  local name="$1"
  local target="$DATA_DIR/longmemeval_${name}.json"
  if [[ -s "$target" ]]; then
    echo "  ✓ longmemeval_${name} already present ($(du -h "$target" | cut -f1))"
    return
  fi
  local url="https://huggingface.co/datasets/xiaowu0162/longmemeval/resolve/main/longmemeval_${name}"
  echo "→ downloading longmemeval_${name} from ${url}"
  curl -fSL --progress-bar -o "$target" "$url"
  echo "  ✓ wrote $target ($(du -h "$target" | cut -f1))"
}

for v in "${VARIANTS[@]}"; do
  case "$v" in
    oracle|s|m) fetch "$v" ;;
    *) echo "unknown variant: $v (expected oracle|s|m)" >&2; exit 2 ;;
  esac
done

echo "done."
