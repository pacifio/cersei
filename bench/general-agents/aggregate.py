"""Merge all results/<framework>.json into results/summary.json.

Fails loudly on schema drift so silent bitrot between runs is impossible.
"""
from __future__ import annotations

import json
from pathlib import Path

RESULTS_DIR = Path(__file__).parent / "results"

REQUIRED_TOP_LEVEL = {
    "framework",
    "version",
    "host",
    "axis_1_instantiation_us",
    "axis_2_per_agent_bytes",
    "axis_3_max_concurrent",
    "axis_4_graph_recall_us",
    "axis_5_semantic_search_us",
}


def validate(report: dict, path: Path) -> None:
    missing = REQUIRED_TOP_LEVEL - set(report)
    if missing:
        raise SystemExit(f"{path.name}: missing keys {sorted(missing)}")


def main() -> None:
    reports = []
    for p in sorted(RESULTS_DIR.glob("*.json")):
        if p.name == "summary.json":
            continue
        try:
            data = json.loads(p.read_text())
        except json.JSONDecodeError as e:
            raise SystemExit(f"{p.name}: invalid JSON ({e})")
        validate(data, p)
        reports.append(data)

    if not reports:
        raise SystemExit(f"No results in {RESULTS_DIR}")

    summary = {
        "schema_version": 1,
        "frameworks": sorted(r["framework"] for r in reports),
        "reports": reports,
    }
    out = RESULTS_DIR / "summary.json"
    out.write_text(json.dumps(summary, indent=2))
    print(f"Wrote {out} with {len(reports)} framework(s): {summary['frameworks']}")


if __name__ == "__main__":
    main()
