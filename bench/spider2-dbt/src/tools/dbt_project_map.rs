//! `DbtProjectMap` — single-call summary of a dbt project skeleton.
//!
//! Walks `<work_dir>/models/**/*.{yml,sql}`, parses minimal facts (model
//! names, `ref()` / `source()` deps, stub vs implemented), topologically
//! sorts the build order, and returns markdown the agent can use as a plan
//! without re-running `dbt parse` + `find` + `grep` for every turn.
//!
//! In-process cache keyed by the `(work_dir, mtime_fingerprint)` so repeated
//! calls within one task are free.
//!
//! Intentionally regex-based — no dbt internals dependency. Matches what
//! SignalPilot's `dbt_project_map` tool returns minus the heavyweight
//! Python-only features (column-contract checks live in the agent's existing
//! Read tool; date-hazard detection is a separate concern).

use async_trait::async_trait;
use cersei_tools::{PermissionLevel, Tool, ToolContext, ToolResult};
use parking_lot::Mutex;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::SystemTime;

pub struct DbtProjectMapTool;

#[async_trait]
impl Tool for DbtProjectMapTool {
    fn name(&self) -> &str {
        "dbt_project_map"
    }

    fn description(&self) -> &str {
        "Scan the dbt project rooted at the working directory and return a \
         markdown summary: every model, its file path, its YML-declared \
         columns count vs SQL select-column count (used to detect stubs), \
         its `ref()` and `source()` dependencies, and a topologically \
         sorted build order. Use this once at the start of a task instead \
         of running `dbt parse` + `find models -name '*.yml'` + grepping \
         SQL for refs. Cached by mtime fingerprint — calling it again \
         after editing a model is cheap. No arguments."
    }

    fn permission_level(&self) -> PermissionLevel {
        PermissionLevel::ReadOnly
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "focus": {
                    "type": "string",
                    "description": "Optional model name. When set, restrict the \
                                    output to this model's neighbourhood (its \
                                    upstream + downstream within 1 hop)."
                }
            }
        })
    }

    async fn execute(&self, input: Value, ctx: &ToolContext) -> ToolResult {
        let focus = input.get("focus").and_then(|v| v.as_str()).map(str::to_string);
        let work_dir = ctx.working_dir.clone();
        let models_dir = work_dir.join("models");
        if !models_dir.exists() {
            return ToolResult::error(format!(
                "{} has no models/ subdir — is this a dbt project?",
                work_dir.display()
            ));
        }

        let map = match scan_or_cached(&work_dir) {
            Ok(m) => m,
            Err(e) => return ToolResult::error(format!("scan failed: {e}")),
        };

        let md = render_markdown(&map, focus.as_deref());
        ToolResult::success(md)
    }
}

// ─── Core data ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
struct Model {
    name: String,
    sql_path: PathBuf,
    yml_path: Option<PathBuf>,
    yml_columns: Vec<String>,
    sql_select_columns: Option<usize>,
    refs: Vec<String>,
    sources: Vec<(String, String)>,
    is_stub: bool,
    materialized: Option<String>,
}

#[derive(Debug, Clone)]
struct ProjectMap {
    models: BTreeMap<String, Model>,
    work_order: Vec<String>,
    cycles: Vec<String>,
    missing_refs: BTreeSet<String>,
}

// ─── Cache ──────────────────────────────────────────────────────────────────

#[derive(Debug)]
struct CacheEntry {
    fingerprint: u64,
    map: ProjectMap,
}

fn cache() -> &'static Mutex<HashMap<PathBuf, CacheEntry>> {
    static CELL: OnceLock<Mutex<HashMap<PathBuf, CacheEntry>>> = OnceLock::new();
    CELL.get_or_init(|| Mutex::new(HashMap::new()))
}

fn fingerprint(work_dir: &Path) -> u64 {
    use std::hash::{Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    let walk = walkdir::WalkDir::new(work_dir.join("models"))
        .max_depth(8)
        .into_iter()
        .flatten()
        .filter(|e| e.file_type().is_file());
    for e in walk {
        let p = e.path();
        let ext = p.extension().and_then(|s| s.to_str()).unwrap_or("");
        if !matches!(ext, "yml" | "yaml" | "sql") {
            continue;
        }
        if let Ok(meta) = e.metadata() {
            if let Ok(mtime) = meta.modified() {
                if let Ok(d) = mtime.duration_since(SystemTime::UNIX_EPOCH) {
                    use std::hash::Hash;
                    p.hash(&mut h);
                    d.as_nanos().hash(&mut h);
                }
            }
        }
    }
    h.finish()
}

fn scan_or_cached(work_dir: &Path) -> anyhow::Result<ProjectMap> {
    let fp = fingerprint(work_dir);
    {
        let g = cache().lock();
        if let Some(entry) = g.get(work_dir) {
            if entry.fingerprint == fp {
                return Ok(entry.map.clone());
            }
        }
    }
    let m = scan(work_dir)?;
    cache().lock().insert(
        work_dir.to_path_buf(),
        CacheEntry {
            fingerprint: fp,
            map: m.clone(),
        },
    );
    Ok(m)
}

// ─── Scan ───────────────────────────────────────────────────────────────────

fn scan(work_dir: &Path) -> anyhow::Result<ProjectMap> {
    let models_dir = work_dir.join("models");
    let mut models: BTreeMap<String, Model> = BTreeMap::new();

    // First pass: SQL files. The model name is the filename stem.
    for entry in walkdir::WalkDir::new(&models_dir)
        .max_depth(8)
        .into_iter()
        .flatten()
        .filter(|e| {
            e.file_type().is_file()
                && e.path().extension().and_then(|s| s.to_str()) == Some("sql")
                && !e
                    .path()
                    .components()
                    .any(|c| c.as_os_str() == "dbt_packages")
        })
    {
        let path = entry.path().to_path_buf();
        let name = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_string();
        if name.is_empty() {
            continue;
        }
        let sql = std::fs::read_to_string(&path).unwrap_or_default();
        let refs = extract_refs(&sql);
        let sources = extract_sources(&sql);
        let select_cols = count_top_select_columns(&sql);
        let is_stub = looks_like_stub(&sql, &refs, &sources, select_cols);
        let materialized = extract_materialized(&sql);
        models.insert(
            name.clone(),
            Model {
                name,
                sql_path: path,
                yml_path: None,
                yml_columns: Vec::new(),
                sql_select_columns: select_cols,
                refs,
                sources,
                is_stub,
                materialized,
            },
        );
    }

    // Second pass: YML files. Attach yml column lists to models discovered above.
    for entry in walkdir::WalkDir::new(&models_dir)
        .max_depth(8)
        .into_iter()
        .flatten()
        .filter(|e| {
            e.file_type().is_file()
                && matches!(
                    e.path().extension().and_then(|s| s.to_str()),
                    Some("yml") | Some("yaml")
                )
                && !e
                    .path()
                    .components()
                    .any(|c| c.as_os_str() == "dbt_packages")
        })
    {
        let path = entry.path();
        let raw = match std::fs::read_to_string(path) {
            Ok(s) => s,
            Err(_) => continue,
        };
        let yml: serde_yaml::Value = match serde_yaml::from_str(&raw) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let Some(model_list) = yml.get("models").and_then(|v| v.as_sequence()) else {
            continue;
        };
        for entry in model_list {
            let name = match entry.get("name").and_then(|n| n.as_str()) {
                Some(s) => s.to_string(),
                None => continue,
            };
            let cols = entry
                .get("columns")
                .and_then(|c| c.as_sequence())
                .map(|seq| {
                    seq.iter()
                        .filter_map(|c| c.get("name").and_then(|n| n.as_str()).map(|s| s.to_string()))
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            if let Some(m) = models.get_mut(&name) {
                m.yml_path = Some(path.to_path_buf());
                m.yml_columns = cols;
            }
        }
    }

    let (work_order, cycles) = topo_sort(&models);
    let known: BTreeSet<&str> = models.keys().map(|s| s.as_str()).collect();
    let mut missing_refs = BTreeSet::new();
    for m in models.values() {
        for r in &m.refs {
            if !known.contains(r.as_str()) {
                missing_refs.insert(r.clone());
            }
        }
    }

    Ok(ProjectMap {
        models,
        work_order,
        cycles,
        missing_refs,
    })
}

// ─── Parse helpers ──────────────────────────────────────────────────────────

fn extract_refs(sql: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut i = 0;
    let bytes = sql.as_bytes();
    while i < bytes.len() {
        if let Some(rest) = sql.get(i..).and_then(|s| s.find("ref(").map(|n| (n, s))) {
            let (n, _s) = rest;
            let start = i + n + 4;
            if start >= sql.len() {
                break;
            }
            let tail = &sql[start..];
            // tolerate `ref('name')` and `ref("name")` and `ref( 'name' )`.
            let trimmed = tail.trim_start();
            let opener = trimmed.chars().next();
            if matches!(opener, Some('\'') | Some('"')) {
                let q = opener.unwrap();
                let after = &trimmed[1..];
                if let Some(end) = after.find(q) {
                    out.push(after[..end].trim().to_string());
                }
            }
            i = start;
        } else {
            break;
        }
    }
    out.sort();
    out.dedup();
    out
}

fn extract_sources(sql: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < sql.len() {
        if let Some(rel) = sql[i..].find("source(") {
            let start = i + rel + 7;
            if start >= sql.len() {
                break;
            }
            // Pull two quoted strings.
            let tail = &sql[start..];
            let mut chars = tail.char_indices();
            let mut acc: Vec<String> = Vec::new();
            let mut cur = String::new();
            let mut in_quote: Option<char> = None;
            while let Some((_pos, c)) = chars.next() {
                match (in_quote, c) {
                    (Some(q), c) if c == q => {
                        acc.push(std::mem::take(&mut cur));
                        in_quote = None;
                        if acc.len() == 2 {
                            break;
                        }
                    }
                    (Some(_), c) => cur.push(c),
                    (None, '\'') | (None, '"') => in_quote = Some(c),
                    (None, ')') => break,
                    _ => {}
                }
            }
            if acc.len() == 2 {
                out.push((acc[0].clone(), acc[1].clone()));
            }
            i = start;
        } else {
            break;
        }
    }
    out.sort();
    out.dedup();
    out
}

/// Cheap heuristic: count comma-separated identifiers in the *outermost*
/// SELECT clause. Used only to flag stubs (`SELECT 1`, `SELECT * FROM …`,
/// `SELECT NULL AS …`).
fn count_top_select_columns(sql: &str) -> Option<usize> {
    let lower = sql.to_lowercase();
    let select_idx = lower.find("select ")?;
    let from_idx = lower[select_idx + 7..]
        .find(" from ")
        .map(|n| select_idx + 7 + n)?;
    let body = &sql[select_idx + 7..from_idx];
    let mut depth = 0i32;
    let mut count = 1usize;
    for c in body.chars() {
        match c {
            '(' => depth += 1,
            ')' => depth -= 1,
            ',' if depth == 0 => count += 1,
            _ => {}
        }
    }
    Some(count)
}

fn looks_like_stub(
    sql: &str,
    refs: &[String],
    sources: &[(String, String)],
    select_cols: Option<usize>,
) -> bool {
    let lc = sql.to_lowercase();
    if lc.trim().is_empty() {
        return true;
    }
    if refs.is_empty() && sources.is_empty() {
        // No deps + tiny body → stub.
        let body_len = lc
            .lines()
            .filter(|l| !l.trim().is_empty() && !l.trim_start().starts_with("--"))
            .map(|l| l.len())
            .sum::<usize>();
        if body_len < 80 {
            return true;
        }
    }
    if select_cols == Some(1) && lc.contains("select 1") {
        return true;
    }
    if lc.contains("select null as") && refs.is_empty() {
        return true;
    }
    false
}

fn extract_materialized(sql: &str) -> Option<String> {
    let lower = sql.to_lowercase();
    let idx = lower.find("materialized")?;
    let after = &sql[idx..];
    let q = after.chars().find(|c| *c == '\'' || *c == '"')?;
    let mut found = false;
    let mut cur = String::new();
    for c in after.chars() {
        if c == q {
            if found {
                return Some(cur);
            }
            found = true;
            continue;
        }
        if found {
            cur.push(c);
        }
    }
    None
}

// ─── Topo sort (Kahn) ───────────────────────────────────────────────────────

fn topo_sort(models: &BTreeMap<String, Model>) -> (Vec<String>, Vec<String>) {
    let mut indeg: BTreeMap<&str, usize> = models.keys().map(|k| (k.as_str(), 0)).collect();
    let mut adj: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for m in models.values() {
        for r in &m.refs {
            if models.contains_key(r) {
                adj.entry(r.as_str()).or_default().push(m.name.as_str());
                *indeg.entry(m.name.as_str()).or_insert(0) += 1;
            }
        }
    }
    let mut queue: Vec<&str> = indeg
        .iter()
        .filter(|(_, n)| **n == 0)
        .map(|(k, _)| *k)
        .collect();
    queue.sort();
    let mut out = Vec::new();
    while let Some(n) = queue.pop() {
        out.push(n.to_string());
        if let Some(children) = adj.get(n) {
            for c in children {
                let entry = indeg.get_mut(*c).unwrap();
                *entry -= 1;
                if *entry == 0 {
                    queue.push(*c);
                }
            }
        }
        queue.sort();
    }
    let cycles: Vec<String> = indeg
        .into_iter()
        .filter_map(|(k, v)| if v > 0 { Some(k.to_string()) } else { None })
        .collect();
    (out, cycles)
}

// ─── Render ─────────────────────────────────────────────────────────────────

fn render_markdown(map: &ProjectMap, focus: Option<&str>) -> String {
    let mut s = String::new();
    s.push_str(&format!(
        "# dbt project — {} models, {} stubs\n\n",
        map.models.len(),
        map.models.values().filter(|m| m.is_stub).count()
    ));

    let in_focus: Box<dyn Fn(&str) -> bool> = match focus {
        None => Box::new(|_| true),
        Some(f) => {
            let f = f.to_string();
            let neighbors: BTreeSet<String> = {
                let mut set = BTreeSet::new();
                set.insert(f.clone());
                if let Some(m) = map.models.get(&f) {
                    set.extend(m.refs.iter().cloned());
                }
                for m in map.models.values() {
                    if m.refs.iter().any(|r| r == &f) {
                        set.insert(m.name.clone());
                    }
                }
                set
            };
            Box::new(move |name| neighbors.contains(name))
        }
    };

    s.push_str("## models\n\n");
    s.push_str("| name | status | mat | yml cols | sql cols | refs | sources |\n");
    s.push_str("|---|---|---|---:|---:|---|---|\n");
    for m in map.models.values() {
        if !in_focus(&m.name) {
            continue;
        }
        let status = if m.is_stub { "STUB" } else { "ok" };
        let mat = m.materialized.as_deref().unwrap_or("(view)");
        let yml_n = m.yml_columns.len();
        let sql_n = m.sql_select_columns.map(|n| n.to_string()).unwrap_or_else(|| "?".into());
        let refs = if m.refs.is_empty() { "—".into() } else { m.refs.join(", ") };
        let sources = if m.sources.is_empty() {
            "—".into()
        } else {
            m.sources
                .iter()
                .map(|(s, t)| format!("{s}.{t}"))
                .collect::<Vec<_>>()
                .join(", ")
        };
        s.push_str(&format!(
            "| `{}` | {} | {} | {} | {} | {} | {} |\n",
            m.name, status, mat, yml_n, sql_n, refs, sources
        ));
    }
    s.push('\n');

    if !map.work_order.is_empty() {
        s.push_str("## build order\n\n");
        for (i, name) in map.work_order.iter().enumerate() {
            if !in_focus(name) {
                continue;
            }
            s.push_str(&format!("{}. `{}`\n", i + 1, name));
        }
        s.push('\n');
    }

    let stubs: Vec<&Model> = map.models.values().filter(|m| m.is_stub).collect();
    if !stubs.is_empty() {
        s.push_str("## stubs to rewrite\n\n");
        for m in stubs {
            s.push_str(&format!(
                "- `{}` — {}\n",
                m.name,
                m.sql_path
                    .strip_prefix(m.sql_path.parent().unwrap_or(Path::new(".")).parent().unwrap_or(Path::new(".")))
                    .unwrap_or(&m.sql_path)
                    .display()
            ));
        }
        s.push('\n');
    }

    if !map.cycles.is_empty() {
        s.push_str("## ⚠ cycles detected\n\n");
        for n in &map.cycles {
            s.push_str(&format!("- `{n}`\n"));
        }
        s.push('\n');
    }

    if !map.missing_refs.is_empty() {
        s.push_str("## ⚠ refs to undeclared models\n\n");
        for r in &map.missing_refs {
            s.push_str(&format!("- `{r}`\n"));
        }
        s.push('\n');
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_refs_basic() {
        let sql = "select * from {{ ref('foo') }} join {{ ref(\"bar\") }} on x";
        let r = extract_refs(sql);
        assert_eq!(r, vec!["bar", "foo"]);
    }

    #[test]
    fn extract_sources_basic() {
        let sql = "select * from {{ source('raw', 'orders') }}";
        let r = extract_sources(sql);
        assert_eq!(r, vec![("raw".into(), "orders".into())]);
    }

    #[test]
    fn count_select_one_basic() {
        let sql = "select 1 from foo";
        assert_eq!(count_top_select_columns(sql), Some(1));
    }

    #[test]
    fn count_select_many_basic() {
        let sql = "select a, b, coalesce(c, d), e from foo";
        assert_eq!(count_top_select_columns(sql), Some(4));
    }

    #[test]
    fn looks_like_stub_select_one() {
        assert!(looks_like_stub("select 1", &[], &[], Some(1)));
    }

    #[test]
    fn looks_like_stub_real_model() {
        let sql = "with a as (select * from {{ ref('x') }}) select id, name, total from a where active";
        let refs = vec!["x".to_string()];
        assert!(!looks_like_stub(sql, &refs, &[], Some(3)));
    }
}
