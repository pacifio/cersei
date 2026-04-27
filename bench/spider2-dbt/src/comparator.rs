//! Spider2-DBT result-vs-gold comparator.
//!
//! Replicates `compare_pandas_table()` from the official Spider2-DBT
//! `eval_utils.py`. Paraphrasing kills the parity guarantee — every constant,
//! tolerance, and special-case here is intentional.
//!
//! Contract: for each gold column (selected by positional index from `cols`),
//! check that ANY column in the prediction matches as a vector. Row count must
//! match. Numeric tolerance is `abs_tol = 1e-2`; NaN-equals-NaN; datetime
//! mismatches normalise via suffix-stripping; sort key for `ignore_order` is
//! `(0, 0.0, "")` (nulls first), then numbers, then strings.

use anyhow::{Context, Result};
use duckdb::Connection;
use std::collections::HashMap;
use std::path::Path;

/// Process-wide gate around every Rust-side `duckdb::Connection::open` call.
///
/// Why: duckdb-rs links the C libduckdb. Concurrent opens from different
/// threads (e.g. comparator on task A finishing while workdir on task B is
/// preparing) trigger SIGSEGV inside the C library. The cost is negligible —
/// open is microseconds and we serialize only the *open*, not the queries.
/// Agent-side `duckdb` CLI invocations are separate processes and unaffected.
pub static DUCKDB_OPEN_GATE: parking_lot::Mutex<()> = parking_lot::Mutex::new(());

const TOLERANCE: f64 = 1e-2;

/// One cell in a result/gold table. Stays close to DuckDB's value union so we
/// preserve numeric vs string distinction (the comparator branches on it).
#[derive(Debug, Clone)]
pub enum Cell {
    Null,
    Int(i64),
    Float(f64),
    Bool(bool),
    Text(String),
}

impl Cell {
    fn is_null(&self) -> bool {
        matches!(self, Cell::Null) || matches!(self, Cell::Float(f) if f.is_nan())
    }

    fn as_f64(&self) -> Option<f64> {
        match self {
            Cell::Int(i) => Some(*i as f64),
            Cell::Float(f) => Some(*f),
            Cell::Bool(true) => Some(1.0),
            Cell::Bool(false) => Some(0.0),
            _ => None,
        }
    }

    /// String form used for both string-equality and the date suffix-strip path.
    fn as_str_repr(&self) -> String {
        match self {
            Cell::Null => String::new(),
            Cell::Int(i) => i.to_string(),
            Cell::Float(f) => f.to_string(),
            Cell::Bool(b) => b.to_string(),
            Cell::Text(s) => s.clone(),
        }
    }

    /// True when this cell looks like a datetime/date (controls the
    /// suffix-strip normalisation path in the original).
    fn looks_like_datetime(&self) -> bool {
        // We approximate Python's `isinstance(a, (datetime, date, pd.Timestamp))`
        // by looking at the textual shape — DuckDB returns dates/timestamps as
        // strings via our extractor anyway.
        if let Cell::Text(s) = self {
            // Heuristic: contains '-' in date positions OR ends with 00:00:00 / .0 / T...
            (s.len() >= 10 && s.as_bytes().get(4) == Some(&b'-') && s.as_bytes().get(7) == Some(&b'-'))
                || s.contains('T')
                || s.ends_with(":00:00")
        } else {
            false
        }
    }
}

/// Comparator-level sort key, port of `_sort_key`. Returns a borrow-free tuple
/// since `f64` isn't `Ord`; caller breaks ties via `partial_cmp`.
fn sort_key(c: &Cell) -> (u8, f64, String) {
    match c {
        Cell::Null => (0, 0.0, String::new()),
        Cell::Float(f) if f.is_nan() => (0, 0.0, String::new()),
        Cell::Int(i) => (1, *i as f64, String::new()),
        Cell::Float(f) => (1, *f, String::new()),
        Cell::Bool(true) => (1, 1.0, String::new()),
        Cell::Bool(false) => (1, 0.0, String::new()),
        Cell::Text(s) => (2, 0.0, s.clone()),
    }
}

/// Datetime suffix-stripping equality, port of `_normalize_for_compare`.
/// Returns Some(true) if equal-after-normalisation, Some(false) if not, None
/// if neither side looks like a datetime (caller should fall back to ==).
fn normalize_for_compare(a: &Cell, b: &Cell) -> Option<bool> {
    if !a.looks_like_datetime() && !b.looks_like_datetime() {
        return None;
    }
    let mut sa = a.as_str_repr();
    let mut sb = b.as_str_repr();
    // .rstrip('0').rstrip('.')
    sa = sa.trim_end_matches('0').trim_end_matches('.').to_string();
    sb = sb.trim_end_matches('0').trim_end_matches('.').to_string();
    for suffix in [" 00:00:00", ".0", "T00:00:00"] {
        if let Some(t) = sa.strip_suffix(suffix) {
            sa = t.to_string();
        }
        if let Some(t) = sb.strip_suffix(suffix) {
            sb = t.to_string();
        }
    }
    Some(sa == sb)
}

/// Compare two same-shape vectors. Port of `vectors_match`.
fn vectors_match(v1: &[Cell], v2: &[Cell], ignore_order: bool) -> bool {
    let mut a: Vec<Cell> = v1.to_vec();
    let mut b: Vec<Cell> = v2.to_vec();
    if ignore_order {
        a.sort_by(|x, y| {
            let (kx, fx, sx) = sort_key(x);
            let (ky, fy, sy) = sort_key(y);
            kx.cmp(&ky)
                .then_with(|| fx.partial_cmp(&fy).unwrap_or(std::cmp::Ordering::Equal))
                .then_with(|| sx.cmp(&sy))
        });
        b.sort_by(|x, y| {
            let (kx, fx, sx) = sort_key(x);
            let (ky, fy, sy) = sort_key(y);
            kx.cmp(&ky)
                .then_with(|| fx.partial_cmp(&fy).unwrap_or(std::cmp::Ordering::Equal))
                .then_with(|| sx.cmp(&sy))
        });
    }
    if a.len() != b.len() {
        return false;
    }
    for (x, y) in a.iter().zip(b.iter()) {
        if x.is_null() && y.is_null() {
            continue;
        }
        if let (Some(fx), Some(fy)) = (x.as_f64(), y.as_f64()) {
            if (fx - fy).abs() > TOLERANCE {
                // close-enough check (abs_tol semantics)
                return false;
            }
            continue;
        }
        // Fallback: string equality with datetime-suffix normalisation.
        if x.as_str_repr() == y.as_str_repr() {
            continue;
        }
        match normalize_for_compare(x, y) {
            Some(true) => continue,
            _ => return false,
        }
    }
    true
}

/// Per-table eval config (port of one row of spider2_eval.jsonl after
/// `_normalize_eval_entry`).
#[derive(Debug, Clone)]
pub struct EvalParams {
    pub gold: String,
    pub condition_tabs: Option<Vec<String>>,
    pub condition_cols: Option<Vec<Vec<usize>>>,
    pub ignore_orders: Option<Vec<bool>>,
}

/// Top-level result for one task.
#[derive(Debug, Clone)]
pub struct EvalOutcome {
    pub pass: bool,
    pub details: String,
}

fn list_tables(con: &Connection) -> Result<Vec<String>> {
    let mut stmt = con.prepare("SHOW TABLES").context("SHOW TABLES")?;
    let rows = stmt
        .query_map([], |r| r.get::<_, String>(0))
        .context("show tables iter")?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r.context("show tables row")?);
    }
    Ok(out)
}

/// Read a whole table into Vec<Vec<Cell>> column-major. Returns
/// `(column_names, columns)` so the caller can index by name AND position.
///
/// Note: duckdb-rs panics on `Statement::column_count` / `column_name` until
/// the statement has been executed. We therefore call `query` first (which
/// drives execution), pull column metadata off the resulting `Rows`, and only
/// then walk rows.
fn read_table(con: &Connection, table: &str) -> Result<(Vec<String>, Vec<Vec<Cell>>)> {
    let q = format!("SELECT * FROM \"{}\"", table.replace('"', "\"\""));
    let mut stmt = con.prepare(&q).with_context(|| format!("prepare {q}"))?;
    let mut rows = stmt.query([]).with_context(|| format!("query {q}"))?;

    // Column metadata is only valid after the statement has been executed —
    // pull it from the Rows handle (which holds the executed cursor).
    let names: Vec<String> = match rows.as_ref() {
        Some(r) => (0..r.column_count())
            .map(|i| r.column_name(i).map(|s| s.to_string()).unwrap_or_default())
            .collect(),
        None => Vec::new(),
    };
    let n_cols = names.len();
    let mut columns: Vec<Vec<Cell>> = vec![Vec::new(); n_cols];

    while let Some(row) = rows.next().context("row iter")? {
        for i in 0..n_cols {
            let val: Cell = if let Ok(v) = row.get::<_, Option<i64>>(i) {
                v.map_or(Cell::Null, Cell::Int)
            } else if let Ok(v) = row.get::<_, Option<f64>>(i) {
                v.map_or(Cell::Null, Cell::Float)
            } else if let Ok(v) = row.get::<_, Option<bool>>(i) {
                v.map_or(Cell::Null, Cell::Bool)
            } else if let Ok(v) = row.get::<_, Option<String>>(i) {
                v.map_or(Cell::Null, Cell::Text)
            } else {
                Cell::Null
            };
            columns[i].push(val);
        }
    }
    Ok((names, columns))
}

/// Resolve a fct_ ↔ fact_ / case-insensitive table name (port of the
/// `effective_tabs` resolution loop in `comparator.py:165-185`).
fn resolve_table_name(want: &str, available: &[String]) -> Option<String> {
    if available.iter().any(|t| t == want) {
        return Some(want.to_string());
    }
    let want_lower = want.to_lowercase();
    let want_fact = want_lower.replace("fct_", "fact_");
    let want_fct = want_lower.replace("fact_", "fct_");
    for got in available {
        let got_lower = got.to_lowercase();
        if want_lower == got_lower || want_fact == got_lower || want_fct == got_lower {
            return Some(got.clone());
        }
    }
    None
}

/// Run the full evaluation: open both DuckDB files, walk eval params, compare
/// every selected column vector. Returns (pass, details-string).
pub fn evaluate(result_db: &Path, gold_db: &Path, params: &EvalParams) -> Result<EvalOutcome> {
    if !result_db.exists() {
        return Ok(EvalOutcome {
            pass: false,
            details: format!("Result DB not found: {}", result_db.display()),
        });
    }
    if !gold_db.exists() {
        return Ok(EvalOutcome {
            pass: false,
            details: format!("Gold DB not found: {}", gold_db.display()),
        });
    }
    let (gold_con, result_con) = {
        let _g = DUCKDB_OPEN_GATE.lock();
        let g = Connection::open(gold_db).context("open gold")?;
        let r = Connection::open(result_db).context("open result")?;
        (g, r)
    };
    let gold_tables = list_tables(&gold_con)?;
    let result_tables = list_tables(&result_con)?;

    let raw_tabs: Vec<String> = params
        .condition_tabs
        .clone()
        .unwrap_or_else(|| gold_tables.clone());
    let n = raw_tabs.len();
    let effective_orders: Vec<bool> = params.ignore_orders.clone().unwrap_or_else(|| vec![false; n]);
    let effective_cols: Vec<Vec<usize>> = params.condition_cols.clone().unwrap_or_else(|| vec![Vec::new(); n]);

    let mut all_match = true;
    let mut details: Vec<String> = Vec::new();

    for (i, tab) in raw_tabs.iter().enumerate() {
        let resolved_gold = resolve_table_name(tab, &gold_tables).unwrap_or_else(|| tab.clone());
        let resolved_pred = match resolve_table_name(&resolved_gold, &result_tables) {
            Some(t) => t,
            None => {
                all_match = false;
                details.push(format!("  {tab}: FAIL — table not in result DB (have: {result_tables:?})"));
                continue;
            }
        };

        let (gold_names, gold_cols) = match read_table(&gold_con, &resolved_gold) {
            Ok(t) => t,
            Err(e) => {
                all_match = false;
                details.push(format!("  {tab}: ERROR reading gold table — {e}"));
                continue;
            }
        };
        let (_pred_names, pred_cols) = match read_table(&result_con, &resolved_pred) {
            Ok(t) => t,
            Err(e) => {
                all_match = false;
                details.push(format!("  {tab}: ERROR reading pred table — {e}"));
                continue;
            }
        };

        let cols_idx: &[usize] = &effective_cols[i];
        let selected_gold: Vec<&Vec<Cell>> = if cols_idx.is_empty() {
            gold_cols.iter().collect()
        } else {
            let mut out = Vec::with_capacity(cols_idx.len());
            for &ci in cols_idx {
                if ci >= gold_cols.len() {
                    all_match = false;
                    details.push(format!("  {tab}: FAIL — gold column index error: {ci}"));
                    out.clear();
                    break;
                }
                out.push(&gold_cols[ci]);
            }
            if out.is_empty() {
                continue;
            }
            out
        };

        // Row count check uses any selected column (they're all the same length).
        let gold_rows = selected_gold.first().map(|c| c.len()).unwrap_or(0);
        let pred_rows = pred_cols.first().map(|c| c.len()).unwrap_or(0);
        if gold_rows != pred_rows {
            all_match = false;
            details.push(format!(
                "  {tab}: FAIL — row count mismatch gold={gold_rows} pred={pred_rows}"
            ));
            continue;
        }

        // Each selected gold column must match SOME pred column.
        let mut tab_pass = true;
        let mut failed_col: Option<String> = None;
        for (k, gold_vec) in selected_gold.iter().enumerate() {
            let any_match = pred_cols
                .iter()
                .any(|pred_vec| vectors_match(gold_vec, pred_vec, effective_orders[i]));
            if !any_match {
                tab_pass = false;
                let col_idx = if cols_idx.is_empty() { k } else { cols_idx[k] };
                failed_col = Some(
                    gold_names
                        .get(col_idx)
                        .cloned()
                        .unwrap_or_else(|| col_idx.to_string()),
                );
                break;
            }
        }
        if tab_pass {
            details.push(format!("  {tab}: PASS"));
        } else {
            all_match = false;
            details.push(format!(
                "  {tab}: FAIL — no pred column matched gold column '{}'",
                failed_col.unwrap_or_default()
            ));
        }
    }

    Ok(EvalOutcome {
        pass: all_match,
        details: details.join("\n"),
    })
}

/// Find the first `*.duckdb` file inside `<gold_root>/<instance_id>/`.
pub fn find_gold_db(gold_root: &Path, instance_id: &str) -> Option<std::path::PathBuf> {
    let dir = gold_root.join(instance_id);
    walkdir::WalkDir::new(&dir)
        .max_depth(1)
        .into_iter()
        .flatten()
        .find(|e| {
            e.file_type().is_file()
                && e.path().extension().and_then(|s| s.to_str()) == Some("duckdb")
        })
        .map(|e| e.path().to_path_buf())
}

/// Find the result DB created by the agent. Looks for the configured filename
/// first, then falls back to any `*.duckdb` under `<work_dir>/`.
pub fn find_result_db(work_dir: &Path, configured: &str) -> Option<std::path::PathBuf> {
    let direct = work_dir.join(configured);
    if direct.exists() {
        return Some(direct);
    }
    walkdir::WalkDir::new(work_dir)
        .max_depth(4)
        .into_iter()
        .flatten()
        .find(|e| {
            e.file_type().is_file()
                && e.path().extension().and_then(|s| s.to_str()) == Some("duckdb")
        })
        .map(|e| e.path().to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cell_int(i: i64) -> Cell {
        Cell::Int(i)
    }
    fn cell_text(s: &str) -> Cell {
        Cell::Text(s.into())
    }
    fn cell_float(f: f64) -> Cell {
        Cell::Float(f)
    }

    #[test]
    fn vectors_match_exact_order() {
        let a = vec![cell_int(1), cell_int(2), cell_int(3)];
        let b = vec![cell_int(1), cell_int(2), cell_int(3)];
        assert!(vectors_match(&a, &b, false));
    }

    #[test]
    fn vectors_match_ignore_order() {
        let a = vec![cell_int(3), cell_int(1), cell_int(2)];
        let b = vec![cell_int(1), cell_int(2), cell_int(3)];
        assert!(vectors_match(&a, &b, true));
        assert!(!vectors_match(&a, &b, false));
    }

    #[test]
    fn vectors_match_floats_within_tolerance() {
        let a = vec![cell_float(1.001), cell_float(2.0)];
        let b = vec![cell_float(1.005), cell_float(1.999)];
        assert!(vectors_match(&a, &b, false));
    }

    #[test]
    fn vectors_match_floats_outside_tolerance_fail() {
        let a = vec![cell_float(1.0), cell_float(2.0)];
        let b = vec![cell_float(1.5), cell_float(2.0)];
        assert!(!vectors_match(&a, &b, false));
    }

    #[test]
    fn vectors_match_nulls_align() {
        let a = vec![Cell::Null, cell_int(1)];
        let b = vec![Cell::Null, cell_int(1)];
        assert!(vectors_match(&a, &b, false));
    }

    #[test]
    fn datetime_suffix_strip_matches() {
        // Verbatim port of comparator.py: rstrip('0').rstrip('.') then strip
        // ' 00:00:00' / '.0' / 'T00:00:00'. The "T00:00:00" path is the one
        // that survives rstrip — `2024-01-01T00:00:00.0` rstrips '0' → `…0.`,
        // then '.' → `2024-01-01T00:00:00`, then the suffix strips cleanly.
        let a = vec![cell_text("2024-01-01")];
        let b = vec![cell_text("2024-01-01T00:00:00.0")];
        assert!(vectors_match(&a, &b, false));
    }

    #[test]
    fn resolve_fct_to_fact() {
        let avail = vec!["fact_orders".to_string(), "dim_users".to_string()];
        assert_eq!(resolve_table_name("fct_orders", &avail), Some("fact_orders".into()));
        assert_eq!(resolve_table_name("dim_users", &avail), Some("dim_users".into()));
        assert_eq!(resolve_table_name("missing", &avail), None);
    }

    #[test]
    fn sort_key_orders_null_then_num_then_str() {
        let mut v = vec![
            cell_text("z"),
            cell_int(2),
            Cell::Null,
            cell_int(1),
            cell_text("a"),
        ];
        v.sort_by(|x, y| {
            let (kx, fx, sx) = sort_key(x);
            let (ky, fy, sy) = sort_key(y);
            kx.cmp(&ky)
                .then_with(|| fx.partial_cmp(&fy).unwrap_or(std::cmp::Ordering::Equal))
                .then_with(|| sx.cmp(&sy))
        });
        // Null first, then 1, 2, then "a", "z"
        match &v[0] {
            Cell::Null => {}
            other => panic!("expected Null first, got {other:?}"),
        }
    }

    // Suppress dead-code warning for unused helper imports.
    #[allow(dead_code)]
    fn _hashmap_compile_anchor() -> HashMap<String, String> {
        HashMap::new()
    }
}
