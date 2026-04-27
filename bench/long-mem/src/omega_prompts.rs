//! Verbatim ports of [Omega-memory's](https://github.com/omega-memory)
//! per-category RAG prompts.
//!
//! Source: Omega-memory's `longmemeval_official.py` — the five RAG prompts
//! (`RAG_PROMPT_VANILLA`, `RAG_PROMPT_ENHANCED`, `RAG_PROMPT_MULTISESSION`,
//! `RAG_PROMPT_PREFERENCE`, `RAG_PROMPT_TEMPORAL`).
//!
//! Omega empirically determined these prompts by running each of them across
//! all six LongMemEval question types and keeping the best per-category
//! performer. Their published numbers:
//!
//! | Category                  | Prompt             | Score |
//! |---------------------------|--------------------|-------|
//! | single-session-assistant  | VANILLA            | 94.6% |
//! | single-session-user       | VANILLA            | 95.7% |
//! | knowledge-update          | ENHANCED           | 87.2% |
//! | multi-session             | MULTISESSION       | 69.9% |
//! | temporal-reasoning        | TEMPORAL           | 70.7% |
//! | single-session-preference | PREFERENCE         | 50.0% |
//!
//! DO NOT modify the prompt strings. The STEP 1/2/3 scaffolding, the
//! supersede-older-values rule, the deduplication-before-counting rule, and
//! the temporal-anchoring walkthrough are all empirically tuned — paraphrasing
//! kills the lift.

use crate::dataset::QuestionType;

/// Best for `single-session-assistant` (94.6 %) and `single-session-user` (95.7 %).
/// Keeps the prompt short so the model doesn't over-think clean factual lookups.
pub const RAG_PROMPT_VANILLA: &str = r#"I will give you several notes from past conversations between you and a user. Please answer the question based on the relevant notes. If the question cannot be answered based on the provided notes, say so.

Notes from past conversations:

{sessions}

Current Date: {question_date}
Question: {question}
Answer:"#;

/// Best for `knowledge-update` (87.2 %). Recency + aggregation + confidence push
/// via the STEP 1/2/3 walkthrough. "LATEST date is the ONLY correct one."
pub const RAG_PROMPT_ENHANCED: &str = r#"I will give you several notes from past conversations between you and a user, ordered from oldest to newest. Please answer the question based on the relevant notes. If the question cannot be answered based on the provided notes, say so.

You MUST follow this process for EVERY question:

STEP 1 — Scan ALL notes for mentions of the queried topic. List every note that discusses it, with its note number and date.

STEP 2 — If the topic appears in multiple notes, compare the values. The note with the LATEST date is the ONLY correct one. Earlier values are SUPERSEDED and WRONG.

STEP 3 — Answer using ONLY the value from the latest note.

CRITICAL rules:
- Notes are in chronological order (oldest first). Higher note numbers are more recent.
- For questions about current state (e.g., "what is my current X?", "how many times have I done Y?"), the answer ALWAYS comes from the LAST note mentioning that topic.
- If a quantity changes across notes (e.g., worn 4 times → worn 6 times), the LATEST number replaces all earlier ones. Do NOT add or average them.
- If the question references a role, title, or name that does NOT exactly match what appears in the notes, say the information is not enough to answer.
- If the question asks "how many" or for a count/total, enumerate all relevant items and then state the final number clearly.
- Give a direct, concise answer. Do not hedge if the evidence is clear.

Notes from past conversations:

{sessions}

Current Date: {question_date}
Question: {question}
Answer:"#;

/// Best for `multi-session` (69.9 %). Recency + aggregation with explicit
/// deduplication-before-counting rule and "start value AND end value" change
/// arithmetic — no confidence push because multi-session is where the noise
/// lives.
pub const RAG_PROMPT_MULTISESSION: &str = r#"I will give you several notes from past conversations between you and a user, ordered from oldest to newest. Please answer the question based on the relevant notes. If the question cannot be answered based on the provided notes, say so.

Important:
- Notes are in chronological order. When the same fact appears in multiple notes with different values, always use the value from the MOST RECENT note.
- If the question asks "how many", for a count, or for a total:
  1. You MUST list EVERY matching item individually, citing its source as [Note #].
  2. VERIFY each item: re-read the question and confirm each item EXACTLY matches what was asked. If the question asks about "types of citrus fruits", only count distinct fruit types the user actually used, not every mention of citrus. If it asks about "projects I led", only count projects where the user was the leader.
  3. REMOVE items that don't strictly match the question's criteria. But NEVER dismiss something the USER claims they did (bought, attended, downloaded, etc.) just because the assistant questioned whether it's real. The user's statement is ground truth.
  4. After filtering, count the remaining items and state the total clearly.
  5. For "how much total" questions: list each amount with its source [Note #], then sum them and state the total.
- When the same fact is UPDATED in a later note (e.g., a number changes from X to Y), use ONLY the latest value. The earlier value is superseded.
- DEDUPLICATION: When counting across notes, watch for the same event/item described differently (e.g., "cousin's wedding" and "Rachel's wedding at a vineyard" may be the same event). If two items could be the same, count them as ONE. Err on the side of merging duplicates rather than double-counting.
- For questions about an "increase", "decrease", or "change" in a quantity: you MUST find BOTH the starting value AND the ending value, then compute the DIFFERENCE. Do NOT report the final total as the increase. Example: if followers went from 250 to 350, the increase is 100.
- Do NOT skip notes. Scan every note for potential matches before answering.
- Give a direct, concise answer. Do not hedge if the evidence is clear.
- NEVER guess, estimate, or calculate values that are not explicitly stated in the notes. If the notes mention a taxi costs $X but never mention the bus/train price (or vice versa), say the information is not enough to answer — do NOT compute a savings amount from missing data.

Notes from past conversations:

{sessions}

Current Date: {question_date}
Question: {question}
Answer:"#;

/// Best for `single-session-preference` (50 %). Focuses on personal info recall,
/// forces the model to reference at least one specific detail, and explicitly
/// forbids generic advice.
pub const RAG_PROMPT_PREFERENCE: &str = r#"I will give you several notes from past conversations between you and a user. Please answer the question based on the user's stated preferences, habits, and personal information found in these notes. If the question cannot be answered based on the provided notes, say so.

Important:
- Focus on what the user explicitly said about their preferences, likes, dislikes, habits, routines, and personal details.
- When the same preference appears in multiple notes with different values, always use the value from the MOST RECENT note (higher note number = more recent).
- If the question asks for a recommendation or suggestion, USE the user's stated preferences to tailor your response. Do NOT say you lack information if the notes contain ANY relevant preferences, interests, or habits — apply them creatively.
- Even if the notes don't mention the exact topic, look for RELATED preferences (e.g., if asked about hotels, use stated preferences about views, amenities, luxury vs budget, or location preferences from ANY context).
- When the user mentions a place, activity, or event, ALWAYS check if the notes contain a SPECIFIC PAST EXPERIENCE with that place/activity. If so, reference it directly (e.g., "You mentioned enjoying X when you visited Denver before" or "Given your experience with Y in high school").
- Your answer MUST reference at least one specific detail from the notes. Generic advice that could apply to anyone is WRONG. The answer should be clearly personalized — someone reading it should be able to tell it was written for this specific user.
- Give a direct, specific answer. Quote the user's own words when possible.

Notes from past conversations:

{sessions}

Current Date: {question_date}
Question: {question}
Answer:"#;

/// Best for `temporal-reasoning` (70.7 %). STEP 1/2/3/4 walkthrough with
/// explicit relative-to-absolute date conversion, recollection-vs-action
/// distinction, and synonym-based fallback before abstention.
pub const RAG_PROMPT_TEMPORAL: &str = r#"I will give you several notes from past conversations between you and a user, ordered from oldest to newest. Each note has a date stamp. Please answer the question based on the relevant notes. If the question cannot be answered based on the provided notes, say so.

You MUST follow these steps for ALL time-based questions:

STEP 1 — Convert every relative date to an ABSOLUTE date:
  For each event mentioned in the notes, write its absolute date. Convert ALL relative references using the note's own date stamp:
  - "last Saturday" = the most recent Saturday BEFORE the note's date
  - "yesterday" = the day before the note's date
  - "two weeks ago" = 14 days before the note's date
  - "last month" = the calendar month before the note's date
  - "next Friday" = the first Friday AFTER the note's date

STEP 2 — Find ALL candidate events, not just the first match:
  When the question asks about something at a specific time (e.g., "two weeks ago", "last Saturday"), scan ALL notes and list every event that could match both the time reference AND the event description. Do NOT stop at the first event near the target date.

STEP 3 — Select the best match by verifying BOTH date AND description:
  - The event must match the question's description (e.g., "art event", "business milestone", "life event of a relative"). A nearby event of the wrong type is wrong.
  - Among events matching the description, pick the one closest to the exact target date. Prefer events within ±2 days; only consider ±3-7 days if no closer match exists.
  - If a note says "I went to X last week" and the note is dated near the target, resolve "last week" to find the EXACT event date, not the note date.

STEP 4 — Compute the answer using ONLY the absolute dates:
  - For "how many days/weeks/months between X and Y": subtract the two absolute dates and convert to the requested unit.
  - For ordering questions: list each event with its absolute date, then sort by date (earliest first).
  - For "how many times" or counting: enumerate each matching event with its absolute date, then state the total count.
  - For "when" questions: state the absolute date directly.

CRITICAL rules:
- RECOLLECTION ≠ ACTION: When a note says "I was thinking about X", "I remembered X", or "I was reminiscing about X", the event X did NOT happen on that note's date. The note's date is when the user RECALLED the event, not when it occurred. Only use notes where the user describes PERFORMING an action to date that action.
- Notes are in chronological order. When the same fact appears in multiple notes with different values, always use the value from the MOST RECENT note.
- Give a direct, concise answer. Do not hedge if the evidence is clear.
- Show your date arithmetic briefly before giving the final answer.
- If you can infer the answer by combining information across multiple notes, DO SO. Do not refuse to answer simply because no single note contains the complete answer.
- When a relative time reference (e.g., "last Saturday", "two weeks ago") appears in a note, ALWAYS resolve it to an absolute date using that note's date stamp before comparing to the question date.
- BEFORE saying "not enough information": re-read every note looking for SYNONYMS or INDIRECT references. "Investment for a competition" could be "bought tools for a contest." "Kitchen appliance" could be "smoker" or "grill." "Piece of jewelry" could be "ring" or "necklace." Try harder to match before abstaining.

Notes from past conversations:

{sessions}

Current Date: {question_date}
Question: {question}
Answer:"#;

/// Pick the Omega prompt Omega's ablation shows is best for this question type.
pub fn prompt_for(qt: QuestionType) -> &'static str {
    match qt {
        QuestionType::SingleSessionUser | QuestionType::SingleSessionAssistant => {
            RAG_PROMPT_VANILLA
        }
        QuestionType::KnowledgeUpdate => RAG_PROMPT_ENHANCED,
        QuestionType::MultiSession => RAG_PROMPT_MULTISESSION,
        QuestionType::TemporalReasoning => RAG_PROMPT_TEMPORAL,
        QuestionType::SingleSessionPreference => RAG_PROMPT_PREFERENCE,
    }
}

/// Fill a prompt template's `{sessions}`, `{question_date}`, `{question}`
/// placeholders. Unknown placeholders are left intact.
pub fn fill(template: &str, sessions: &str, question_date: &str, question: &str) -> String {
    template
        .replace("{sessions}", sessions)
        .replace("{question_date}", question_date)
        .replace("{question}", question)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enhanced_has_step_scaffolding() {
        assert!(RAG_PROMPT_ENHANCED.contains("STEP 1 — Scan ALL notes"));
        assert!(RAG_PROMPT_ENHANCED.contains("STEP 2 — If the topic appears in multiple notes"));
        assert!(RAG_PROMPT_ENHANCED.contains("STEP 3 — Answer using ONLY the value from the latest note"));
        assert!(RAG_PROMPT_ENHANCED.contains("SUPERSEDED and WRONG"));
    }

    #[test]
    fn multisession_forbids_double_counting() {
        assert!(RAG_PROMPT_MULTISESSION.contains("DEDUPLICATION"));
        assert!(RAG_PROMPT_MULTISESSION.contains("count them as ONE"));
        assert!(RAG_PROMPT_MULTISESSION.contains("compute the DIFFERENCE"));
    }

    #[test]
    fn preference_forbids_generic_advice() {
        assert!(RAG_PROMPT_PREFERENCE.contains("Generic advice that could apply to anyone is WRONG"));
        assert!(RAG_PROMPT_PREFERENCE.contains("reference at least one specific detail"));
    }

    #[test]
    fn temporal_has_4_steps_and_recollection_rule() {
        assert!(RAG_PROMPT_TEMPORAL.contains("STEP 1 — Convert every relative date"));
        assert!(RAG_PROMPT_TEMPORAL.contains("STEP 4 — Compute the answer"));
        assert!(RAG_PROMPT_TEMPORAL.contains("RECOLLECTION ≠ ACTION"));
    }

    #[test]
    fn prompt_selection_covers_all_types() {
        for qt in [
            QuestionType::SingleSessionUser,
            QuestionType::SingleSessionAssistant,
            QuestionType::SingleSessionPreference,
            QuestionType::MultiSession,
            QuestionType::TemporalReasoning,
            QuestionType::KnowledgeUpdate,
        ] {
            let p = prompt_for(qt);
            assert!(p.contains("{sessions}"));
            assert!(p.contains("{question}"));
            assert!(p.contains("{question_date}"));
        }
    }

    #[test]
    fn fill_substitutes_all_placeholders() {
        let filled = fill(RAG_PROMPT_VANILLA, "SESS", "2026-04-24", "Q?");
        assert!(filled.contains("SESS"));
        assert!(filled.contains("2026-04-24"));
        assert!(filled.contains("Q?"));
        assert!(!filled.contains("{sessions}"));
    }
}
