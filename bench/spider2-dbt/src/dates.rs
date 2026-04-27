//! Date determinism — libfaketime wrapper.
//!
//! SignalPilot's `derive_gold_dates.py` reverse-engineers the date each gold
//! DB was built on (calendar spines, age columns, CURRENT_DATE landmarks).
//! That mapping lives in `gold_build_dates.json`. When the agent's `dbt run`
//! is invoked, libfaketime injects that date as `CURRENT_DATE` so models
//! using `current_date - INTERVAL '7 days'` produce the same rows the gold
//! was built from.
//!
//! On macOS / Windows we don't have libfaketime, so we just return the env
//! unchanged. The runner reports this and bench results carry a deterministic
//! flag in the summary.

use anyhow::{Context, Result};
use std::collections::HashMap;
use std::path::Path;

#[derive(Debug, Clone, Default)]
pub struct GoldDates {
    map: HashMap<String, String>,
}

impl GoldDates {
    pub fn empty() -> Self {
        Self::default()
    }

    pub fn load(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self::empty());
        }
        let raw = std::fs::read_to_string(path)
            .with_context(|| format!("read {}", path.display()))?;
        let map: HashMap<String, String> =
            serde_json::from_str(&raw).context("parse gold_build_dates.json")?;
        Ok(Self { map })
    }

    pub fn get(&self, instance_id: &str) -> Option<&str> {
        self.map.get(instance_id).map(|s| s.as_str())
    }

    /// Inject libfaketime env vars for a `dbt` invocation. Pure data — caller
    /// passes this map to `Command::envs()`. On non-Linux we return an empty
    /// map (libfaketime isn't available; bench summary records the loss).
    pub fn faketime_env(&self, instance_id: &str) -> HashMap<String, String> {
        let mut env = HashMap::new();
        if !cfg!(target_os = "linux") {
            return env;
        }
        if let Some(date) = self.get(instance_id) {
            env.insert("FAKETIME".into(), format!("@{date} 00:00:00"));
            env.insert("FAKETIME_DONT_FAKE_MONOTONIC".into(), "1".into());
            env.insert("DO_NOT_TRACK".into(), "1".into());
            env.insert("DBT_NO_VERSION_CHECK".into(), "1".into());
        }
        env
    }
}
