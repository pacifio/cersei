//! Spider2-DBT dataset loader.
//!
//! Reads `spider2-dbt.jsonl` (task definitions) and `spider2_eval.jsonl` (eval
//! configs) from `$SPIDER2_DBT_DIR`.

use anyhow::{anyhow, Context, Result};
use serde::Deserialize;
use std::path::{Path, PathBuf};

use crate::comparator::EvalParams;

#[derive(Debug, Clone, Deserialize)]
pub struct Task {
    pub instance_id: String,
    pub instruction: String,
    /// Free-form — Spider2 sometimes ships extra fields. We don't care, we
    /// just pass the original JSON line through to the agent.
    #[serde(default)]
    pub db_id: Option<String>,
}

/// Layout of one Spider2-DBT suite on disk. Reflects the official xlang-ai
/// release: `evaluation_suite/gold/spider2_eval.jsonl` is the catalogue, and
/// each `examples/<instance_id>/` is a self-contained dbt project.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct SuiteLayout {
    /// Root dir, e.g. `~/spider2-repo/spider2-dbt`.
    pub root: PathBuf,
    /// Eval JSONL path. The official release stores it at
    /// `<root>/evaluation_suite/gold/spider2_eval.jsonl`. We also accept
    /// `<root>/evaluation_suite/spider2_eval.jsonl` for forks that hoist it.
    pub eval_jsonl: PathBuf,
    /// Per-task example skeleton root: `<root>/examples/`.
    pub examples_root: PathBuf,
    /// Gold DBs root. The official release puts gold .duckdb files directly
    /// under `<root>/evaluation_suite/gold/<instance_id>/` after running
    /// `python setup.py` to unpack the gdown archives.
    pub gold_root: PathBuf,
}

impl SuiteLayout {
    pub fn discover(root: &Path) -> Result<Self> {
        let root = root.to_path_buf();
        if !root.exists() {
            return Err(anyhow!(
                "Spider2-DBT dataset not found at {} — see bench/spider2-dbt/setup.sh",
                root.display()
            ));
        }
        let examples_root = root.join("examples");
        if !examples_root.exists() {
            return Err(anyhow!(
                "Missing {}/examples — clone xlang-ai/Spider2 first",
                root.display()
            ));
        }
        let gold_root = root.join("evaluation_suite").join("gold");
        let eval_jsonl = {
            let a = gold_root.join("spider2_eval.jsonl");
            let b = root.join("evaluation_suite").join("spider2_eval.jsonl");
            if a.exists() {
                a
            } else if b.exists() {
                b
            } else {
                return Err(anyhow!(
                    "Could not find spider2_eval.jsonl under {}/evaluation_suite",
                    root.display()
                ));
            }
        };
        Ok(Self {
            root,
            eval_jsonl,
            examples_root,
            gold_root,
        })
    }
}

/// Load every task. The official Spider2-DBT release does not ship a
/// per-instance instruction file, so the catalogue is derived from the
/// intersection of `evaluation_suite/gold/spider2_eval.jsonl` and the
/// `examples/<instance_id>/` skeletons that exist on disk. The instruction
/// is synthesised from the project layout (it points the agent at the dbt
/// project and asks it to produce the tables the comparator will read).
pub fn load_tasks(layout: &SuiteLayout) -> Result<Vec<Task>> {
    let raw = std::fs::read_to_string(&layout.eval_jsonl)
        .with_context(|| format!("read {}", layout.eval_jsonl.display()))?;
    let mut instance_ids: Vec<String> = Vec::new();
    for (i, line) in raw.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        #[derive(Deserialize)]
        struct OnlyId {
            instance_id: String,
        }
        let id: OnlyId = serde_json::from_str(line)
            .with_context(|| format!("parse eval-jsonl line {}: {line}", i + 1))?;
        instance_ids.push(id.instance_id);
    }

    let mut out = Vec::new();
    for id in instance_ids {
        let example_dir = layout.examples_root.join(&id);
        if !example_dir.exists() {
            tracing::warn!(
                instance_id = %id,
                "skipping: missing examples/{} (dataset incomplete)",
                id
            );
            continue;
        }
        out.push(Task {
            instance_id: id.clone(),
            instruction: synthesize_instruction(&id),
            db_id: None,
        });
    }
    Ok(out)
}

/// Synthesise the natural-language instruction since the official Spider2-DBT
/// release stores the requirement in the dbt project structure (YML contracts
/// + stub SQL files) rather than a free-text JSONL field. The system prompt
/// already tells the agent to read `dbt_project.yml`, walk `models/**/*.yml`,
/// and rewrite stub SQL files; the per-task instruction just pins it to this
/// instance.
fn synthesize_instruction(instance_id: &str) -> String {
    format!(
        "Solve Spider2-DBT instance `{instance_id}`. The dbt project skeleton \
         is in your working directory; `dbt_project.yml` configures it and \
         `models/**/*.yml` lists every model that must be materialised as a \
         DuckDB table. Stub `*.sql` files are placeholders you must rewrite. \
         Build every model so that the resulting DuckDB matches the YML \
         contracts and the row counts captured in `reference_snapshot.md`."
    )
}

/// Spider2's eval JSONL has two shapes:
///   - flat:   `{"instance_id": ..., "condition_cols": [...], "ignore_order": true, ...}`
///   - nested: `{"instance_id": ..., "evaluation": {"parameters": {...}}}`
///
/// `_normalize_eval_entry` in tasks.py canonicalises both into the nested form.
/// We do the same here on parse.
#[derive(Debug, Deserialize)]
struct RawEvalEntry {
    instance_id: String,
    #[serde(default)]
    evaluation: Option<RawNested>,
    #[serde(default)]
    condition_cols: Option<Vec<Vec<usize>>>,
    #[serde(default)]
    condition_tabs: Option<Vec<String>>,
    #[serde(default)]
    ignore_order: Option<bool>,
    #[serde(default)]
    ignore_orders: Option<Vec<bool>>,
    #[serde(default)]
    gold: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RawNested {
    parameters: RawNestedParams,
}

#[derive(Debug, Deserialize)]
struct RawNestedParams {
    #[serde(default)]
    condition_cols: Option<Vec<Vec<usize>>>,
    #[serde(default)]
    condition_tabs: Option<Vec<String>>,
    #[serde(default)]
    ignore_orders: Option<Vec<bool>>,
    #[serde(default)]
    gold: Option<String>,
}

pub fn load_eval(layout: &SuiteLayout, instance_id: &str) -> Result<Option<EvalParams>> {
    if !layout.eval_jsonl.exists() {
        return Ok(None);
    }
    let raw = std::fs::read_to_string(&layout.eval_jsonl)
        .with_context(|| format!("read {}", layout.eval_jsonl.display()))?;
    for line in raw.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let entry: RawEvalEntry =
            serde_json::from_str(line).with_context(|| format!("parse eval line: {line}"))?;
        if entry.instance_id != instance_id {
            continue;
        }
        // Promote to nested.
        if let Some(nested) = entry.evaluation {
            return Ok(Some(EvalParams {
                gold: nested.parameters.gold.unwrap_or_default(),
                condition_tabs: nested.parameters.condition_tabs,
                condition_cols: nested.parameters.condition_cols,
                ignore_orders: nested.parameters.ignore_orders,
            }));
        }
        // Flat form: `ignore_order: bool` collapses into a 1-element `ignore_orders`.
        let ignore_orders = entry
            .ignore_orders
            .or_else(|| entry.ignore_order.map(|b| vec![b]));
        return Ok(Some(EvalParams {
            gold: entry.gold.unwrap_or_default(),
            condition_tabs: entry.condition_tabs,
            condition_cols: entry.condition_cols,
            ignore_orders,
        }));
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_flat_eval_entry() {
        let line = r#"{"instance_id":"x","condition_cols":[[0,1]],"ignore_order":true,"gold":"x.duckdb"}"#;
        let raw: RawEvalEntry = serde_json::from_str(line).unwrap();
        assert_eq!(raw.instance_id, "x");
        assert_eq!(raw.condition_cols.as_ref().unwrap()[0], vec![0, 1]);
        assert_eq!(raw.ignore_order, Some(true));
    }

    #[test]
    fn parses_nested_eval_entry() {
        let line = r#"{"instance_id":"x","evaluation":{"parameters":{"condition_cols":[[2]],"gold":"y.duckdb"}}}"#;
        let raw: RawEvalEntry = serde_json::from_str(line).unwrap();
        assert_eq!(raw.instance_id, "x");
        assert_eq!(raw.evaluation.unwrap().parameters.condition_cols.unwrap()[0], vec![2]);
    }
}
