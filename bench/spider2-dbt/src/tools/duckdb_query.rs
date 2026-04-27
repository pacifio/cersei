//! `DuckDbQuery` — read-only SQL against the workdir's DuckDB with caching.
//!
//! Why this exists: `bash` + `duckdb -readonly` works but the agent has to
//! parse the CLI's text output, can't see structured schemas, and re-runs
//! the same exploratory queries (DESCRIBE, SHOW TABLES, COUNT(*)) every
//! turn because results aren't cached. This tool returns a structured
//! markdown table plus row count + execution time, and caches results
//! keyed by `(db_mtime, normalised_sql)` so repeats inside a task are free.
//!
//! Read-only is enforced by **rejecting any statement** whose first
//! non-whitespace token isn't one of `SELECT`, `WITH`, `SHOW`, `DESCRIBE`,
//! `EXPLAIN`, `PRAGMA`. The agent has the bash tool for writes anyway.

use async_trait::async_trait;
use cersei_tools::{PermissionLevel, Tool, ToolContext, ToolResult};
use parking_lot::Mutex;
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{Instant, SystemTime};

const DEFAULT_LIMIT: u32 = 200;
const MAX_LIMIT: u32 = 5_000;

pub struct DuckDbQueryTool;

#[derive(Debug, Deserialize)]
struct Input {
    sql: String,
    #[serde(default)]
    limit: Option<u32>,
    /// Optional explicit DuckDB file path. When omitted the tool walks
    /// `working_dir` to find the first `*.duckdb`.
    #[serde(default)]
    db: Option<String>,
}

#[async_trait]
impl Tool for DuckDbQueryTool {
    fn name(&self) -> &str {
        "duckdb_query"
    }

    fn description(&self) -> &str {
        "Run a read-only SQL query against this task's seeded DuckDB and \
         return a markdown table + row count + timing. Use this for SHOW \
         TABLES, DESCRIBE, COUNT(*), value spot-checks, sample row \
         inspection — anything you'd otherwise pipe to `duckdb -readonly` \
         in bash. The result is cached by (db_mtime, sql) so repeating \
         the same query mid-turn is free. Rejects writes (INSERT, UPDATE, \
         DELETE, CREATE, DROP, ALTER, COPY, ATTACH, INSTALL, LOAD); use \
         the `bash` tool with `dbt run` for writes. Default limit 200, \
         max 5000."
    }

    fn permission_level(&self) -> PermissionLevel {
        PermissionLevel::ReadOnly
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "sql":   { "type": "string", "description": "SQL to execute. Read-only statements only." },
                "limit": { "type": "integer", "description": "Max rows returned (default 200, max 5000)." },
                "db":    { "type": "string",  "description": "Optional path to the DuckDB file. Defaults to the first *.duckdb under the working directory." }
            },
            "required": ["sql"]
        })
    }

    async fn execute(&self, input: Value, ctx: &ToolContext) -> ToolResult {
        let parsed: Input = match serde_json::from_value(input) {
            Ok(p) => p,
            Err(e) => return ToolResult::error(format!("invalid input: {e}")),
        };
        let sql = parsed.sql.trim().to_string();
        if let Err(e) = ensure_read_only(&sql) {
            return ToolResult::error(e);
        }
        let limit = parsed
            .limit
            .unwrap_or(DEFAULT_LIMIT)
            .clamp(1, MAX_LIMIT);

        let db_path = match parsed.db.as_deref() {
            Some(p) => PathBuf::from(p),
            None => match discover_db(&ctx.working_dir) {
                Some(p) => p,
                None => {
                    return ToolResult::error(format!(
                        "no *.duckdb file found under {}",
                        ctx.working_dir.display()
                    ));
                }
            },
        };
        if !db_path.exists() {
            return ToolResult::error(format!("db not found: {}", db_path.display()));
        }

        let key = cache_key(&db_path, &sql, limit);
        if let Some(hit) = cache().lock().get(&key).cloned() {
            return ToolResult::success(format!("{}\n_(cache hit)_\n", hit));
        }

        let started = Instant::now();
        let outcome = run_query(&db_path, &sql, limit);
        let elapsed_ms = started.elapsed().as_millis();

        match outcome {
            Ok(result) => {
                let body = render_markdown(&result, elapsed_ms);
                cache().lock().insert(key, body.clone());
                ToolResult::success(body)
            }
            Err(e) => ToolResult::error(format!("query error after {elapsed_ms} ms: {e}")),
        }
    }
}

// ─── Cache ──────────────────────────────────────────────────────────────────

#[derive(Debug, Hash, PartialEq, Eq)]
struct CacheKey {
    db: PathBuf,
    db_mtime_nanos: u128,
    sql_norm: String,
    limit: u32,
}

fn cache() -> &'static Mutex<HashMap<CacheKey, String>> {
    static CELL: OnceLock<Mutex<HashMap<CacheKey, String>>> = OnceLock::new();
    CELL.get_or_init(|| Mutex::new(HashMap::new()))
}

fn cache_key(db: &Path, sql: &str, limit: u32) -> CacheKey {
    let mtime_nanos = std::fs::metadata(db)
        .ok()
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    CacheKey {
        db: db.to_path_buf(),
        db_mtime_nanos: mtime_nanos,
        sql_norm: normalize_sql(sql),
        limit,
    }
}

fn normalize_sql(sql: &str) -> String {
    sql.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase()
}

// ─── Read-only enforcement ──────────────────────────────────────────────────

fn ensure_read_only(sql: &str) -> Result<(), String> {
    let head = sql
        .lines()
        .find(|l| !l.trim().is_empty() && !l.trim_start().starts_with("--"))
        .unwrap_or("")
        .trim_start()
        .to_uppercase();
    let first_word = head.split_whitespace().next().unwrap_or("");
    let allowed = [
        "SELECT", "WITH", "SHOW", "DESCRIBE", "DESC", "EXPLAIN", "PRAGMA", "VALUES",
    ];
    if allowed.contains(&first_word) {
        return Ok(());
    }
    Err(format!(
        "read-only tool: rejected statement starting with `{first_word}`. \
         Allowed: {}. Use the bash tool to run dbt or modify the DB.",
        allowed.join(", ")
    ))
}

// ─── DB discovery ───────────────────────────────────────────────────────────

fn discover_db(work_dir: &Path) -> Option<PathBuf> {
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

// ─── Query execution ────────────────────────────────────────────────────────

#[derive(Debug)]
struct QueryResult {
    columns: Vec<String>,
    rows: Vec<Vec<String>>,
    truncated: bool,
}

fn run_query(db: &Path, sql: &str, limit: u32) -> Result<QueryResult, String> {
    // Take the bench-wide DuckDB open mutex (same gate workdir/comparator use).
    // We open the DB read-only so we never collide with `dbt run` writers.
    let _g = crate::comparator::DUCKDB_OPEN_GATE.lock();
    let con = duckdb::Connection::open_with_flags(
        db,
        duckdb::Config::default()
            .access_mode(duckdb::AccessMode::ReadOnly)
            .map_err(|e| format!("config: {e}"))?,
    )
    .map_err(|e| format!("open db: {e}"))?;

    let wrapped = format!("SELECT * FROM ({}) AS __cersei_q LIMIT {}", sql, limit + 1);
    let mut stmt = con.prepare(&wrapped).map_err(|e| format!("prepare: {e}"))?;
    let mut rows = stmt.query([]).map_err(|e| format!("execute: {e}"))?;

    let columns: Vec<String> = match rows.as_ref() {
        Some(r) => (0..r.column_count())
            .map(|i| r.column_name(i).map(|s| s.to_string()).unwrap_or_default())
            .collect(),
        None => Vec::new(),
    };
    let n_cols = columns.len();
    let mut out_rows: Vec<Vec<String>> = Vec::new();
    let mut over = false;

    while let Some(row) = rows.next().map_err(|e| format!("row: {e}"))? {
        if out_rows.len() as u32 >= limit {
            over = true;
            break;
        }
        let mut cells = Vec::with_capacity(n_cols);
        for i in 0..n_cols {
            let v: String = row
                .get::<_, Option<String>>(i)
                .ok()
                .flatten()
                .or_else(|| row.get::<_, Option<i64>>(i).ok().flatten().map(|x| x.to_string()))
                .or_else(|| row.get::<_, Option<f64>>(i).ok().flatten().map(|x| x.to_string()))
                .or_else(|| row.get::<_, Option<bool>>(i).ok().flatten().map(|x| x.to_string()))
                .unwrap_or_else(|| "NULL".to_string());
            cells.push(v);
        }
        out_rows.push(cells);
    }
    Ok(QueryResult {
        columns,
        rows: out_rows,
        truncated: over,
    })
}

// ─── Render ─────────────────────────────────────────────────────────────────

fn render_markdown(r: &QueryResult, elapsed_ms: u128) -> String {
    let mut s = String::new();
    s.push_str(&format!(
        "**{} row{}** in {} ms{}\n\n",
        r.rows.len(),
        if r.rows.len() == 1 { "" } else { "s" },
        elapsed_ms,
        if r.truncated { " (truncated to limit)" } else { "" }
    ));
    if r.columns.is_empty() {
        s.push_str("_(no columns)_\n");
        return s;
    }
    s.push_str("| ");
    s.push_str(&r.columns.join(" | "));
    s.push_str(" |\n|");
    for _ in &r.columns {
        s.push_str(" --- |");
    }
    s.push('\n');
    for row in &r.rows {
        s.push_str("| ");
        s.push_str(
            &row.iter()
                .map(|c| sanitize_cell(c))
                .collect::<Vec<_>>()
                .join(" | "),
        );
        s.push_str(" |\n");
    }
    s
}

/// Markdown cells must not contain raw `|` or newlines.
fn sanitize_cell(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '|' => out.push_str("\\|"),
            '\n' => out.push_str("\\n"),
            '\r' => {}
            _ => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_inserts() {
        assert!(ensure_read_only("INSERT INTO foo VALUES (1)").is_err());
        assert!(ensure_read_only("update foo set x=1").is_err());
        assert!(ensure_read_only("DROP TABLE foo").is_err());
        assert!(ensure_read_only("ATTACH 'x.db' AS db2").is_err());
    }

    #[test]
    fn allows_reads() {
        assert!(ensure_read_only("SELECT 1").is_ok());
        assert!(ensure_read_only("with a as (select 1) select * from a").is_ok());
        assert!(ensure_read_only("SHOW TABLES").is_ok());
        assert!(ensure_read_only("DESCRIBE foo").is_ok());
        assert!(ensure_read_only("EXPLAIN SELECT 1").is_ok());
        assert!(ensure_read_only("PRAGMA database_size").is_ok());
        // leading SQL comments allowed.
        assert!(ensure_read_only("-- comment\nSELECT 1").is_ok());
    }

    #[test]
    fn normalize_sql_whitespace() {
        assert_eq!(
            normalize_sql("Select   1\n  from    Foo"),
            "select 1 from foo"
        );
    }

    #[test]
    fn sanitize_cell_pipes_and_newlines() {
        assert_eq!(sanitize_cell("a|b\nc"), "a\\|b\\nc");
    }
}
