//! LongMemEval dataset schema + loader.
//!
//! Ported from `_inspirations/mastra/explorations/longmemeval/src/data/types.ts`.
//! Abstention detection matches Mastra: a question is an abstention case iff
//! `question_id.ends_with("_abs")` (loader.ts:83 and commands/run.ts:1119).

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum QuestionType {
    SingleSessionUser,
    SingleSessionAssistant,
    SingleSessionPreference,
    TemporalReasoning,
    KnowledgeUpdate,
    MultiSession,
}

impl QuestionType {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::SingleSessionUser => "single-session-user",
            Self::SingleSessionAssistant => "single-session-assistant",
            Self::SingleSessionPreference => "single-session-preference",
            Self::TemporalReasoning => "temporal-reasoning",
            Self::KnowledgeUpdate => "knowledge-update",
            Self::MultiSession => "multi-session",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Turn {
    pub role: String, // "user" | "assistant"
    pub content: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub has_answer: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Question {
    pub question_id: String,
    pub question_type: QuestionType,
    pub question: String,
    /// The ground-truth answer. Some rows in `longmemeval_s` / `_oracle`
    /// have this as a bare integer (e.g. `"answer": 3`) — we stringify
    /// on load so the downstream code can treat it uniformly.
    #[serde(deserialize_with = "deserialize_scalar_as_string")]
    pub answer: String,
    pub question_date: String,
    pub haystack_session_ids: Vec<String>,
    pub haystack_dates: Vec<String>,
    pub haystack_sessions: Vec<Vec<Turn>>,
    pub answer_session_ids: Vec<String>,
}

fn deserialize_scalar_as_string<'de, D>(de: D) -> std::result::Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de::Error;
    let v = serde_json::Value::deserialize(de)?;
    match v {
        serde_json::Value::String(s) => Ok(s),
        serde_json::Value::Number(n) => Ok(n.to_string()),
        serde_json::Value::Bool(b) => Ok(b.to_string()),
        serde_json::Value::Null => Ok(String::new()),
        other => Err(D::Error::custom(format!(
            "expected string/number/bool for answer, got {other}"
        ))),
    }
}

impl Question {
    /// Matches Mastra's abstention detection (`_abs` suffix on question_id).
    pub fn is_abstention(&self) -> bool {
        self.question_id.ends_with("_abs")
    }

    /// Total turns across all haystack sessions.
    pub fn total_turns(&self) -> usize {
        self.haystack_sessions.iter().map(|s| s.len()).sum()
    }

    pub fn session_count(&self) -> usize {
        self.haystack_sessions.len()
    }
}

pub fn load_dataset(path: &Path) -> Result<Vec<Question>> {
    let bytes =
        std::fs::read(path).with_context(|| format!("reading dataset from {}", path.display()))?;
    let questions: Vec<Question> = serde_json::from_slice(&bytes)
        .with_context(|| format!("parsing {} as LongMemEval JSON", path.display()))?;
    Ok(questions)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_oracle() {
        let p = std::path::PathBuf::from("data/longmemeval_oracle.json");
        if !p.exists() {
            eprintln!("skipping: {} not downloaded (run ./setup.sh)", p.display());
            return;
        }
        let qs = load_dataset(&p).expect("oracle parses");
        assert!(
            qs.len() >= 300,
            "expected >=300 oracle questions, got {}",
            qs.len()
        );

        let any_abs = qs.iter().any(|q| q.is_abstention());
        assert!(any_abs, "oracle should contain _abs questions");

        let total_types: std::collections::HashSet<QuestionType> =
            qs.iter().map(|q| q.question_type).collect();
        assert!(total_types.len() >= 5, "expected multiple question types");
    }
}
