//! Auto-compact: context window management for long conversations.
//!
//! When the conversation approaches the context window limit, older messages
//! are summarized to free space while preserving essential context.

use cersei_provider::Provider;
use cersei_types::*;

// ─── Constants ───────────────────────────────────────────────────────────────

/// Fraction of context window that triggers auto-compact.
pub const AUTOCOMPACT_TRIGGER_FRACTION: f64 = 0.90;
/// Number of recent messages to always preserve (never compacted).
pub const KEEP_RECENT_MESSAGES: usize = 10;
/// Max consecutive failures before disabling auto-compact.
pub const MAX_CONSECUTIVE_FAILURES: u32 = 3;
/// Warning threshold (80% of context window).
pub const WARNING_PCT: f64 = 0.80;
/// Critical threshold (95% of context window).
pub const CRITICAL_PCT: f64 = 0.95;

// ─── Types ───────────────────────────────────────────────────────────────────

/// Session-level compaction tracking.
#[derive(Debug, Clone, Default)]
pub struct AutoCompactState {
    pub compaction_count: u32,
    pub consecutive_failures: u32,
    pub disabled: bool,
}

impl AutoCompactState {
    pub fn on_success(&mut self) {
        self.compaction_count += 1;
        self.consecutive_failures = 0;
    }

    pub fn on_failure(&mut self) {
        self.consecutive_failures += 1;
        if self.consecutive_failures >= MAX_CONSECUTIVE_FAILURES {
            self.disabled = true;
        }
    }
}

/// Context window fullness level.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenWarningState {
    /// Below 80% — no action needed.
    Ok,
    /// 80-95% — warn user, consider compacting.
    Warning,
    /// Above 95% — critical, must compact or will fail.
    Critical,
}

/// A semantically coherent group of messages for summarization.
#[derive(Debug, Clone)]
pub struct MessageGroup {
    pub messages: Vec<Message>,
    pub topic_hint: Option<String>,
    pub token_estimate: usize,
}

/// Result of a compaction operation.
#[derive(Debug, Clone)]
pub struct CompactResult {
    pub messages_before: usize,
    pub messages_after: usize,
    pub tokens_freed_estimate: u64,
    pub summary: String,
}

/// What triggered the compaction.
#[derive(Debug, Clone, Copy)]
pub enum CompactTrigger {
    AutoThreshold,
    Manual,
    ContextOverflow,
}

// ─── Token estimation ────────────────────────────────────────────────────────

/// Rough token estimate for a message (~4 chars per token).
pub fn estimate_tokens(text: &str) -> u64 {
    (text.len() as u64) / 4
}

/// Estimate tokens for a list of messages.
pub fn estimate_messages_tokens(messages: &[Message]) -> u64 {
    messages
        .iter()
        .map(|m| estimate_tokens(&m.get_all_text()))
        .sum()
}

/// Get context window size for a model.
///
/// B2 moved the table into `cersei_provider::quirks` so the runner, the
/// F-09 `num_ctx` path, and router-time quirk resolution budget against one
/// truth. This wrapper keeps the agent-side call sites and tests stable.
pub fn context_window_for_model(model: &str) -> u64 {
    cersei_provider::quirks::context_window_for_model(model)
}

// ─── Warning state ───────────────────────────────────────────────────────────

/// Calculate the token warning state given current usage.
pub fn calculate_token_warning_state(tokens_used: u64, context_limit: u64) -> TokenWarningState {
    if context_limit == 0 {
        return TokenWarningState::Ok;
    }
    let pct = tokens_used as f64 / context_limit as f64;
    if pct >= CRITICAL_PCT {
        TokenWarningState::Critical
    } else if pct >= WARNING_PCT {
        TokenWarningState::Warning
    } else {
        TokenWarningState::Ok
    }
}

// ─── Should compact ──────────────────────────────────────────────────────────

/// Check if compaction should trigger.
pub fn should_compact(tokens_used: u64, context_limit: u64) -> bool {
    if context_limit == 0 {
        return false;
    }
    (tokens_used as f64 / context_limit as f64) >= AUTOCOMPACT_TRIGGER_FRACTION
}

/// Check if auto-compact should run (considering state/circuit breaker).
pub fn should_auto_compact(tokens_used: u64, context_limit: u64, state: &AutoCompactState) -> bool {
    if state.disabled {
        return false;
    }
    should_compact(tokens_used, context_limit)
}

/// Check if context collapse is needed (emergency, >98%).
pub fn should_context_collapse(tokens_used: u64, context_limit: u64) -> bool {
    if context_limit == 0 {
        return false;
    }
    (tokens_used as f64 / context_limit as f64) >= 0.98
}

// ─── Message grouping ────────────────────────────────────────────────────────

/// Extract a topic hint from messages (first file path or tool name).
fn extract_topic_hint(messages: &[Message]) -> Option<String> {
    for msg in messages {
        for block in msg.content_blocks() {
            match &block {
                ContentBlock::ToolUse { name, input, .. } => {
                    if let Some(path) = input.get("file_path").and_then(|v| v.as_str()) {
                        return Some(path.to_string());
                    }
                    return Some(name.clone());
                }
                _ => {}
            }
        }
    }
    None
}

/// Group messages into semantically coherent chunks at API-round boundaries.
/// Each group = one assistant response + its tool results.
pub fn group_messages_for_compact(messages: &[Message]) -> Vec<MessageGroup> {
    let mut groups: Vec<MessageGroup> = Vec::new();
    let mut current: Vec<Message> = Vec::new();

    for msg in messages {
        current.push(msg.clone());
        // End group at assistant messages that don't have tool use (end of a "round")
        if msg.role == Role::Assistant && !msg.has_tool_use() {
            let token_est = current.iter().map(|m| m.get_all_text().len() / 4).sum();
            let hint = extract_topic_hint(&current);
            groups.push(MessageGroup {
                messages: std::mem::take(&mut current),
                topic_hint: hint,
                token_estimate: token_est,
            });
        }
    }
    // Leftover messages
    if !current.is_empty() {
        let token_est = current.iter().map(|m| m.get_all_text().len() / 4).sum();
        let hint = extract_topic_hint(&current);
        groups.push(MessageGroup {
            messages: current,
            topic_hint: hint,
            token_estimate: token_est,
        });
    }
    groups
}

// ─── Tool-pair-aware splitting (F-04) ────────────────────────────────────────

/// `tool_result` ids in `msg` that no earlier `tool_use` in `msgs` answers.
///
/// This is the same check every provider runs server-side. A `tool_result`
/// without its `tool_use` in the *same request* is a 400, and the runner maps a
/// 400 to `CerseiError::Provider`, which `is_retryable()` does not match — so
/// the conversation is wedged for good rather than retried.
pub fn find_orphaned_tool_results(msgs: &[Message]) -> Vec<String> {
    let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
    let mut orphaned = Vec::new();
    for m in msgs {
        let MessageContent::Blocks(blocks) = &m.content else {
            continue;
        };
        // Same message first: a tool_use and its result never share a message
        // in practice, but scanning uses before results keeps that from
        // mattering.
        for b in blocks {
            if let ContentBlock::ToolUse { id, .. } = b {
                seen.insert(id);
            }
        }
        for b in blocks {
            if let ContentBlock::ToolResult { tool_use_id, .. } = b {
                if !seen.contains(tool_use_id.as_str()) {
                    orphaned.push(tool_use_id.clone());
                }
            }
        }
    }
    orphaned
}

/// True if `msg` carries any `tool_result` block.
fn carries_tool_result(msg: &Message) -> bool {
    match &msg.content {
        MessageContent::Blocks(b) => b
            .iter()
            .any(|x| matches!(x, ContentBlock::ToolResult { .. })),
        _ => false,
    }
}

/// Where to cut a history so that `msgs[split..]` is a valid request.
///
/// The naive `len - keep_n` is wrong half the time. The runner builds a
/// strictly alternating history (idx 0 `user`, odd `assistant[tool_use]`, even
/// `user[tool_result]`), and `KEEP_RECENT_MESSAGES` is even, so
/// `parity(split) == parity(len)`: for every even-length conversation the cut
/// lands on a `user[tool_result]` and discards the `tool_use` that answers it.
///
/// Backing off one message reaches the `assistant[tool_use]`, which repairs the
/// pair and, for the summary path, also removes the `user(summary)` /
/// `user(tool_result)` adjacency that Gemini and most local chat templates
/// reject. It only ever keeps *more* context than asked, never less.
pub fn pair_aware_split(msgs: &[Message], keep_n: usize) -> usize {
    let mut split = msgs.len().saturating_sub(keep_n);
    while split > 0 && carries_tool_result(&msgs[split]) {
        split -= 1;
    }
    split
}

// ─── Snip compact (simple truncation) ────────────────────────────────────────

/// Stands in for the turns the snip fallback dropped, so the history still
/// opens with a `user` message. Kept short: it is pure overhead on a path taken
/// precisely when context is scarce.
pub const SNIP_TRUNCATION_NOTICE: &str =
    "[earlier turns were dropped to free context; continue from the messages below]";

/// Remove oldest messages, keeping the newest `keep_n` — plus one more when
/// that boundary would sever a `tool_use`/`tool_result` pair (see
/// [`pair_aware_split`]). `keep_n` is a floor, not an exact count.
///
/// Returns (remaining messages, estimated tokens freed).
pub fn snip_compact(messages: Vec<Message>, keep_n: usize) -> (Vec<Message>, u64) {
    if messages.len() <= keep_n {
        return (messages, 0);
    }
    let split = pair_aware_split(&messages, keep_n);
    let freed = estimate_messages_tokens(&messages[..split]);
    let mut kept = messages[split..].to_vec();

    // Backing off to keep a tool pair intact lands on the `assistant[tool_use]`,
    // so the surviving history opens with an assistant turn. The summary path
    // gets away with that because it prepends `user(summary)`; this path
    // prepends nothing, and Anthropic rejects any request whose first message is
    // not `user`. Fixing only the orphan would have swapped one guaranteed 400
    // for another — so the marker goes in here, where it also tells the model
    // why its history starts mid-task.
    if !matches!(kept.first().map(|m| m.role), Some(Role::User) | None) {
        kept.insert(0, Message::user(SNIP_TRUNCATION_NOTICE));
    }
    (kept, freed)
}

/// Calculate how many messages to keep given a token budget.
pub fn calculate_messages_to_keep_index(messages: &[Message], token_budget: u64) -> usize {
    let mut total: u64 = 0;
    for (i, msg) in messages.iter().rev().enumerate() {
        total += estimate_tokens(&msg.get_all_text());
        if total > token_budget {
            return messages.len() - i;
        }
    }
    0 // keep all
}

// ─── Collapse strategies ─────────────────────────────────────────────────────

/// Collapse repeated file read results: if the same file is read multiple
/// times, only keep the latest result.
pub fn collapse_read_tool_results(messages: Vec<Message>) -> Vec<Message> {
    let mut seen_files: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut result: Vec<Message> = Vec::new();

    // Process in reverse to keep latest reads
    for msg in messages.into_iter().rev() {
        let dominated = match &msg.content {
            MessageContent::Blocks(blocks) => {
                blocks.iter().all(|b| {
                    if let ContentBlock::ToolResult {
                        tool_use_id,
                        content,
                        ..
                    } = b
                    {
                        // Check if this is a file read result we've already seen
                        if let ToolResultContent::Text(text) = content {
                            if text.contains('\t') {
                                // Line-numbered output = file read
                                let key = tool_use_id.clone();
                                if seen_files.contains(&key) {
                                    return true; // dominated, skip
                                }
                                seen_files.insert(key);
                            }
                        }
                        false
                    } else {
                        false
                    }
                })
            }
            _ => false,
        };

        if !dominated {
            result.push(msg);
        }
    }

    result.reverse();
    result
}

// ─── Compact prompt ──────────────────────────────────────────────────────────

/// Build the compaction prompt for the LLM.
pub fn get_compact_prompt(custom_instructions: Option<&str>) -> String {
    let mut prompt = String::from(
        "Summarize the conversation so far. Focus on:\n\
        1. Key decisions made and their rationale\n\
        2. Files that were read, created, or modified (with paths)\n\
        3. Tool results that are still relevant\n\
        4. Outstanding tasks or next steps\n\
        5. Any errors encountered and how they were resolved\n\n\
        Be concise but preserve all actionable information. \
        Use bullet points. Include file paths verbatim.",
    );
    if let Some(instructions) = custom_instructions {
        prompt.push_str("\n\nAdditional context: ");
        prompt.push_str(instructions);
    }
    prompt
}

/// Format raw compact output into a summary message.
pub fn format_compact_summary(raw: &str) -> String {
    format!(
        "<context_summary>\n\
        The following is a summary of the conversation so far:\n\n\
        {}\n\
        </context_summary>",
        raw.trim()
    )
}

// ─── Full compaction (requires provider call) ────────────────────────────────

/// Compact the conversation by summarizing older messages.
///
/// 1. Split messages into "old" (to compact) and "recent" (to keep)
/// 2. Group old messages by topic
/// 3. Send to provider for summarization
/// 4. Replace old messages with summary
pub async fn compact_conversation(
    provider: &dyn Provider,
    messages: &[Message],
    model: &str,
    keep_recent: usize,
    custom_instructions: Option<&str>,
) -> Result<CompactResult> {
    let messages_before = messages.len();

    if messages.len() <= keep_recent {
        return Ok(CompactResult {
            messages_before,
            messages_after: messages_before,
            tokens_freed_estimate: 0,
            summary: String::new(),
        });
    }

    // The same split the caller will actually apply (F-04). Using the naive
    // `len - keep_recent` here instead would summarise one message that the
    // runner then also keeps verbatim, and would make the `messages_after` and
    // `tokens_freed_estimate` reported below describe a history nobody builds.
    let split_idx = pair_aware_split(messages, keep_recent);
    let old_messages = &messages[..split_idx];
    let recent_messages = &messages[split_idx..];

    // Build compaction request
    let old_text: String = old_messages
        .iter()
        .map(|m| {
            let role = match m.role {
                Role::User => "User",
                Role::Assistant => "Assistant",
                Role::System => "System",
            };
            format!("{}: {}", role, m.get_all_text())
        })
        .collect::<Vec<_>>()
        .join("\n\n");

    let compact_prompt = get_compact_prompt(custom_instructions);
    let request = cersei_provider::CompletionRequest {
        model: model.to_string(),
        messages: vec![
            Message::user(format!(
                "Here is the conversation history to summarize:\n\n{}\n\n{}",
                old_text, compact_prompt
            )),
        ],
        system: Some("You are a conversation summarizer. Be concise and preserve all actionable information.".into()),
        tools: Vec::new(),
        max_tokens: 4096,
        temperature: Some(0.0),
        stop_sequences: Vec::new(),
        options: cersei_provider::ProviderOptions::default(),
    };

    // Collect streaming response into a complete message
    let stream = provider.complete(request).await?;
    let mut rx = stream.into_receiver();
    let mut accumulator = cersei_provider::StreamAccumulator::new();
    while let Some(event) = rx.recv().await {
        accumulator.process_event(event);
    }
    let response = accumulator.into_response()?;
    let summary_text = response.message.get_all_text();
    let formatted_summary = format_compact_summary(&summary_text);

    let tokens_freed = estimate_messages_tokens(old_messages);

    // Build compacted messages: summary + recent
    let messages_after = 1 + recent_messages.len(); // summary message + recent

    Ok(CompactResult {
        messages_before,
        messages_after,
        tokens_freed_estimate: tokens_freed,
        summary: formatted_summary,
    })
}

/// Check and run auto-compact if needed. Returns None if no compaction needed.
pub async fn auto_compact_if_needed(
    provider: &dyn Provider,
    messages: &[Message],
    model: &str,
    tokens_used: u64,
    state: &mut AutoCompactState,
) -> Option<CompactResult> {
    let context_limit = context_window_for_model(model);
    if !should_auto_compact(tokens_used, context_limit, state) {
        return None;
    }

    match compact_conversation(provider, messages, model, KEEP_RECENT_MESSAGES, None).await {
        Ok(result) => {
            state.on_success();
            Some(result)
        }
        Err(_) => {
            state.on_failure();
            None
        }
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_messages(n: usize) -> Vec<Message> {
        (0..n)
            .map(|i| {
                if i % 2 == 0 {
                    Message::user(format!("User message {}", i))
                } else {
                    Message::assistant(format!("Assistant response {} with some longer text to simulate real content that takes up tokens in the context window.", i))
                }
            })
            .collect()
    }

    #[test]
    fn test_token_warning_ok() {
        assert_eq!(
            calculate_token_warning_state(50_000, 200_000),
            TokenWarningState::Ok
        );
    }

    #[test]
    fn test_token_warning_warning() {
        assert_eq!(
            calculate_token_warning_state(170_000, 200_000),
            TokenWarningState::Warning
        );
    }

    #[test]
    fn test_token_warning_critical() {
        assert_eq!(
            calculate_token_warning_state(196_000, 200_000),
            TokenWarningState::Critical
        );
    }

    #[test]
    fn test_should_compact() {
        assert!(!should_compact(100_000, 200_000)); // 50%
        assert!(!should_compact(170_000, 200_000)); // 85%
        assert!(should_compact(185_000, 200_000)); // 92.5%
        assert!(should_compact(195_000, 200_000)); // 97.5%
    }

    #[test]
    fn test_should_auto_compact_disabled() {
        let state = AutoCompactState {
            disabled: true,
            ..Default::default()
        };
        assert!(!should_auto_compact(195_000, 200_000, &state));
    }

    #[test]
    fn test_circuit_breaker() {
        let mut state = AutoCompactState::default();
        state.on_failure();
        state.on_failure();
        assert!(!state.disabled);
        state.on_failure(); // 3rd failure
        assert!(state.disabled);
    }

    #[test]
    fn test_snip_compact() {
        let messages = make_messages(20);
        let (kept, freed) = snip_compact(messages, 10);
        assert_eq!(kept.len(), 10);
        assert!(freed > 0);
    }

    #[test]
    fn test_snip_compact_already_small() {
        let messages = make_messages(5);
        let (kept, freed) = snip_compact(messages, 10);
        assert_eq!(kept.len(), 5);
        assert_eq!(freed, 0);
    }

    #[test]
    fn test_group_messages() {
        let mut messages = Vec::new();
        messages.push(Message::user("Read file A"));
        messages.push(Message::assistant("Contents of A"));
        messages.push(Message::user("Now edit B"));
        messages.push(Message::assistant("Edited B"));

        let groups = group_messages_for_compact(&messages);
        assert_eq!(groups.len(), 2);
    }

    #[test]
    fn test_estimate_tokens() {
        assert_eq!(estimate_tokens("hello world"), 2); // 11 chars / 4
        assert_eq!(estimate_tokens(""), 0);
        assert!(estimate_tokens(&"x".repeat(1000)) > 200);
    }

    #[test]
    fn test_context_window_for_model() {
        assert_eq!(context_window_for_model("claude-sonnet-4-6"), 200_000);
        assert_eq!(context_window_for_model("gpt-4o"), 128_000);
        assert_eq!(context_window_for_model("gpt-4"), 8_192);
        // Newer Claude ids carry none of the tier names.
        assert_eq!(context_window_for_model("claude-fable-5"), 200_000);
        // F-09: unknown tags are usually local Ollama models with small real
        // windows — overestimating means silent front-truncation, so the
        // catch-all must be conservative.
        assert_eq!(context_window_for_model("qwen2.5-coder:7b"), 8_192);
        assert_eq!(context_window_for_model("deepseek-r1"), 8_192);
    }

    #[test]
    fn test_compact_prompt_with_instructions() {
        let prompt = get_compact_prompt(Some("Focus on API changes"));
        assert!(prompt.contains("Focus on API changes"));
        assert!(prompt.contains("Summarize"));
    }

    #[test]
    fn test_format_compact_summary() {
        let summary = format_compact_summary("- Did X\n- Did Y");
        assert!(summary.contains("<context_summary>"));
        assert!(summary.contains("- Did X"));
    }

    #[test]
    fn test_calculate_messages_to_keep_index() {
        let messages = make_messages(20);
        let idx = calculate_messages_to_keep_index(&messages, 100);
        assert!(idx > 0);
        assert!(idx < 20);
    }

    #[test]
    fn test_messages_to_keep_all_fit() {
        let messages = make_messages(3);
        let idx = calculate_messages_to_keep_index(&messages, 100_000);
        assert_eq!(idx, 0); // keep all
    }
}
