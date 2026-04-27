//! Per-task + summary result writers. Same shape as
//! `bench/long-mem/results-018/summary-longmemeval_s.json` so existing leaderboard
//! tooling doesn't need a new parser.

use anyhow::{Context, Result};
use serde::Serialize;
use std::path::Path;

#[derive(Debug, Clone, Serialize)]
pub struct TaskResult {
    pub instance_id: String,
    pub pass: bool,
    pub turns: u32,
    pub elapsed_ms: u128,
    pub fail_reason: Option<String>,
    /// Whether the run used libfaketime + a known gold-build-date.
    pub deterministic_dates: bool,
    /// Brief comparator trace (per-table verdicts). Truncated to 4 KB.
    pub details: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Summary {
    pub config: String,
    pub model: String,
    pub total: usize,
    pub passed: usize,
    pub pass_rate: f64,
    pub avg_elapsed_ms: u128,
    pub total_elapsed_ms: u128,
    pub deterministic_dates: bool,
    pub timestamp: String,
    pub per_task: Vec<TaskResult>,
}

pub fn write_summary(path: &Path, summary: &Summary) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    let json = serde_json::to_string_pretty(summary)?;
    std::fs::write(path, json).with_context(|| format!("write {}", path.display()))?;
    Ok(())
}

pub fn truncate_details(s: &str, max_bytes: usize) -> String {
    if s.len() <= max_bytes {
        return s.to_string();
    }
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…(truncated)", &s[..end])
}
