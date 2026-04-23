//! Aggregation + metrics matching Mastra's `BenchmarkMetrics` shape.
//!
//! Note: overall_accuracy is the MACRO average (mean of per-type accuracies,
//! excluding abstention), matching Mastra cli.ts line 99. Abstention is
//! reported as a separate `abstention_accuracy` field.

use crate::dataset::QuestionType;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PerQuestion {
    pub question_id: String,
    pub question_type: QuestionType,
    pub is_abstention: bool,
    pub question: String,
    pub expected_answer: String,
    pub hypothesis: String,
    pub is_correct: bool,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub judge_tokens: u64,
    pub elapsed_ms: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TypeBreakdown {
    pub correct: u32,
    pub total: u32,
    pub accuracy: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BenchmarkMetrics {
    pub config: String,
    pub dataset: String,
    pub judge_model: String,
    pub total_questions: u32,
    pub correct_answers: u32,
    /// Macro average across question types (excludes abstention).
    pub overall_accuracy: f32,
    pub accuracy_by_type: HashMap<String, TypeBreakdown>,
    pub abstention_total: u32,
    pub abstention_correct: u32,
    pub abstention_accuracy: f32,
    pub avg_elapsed_ms: u64,
    pub total_input_tokens: u64,
    pub total_output_tokens: u64,
    pub total_judge_tokens: u64,
}

pub fn summarize(
    config: &str,
    dataset: &str,
    judge_model: &str,
    rows: &[PerQuestion],
) -> BenchmarkMetrics {
    let mut by_type: HashMap<String, TypeBreakdown> = HashMap::new();
    let mut abs_total = 0u32;
    let mut abs_correct = 0u32;
    let mut total_elapsed = 0u64;
    let mut correct_total = 0u32;
    let (mut in_tok, mut out_tok, mut judge_tok) = (0u64, 0u64, 0u64);

    for r in rows {
        in_tok += r.input_tokens;
        out_tok += r.output_tokens;
        judge_tok += r.judge_tokens;
        total_elapsed += r.elapsed_ms;
        if r.is_correct {
            correct_total += 1;
        }

        if r.is_abstention {
            abs_total += 1;
            if r.is_correct {
                abs_correct += 1;
            }
            continue; // excluded from per-type breakdown
        }

        let bucket = by_type
            .entry(r.question_type.as_str().to_string())
            .or_default();
        bucket.total += 1;
        if r.is_correct {
            bucket.correct += 1;
        }
    }

    for breakdown in by_type.values_mut() {
        breakdown.accuracy = if breakdown.total > 0 {
            breakdown.correct as f32 / breakdown.total as f32
        } else {
            0.0
        };
    }

    // Macro average across types (matches Mastra cli.ts:99).
    let overall_accuracy = if by_type.is_empty() {
        0.0
    } else {
        let sum: f32 = by_type.values().map(|b| b.accuracy).sum();
        sum / by_type.len() as f32
    };
    let abstention_accuracy = if abs_total > 0 {
        abs_correct as f32 / abs_total as f32
    } else {
        0.0
    };
    let avg_elapsed_ms = if rows.is_empty() {
        0
    } else {
        total_elapsed / rows.len() as u64
    };

    BenchmarkMetrics {
        config: config.to_string(),
        dataset: dataset.to_string(),
        judge_model: judge_model.to_string(),
        total_questions: rows.len() as u32,
        correct_answers: correct_total,
        overall_accuracy,
        accuracy_by_type: by_type,
        abstention_total: abs_total,
        abstention_correct: abs_correct,
        abstention_accuracy,
        avg_elapsed_ms,
        total_input_tokens: in_tok,
        total_output_tokens: out_tok,
        total_judge_tokens: judge_tok,
    }
}
