//! Orchestrate ingest → answer → judge per question.

use crate::configs::Config;
use crate::dataset::Question;
use crate::judge;
use crate::report::PerQuestion;
use anyhow::{Context, Result};
use cersei_provider::{CompletionRequest, Provider};
use cersei_types::{Message, Role};
use std::sync::Arc;
use std::time::Instant;

const ANSWERER_BASE_SYSTEM: &str = "You are a helpful assistant. Answer the user's question using ONLY the information provided. \
If the information does not contain the answer, say so plainly — do NOT guess. \
Be concise: one or two short sentences is ideal. Do NOT quote or restate the context.";

/// Answer one question given a retrieved-context string.
///
/// Prompt stack (from 0.1.8):
///   1. System: `ANSWERER_BASE_SYSTEM` + Mastra's `OBSERVATION_CONTEXT_INSTRUCTIONS`
///      (KNOWLEDGE UPDATES / PLANNED ACTIONS / MOST RECENT USER INPUT —
///      carries Mastra's LongMemEval-specific guidance).
///   2. User: Omega-memory's **per-category RAG prompt** (VANILLA / ENHANCED /
///      MULTISESSION / PREFERENCE / TEMPORAL) filled with the retrieved
///      context as `{sessions}`.
///
/// The Mastra system clauses reinforce Omega's STEP scaffolding (both push
/// "most recent wins"), so stacking them is additive.
pub async fn answer<P: Provider + ?Sized>(
    provider: &P,
    model: &str,
    q: &Question,
    context: &str,
) -> Result<(String, u64, u64)> {
    let system = format!(
        "{}\n\n{}",
        ANSWERER_BASE_SYSTEM,
        crate::mastra_prompts::OBSERVATION_CONTEXT_INSTRUCTIONS
    );

    let template = crate::omega_prompts::prompt_for(q.question_type);
    let user_prompt =
        crate::omega_prompts::fill(template, context, &q.question_date, &q.question);

    let mut req = CompletionRequest::new(model);
    req.system = Some(system);
    req.messages.push(Message::user(user_prompt));
    req.temperature = Some(0.0);
    // Leave headroom for Gemini 2.5 Flash's thinking tokens. On OpenAI the
    // extra budget is ignored once the model stops. Omega's per-category
    // configs budget 512 (vanilla) or 2048 (enhanced/multisession/temporal) —
    // we pick the generous setting so thinking tokens have room.
    req.max_tokens = 2048;

    let resp = provider
        .complete_blocking(req)
        .await
        .context("answerer completion failed")?;
    let text = resp.message.get_all_text().trim().to_string();
    Ok((text, resp.usage.input_tokens, resp.usage.output_tokens))
}

/// Run one question through one config end-to-end.
pub async fn run_question<C, P>(
    cfg: &mut C,
    provider: Arc<P>,
    answerer_model: &str,
    judge_model: &str,
    q: &Question,
) -> Result<PerQuestion>
where
    C: Config + ?Sized,
    P: Provider + Send + Sync + ?Sized + 'static,
{
    let t0 = Instant::now();

    cfg.ingest(q).await.context("ingest failed")?;
    let context = cfg.retrieve(q).await.context("retrieve failed")?;

    let (hypothesis, in_tok, out_tok) = answer(&*provider, answerer_model, q, &context).await?;

    let is_correct = judge::score(&*provider, judge_model, q, &hypothesis)
        .await
        .unwrap_or(false);

    // Rough judge token cost — the judge request is <200 tokens in, <4 out.
    let judge_tokens = 200 + 4;

    Ok(PerQuestion {
        question_id: q.question_id.clone(),
        question_type: q.question_type,
        is_abstention: q.is_abstention(),
        question: q.question.clone(),
        expected_answer: q.answer.clone(),
        hypothesis,
        is_correct,
        input_tokens: in_tok,
        output_tokens: out_tok,
        judge_tokens: judge_tokens as u64,
        elapsed_ms: t0.elapsed().as_millis() as u64,
    })
}
