//! Config A — JsonlMemory full-context baseline.
//!
//! Dumps every turn of every session into a single context string. No
//! retrieval, no filtering. This is the control: the answerer has "perfect"
//! access to the entire haystack, bounded only by the LLM context window.

use crate::configs::Config;
use crate::dataset::Question;
use anyhow::Result;
use async_trait::async_trait;

pub struct BaselineConfig {
    context: String,
}

impl Default for BaselineConfig {
    fn default() -> Self {
        Self::new()
    }
}

impl BaselineConfig {
    pub fn new() -> Self {
        Self {
            context: String::new(),
        }
    }
}

#[async_trait]
impl Config for BaselineConfig {
    fn name(&self) -> &'static str {
        "baseline-jsonl"
    }

    async fn ingest(&mut self, q: &Question) -> Result<()> {
        let mut out = String::new();
        for (i, session) in q.haystack_sessions.iter().enumerate() {
            let date = q
                .haystack_dates
                .get(i)
                .map(String::as_str)
                .unwrap_or("unknown-date");
            let id = q
                .haystack_session_ids
                .get(i)
                .map(String::as_str)
                .unwrap_or("");
            out.push_str(&format!("=== Session {i} ({date}) [{id}] ===\n"));
            for turn in session {
                out.push_str(&format!("{}: {}\n", turn.role, turn.content));
            }
            out.push('\n');
        }
        self.context = out;
        Ok(())
    }

    async fn retrieve(&self, _q: &Question) -> Result<String> {
        Ok(self.context.clone())
    }
}
