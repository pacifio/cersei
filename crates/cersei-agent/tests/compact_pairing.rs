//! F-04: compaction must not cut between an assistant `tool_use` and the user
//! `tool_result` that answers it.
//!
//! The runner builds a strictly alternating history — idx 0 `user(prompt)`, odd
//! `assistant[tool_use]`, even `user[tool_result]` — and both compaction paths
//! split at `len - KEEP_RECENT_MESSAGES`. `KEEP_RECENT_MESSAGES` is 10, so
//! `parity(split_idx) == parity(len)`, and for every even-length conversation
//! the split lands on a `user[tool_result]` whose `tool_use` was just discarded.
//!
//! Every provider rejects that request server-side: a `tool_result` with no
//! matching `tool_use` in the same request is a 400. The runner turns a 400 into
//! `CerseiError::Provider`, which `is_retryable()` does not match — so the
//! conversation is wedged permanently, at exactly the moment the context was
//! full enough to need compacting.
//!
//! These tests mirror `joy/compact-probe` so the two agree on what "orphaned"
//! means, but unlike the probe they exercise the real `cersei_agent::compact`
//! entry points rather than a copy of the formula.

use cersei_agent::compact::{
    find_orphaned_tool_results, pair_aware_split, snip_compact, KEEP_RECENT_MESSAGES,
};
use cersei_types::*;

/// Reproduce the history shape the runner builds.
fn build_history(n: usize) -> Vec<Message> {
    let mut v = vec![Message::user("initial task")];
    let mut i = 0;
    while v.len() < n {
        let id = format!("toolu_{i}");
        v.push(Message {
            role: Role::Assistant,
            content: MessageContent::Blocks(vec![ContentBlock::ToolUse {
                id: id.clone(),
                name: "Read".into(),
                input: serde_json::json!({"file_path": "/a.rs"}),
            }]),
            id: None,
            metadata: None,
        });
        if v.len() >= n {
            break;
        }
        v.push(Message {
            role: Role::User,
            content: MessageContent::Blocks(vec![ContentBlock::ToolResult {
                tool_use_id: id,
                content: ToolResultContent::Text("file contents".into()),
                is_error: Some(false),
            }]),
            id: None,
            metadata: None,
        });
        i += 1;
    }
    v
}

/// The span of conversation lengths the probe reports on.
const LENGTHS: std::ops::RangeInclusive<usize> = 12..=41;

/// The snip fallback must not hand the provider an orphaned `tool_result`.
///
/// This is the measurable half of the F-04 gate: `snip_compact` is real code
/// that both the runner and `joy/compact-probe` call, so a fix here moves the
/// probe's `snip:orphan` line. Before the fix this failed for 15 of 30 lengths.
#[test]
fn snip_compact_never_orphans_a_tool_result() {
    let mut bad = Vec::new();
    for n in LENGTHS {
        let msgs = build_history(n);
        let (kept, _) = snip_compact(msgs, KEEP_RECENT_MESSAGES);
        let orphans = find_orphaned_tool_results(&kept);
        if !orphans.is_empty() {
            bad.push(format!("len={n} orphans={orphans:?}"));
        }
    }
    assert!(
        bad.is_empty(),
        "snip_compact produced {} request(s) a provider would 400:\n  {}",
        bad.len(),
        bad.join("\n  ")
    );
}

/// The LLM-summary path prepends `user(summary)` and keeps `msgs[split..]`.
/// Same requirement, different call site.
#[test]
fn llm_compact_split_never_orphans_a_tool_result() {
    let mut bad = Vec::new();
    for n in LENGTHS {
        let msgs = build_history(n);
        let split = pair_aware_split(&msgs, KEEP_RECENT_MESSAGES);
        let mut out = vec![Message::user("<summary of earlier turns>")];
        out.extend_from_slice(&msgs[split..]);
        let orphans = find_orphaned_tool_results(&out);
        if !orphans.is_empty() {
            bad.push(format!("len={n} split={split} orphans={orphans:?}"));
        }
    }
    assert!(
        bad.is_empty(),
        "LLM-compaction split produced {} request(s) a provider would 400:\n  {}",
        bad.len(),
        bad.join("\n  ")
    );
}

/// Backing off past the `tool_result` lands on the `assistant[tool_use]` that
/// answers it, which also removes the `user(summary)`/`user(tool_result)`
/// adjacency that Gemini and most local chat templates reject or silently merge.
#[test]
fn llm_compact_split_never_produces_two_consecutive_user_messages() {
    let mut bad = Vec::new();
    for n in LENGTHS {
        let msgs = build_history(n);
        let split = pair_aware_split(&msgs, KEEP_RECENT_MESSAGES);
        let mut out = vec![Message::user("<summary of earlier turns>")];
        out.extend_from_slice(&msgs[split..]);
        let doubled = out
            .windows(2)
            .filter(|w| w[0].role == Role::User && w[1].role == Role::User)
            .count();
        if doubled > 0 {
            bad.push(format!("len={n} split={split} consecutive_user={doubled}"));
        }
    }
    assert!(
        bad.is_empty(),
        "{} length(s) put two user messages back to back:\n  {}",
        bad.len(),
        bad.join("\n  ")
    );
}

/// The split index itself must never point at a message carrying a
/// `tool_result`, since that message becomes the first real turn in the request.
#[test]
fn pair_aware_split_never_lands_on_a_tool_result() {
    for n in LENGTHS {
        let msgs = build_history(n);
        let split = pair_aware_split(&msgs, KEEP_RECENT_MESSAGES);
        if split == 0 {
            continue; // nothing was dropped, so nothing can be orphaned
        }
        let lands_on_result = matches!(&msgs[split].content, MessageContent::Blocks(b)
            if b.iter().any(|x| matches!(x, ContentBlock::ToolResult { .. })));
        assert!(
            !lands_on_result,
            "len={n}: split index {split} points at a tool_result whose tool_use \
             is about to be discarded"
        );
    }
}

/// Backing off must only ever keep *more* context, never less — otherwise the
/// fix would silently discard turns the caller asked to preserve.
///
/// Deliberately no upper bound on the back-off: `pair_aware_split` walks a *run*
/// of `tool_result`-carrying messages, and while the runner batches every
/// parallel result into one message (so the run is currently always length 1),
/// a restored session or a future per-result message shape can make it longer.
/// Asserting `naive - split <= 1` would encode today's message packing as a
/// requirement and fail on a correct implementation later.
#[test]
fn pair_aware_split_only_ever_keeps_more() {
    for n in LENGTHS {
        let msgs = build_history(n);
        let naive = msgs.len().saturating_sub(KEEP_RECENT_MESSAGES);
        let split = pair_aware_split(&msgs, KEEP_RECENT_MESSAGES);
        assert!(
            split <= naive,
            "len={n}: split {split} dropped more than the requested {naive}"
        );
    }
}

/// The snip fallback must emit a request the provider will actually accept.
///
/// Orphan-freedom alone is not that. Backing the split off onto the
/// `assistant[tool_use]` fixes the pair but opens the history with an assistant
/// turn, and Anthropic rejects any request whose first message is not `user` —
/// so a test that only counted orphans would stay green while every single
/// snip-compacted turn still died at the provider. Before the truncation marker
/// was added this failed for all 30 lengths.
#[test]
fn snip_compact_output_is_a_sendable_request() {
    let mut bad = Vec::new();
    for n in LENGTHS {
        let msgs = build_history(n);
        let (kept, _) = snip_compact(msgs, KEEP_RECENT_MESSAGES);

        if kept.first().map(|m| m.role) != Some(Role::User) {
            bad.push(format!(
                "len={n}: history opens with {:?}, not User",
                kept.first().map(|m| m.role)
            ));
            continue;
        }
        let orphans = find_orphaned_tool_results(&kept);
        if !orphans.is_empty() {
            bad.push(format!("len={n}: orphans={orphans:?}"));
        }
        let doubled = kept
            .windows(2)
            .filter(|w| w[0].role == Role::User && w[1].role == Role::User)
            .count();
        if doubled > 0 {
            bad.push(format!("len={n}: {doubled} consecutive user message(s)"));
        }
    }
    assert!(
        bad.is_empty(),
        "snip_compact produced {} unsendable request(s):\n  {}",
        bad.len(),
        bad.join("\n  ")
    );
}

/// The detector the runner uses pre-request must actually detect an orphan, or
/// the assertion half of F-04 is decorative.
#[test]
fn find_orphaned_tool_results_flags_a_missing_tool_use() {
    let full = build_history(6);
    // Drop the leading user turn AND the first assistant tool_use, leaving that
    // tool's result behind with nothing to answer.
    let severed = full[2..].to_vec();
    let orphans = find_orphaned_tool_results(&severed);
    assert_eq!(
        orphans,
        vec!["toolu_0".to_string()],
        "a tool_result whose tool_use was cut away must be reported"
    );

    assert!(
        find_orphaned_tool_results(&full).is_empty(),
        "an intact history must report nothing"
    );
}
