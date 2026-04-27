#!/usr/bin/env bash
# Spider2-DBT bench setup. Idempotent.
#
# Stage 1: Clones xlang-ai/Spider2 into $SPIDER2_DBT_DIR (default
#          ~/spider2-repo/spider2-dbt) if missing.
# Stage 2: Validates the layout. The official release ships:
#            - examples/<instance_id>/...     (70 dbt project skeletons)
#            - evaluation_suite/gold/spider2_eval.jsonl  (68 eval entries)
#          But NOT the source/gold .duckdb files — those come from two
#          gdown archives + python setup.py inside the dataset dir.
#          We surface clear next-step instructions if those are missing.

set -euo pipefail

ROOT="${SPIDER2_DBT_DIR:-$HOME/spider2-repo/spider2-dbt}"
PARENT="$(dirname "$ROOT")"

if [[ ! -d "$ROOT" ]]; then
  mkdir -p "$PARENT"
  cd "$PARENT"
  if [[ -d "Spider2" ]]; then
    echo "→ found existing $PARENT/Spider2 — symlinking spider2-dbt subdir"
  else
    echo "→ cloning xlang-ai/Spider2 into $PARENT"
    git clone --depth 1 https://github.com/xlang-ai/Spider2.git
  fi
  if [[ -d "$PARENT/Spider2/spider2-dbt" ]]; then
    ln -sfn "$PARENT/Spider2/spider2-dbt" "$ROOT"
  else
    echo "✗ $PARENT/Spider2/spider2-dbt missing after clone — inspect manually" >&2
    exit 1
  fi
fi

echo "→ validating layout under $ROOT"
fail=0
for f in "examples" "evaluation_suite"; do
  if [[ ! -e "$ROOT/$f" ]]; then
    echo "✗ missing $ROOT/$f" >&2
    fail=1
  fi
done

EVAL_JSONL=""
if   [[ -f "$ROOT/evaluation_suite/gold/spider2_eval.jsonl"     ]]; then
  EVAL_JSONL="$ROOT/evaluation_suite/gold/spider2_eval.jsonl"
elif [[ -f "$ROOT/evaluation_suite/spider2_eval.jsonl"           ]]; then
  EVAL_JSONL="$ROOT/evaluation_suite/spider2_eval.jsonl"
else
  echo "✗ missing $ROOT/evaluation_suite/gold/spider2_eval.jsonl" >&2
  fail=1
fi

if (( fail )); then
  exit 1
fi

EVAL_COUNT=$(wc -l < "$EVAL_JSONL" | tr -d ' ')
EXAMPLE_COUNT=$(find "$ROOT/examples" -mindepth 1 -maxdepth 1 -type d | wc -l | tr -d ' ')
GOLD_DBS=$(find "$ROOT/evaluation_suite/gold" -maxdepth 3 -name "*.duckdb" 2>/dev/null | wc -l | tr -d ' ')

echo "✓ skeletons:    $EXAMPLE_COUNT examples"
echo "✓ eval entries: $EVAL_COUNT in $EVAL_JSONL"
echo "  gold DBs:     $GOLD_DBS .duckdb files under evaluation_suite/gold/"

if (( GOLD_DBS == 0 )); then
  cat <<EOF >&2

⚠  No gold .duckdb files found. Spider2-DBT ships these via two Google Drive
   archives + a setup script. The bench cannot evaluate without them.

   Run inside the dataset dir:

     cd "$ROOT"
     pip install gdown                       # one-off
     gdown 'https://drive.google.com/uc?id=1N3f7BSWC4foj-V-1C9n8M2XmgV7FOcqL'
     gdown 'https://drive.google.com/uc?id=1s0USV_iQLo4oe05QqAMnhGGp5jeejCzp'
     python setup.py                         # unpacks into examples/ + gold/

EOF
  exit 1
fi

cat <<EOF
✓ Spider2-DBT dataset ready at $ROOT

Next:
  source .env                                 # GOOGLE_API_KEY (header auth)
  cargo run --release -p spider2-dbt-bench -- --suite smoke --limit 5 \\
    --model gemini-3.1-pro-preview
EOF
