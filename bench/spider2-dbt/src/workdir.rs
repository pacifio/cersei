//! Per-task workdir lifecycle.
//!
//! Steps:
//!   1. Create a temp dir
//!   2. Copy the task example skeleton into it
//!   3. Open the seeded DuckDB and write a `reference_snapshot.md` capturing
//!      pre-existing tables: row counts, column types, 3-row sample. The agent
//!      reads this to know its target.
//!   4. Hand back the workdir path + the seeded DB path.

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

use crate::dataset::{SuiteLayout, Task};

const SAMPLE_ROWS: usize = 3;

#[derive(Debug)]
pub struct Workdir {
    pub root: PathBuf,
    /// The DuckDB file the agent should write into. Discovered after copy.
    pub db_path: Option<PathBuf>,
    /// Number of pre-existing tables (informational).
    pub seeded_tables: usize,
}

/// Copy `<examples>/<instance_id>/` into a fresh tempdir. Returns the workdir
/// owned by the caller (drop deletes the dir unless `--keep-workdirs` was set,
/// which the runner handles by leaking the TempDir handle).
///
/// `eval_tables` filters the reference snapshot — when set, only those table
/// names (and any obvious upstream sources sharing a prefix) are captured.
/// On large projects (shopify001 has 47 tables) this makes the verifier's
/// CHECK 6 value-spot-check land on the 2-3 tables that actually matter.
pub fn prepare(
    layout: &SuiteLayout,
    task: &Task,
    into: &Path,
    eval_tables: Option<&[String]>,
) -> Result<Workdir> {
    let src = layout.examples_root.join(&task.instance_id);
    if !src.exists() {
        return Err(anyhow::anyhow!(
            "task skeleton not found at {} (instance_id: {})",
            src.display(),
            task.instance_id
        ));
    }
    std::fs::create_dir_all(into).with_context(|| format!("mkdir {}", into.display()))?;

    let mut copy_opts = fs_extra::dir::CopyOptions::new();
    copy_opts.copy_inside = true;
    copy_opts.overwrite = true;
    fs_extra::dir::copy(&src, into, &copy_opts)
        .with_context(|| format!("copy {} -> {}", src.display(), into.display()))?;

    let dest = into.join(src.file_name().unwrap_or_default());
    let db_path = discover_duckdb(&dest);
    let mut seeded_tables = 0;
    if let Some(ref db) = db_path {
        // Pre-install common DuckDB extensions ONCE per workdir so the agent's
        // `dbt run` invocations don't auto-fetch over the network mid-turn.
        // The official Spider2-DBT projects routinely use icu/json/parquet.
        // Extension files cache to `~/.duckdb/` and are shared across all
        // duckdb processes (CLI + dbt-duckdb), so this single warm-up
        // eliminates the network-hang failure mode that was burning the
        // 30-minute phase budget on retries.
        if let Err(e) = preinstall_extensions(db) {
            tracing::warn!(error = %e, "pre-install of duckdb extensions failed; agent may hit network timeouts");
        }
        seeded_tables = write_reference_snapshot(db, &dest, eval_tables).unwrap_or_else(|e| {
            tracing::warn!(error = %e, "reference_snapshot capture failed; agent will lack pre-existing row counts");
            0
        });
    }
    Ok(Workdir {
        root: dest,
        db_path,
        seeded_tables,
    })
}

/// Pre-install + load extensions duckdb's auto-loader would otherwise fetch
/// on first use. Installs to the shared user cache (~/.duckdb/extensions/...),
/// so `dbt-duckdb`'s subprocesses pick them up without their own network
/// fetch.
fn preinstall_extensions(db: &Path) -> Result<()> {
    let _g = crate::comparator::DUCKDB_OPEN_GATE.lock();
    let con = duckdb::Connection::open(db).context("open duckdb for ext warm-up")?;
    for ext in ["icu", "json", "parquet", "httpfs"] {
        // Some envs lack network entirely — log and continue if INSTALL fails.
        if let Err(e) = con.execute_batch(&format!("INSTALL {ext}; LOAD {ext};")) {
            tracing::debug!(ext, error = %e, "extension preinstall skipped");
        }
    }
    Ok(())
}

fn discover_duckdb(dir: &Path) -> Option<PathBuf> {
    walkdir::WalkDir::new(dir)
        .max_depth(3)
        .into_iter()
        .flatten()
        .find(|e| {
            e.file_type().is_file()
                && e.path().extension().and_then(|s| s.to_str()) == Some("duckdb")
        })
        .map(|e| e.path().to_path_buf())
}

/// Capture pre-existing tables into `<workdir>/reference_snapshot.md`. Verbatim
/// shape (table, row_count, columns + types, sample rows) so the verifier
/// subagent's "CHECK 3 — Row Count" and "CHECK 6 — Value Spot-Check" can match
/// SignalPilot's prompt expectations character-for-character.
fn write_reference_snapshot(
    db: &Path,
    work_dir: &Path,
    eval_tables: Option<&[String]>,
) -> Result<usize> {
    let _g = crate::comparator::DUCKDB_OPEN_GATE.lock();
    let con = duckdb::Connection::open(db).context("open seeded duckdb")?;
    let all_tables: Vec<String> = {
        let mut stmt = con.prepare("SHOW TABLES")?;
        stmt.query_map([], |r| r.get::<_, String>(0))?
            .filter_map(|r| r.ok())
            .collect()
    };

    // Filter the snapshot to (a) every eval-relevant table and (b) any
    // pre-existing table whose name shares a token with one of those names
    // (catches obvious upstream sources like `shopify_order_data` upstream
    // of `shopify__orders`). Falls back to all tables when the eval config
    // has no `condition_tabs`.
    let table_names: Vec<&String> = match eval_tables {
        Some(want) if !want.is_empty() => {
            let want_lower: Vec<String> = want.iter().map(|s| s.to_lowercase()).collect();
            all_tables
                .iter()
                .filter(|t| {
                    let tl = t.to_lowercase();
                    want_lower.iter().any(|w| {
                        tl == *w
                            || tl.replace("fct_", "fact_") == *w
                            || tl.replace("fact_", "fct_") == *w
                            || w.split('_')
                                .filter(|seg| seg.len() >= 4)
                                .any(|seg| tl.contains(seg))
                    })
                })
                .collect()
        }
        _ => all_tables.iter().collect(),
    };

    let mut md = String::new();
    md.push_str("# Reference snapshot\n\n");
    md.push_str(
        "Captured BEFORE the agent ran. These row counts and column types are the\n\
         build target — the gold DB compares your output against rows derived from\n\
         this state. If your model overwrites a pre-existing table, the verifier\n\
         expects the new rows to align with the row count and types here.\n\n",
    );
    if let Some(want) = eval_tables {
        if !want.is_empty() {
            md.push_str(&format!(
                "Filtered to eval-relevant tables (out of {} total in the seeded DB):\n",
                all_tables.len()
            ));
            for w in want {
                md.push_str(&format!("- `{w}`\n"));
            }
            md.push('\n');
        }
    }

    for tab in &table_names {
        let tab: &str = tab.as_str();
        let safe = tab.replace('"', "\"\"");
        let count: i64 = con
            .query_row(&format!("SELECT COUNT(*) FROM \"{safe}\""), [], |r| r.get(0))
            .unwrap_or(0);
        md.push_str(&format!("## {tab}\n\n"));
        md.push_str(&format!("- row_count: {count}\n"));

        // Column types.
        let cols: Vec<(String, String)> = {
            let mut stmt = con.prepare(&format!("PRAGMA table_info(\"{safe}\")"))?;
            stmt.query_map([], |r| {
                Ok((r.get::<_, String>(1)?, r.get::<_, String>(2)?))
            })?
            .filter_map(|r| r.ok())
            .collect()
        };
        if !cols.is_empty() {
            md.push_str("- columns:\n");
            for (name, ty) in &cols {
                md.push_str(&format!("  - `{name}` ({ty})\n"));
            }
        }

        // Sample rows: small Markdown table.
        let header = cols
            .iter()
            .map(|(n, _)| n.clone())
            .collect::<Vec<_>>();
        if !header.is_empty() && count > 0 {
            md.push_str("\n| ");
            md.push_str(&header.join(" | "));
            md.push_str(" |\n|");
            for _ in &header {
                md.push_str(" --- |");
            }
            md.push('\n');
            let mut stmt = con.prepare(&format!(
                "SELECT * FROM \"{safe}\" LIMIT {SAMPLE_ROWS}"
            ))?;
            let mut rows = stmt.query([])?;
            // duckdb-rs requires execution before column_count is valid.
            let n_cols = rows.as_ref().map(|r| r.column_count()).unwrap_or(0);
            while let Some(r) = rows.next()? {
                let mut cells = Vec::with_capacity(n_cols);
                for i in 0..n_cols {
                    let v: String = r
                        .get::<_, Option<String>>(i)
                        .ok()
                        .flatten()
                        .or_else(|| r.get::<_, Option<i64>>(i).ok().flatten().map(|x| x.to_string()))
                        .or_else(|| r.get::<_, Option<f64>>(i).ok().flatten().map(|x| x.to_string()))
                        .or_else(|| r.get::<_, Option<bool>>(i).ok().flatten().map(|x| x.to_string()))
                        .unwrap_or_else(|| "NULL".to_string());
                    cells.push(v);
                }
                md.push_str("| ");
                md.push_str(&cells.join(" | "));
                md.push_str(" |\n");
            }
        }
        md.push('\n');
    }

    std::fs::write(work_dir.join("reference_snapshot.md"), md)
        .context("write reference_snapshot.md")?;
    Ok(table_names.len())
}
