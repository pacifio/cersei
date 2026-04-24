//! LLM-driven query expansion (lex / vec / HyDE) — verbatim port of
//! [Omega-memory's](https://github.com/omega-memory) `query_expansion.py`
//! prompt and contract.
//!
//! Source: `_inspirations/omega-memory/src/omega/query_expansion.py:30-41`.
//!
//! The system prompt is copied byte-for-byte. Paraphrasing kills the lift —
//! Omega's +1-2 pp comes from the exact variant counts and the "no verbatim
//! repeats / different vocabulary" rule.

use anyhow::{Context, Result};
use cersei_provider::{CompletionRequest, Provider};
use cersei_types::Message;
use serde::Deserialize;

// VERBATIM from `omega/query_expansion.py:30-41`.
const OMEGA_EXPANSION_SYSTEM: &str = r#"You generate search query variants to improve memory retrieval.
Output JSON only, no explanation. Schema:
{"lex": ["keyword variant 1", ...], "vec": ["natural language rephrase 1", ...], "hyde": "hypothetical memory passage or empty string"}

Rules:
- lex: 2-3 variants, each 2-5 words, keyword-focused (for full-text search)
- vec: 2-3 variants, natural language rephrasings (for embedding similarity)
- hyde: A 1-2 sentence passage that a stored memory answering the query might contain. Only generate if the query is conceptual/vague. Empty string otherwise.
- Do NOT repeat the original query verbatim in any variant.
- Keep variants diverse — different vocabulary, not just rewordings."#;

/// Parsed LLM response.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct QueryVariants {
    #[serde(default)]
    pub lex: Vec<String>,
    #[serde(default)]
    pub vec: Vec<String>,
    #[serde(default)]
    pub hyde: String,
}

impl QueryVariants {
    pub fn is_empty(&self) -> bool {
        self.lex.is_empty() && self.vec.is_empty() && self.hyde.trim().is_empty()
    }

    /// All free-text semantic rephrasings — union of `vec` variants and the
    /// HyDE passage. Useful for embedding-based multi-query retrieval.
    pub fn semantic_variants(&self) -> Vec<String> {
        let mut out = self.vec.clone();
        if !self.hyde.trim().is_empty() {
            out.push(self.hyde.clone());
        }
        out
    }
}

/// Heuristic used by Omega to decide whether HyDE is worth generating. HyDE
/// is only useful when the query is vague or conceptual — on short, specific
/// lookups it burns tokens for no recall gain.
pub fn looks_vague(query: &str) -> bool {
    let q = query.trim();
    // ≥ 80 chars OR a question word that implies synthesis (Omega's rule).
    q.len() >= 80
        || ["which", "what kinds", "how many", "how did", "recommend", "suggest"]
            .iter()
            .any(|w| q.to_ascii_lowercase().contains(w))
}

/// Run a single LLM call to produce `QueryVariants`. On parse / transport
/// failure returns empty variants so the caller can fall back to the original
/// query unmodified — never breaks the answerer loop.
pub async fn expand_query<P: Provider + ?Sized>(
    provider: &P,
    model: &str,
    query: &str,
    include_hyde: bool,
) -> Result<QueryVariants> {
    if query.trim().len() < 3 {
        return Ok(QueryVariants::default());
    }

    let hyde_instruction = if include_hyde {
        "Generate a hyde passage."
    } else {
        "Set hyde to empty string."
    };
    let user = format!("Query: {query}\nMax variants per type: 3\n{hyde_instruction}");

    let mut req = CompletionRequest::new(model);
    req.system = Some(OMEGA_EXPANSION_SYSTEM.to_string());
    req.messages.push(Message::user(user));
    req.temperature = Some(0.3);
    // Generous headroom for Gemini 2.5 Flash thinking tokens. Omega's
    // production budget is 300, but they're on non-thinking GPT.
    req.max_tokens = 1024;

    let resp = provider
        .complete_blocking(req)
        .await
        .context("query expansion completion failed")?;
    let text = resp.message.get_all_text().to_string();
    let variants = parse_variants_json(&text).unwrap_or_default();
    Ok(variants)
}

/// Tolerant JSON extractor — accepts raw JSON, ```json fenced blocks, and
/// JSON-with-explanation-before-or-after. Returns `None` on total failure.
pub fn parse_variants_json(raw: &str) -> Option<QueryVariants> {
    // Fast path: pure JSON.
    if let Ok(v) = serde_json::from_str::<QueryVariants>(raw.trim()) {
        return Some(v);
    }

    // Strip ```json fences if present.
    let stripped = raw
        .trim()
        .trim_start_matches("```json")
        .trim_start_matches("```JSON")
        .trim_start_matches("```")
        .trim_end_matches("```")
        .trim();
    if let Ok(v) = serde_json::from_str::<QueryVariants>(stripped) {
        return Some(v);
    }

    // Last resort: find the first balanced `{ ... }` substring and try again.
    let open = raw.find('{')?;
    let mut depth = 0i32;
    let mut end = None;
    for (i, b) in raw[open..].bytes().enumerate() {
        match b {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    end = Some(open + i + 1);
                    break;
                }
            }
            _ => {}
        }
    }
    let end = end?;
    serde_json::from_str::<QueryVariants>(&raw[open..end]).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_raw_json() {
        let raw = r#"{"lex":["a b","c d"],"vec":["rephrased 1"],"hyde":"passage"}"#;
        let v = parse_variants_json(raw).unwrap();
        assert_eq!(v.lex, vec!["a b", "c d"]);
        assert_eq!(v.vec, vec!["rephrased 1"]);
        assert_eq!(v.hyde, "passage");
    }

    #[test]
    fn strips_fences() {
        let raw = "```json\n{\"lex\":[],\"vec\":[\"x\"],\"hyde\":\"\"}\n```";
        let v = parse_variants_json(raw).unwrap();
        assert_eq!(v.vec, vec!["x"]);
    }

    #[test]
    fn extracts_from_chatty_model() {
        let raw = "Sure! Here are the variants:\n{\"lex\":[\"x y\"],\"vec\":[],\"hyde\":\"\"}\nDone.";
        let v = parse_variants_json(raw).unwrap();
        assert_eq!(v.lex, vec!["x y"]);
    }

    #[test]
    fn returns_none_on_total_garbage() {
        assert!(parse_variants_json("no json anywhere").is_none());
    }

    #[test]
    fn looks_vague_triggers_on_long_queries() {
        assert!(looks_vague(&"word ".repeat(20)));
    }

    #[test]
    fn looks_vague_triggers_on_recommend_keyword() {
        assert!(looks_vague("Can you recommend a hotel?"));
    }

    #[test]
    fn looks_vague_false_on_short_specific() {
        assert!(!looks_vague("What color was my car?"));
    }

    #[test]
    fn semantic_variants_merges_vec_and_hyde() {
        let v = QueryVariants {
            lex: vec!["a".into()],
            vec: vec!["rephrased".into()],
            hyde: "passage".into(),
        };
        assert_eq!(v.semantic_variants(), vec!["rephrased", "passage"]);
    }
}
