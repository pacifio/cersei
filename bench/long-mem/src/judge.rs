//! LongMemEval LLM-as-judge scorer.
//!
//! Ports the exact rubric prompts from
//! `_inspirations/mastra/explorations/longmemeval/src/evaluation/longmemeval-metric.ts`,
//! which in turn copies verbatim from the official benchmark:
//! <https://github.com/xiaowu0162/LongMemEval/blob/main/src/evaluation/evaluate_qa.py>.
//!
//! DO NOT modify the prompt strings — they must match the official benchmark
//! for comparable results against Mastra / Supermemory / Zep / EMem.

use crate::dataset::{Question, QuestionType};
use anyhow::{Context, Result};
use cersei_provider::{CompletionRequest, Provider};
use cersei_types::{Message, Role};

/// Build the judge prompt for a given question. Mirrors Mastra's
/// `getEvalPrompt(taskType, question, answer, response, isAbstention)`.
pub fn build_prompt(q: &Question, response: &str) -> String {
    if q.is_abstention() {
        return format!(
            "I will give you an unanswerable question, an explanation, and a response from a model. \
Please answer yes if the model correctly identifies the question as unanswerable. The model could say that \
the information is incomplete, or some other information is given but the asked information is not.

Question: {question}

Explanation: {answer}

Model Response: {response}

Does the model correctly identify the question as unanswerable? Answer yes or no only.",
            question = q.question,
            answer = q.answer,
            response = response
        );
    }

    match q.question_type {
        QuestionType::SingleSessionUser
        | QuestionType::SingleSessionAssistant
        | QuestionType::MultiSession => format!(
            "I will give you a question, a correct answer, and a response from a model. \
Please answer yes if the response contains the correct answer. Otherwise, answer no. \
If the response is equivalent to the correct answer or contains all the intermediate steps to get the correct answer, you should also answer yes. \
If the response only contains a subset of the information required by the answer, answer no.

Question: {question}

Correct Answer: {answer}

Model Response: {response}

Is the model response correct? Answer yes or no only.",
            question = q.question,
            answer = q.answer,
            response = response
        ),
        QuestionType::TemporalReasoning => format!(
            "I will give you a question, a correct answer, and a response from a model. \
Please answer yes if the response contains the correct answer. Otherwise, answer no. \
If the response is equivalent to the correct answer or contains all the intermediate steps to get the correct answer, you should also answer yes. \
If the response only contains a subset of the information required by the answer, answer no. \
In addition, do not penalize off-by-one errors for the number of days. \
If the question asks for the number of days/weeks/months, etc., and the model makes off-by-one errors (e.g., predicting 19 days when the answer is 18), the model's response is still correct.

Question: {question}

Correct Answer: {answer}

Model Response: {response}

Is the model response correct? Answer yes or no only.",
            question = q.question,
            answer = q.answer,
            response = response
        ),
        QuestionType::KnowledgeUpdate => format!(
            "I will give you a question, a correct answer, and a response from a model. \
Please answer yes if the response contains the correct answer. Otherwise, answer no. \
If the response contains some previous information along with an updated answer, the response should be considered as correct as long as the updated answer is the required answer.

Question: {question}

Correct Answer: {answer}

Model Response: {response}

Is the model response correct? Answer yes or no only.",
            question = q.question,
            answer = q.answer,
            response = response
        ),
        QuestionType::SingleSessionPreference => format!(
            "I will give you a question, a rubric for desired personalized response, and a response from a model. \
Please answer yes if the response satisfies the desired response. Otherwise, answer no. \
The model does not need to reflect all the points in the rubric. \
The response is correct as long as it recalls and utilizes the user's personal information correctly.

Question: {question}

Rubric: {answer}

Model Response: {response}

Is the model response correct? Answer yes or no only.",
            question = q.question,
            answer = q.answer,
            response = response
        ),
    }
}

/// Run the judge. Returns Ok(true) if the judge said "yes".
pub async fn score<P: Provider + ?Sized>(
    provider: &P,
    model: &str,
    q: &Question,
    response: &str,
) -> Result<bool> {
    let prompt = build_prompt(q, response);
    let mut req = CompletionRequest::new(model);
    req.messages.push(Message::user(prompt));
    req.temperature = Some(0.0);
    // Budget room for thinking tokens on Gemini 2.5 Flash. On OpenAI
    // `gpt-4o-mini` this is wildly over-provisioned but harmless — response
    // is "yes" / "no" either way.
    req.max_tokens = 512;

    let resp = provider
        .complete_blocking(req)
        .await
        .context("judge completion failed")?;
    let text = resp.message.get_all_text().to_ascii_lowercase();
    let trimmed = text.trim();
    // Match Mastra's parsing: exactly "yes" or starts with "yes."
    Ok(trimmed == "yes" || trimmed.starts_with("yes.") || trimmed.starts_with("yes "))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dataset::{QuestionType, Turn};

    fn q(id: &str, ty: QuestionType) -> Question {
        Question {
            question_id: id.into(),
            question_type: ty,
            question: "When did I buy the car?".into(),
            answer: "2023".into(),
            question_date: "2024/01/01".into(),
            haystack_session_ids: vec![],
            haystack_dates: vec![],
            haystack_sessions: vec![vec![Turn {
                role: "user".into(),
                content: "irrelevant".into(),
                has_answer: None,
            }]],
            answer_session_ids: vec![],
        }
    }

    #[test]
    fn abstention_prompt_uses_explanation_wording() {
        let p = build_prompt(&q("gpt4_123_abs", QuestionType::MultiSession), "no info");
        assert!(p.contains("unanswerable"));
        assert!(p.contains("Explanation:"));
    }

    #[test]
    fn temporal_prompt_has_off_by_one_leniency() {
        let p = build_prompt(&q("t1", QuestionType::TemporalReasoning), "19 days");
        assert!(p.contains("off-by-one"));
    }

    #[test]
    fn knowledge_update_prompt_accepts_previous_plus_new() {
        let p = build_prompt(&q("k1", QuestionType::KnowledgeUpdate), "old + new");
        assert!(p.contains("previous information along with an updated"));
    }

    #[test]
    fn preference_prompt_uses_rubric_label() {
        let p = build_prompt(&q("p1", QuestionType::SingleSessionPreference), "x");
        assert!(p.contains("Rubric:"));
        assert!(!p.contains("Correct Answer:"));
    }

    #[test]
    fn default_prompt_uses_correct_answer_label() {
        let p = build_prompt(&q("s1", QuestionType::SingleSessionUser), "x");
        assert!(p.contains("Correct Answer:"));
    }
}
