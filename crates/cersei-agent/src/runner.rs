//! Agent runner: the core agentic loop.

use crate::compact;
use crate::events::{AgentControl, AgentEvent};
use crate::{Agent, AgentOutput, ToolCallRecord};
use cersei_hooks::{HookAction, HookContext, HookEvent};
use cersei_provider::{CompletionRequest, ProviderOptions, StreamAccumulator};
use cersei_tools::permissions::{PermissionDecision, PermissionRequest};
use cersei_tools::{Tool, ToolContext, ToolResult};
use cersei_types::*;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::{mpsc, oneshot};

// ─── Retry jitter ────────────────────────────────────────────────────────────

/// Simple pseudo-random jitter for retry delays (no external crate needed).
fn rand_jitter() -> u64 {
    use std::time::SystemTime;
    let seed = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .subsec_nanos() as u64;
    seed ^ (seed >> 16) ^ (seed << 7)
}

// ─── Read-before-edit guard (F-11) ───────────────────────────────────────────

/// Paths a call would write to, as the model named them.
///
/// `ApplyPatch` is the awkward one: its targets are not a parameter, they are
/// inside the patch body. They are read out of the `+++ ` headers and put
/// through the *same* normalisation `apply_patch.rs` applies before it joins
/// against the working directory — timestamp stripped, git-style `b/` prefix
/// stripped.
///
/// Keeping the two in step is load-bearing, not tidiness. A guard that resolves
/// a target differently from the tool does not merely mis-report: for
/// `+++ b/a.rs` it would look up `<wd>/b/a.rs`, find nothing, conclude the file
/// is new and needs no prior read, and wave through an overwrite of an unread
/// `<wd>/a.rs`. The failure is silent and in the unsafe direction.
fn write_targets(tool_name: &str, tool_input: &serde_json::Value) -> Vec<String> {
    let named = || {
        tool_input
            .get("file_path")
            .and_then(serde_json::Value::as_str)
            .map(|s| vec![s.to_string()])
            .unwrap_or_default()
    };
    match tool_name {
        "Write" | "write" | "Edit" | "edit" | "MultiEdit" | "multi_edit" | "NotebookEdit"
        | "notebook_edit" => named(),
        "ApplyPatch" | "apply_patch" => tool_input
            .get("patch")
            .and_then(serde_json::Value::as_str)
            .map(|p| {
                p.lines()
                    .filter_map(|l| l.strip_prefix("+++ "))
                    // Mirrors apply_patch.rs: timestamp, then git prefix.
                    .map(|t| t.split('\t').next().unwrap_or(t))
                    .map(|t| t.strip_prefix("b/").unwrap_or(t))
                    .map(|t| t.trim().to_string())
                    .filter(|t| !t.is_empty() && t != "/dev/null")
                    .collect()
            })
            .unwrap_or_default(),
        _ => Vec::new(),
    }
}

/// One spelling for one file, so the read side and the write side can be
/// compared at all.
///
/// The set of seen files is keyed by this. Comparing raw strings meant
/// `Read("src/x.rs")` followed by `Edit("/wd/src/x.rs")` looked like two
/// different files and the edit was refused, and it made `./x` and `x`
/// distinct. Relative paths are resolved against the tool context's working
/// directory; `canonicalize` then resolves symlinks and `..`, and is expected
/// to fail for a path that does not exist yet — the lexical form is the right
/// answer there.
fn resolve_path(working_dir: &std::path::Path, p: &str) -> String {
    let joined = if std::path::Path::new(p).is_absolute() {
        std::path::PathBuf::from(p)
    } else {
        working_dir.join(p)
    };
    std::fs::canonicalize(&joined)
        .unwrap_or(joined)
        .to_string_lossy()
        .to_string()
}

/// Refuse a blind overwrite: writing a file that exists but was never read.
///
/// Returns the message to hand back *instead of* running the tool. This must be
/// consulted before dispatch. It used to be applied to the returned
/// `ToolResult` after `execute` had already completed, which meant the file was
/// modified and then the model was told the edit had been blocked — the worst
/// of both, since it left disk and conversation disagreeing about what
/// happened.
///
/// A path that does not exist yet is a creation, not an overwrite, and needs no
/// prior read.
fn read_before_edit_block(
    tool_name: &str,
    tool_input: &serde_json::Value,
    files_read: &std::collections::HashSet<String>,
    working_dir: &std::path::Path,
) -> Option<String> {
    for target in write_targets(tool_name, tool_input) {
        // Both sides go through `resolve_path`, so a file counts as seen no
        // matter which spelling the model used for the read and the write.
        let resolved = resolve_path(working_dir, &target);
        if files_read.contains(&resolved) {
            continue;
        }
        if !std::path::Path::new(&resolved).exists() {
            continue;
        }
        return Some(format!(
            "{tool_name} was not run: '{target}' already exists and you have not read it in \
             this session, so this call would overwrite content you have never seen. Call Read \
             with file_path='{target}' first, then send this {tool_name} call again. Nothing \
             was written."
        ));
    }
    None
}

/// Decide which calls in a parallel batch must be refused, before any of them
/// runs.
///
/// Taking the whole batch is what makes the ordering enforceable rather than
/// merely intended: the result is computed from `tool_use_blocks` and then
/// captured by the dispatch closures, so there is no way to build the futures
/// without having decided the refusals first.
///
/// Note the concurrency semantics this fixes in place: `files_read` is the set
/// as of the *start* of the batch. A model that issues `Read(f)` and `Edit(f)`
/// in the same parallel batch still has the edit refused, because the read has
/// not completed when the batch is dispatched and nothing orders the two.
fn refusals_for_batch(
    calls: &[(String, String, serde_json::Value)],
    files_read: &std::collections::HashSet<String>,
    working_dir: &std::path::Path,
) -> std::collections::HashMap<String, String> {
    let mut out = std::collections::HashMap::new();
    for (id, name, input) in calls {
        if let Some(msg) = read_before_edit_block(name, input, files_read, working_dir) {
            out.insert(id.clone(), msg);
        }
    }
    out
}

// ─── Repeated-failure steering (F-06) ────────────────────────────────────────

/// Consecutive failures of one tool after which the advice stops being gentle.
const MAX_TOOL_ERRORS_PER_TOOL: u32 = 3;

/// Advice appended to a failing tool result, escalating with the streak.
///
/// This is steering, not a budget. The old text counted down "N attempts
/// remaining" while nothing anywhere compared against a limit, so the countdown
/// ran past zero into negative numbers and promised an intervention that never
/// came. Rather than invent that intervention — refusing a tool outright can
/// leave a turn with no way forward, which is the failure mode this work exists
/// to remove — the claim is dropped and the wording says only what is true: the
/// same call keeps failing, so try a different one.
///
/// [`MAX_TOOL_ERRORS_PER_TOOL`] is the point at which the advice turns blunt.
fn error_budget_note(tool_name: &str, count: u32) -> String {
    if count >= MAX_TOOL_ERRORS_PER_TOOL {
        format!(
            "[Tool '{tool_name}' has now failed {count} times in a row. Do not call it again \
             with a variation of this input — that has not worked {count} times. Use a \
             different tool, or tell the user what is blocking you and ask how to proceed.]"
        )
    } else {
        format!(
            "[Tool '{tool_name}' has failed {count} time(s) in a row. Read the error above and \
             change your approach — do not resend the same call.]"
        )
    }
}

// ─── Tool result size management ─────────────────────────────────────────────

/// Maximum number of lines to keep in a tool result before truncation.
const MAX_HEAD_LINES: usize = 80;
const MAX_TAIL_LINES: usize = 80;
/// Char-based fallback for results without many newlines.
const MAX_SINGLE_RESULT_CHARS: usize = 20_000;

/// Truncate an individual tool result using a head+tail line strategy.
/// Keeps the first N and last N lines, which preserves both the command
/// context (head) and error messages (tail) — errors are usually at the end.
fn cap_tool_result(content: &str) -> String {
    let lines: Vec<&str> = content.lines().collect();
    let total_lines = lines.len();

    // Line-based truncation if enough lines
    if total_lines > MAX_HEAD_LINES + MAX_TAIL_LINES + 5 {
        let head: String = lines[..MAX_HEAD_LINES].join("\n");
        let tail: String = lines[total_lines.saturating_sub(MAX_TAIL_LINES)..].join("\n");
        let omitted = total_lines - MAX_HEAD_LINES - MAX_TAIL_LINES;
        return format!(
            "{head}\n\n[... {omitted} lines omitted ({total_lines} total). Pipe through `head` or `tail` for specific sections ...]\n\n{tail}"
        );
    }

    // Char-based fallback for single long lines or binary-ish output
    if content.len() > MAX_SINGLE_RESULT_CHARS {
        // Floor/ceil the cut points to char boundaries so we never slice
        // through a multibyte UTF-8 sequence (which would panic).
        let mut head_end = MAX_SINGLE_RESULT_CHARS * 70 / 100;
        while head_end > 0 && !content.is_char_boundary(head_end) {
            head_end -= 1;
        }
        let tail_chars = MAX_SINGLE_RESULT_CHARS * 20 / 100;
        let mut tail_start = content.len().saturating_sub(tail_chars);
        while tail_start < content.len() && !content.is_char_boundary(tail_start) {
            tail_start += 1;
        }
        let omitted = tail_start.saturating_sub(head_end);
        return format!(
            "{}\n\n[... {omitted} chars omitted ...]\n\n{}",
            &content[..head_end],
            &content[tail_start..]
        );
    }

    content.to_string()
}

/// Truncate oldest tool results when cumulative size exceeds budget.
/// Modifies messages in place.
pub fn apply_tool_result_budget(messages: &mut [Message], budget_chars: usize) {
    // Collect total tool result size
    let total: usize = messages
        .iter()
        .flat_map(|m| match &m.content {
            MessageContent::Blocks(blocks) => blocks
                .iter()
                .filter_map(|b| {
                    if let ContentBlock::ToolResult { content, .. } = b {
                        Some(match content {
                            ToolResultContent::Text(t) => t.len(),
                            ToolResultContent::Blocks(b) => b
                                .iter()
                                .map(|bb| {
                                    if let ContentBlock::Text { text } = bb {
                                        text.len()
                                    } else {
                                        0
                                    }
                                })
                                .sum(),
                        })
                    } else {
                        None
                    }
                })
                .collect::<Vec<_>>(),
            _ => vec![],
        })
        .sum();

    if total <= budget_chars {
        return;
    }

    // Truncate oldest tool results first (skip the last KEEP_RECENT messages)
    let keep_recent = 6; // don't touch recent tool results
    let truncatable_end = messages.len().saturating_sub(keep_recent);
    let mut freed = 0usize;
    let target_free = total - budget_chars;

    for msg in messages[..truncatable_end].iter_mut() {
        if freed >= target_free {
            break;
        }
        if let MessageContent::Blocks(blocks) = &mut msg.content {
            for block in blocks.iter_mut() {
                if freed >= target_free {
                    break;
                }
                if let ContentBlock::ToolResult { content, .. } = block {
                    let size = match content {
                        ToolResultContent::Text(t) => t.len(),
                        ToolResultContent::Blocks(_) => 100,
                    };
                    if size > 200 {
                        freed += size;
                        *content = ToolResultContent::Text(
                            "[truncated — re-read file if needed]".to_string(),
                        );
                    }
                }
            }
        }
    }
}

// ─── Control channel state ───────────────────────────────────────────────────

/// What the control-channel pump feeds back into the agentic loop.
#[derive(Default)]
struct ControlState {
    /// Messages from `AgentStream::inject_message`, drained at turn boundaries.
    injected: parking_lot::Mutex<Vec<String>>,
    /// One slot per outstanding permission question, keyed by request id.
    ///
    /// A oneshot per question rather than a shared map plus a `Notify`: the
    /// slot is registered *before* the question goes out, so a decision that
    /// arrives immediately has somewhere to land. A notification-based design
    /// can lose the wake in the window between checking for an answer and
    /// registering as a waiter.
    pending_permissions:
        parking_lot::Mutex<std::collections::HashMap<String, oneshot::Sender<PermissionDecision>>>,
}

/// Build the permission request for one tool call.
///
/// Shared by the stream-deferred pre-dispatch ask and the in-dispatch policy
/// check so the two cannot describe the same call differently — they are keyed
/// by `id`, so a drift there would silently mismatch a decision to its call.
fn permission_request_for(
    tool: &dyn Tool,
    tool_id: &str,
    tool_input: &serde_json::Value,
) -> PermissionRequest {
    PermissionRequest {
        tool_name: tool.name().to_string(),
        tool_input: tool_input.clone(),
        permission_level: tool.permission_level(),
        description: format!("Execute tool '{}'", tool.name()),
        id: tool_id.to_string(),
    }
}

/// The outcome of putting a permission question to the stream.
///
/// "Cancelled" is deliberately distinct from "nobody could answer": collapsing
/// the two into one `None` made a cancel fall through to the policy's own
/// `check`, and `StreamDeferredPolicy` allows — so cancelling mid-prompt ran
/// the very tool the user was being asked about.
enum PermissionAsk {
    Decided(PermissionDecision),
    /// No stream is consuming events — the plain `run()` path, or a detached
    /// handle. The policy's own decision stands.
    Undeliverable,
    /// The run was cancelled with the question outstanding.
    Cancelled,
}

impl ControlState {
    /// Ask the stream to decide a permission request, and wait for the answer.
    async fn ask_stream(
        &self,
        event_tx: &mpsc::Sender<AgentEvent>,
        request: &PermissionRequest,
        cancel_token: &tokio_util::sync::CancellationToken,
    ) -> PermissionAsk {
        let (tx, rx) = oneshot::channel();
        self.pending_permissions
            .lock()
            .insert(request.id.clone(), tx);

        if event_tx
            .send(AgentEvent::PermissionRequired(request.clone()))
            .await
            .is_err()
        {
            self.pending_permissions.lock().remove(&request.id);
            return PermissionAsk::Undeliverable;
        }

        let outcome = tokio::select! {
            answer = rx => match answer {
                Ok(decision) => PermissionAsk::Decided(decision),
                // The pump is gone, so no answer can ever arrive.
                Err(_) => PermissionAsk::Undeliverable,
            },
            _ = cancel_token.cancelled() => PermissionAsk::Cancelled,
            // The consumer went away mid-question. Without this arm a detached
            // stream — which does not cancel the token — parks the run forever.
            _ = event_tx.closed() => PermissionAsk::Undeliverable,
        };
        if !matches!(outcome, PermissionAsk::Decided(_)) {
            self.pending_permissions.lock().remove(&request.id);
        }
        outcome
    }

    /// Route a decision from the control channel to whoever is waiting on it.
    /// Unsolicited or late responses have no slot and are dropped.
    fn resolve_permission(&self, request_id: &str, decision: PermissionDecision) {
        if let Some(tx) = self.pending_permissions.lock().remove(request_id) {
            let _ = tx.send(decision);
        } else {
            tracing::debug!(
                request_id,
                "permission response with no outstanding request — ignoring"
            );
        }
    }
}

/// Run the agent without streaming (blocking until complete).
pub async fn run_agent(agent: &Agent, prompt: &str) -> Result<AgentOutput> {
    // The receiver is dropped immediately, on purpose. Nothing consumes events
    // on this path — `agent.emit` is what feeds listeners — so closing the
    // channel makes every `let _ = event_tx.send(..)` a cheap no-op.
    //
    // It used to be bound as `_event_rx`, which is a *binding* (unlike a bare
    // `_`) and so lived to the end of the function: the channel stayed open,
    // nobody drained its 512 slots, and `send().await` blocked forever once it
    // filled. Any `run()` emitting more than 512 events — a few hundred text
    // deltas — deadlocked.
    let (event_tx, event_rx) = mpsc::channel(512);
    drop(event_rx);
    // Likewise for control: no `AgentStream` exists here to send on it, and the
    // sender must drop so the pump's `recv()` completes instead of parking.
    let (control_tx, control_rx) = mpsc::unbounded_channel();
    drop(control_tx);

    let prompt = prompt.to_string();
    let cancel_token = agent.begin_run();

    // Run in a background task and collect events
    let result = run_agent_streaming(agent, &prompt, event_tx, control_rx, cancel_token).await;

    match result {
        Ok(output) => {
            agent.emit(AgentEvent::Complete(output.clone()));
            Ok(output)
        }
        Err(e) => {
            agent.emit(AgentEvent::Error(e.to_string()));
            Err(e)
        }
    }
}

/// Abort a spawned task when this guard goes out of scope.
///
/// `run_agent_streaming` returns from a dozen places; a guard is what makes the
/// control pump's lifetime match the run's without threading a shutdown through
/// every one of them.
struct AbortOnDrop(tokio::task::JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Core agentic loop with streaming events.
///
/// `cancel_token` is the token for *this* run, claimed by the caller via
/// `Agent::begin_run`. It is passed in rather than read off the agent so the
/// stream handle and the loop provably observe the same token.
pub async fn run_agent_streaming(
    agent: &Agent,
    prompt: &str,
    event_tx: mpsc::Sender<AgentEvent>,
    control_rx: mpsc::UnboundedReceiver<AgentControl>,
    cancel_token: tokio_util::sync::CancellationToken,
) -> Result<AgentOutput> {
    // ── Control channel pump ──
    // Everything an `AgentStream` sends arrives here. Until this existed the
    // receiver was bound as `_control_rx` and never read, so `cancel()`,
    // `inject_message()` and `respond_permission()` — all three documented in
    // wiki/05-events-streaming.md — died in an undrained buffer.
    let control = Arc::new(ControlState::default());
    let _pump = AbortOnDrop(tokio::spawn({
        let control = Arc::clone(&control);
        let cancel_token = cancel_token.clone();
        let mut control_rx = control_rx;
        async move {
            while let Some(msg) = control_rx.recv().await {
                match msg {
                    AgentControl::Cancel => {
                        cancel_token.cancel();
                        return;
                    }
                    AgentControl::InjectMessage(text) => {
                        control.injected.lock().push(text);
                    }
                    AgentControl::PermissionResponse {
                        request_id,
                        decision,
                    } => control.resolve_permission(&request_id, decision),
                }
            }
        }
    }));

    // Load session history (skip if messages were pre-populated via with_messages)
    if agent.messages.lock().is_empty() {
        if let (Some(memory), Some(session_id)) = (&agent.memory, &agent.session_id) {
            let history = memory.load(session_id).await?;
            if !history.is_empty() {
                let count = history.len();
                agent.messages.lock().extend(history);
                let _ = event_tx
                    .send(AgentEvent::SessionLoaded {
                        session_id: session_id.clone(),
                        message_count: count,
                    })
                    .await;
                agent.emit(AgentEvent::SessionLoaded {
                    session_id: session_id.clone(),
                    message_count: count,
                });
            }
        }
    } // end session load guard

    // Add user prompt (with exploration hint for analysis tasks)
    let is_analysis = prompt.contains("index")
        || prompt.contains("analyze")
        || prompt.contains("explore")
        || prompt.contains("understand")
        || prompt.contains("tell me about")
        || prompt.contains("summary");

    let expanded_prompt = if is_analysis {
        format!(
            "{}\n\n[system hint: The project_intel section in your context shows the most important files ranked by dependency graph analysis (tree-sitter). Use parallel Read calls to read those files — entry points, stores, commands, and type files listed there. Read at least 10 files before writing output. Focus on files with the most symbols and imports.]",
            prompt
        )
    } else {
        prompt.to_string()
    };

    agent.messages.lock().push(Message::user(&expanded_prompt));

    let mut tool_calls: Vec<ToolCallRecord> = Vec::new();
    let mut turn: u32 = 0;
    let mut last_stop_reason = StopReason::EndTurn;
    let mut _last_usage = Usage::default();
    let mut max_tokens_retries: u32 = 0;
    const MAX_TOKENS_RETRY_LIMIT: u32 = 3;
    let mut had_tool_use = false;
    let mut depth_nudge_sent = false;
    // F-08: the no-tool-call nudge fires at most once per session, and its
    // retry turn carries a one-shot forced tool choice.
    let mut no_tool_nudge_sent = false;
    let mut force_tool_choice = false;
    let mut benchmark_retries: u32 = 0;
    const BENCHMARK_MAX_RETRIES: u32 = 4;
    let mut doom_loop_warned = false;
    let mut completion_verified = false;

    // Runtime guards
    let mut files_read: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut tool_error_counts: std::collections::HashMap<String, u32> =
        std::collections::HashMap::new();

    // Build tool context
    let tool_ctx = ToolContext {
        working_dir: agent.working_dir.clone(),
        session_id: agent
            .session_id
            .clone()
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
        permissions: Arc::clone(&agent.permission_policy),
        cost_tracker: Arc::clone(&agent.cost_tracker),
        mcp_manager: agent.mcp_manager.clone(),
        extensions: agent.extensions.clone(),
    };

    // Agentic loop
    loop {
        turn += 1;
        if turn > agent.max_turns {
            break;
        }

        // Check cancellation
        if cancel_token.is_cancelled() {
            return Err(CerseiError::Cancelled);
        }

        // ── Messages injected through `AgentStream::inject_message` ──
        // A turn boundary is the one place history is quiescent: the previous
        // turn's tool results are all in, and the next request has not been
        // built yet.
        for text in std::mem::take(&mut *control.injected.lock()) {
            agent.messages.lock().push(Message::user(&text));
        }

        let _ = event_tx.send(AgentEvent::TurnStart { turn }).await;
        agent.emit(AgentEvent::TurnStart { turn });

        // Apply tool result budget to keep context manageable
        {
            let mut msgs = agent.messages.lock();
            apply_tool_result_budget(&mut msgs, agent.tool_result_budget);
        }

        // Build completion request
        let messages = agent.messages.lock().clone();
        let tool_defs: Vec<ToolDefinition> =
            agent.tools.iter().map(|t| t.to_definition()).collect();

        let model = agent
            .model
            .clone()
            .unwrap_or_else(|| "claude-sonnet-4-6".to_string());

        let mut options = ProviderOptions::default();
        if let Some(budget) = agent.thinking_budget {
            options.set("thinking_budget", budget);
        }
        // F-08: one-shot — applies only to the retry turn right after the
        // no-tool-call nudge, then reverts to the provider default (auto).
        if force_tool_choice {
            force_tool_choice = false;
            options.set("tool_choice", "required");
        }
        // F-09: the window this loop budgets against rides on every request;
        // only providers flagged for it (Ollama) put it on the wire as
        // options.num_ctx. Without it Ollama stays at its server-side
        // default window and silently truncates the prompt front.
        options.set("num_ctx", compact::context_window_for_model(&model));

        // Todo nudge: on turns > 2, remind model about incomplete todos
        let system_with_nudge = if turn > 2 {
            let session_id = agent.session_id.as_deref().unwrap_or("default");
            let todos = cersei_tools::todo_write::get_todos(session_id);
            let incomplete = todos
                .iter()
                .filter(|t| t.status != cersei_tools::todo_write::TodoStatus::Completed)
                .count();
            if incomplete > 0 {
                let nudge = format!(
                    "\n\n[system reminder: You have {} incomplete task{} in your TodoWrite list. Make sure to complete all tasks before ending your response. Use tools to make progress on each task.]",
                    incomplete,
                    if incomplete == 1 { "" } else { "s" }
                );
                agent.system_prompt.as_ref().map(|s| format!("{s}{nudge}"))
            } else {
                agent.system_prompt.clone()
            }
        } else {
            agent.system_prompt.clone()
        };

        // F-04: last line of defence. Compaction is the known way to sever a
        // tool_use/tool_result pair, but anything that rewrites history can do
        // it, and the provider's answer is always a 400 that the retry loop
        // cannot rescue. Report it here, naming the ids, so the cause is in the
        // log next to the request that carried it rather than inferred later
        // from an opaque provider error.
        let orphaned = compact::find_orphaned_tool_results(&messages);
        if !orphaned.is_empty() {
            tracing::error!(
                orphaned_tool_use_ids = ?orphaned,
                message_count = messages.len(),
                "request carries tool_result blocks with no matching tool_use; \
                 the provider will reject this with a 400"
            );
        }
        // §10.5 #3, the mirror rule: an assistant tool_use with no tool_result
        // anywhere in the request is the same unretryable 400 from the other
        // direction. Every request this loop builds ends with a user message,
        // so nothing is legitimately unanswered here.
        let unanswered = compact::find_unanswered_tool_uses(&messages);
        if !unanswered.is_empty() {
            tracing::error!(
                unanswered_tool_use_ids = ?unanswered,
                message_count = messages.len(),
                "request carries tool_use blocks with no matching tool_result; \
                 the provider will reject this with a 400"
            );
        }

        let tools_available = !tool_defs.is_empty();
        let request = CompletionRequest {
            model: model.clone(),
            messages: messages.clone(),
            system: system_with_nudge,
            tools: tool_defs,
            max_tokens: agent.max_tokens,
            temperature: agent.temperature,
            stop_sequences: Vec::new(),
            options,
        };

        let _ = event_tx
            .send(AgentEvent::ModelRequestStart {
                turn,
                message_count: messages.len(),
                token_estimate: 0,
            })
            .await;

        // Send to provider with automatic retry on transient errors
        let mut retry_count = 0u32;
        const MAX_RETRIES: u32 = 5;

        let (mut rx, mut accumulator) = loop {
            let req_clone = request.clone();
            // `complete()` now awaits the provider's response headers before it
            // returns (F-02) — that is what lets a 429 come back as a retryable
            // `Err` instead of a stream event the retry loop can't see. But it
            // also means this loop, not the stream loop below, is where the
            // request spends its time-to-first-byte, and this loop is outside
            // the `select!` that watches `cancel_token`. No provider configures
            // a client timeout, so without this branch a cancel is ignored
            // until the first byte arrives — forever, against a server that
            // accepts the connection and then goes quiet.
            let outcome = tokio::select! {
                result = agent.provider.complete(req_clone) => result,
                _ = cancel_token.cancelled() => return Err(CerseiError::Cancelled),
            };
            match outcome {
                Ok(stream) => {
                    break (stream.into_receiver(), StreamAccumulator::new());
                }
                Err(e) if e.is_retryable() && retry_count < MAX_RETRIES => {
                    retry_count += 1;
                    let delay_ms = (1000 * 2u64.pow(retry_count - 1)).min(30_000); // 1s, 2s, 4s, 8s, 16s
                    let jitter = (delay_ms / 4) as u64;
                    let actual_delay = delay_ms + (rand_jitter() % jitter.max(1));
                    tracing::warn!(
                        "Provider error (retryable, attempt {}/{}): {}. Retrying in {}ms...",
                        retry_count,
                        MAX_RETRIES,
                        e,
                        actual_delay
                    );
                    let _ = event_tx
                        .send(AgentEvent::Status(format!(
                            "Rate limited. Retrying in {:.1}s... ({}/{})",
                            actual_delay as f64 / 1000.0,
                            retry_count,
                            MAX_RETRIES
                        )))
                        .await;
                    agent.emit(AgentEvent::Status(format!(
                        "Retrying in {:.1}s ({}/{})",
                        actual_delay as f64 / 1000.0,
                        retry_count,
                        MAX_RETRIES
                    )));
                    // Same reasoning as the `complete()` await above, and newly
                    // load-bearing: until F-02 this sleep was unreachable, so
                    // its uncancellability never showed. Five retries is up to
                    // ~31s of it.
                    tokio::select! {
                        _ = tokio::time::sleep(std::time::Duration::from_millis(actual_delay)) => {}
                        _ = cancel_token.cancelled() => return Err(CerseiError::Cancelled),
                    }
                    continue;
                }
                Err(e) => return Err(e),
            }
        };

        let _ = event_tx
            .send(AgentEvent::ModelResponseStart {
                turn,
                model: model.clone(),
            })
            .await;

        // Process stream events (with cancellation support)
        loop {
            tokio::select! {
                event = rx.recv() => {
                    match event {
                        Some(event) => {
                            match &event {
                                StreamEvent::TextDelta { text, .. } => {
                                    let _ = event_tx.send(AgentEvent::TextDelta(text.clone())).await;
                                    agent.emit(AgentEvent::TextDelta(text.clone()));
                                }
                                StreamEvent::ThinkingDelta { thinking, .. } => {
                                    let _ = event_tx
                                        .send(AgentEvent::ThinkingDelta(thinking.clone()))
                                        .await;
                                    agent.emit(AgentEvent::ThinkingDelta(thinking.clone()));
                                }
                                StreamEvent::Error { message } => {
                                    return Err(CerseiError::Provider(message.clone()));
                                }
                                _ => {}
                            }
                            accumulator.process_event(event);
                        }
                        None => break, // Stream ended
                    }
                }
                _ = cancel_token.cancelled() => {
                    return Err(CerseiError::Cancelled);
                }
            }
        }

        // Convert accumulated response
        let response = accumulator.into_response()?;
        last_stop_reason = response.stop_reason.clone();
        _last_usage = response.usage.clone();

        // Update cumulative usage
        agent.cumulative_usage.lock().merge(&response.usage);
        agent.cost_tracker.add_with_model(&response.usage, &model);

        // Emit cost update
        let cumulative = agent.cumulative_usage.lock().clone();
        let _ = event_tx
            .send(AgentEvent::CostUpdate {
                turn_cost: response.usage.cost_usd.unwrap_or(0.0),
                cumulative_cost: cumulative.cost_usd.unwrap_or(0.0),
                input_tokens: cumulative.input_tokens,
                output_tokens: cumulative.output_tokens,
            })
            .await;
        agent.emit(AgentEvent::CostUpdate {
            turn_cost: response.usage.cost_usd.unwrap_or(0.0),
            cumulative_cost: cumulative.cost_usd.unwrap_or(0.0),
            input_tokens: cumulative.input_tokens,
            output_tokens: cumulative.output_tokens,
        });

        // Add assistant message to history
        agent.messages.lock().push(response.message.clone());

        // Fire PostModelTurn hooks
        let hook_ctx = HookContext {
            event: HookEvent::PostModelTurn,
            tool_name: None,
            tool_input: None,
            tool_result: None,
            tool_is_error: None,
            turn,
            cumulative_cost_usd: cumulative.cost_usd.unwrap_or(0.0),
            message_count: agent.messages.lock().len(),
        };
        let hook_action = cersei_hooks::run_hooks(&agent.hooks, &hook_ctx).await;
        if let HookAction::Block(reason) = hook_action {
            return Err(CerseiError::Provider(format!(
                "Blocked by hook: {}",
                reason
            )));
        }

        // Fire TurnsElapsed every `turns_elapsed_cadence` turns (default 10).
        // Callers can register a SkillNudgeHook here for agent-curated skill
        // creation without blocking the agent loop.
        if turn > 0 && turn % agent.turns_elapsed_cadence == 0 {
            let cadence_ctx = HookContext {
                event: HookEvent::TurnsElapsed,
                tool_name: None,
                tool_input: None,
                tool_result: None,
                tool_is_error: None,
                turn,
                cumulative_cost_usd: cumulative.cost_usd.unwrap_or(0.0),
                message_count: agent.messages.lock().len(),
            };
            // Don't block on TurnsElapsed hooks — best-effort, fire and forget.
            let _ = cersei_hooks::run_hooks(&agent.hooks, &cadence_ctx).await;
        }

        let _ = event_tx
            .send(AgentEvent::TurnComplete {
                turn,
                stop_reason: response.stop_reason.clone(),
                usage: response.usage.clone(),
            })
            .await;
        agent.emit(AgentEvent::TurnComplete {
            turn,
            stop_reason: response.stop_reason.clone(),
            usage: response.usage.clone(),
        });

        // Handle stop reason
        match &response.stop_reason {
            StopReason::EndTurn => {
                // ── Completion verification nudge ──
                // If agent is finishing but hasn't verified its output, nudge once.
                if agent.benchmark_mode && !completion_verified && turn >= 3 {
                    let recent_has_verify = tool_calls.iter().rev().take(5).any(|tc| {
                        let cmd = tc
                            .input
                            .get("command")
                            .and_then(|v| v.as_str())
                            .unwrap_or("");
                        cmd.contains("cat ")
                            || cmd.contains("python ")
                            || cmd.contains("test")
                            || cmd.contains("verify")
                            || cmd.contains("node ")
                            || cmd.contains("./")
                            || cmd.contains("check")
                    });
                    if !recent_has_verify {
                        completion_verified = true;
                        agent.messages.lock().push(Message::user(
                            "[system] Before finishing, verify your solution is correct:\n\
                             1. Check that all expected output files exist and have correct content\n\
                             2. Run your solution to confirm it produces the right output\n\
                             3. Re-read the original instruction — did you satisfy EVERY requirement?"
                        ));
                        let _ = event_tx
                            .send(AgentEvent::Status(
                                "Nudging agent to verify before completion".into(),
                            ))
                            .await;
                        continue;
                    }
                }

                // ── Benchmark self-verification ──
                // In TB 2.0 tests are run externally by the verifier AFTER the agent
                // finishes. We only intervene if:
                // 1) The instruction mentions a specific test/verify command — nudge
                //    the agent to run it if it hasn't.
                // 2) The agent ran such a command and it failed — nudge to retry.
                // We do NOT hardcode /tests/run-tests.sh — that path doesn't exist
                // during agent execution in TB 2.0.
                if agent.benchmark_mode && benchmark_retries < BENCHMARK_MAX_RETRIES {
                    // Check if the instruction mentions a verification command
                    let has_instruction_tests = prompt.contains("test_outputs.py")
                        || prompt.contains("run_tests")
                        || prompt.contains("run-tests")
                        || prompt.contains("pytest")
                        || prompt.contains("verify.py")
                        || prompt.contains("check.py")
                        || prompt.contains("npm test")
                        || prompt.contains("cargo test")
                        || prompt.contains("make test");

                    if has_instruction_tests {
                        let verification = benchmark_check_tests(&tool_calls);
                        match verification {
                            BenchmarkVerification::TestsNotRun => {
                                if benchmark_retries == 0 {
                                    benchmark_retries += 1;
                                    agent.messages.lock().push(Message::user(
                                        "[system] The task instruction mentions a verification command. \
                                         Run it now to check your solution. Look at the instruction again \
                                         for the exact command."
                                    ));
                                    let _ = event_tx
                                        .send(AgentEvent::Status(
                                            "Benchmark: nudge to run instruction's test command"
                                                .into(),
                                        ))
                                        .await;
                                    continue;
                                }
                                break;
                            }
                            BenchmarkVerification::TestsFailed(ref test_output) => {
                                benchmark_retries += 1;
                                let truncated: String = test_output.chars().take(3000).collect();
                                agent.messages.lock().push(Message::user(
                                    &format!(
                                        "[system] Verification FAILED (attempt {}/{}).\n\n\
                                         Output:\n```\n{}\n```\n\n\
                                         Try a COMPLETELY DIFFERENT approach. Do NOT patch — rewrite.",
                                        benchmark_retries, BENCHMARK_MAX_RETRIES, truncated
                                    )
                                ));
                                let _ = event_tx
                                    .send(AgentEvent::Status(format!(
                                        "Benchmark: retry {}/{}",
                                        benchmark_retries, BENCHMARK_MAX_RETRIES
                                    )))
                                    .await;
                                continue;
                            }
                            BenchmarkVerification::TestsPassed => {
                                break;
                            }
                        }
                    }
                    // No test command in instruction — let the agent finish.
                    // The external verifier will run tests after.
                }

                // F-08: a prose-only session with tools available is *the*
                // characteristic weak-model failure, and it was previously
                // handled only in benchmark mode. The system prompt orders
                // "ALWAYS verify information about the codebase using tools
                // before answering"; this is the once-per-session enforcement.
                // The retry turn also carries a forced tool choice
                // (tool_choice: required / {type:"any"} / mode ANY) where the
                // provider supports it.
                if !had_tool_use && tools_available && !no_tool_nudge_sent {
                    no_tool_nudge_sent = true;
                    force_tool_choice = true;
                    agent.messages.lock().push(Message::user(
                        "[system] You answered without using any tools. Claims about \
                         the codebase must be verified with tools before answering. \
                         Gather evidence first (Read, Grep, Glob, Bash, ...), then \
                         give your final answer grounded in what the tools returned."
                    ));
                    let _ = event_tx
                        .send(AgentEvent::Status(
                            "Nudging agent to use tools before answering".into(),
                        ))
                        .await;
                    continue; // Don't break — force another round
                }

                // Depth nudge: if we had tool calls but ended very early (turn <= 3),
                // push the model to explore deeper before giving final answer.
                // This prevents shallow 1-round analysis. Only nudge once.
                if had_tool_use && turn <= 4 && !depth_nudge_sent {
                    depth_nudge_sent = true;
                    agent.messages.lock().push(Message::user(
                        "[system] Your analysis is not deep enough yet. You MUST read actual source code files before writing a summary. Use Read to examine at least 8-10 source files (stores, components, commands, types, configs). Use parallel Read calls. Do NOT write the final output until you have read enough source files to provide specific details about implementations, not just file names."
                    ));
                    continue; // Don't break — force another round
                }
                break;
            }
            StopReason::ToolUse => {
                max_tokens_retries = 0;
                had_tool_use = true;
                // Process tool calls
                let tool_use_blocks: Vec<(String, String, serde_json::Value)> = response
                    .message
                    .content_blocks()
                    .into_iter()
                    .filter_map(|b| {
                        if let ContentBlock::ToolUse { id, name, input } = b {
                            Some((id, name, input))
                        } else {
                            None
                        }
                    })
                    .collect();

                // Phase 1: Emit ToolStart events for all tools
                for (tool_id, tool_name, tool_input) in &tool_use_blocks {
                    let _ = event_tx
                        .send(AgentEvent::ToolStart {
                            name: tool_name.clone(),
                            id: tool_id.clone(),
                            input: tool_input.clone(),
                        })
                        .await;
                    agent.emit(AgentEvent::ToolStart {
                        name: tool_name.clone(),
                        id: tool_id.clone(),
                        input: tool_input.clone(),
                    });
                }

                // Phase 2: Execute all tools in PARALLEL via join_all
                let msg_count = agent.messages.lock().len();
                // Built once, outside the per-call closure: this is the same
                // `agent.tools` the lookup below uses (so MCP-injected tools
                // stay consistent), and building it inside the closure would
                // re-allocate every tool name for every parallel call (F-A15).
                let registered_tool_names: Vec<String> =
                    agent.tools.iter().map(|t| t.name().to_string()).collect();

                // ── Guard: read-before-edit, decided BEFORE dispatch (F-11) ──
                // This used to run over the *returned* ToolResult, which meant
                // the file had already been written by the time the model was
                // told the edit was blocked: disk and conversation disagreed
                // about whether the edit happened.
                let refusals =
                    refusals_for_batch(&tool_use_blocks, &files_read, &tool_ctx.working_dir);

                // ── Stream-deferred permissions, also decided BEFORE dispatch ──
                // Asking inside the parallel batch would interleave prompts
                // from concurrent tool calls, so the questions are put to the
                // caller here, sequentially and in call order. Refused calls
                // are skipped: they never run, so there is nothing to permit.
                let mut stream_decisions: std::collections::HashMap<String, PermissionDecision> =
                    std::collections::HashMap::new();
                if agent.permission_policy.defers_to_stream() {
                    for (tool_id, tool_name, tool_input) in &tool_use_blocks {
                        if refusals.contains_key(tool_id) {
                            continue;
                        }
                        let Some(tool) = agent.tools.iter().find(|t| t.name() == tool_name.as_str())
                        else {
                            continue;
                        };
                        let request =
                            permission_request_for(tool.as_ref(), tool_id, tool_input);
                        agent.emit(AgentEvent::PermissionRequired(request.clone()));
                        match control.ask_stream(&event_tx, &request, &cancel_token).await {
                            PermissionAsk::Decided(decision) => {
                                stream_decisions.insert((*tool_id).clone(), decision);
                            }
                            // Nobody is consuming the stream; the policy's own
                            // `check` decides below, preserving prior behaviour.
                            PermissionAsk::Undeliverable => {
                                tracing::warn!(
                                    tool = %tool_name,
                                    "no stream consumer answered the permission request — \
                                     falling back to the policy's own decision"
                                );
                            }
                            // Bail before dispatch. Falling through would hand
                            // the batch to the policy, and a stream-deferred
                            // policy allows — running the very tool the user
                            // cancelled out of.
                            PermissionAsk::Cancelled => return Err(CerseiError::Cancelled),
                        }
                    }
                }

                let exec_futures: Vec<_> = tool_use_blocks
                    .iter()
                    .map(|(tool_id, tool_name, tool_input)| {
                        let tool_name = tool_name.clone();
                        let registered_tool_names = registered_tool_names.clone();
                        let refusal = refusals.get(tool_id).cloned();
                        let stream_decision = stream_decisions.remove(tool_id);
                        let tool_id = tool_id.clone();
                        let tool_input = tool_input.clone();
                        let tool_ctx = tool_ctx.clone();
                        let permission_policy = Arc::clone(&agent.permission_policy);
                        let hooks = agent.hooks.clone();
                        let cumulative_cost = cumulative.cost_usd.unwrap_or(0.0);

                        // Find tool reference by name
                        let tool_idx = agent.tools.iter().position(|t| t.name() == tool_name);

                        async move {
                            let start = Instant::now();

                            let result = if let Some(msg) = refusal {
                                // Refused before dispatch: the tool never runs,
                                // so nothing reaches disk.
                                ToolResult::error(msg)
                            } else if let Some(idx) = tool_idx {
                                let tool = &agent.tools[idx];
                                // Check permissions
                                let perm_req =
                                    permission_request_for(tool.as_ref(), &tool_id, &tool_input);

                                // A decision the caller already gave over the
                                // stream wins; otherwise ask the policy.
                                let decision = match stream_decision {
                                    Some(d) => d,
                                    None => permission_policy.check(&perm_req).await,
                                };

                                match decision {
                                    PermissionDecision::Allow
                                    | PermissionDecision::AllowOnce
                                    | PermissionDecision::AllowForSession => {
                                        let hook_ctx = HookContext {
                                            event: HookEvent::PreToolUse,
                                            tool_name: Some(tool_name.clone()),
                                            tool_input: Some(tool_input.clone()),
                                            tool_result: None,
                                            tool_is_error: None,
                                            turn,
                                            cumulative_cost_usd: cumulative_cost,
                                            message_count: msg_count,
                                        };
                                        let hook_action =
                                            cersei_hooks::run_hooks(&hooks, &hook_ctx).await;

                                        match hook_action {
                                            HookAction::Block(reason) => ToolResult::error(
                                                format!("Blocked by hook: {}", reason),
                                            ),
                                            HookAction::ModifyInput(new_input) => {
                                                tool.execute(new_input, &tool_ctx).await
                                            }
                                            _ => tool.execute(tool_input.clone(), &tool_ctx).await,
                                        }
                                    }
                                    PermissionDecision::Deny(reason) => {
                                        ToolResult::error(format!("Permission denied: {}", reason))
                                    }
                                }
                            } else {
                                // F-A15: weak models hallucinate tool names
                                // constantly, and "Unknown tool: X" gave them
                                // nothing to correct toward.
                                cersei_tools::tool_feedback::not_found(
                                    "tool",
                                    &tool_name,
                                    &registered_tool_names,
                                    "Call ToolSearch with a keyword to find the right tool, then call that tool by its exact name.",
                                )
                            };

                            let duration = start.elapsed();
                            (tool_id, tool_name, tool_input, result, duration)
                        }
                    })
                    .collect();

                let results = futures::future::join_all(exec_futures).await;

                // Phase 3: Process results sequentially (emit events, build result blocks)
                let mut result_blocks: Vec<ContentBlock> = Vec::new();

                for (tool_id, tool_name, tool_input, mut result, duration) in results {
                    // ── Bookkeeping for the read-before-edit guard ──
                    // The refusal itself now happens before dispatch; see
                    // `refusals_for_batch`. What remains here is recording the
                    // files whose contents the model can be said to know,
                    // which needs the result to confirm the call succeeded.
                    //
                    // A successful *write* counts as much as a read: the model
                    // supplied that content, so it is not overwriting anything
                    // unseen. Recording only reads meant a file the model had
                    // just created with `Write` could never be written again —
                    // it existed on disk, was absent from this set, and every
                    // later `Write`/`Edit` was refused as a blind overwrite of
                    // content the model itself had authored.
                    if !result.is_error {
                        for target in write_targets(&tool_name, &tool_input) {
                            files_read.insert(resolve_path(&tool_ctx.working_dir, &target));
                        }
                    }
                    if (tool_name == "Read" || tool_name == "read") && !result.is_error {
                        if let Some(path) = tool_input.get("file_path").and_then(|v| v.as_str()) {
                            files_read.insert(resolve_path(&tool_ctx.working_dir, path));
                        }
                    }
                    // The write-side guard now runs before dispatch; see
                    // `refusals_for_batch`. Only the bookkeeping remains here,
                    // because it needs the result to know the Read succeeded.

                    // ── Guard: Per-tool error counter with reflection (F-06) ──
                    if result.is_error {
                        let count = tool_error_counts.entry(tool_name.clone()).or_insert(0);
                        *count += 1;
                        result.content =
                            format!("{}\n\n{}", result.content, error_budget_note(&tool_name, *count));
                    } else {
                        tool_error_counts.remove(&tool_name);
                    }

                    // Compress before emitting ToolEnd so the savings stats ride
                    // along on the event (error results are not compressed).
                    let (capped_content, compression) = if result.is_error {
                        // F-07: errors skip *compression* (a stack trace does
                        // not summarise well) but must still be capped. They
                        // were previously exempt from both, so an unbounded
                        // failure body — a compiler dump, a full stack trace —
                        // entered history whole and was re-sent on every
                        // subsequent turn of the conversation.
                        (cap_tool_result(&result.content), None)
                    } else {
                        let level = *agent.compression_level.lock();
                        let (compressed, stats) =
                            cersei_compression::compress_tool_output_with_stats(
                                &tool_name,
                                &tool_input,
                                &result.content,
                                level,
                            );
                        (cap_tool_result(&compressed), Some(stats))
                    };

                    let _ = event_tx
                        .send(AgentEvent::ToolEnd {
                            name: tool_name.clone(),
                            id: tool_id.clone(),
                            result: result.content.clone(),
                            is_error: result.is_error,
                            duration,
                            compression,
                        })
                        .await;
                    agent.emit(AgentEvent::ToolEnd {
                        name: tool_name.clone(),
                        id: tool_id.clone(),
                        result: result.content.clone(),
                        is_error: result.is_error,
                        duration,
                        compression,
                    });

                    tool_calls.push(ToolCallRecord {
                        name: tool_name,
                        id: tool_id.clone(),
                        input: tool_input,
                        result: result.content.clone(),
                        is_error: result.is_error,
                        duration,
                    });
                    result_blocks.push(ContentBlock::ToolResult {
                        tool_use_id: tool_id,
                        content: ToolResultContent::Text(capped_content),
                        is_error: Some(result.is_error),
                    });
                }

                // Add tool results as user message
                agent
                    .messages
                    .lock()
                    .push(Message::user_blocks(result_blocks));

                // ── Doom loop detection ──
                // Detects two patterns:
                // 1. 3+ consecutive identical tool calls that all error
                // 2. Repeating 2-call pattern [A,B][A,B][A,B] (alternating failures)
                if !doom_loop_warned && tool_calls.len() >= 6 {
                    let names: Vec<&str> = tool_calls
                        .iter()
                        .rev()
                        .take(6)
                        .map(|tc| tc.name.as_str())
                        .collect();
                    let errors: Vec<bool> = tool_calls
                        .iter()
                        .rev()
                        .take(6)
                        .map(|tc| tc.is_error)
                        .collect();

                    // Pattern 1: 3+ identical consecutive failing calls
                    let is_3_identical = names.len() >= 3
                        && names[0] == names[1]
                        && names[1] == names[2]
                        && errors[0]
                        && errors[1]
                        && errors[2];

                    // Pattern 2: [A,B][A,B][A,B] alternating pattern
                    let is_2_pattern = names.len() >= 6
                        && names[0] == names[2]
                        && names[2] == names[4]
                        && names[1] == names[3]
                        && names[3] == names[5];

                    if is_3_identical || is_2_pattern {
                        doom_loop_warned = true;
                        agent.messages.lock().push(Message::user(
                            "[system] You are stuck in a repetitive loop. Your recent tool calls \
                             are repeating the same pattern. STOP and reconsider:\n\
                             1. What exactly is going wrong? Read the error messages carefully.\n\
                             2. Is there a COMPLETELY different approach to this problem?\n\
                             3. Try a different tool, different arguments, or a different algorithm.\n\
                             Do NOT repeat the same commands."
                        ));
                        let _ = event_tx
                            .send(AgentEvent::Status(
                                "Doom loop detected — forcing new approach".into(),
                            ))
                            .await;
                    }
                }
            }
            StopReason::MaxTokens => {
                max_tokens_retries += 1;
                if max_tokens_retries > MAX_TOKENS_RETRY_LIMIT {
                    break; // Give up after 3 retries
                }
                agent
                    .messages
                    .lock()
                    .push(Message::user("Continue from exactly where you stopped."));
            }
            _ => break,
        }

        // Auto-compact: check context utilization after each turn
        if agent.auto_compact {
            let model_name = agent.model.as_deref().unwrap_or("claude-sonnet-4-6");
            let tokens_used = compact::estimate_messages_tokens(&agent.messages.lock());
            let context_window = compact::context_window_for_model(model_name);
            let pct = if context_window > 0 {
                tokens_used as f64 / context_window as f64
            } else {
                0.0
            };

            // Emit token warnings
            if pct >= compact::WARNING_PCT {
                use crate::events::WarningState;
                let state = if pct >= compact::CRITICAL_PCT {
                    WarningState::Critical
                } else {
                    WarningState::Warning
                };
                let _ = event_tx
                    .send(AgentEvent::TokenWarning {
                        pct_used: pct,
                        state,
                    })
                    .await;
                agent.emit(AgentEvent::TokenWarning {
                    pct_used: pct,
                    state,
                });
            }

            // Auto-compact at 90%: try LLM summarization, fall back to snip
            if compact::should_compact(tokens_used, context_window) {
                let msgs_snapshot = agent.messages.lock().clone();
                let model_name_owned = model_name.to_string();

                // Try LLM-based summarization first
                match compact::compact_conversation(
                    agent.provider.as_ref(),
                    &msgs_snapshot,
                    &model_name_owned,
                    compact::KEEP_RECENT_MESSAGES,
                    None,
                )
                .await
                {
                    Ok(result) if !result.summary.is_empty() => {
                        let mut msgs = agent.messages.lock();
                        let before = msgs.len();
                        // F-04: not `len - KEEP_RECENT_MESSAGES`. That lands on
                        // a `user[tool_result]` for every even-length history
                        // and discards the `tool_use` answering it, which the
                        // provider rejects with a 400 — an error the retry loop
                        // does not match, so the conversation wedges exactly
                        // when the context was full enough to need compacting.
                        let split_idx =
                            compact::pair_aware_split(&msgs, compact::KEEP_RECENT_MESSAGES);
                        let recent = msgs[split_idx..].to_vec();
                        *msgs = vec![Message::user(&result.summary)];
                        msgs.extend(recent);
                        tracing::info!(
                            "LLM compact: {before} → {} messages, freed ~{} tokens",
                            msgs.len(),
                            result.tokens_freed_estimate
                        );
                    }
                    _ => {
                        // Fallback: snip-compact (truncation)
                        let mut msgs = agent.messages.lock();
                        let before = msgs.len();
                        let (compacted, freed) = compact::snip_compact(
                            std::mem::take(&mut *msgs),
                            compact::KEEP_RECENT_MESSAGES,
                        );
                        *msgs = compacted;
                        tracing::info!(
                            "Snip compact (fallback): {before} → {} messages, freed ~{freed} tokens",
                            msgs.len()
                        );
                    }
                }
            }
        }
    }

    // Persist session
    if let (Some(memory), Some(session_id)) = (&agent.memory, &agent.session_id) {
        let messages = agent.messages.lock().clone();
        memory.store(session_id, &messages).await?;
        let _ = event_tx
            .send(AgentEvent::SessionSaved {
                session_id: session_id.clone(),
            })
            .await;
        agent.emit(AgentEvent::SessionSaved {
            session_id: session_id.clone(),
        });
    }

    // Build output
    let last_message = agent
        .messages
        .lock()
        .iter()
        .rev()
        .find(|m| m.role == Role::Assistant)
        .cloned()
        .unwrap_or_else(|| Message::assistant(""));

    let output = AgentOutput {
        message: last_message,
        usage: agent.cumulative_usage.lock().clone(),
        stop_reason: last_stop_reason,
        turns: turn,
        tool_calls,
    };

    // Notify reporters
    for reporter in &agent.reporters {
        reporter.on_complete(&output).await;
    }

    Ok(output)
}

// ─── Benchmark self-verification helpers ────────────────────────────────────

#[derive(Debug)]
enum BenchmarkVerification {
    TestsNotRun,
    TestsFailed(String), // carries the test output for retry feedback
    TestsPassed,
}

/// Analyze tool call history to determine if tests were run and whether they passed.
fn benchmark_check_tests(tool_calls: &[ToolCallRecord]) -> BenchmarkVerification {
    let test_patterns = [
        "run-tests",
        "run_tests",
        "pytest",
        "python -m pytest",
        "bash run-tests.sh",
        "npm test",
        "cargo test",
        "go test",
        "make test",
        "jest",
        "mocha",
        "unittest",
    ];

    let mut found_test_run = false;
    let mut last_test_failed = false;
    let mut last_test_output = String::new();

    // Check the most recent tool calls (last 30) for test execution
    for tc in tool_calls.iter().rev().take(30) {
        if tc.name != "Bash" && tc.name != "bash" {
            continue;
        }

        let cmd = tc
            .input
            .get("command")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        let is_test_cmd = test_patterns.iter().any(|p| cmd.contains(p));
        if !is_test_cmd {
            continue;
        }

        found_test_run = true;
        last_test_output = tc.result.clone();

        // Primary signal: exit code (most reliable)
        if tc.is_error {
            last_test_failed = true;
            break;
        }

        // Secondary: parse output for pass/fail indicators
        let result_lower = tc.result.to_lowercase();

        let has_pass = result_lower.contains("passed")
            || result_lower.contains("success")
            || result_lower.contains("all tests")
            || result_lower.contains("exit code 0")
            || tc.result.contains("PASSED")
            || tc.result.contains("PASS")
            || (result_lower.contains(" ok") && !result_lower.contains("not ok"));

        let has_failure = result_lower.contains("failed")
            || result_lower.contains("failure")
            || result_lower.contains("traceback")
            || result_lower.contains("not ok")
            || result_lower.contains("assertion")
            || (result_lower.contains("error")
                && !result_lower.contains("error handling")
                && !result_lower.contains("error_"));

        if has_failure && !has_pass {
            last_test_failed = true;
        } else {
            last_test_failed = false;
        }
        break; // Only care about the most recent test run
    }

    if !found_test_run {
        BenchmarkVerification::TestsNotRun
    } else if last_test_failed {
        BenchmarkVerification::TestsFailed(last_test_output)
    } else {
        BenchmarkVerification::TestsPassed
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod guard_tests {
    use super::*;
    use serde_json::json;
    use std::collections::HashSet;

    /// The seen-file set, built the way the runner builds it: every path
    /// normalised through `resolve_path`, never stored raw.
    fn seen(wd: &std::path::Path, paths: &[&str]) -> HashSet<String> {
        paths.iter().map(|p| resolve_path(wd, p)).collect()
    }

    /// The core of F-11: the refusal has to be decidable *before* dispatch.
    ///
    /// The old guard ran over the returned `ToolResult`, so by the time it
    /// replaced the content with "you must Read first" the write had already
    /// landed. Deciding from (name, input, files_read) alone is what makes it
    /// possible to refuse without running the tool.
    #[test]
    fn existing_but_unread_file_is_refused_for_every_writing_tool() {
        let tmp = tempfile::tempdir().unwrap();
        let f = tmp.path().join("a.rs");
        std::fs::write(&f, "existing\n").unwrap();
        let p = f.to_str().unwrap();
        let none = seen(tmp.path(), &[]);

        for tool in ["Edit", "Write", "MultiEdit", "NotebookEdit"] {
            let block = read_before_edit_block(tool, &json!({ "file_path": p }), &none, tmp.path());
            assert!(
                block.is_some(),
                "{tool} may not overwrite an unread file that already exists"
            );
            let msg = block.unwrap();
            assert!(msg.contains(p), "{tool}: message must name the file: {msg}");
            assert!(
                msg.contains("Read"),
                "{tool}: message must say what to do: {msg}"
            );
        }
    }

    #[test]
    fn reading_the_file_first_lifts_the_refusal() {
        let tmp = tempfile::tempdir().unwrap();
        let f = tmp.path().join("a.rs");
        std::fs::write(&f, "existing\n").unwrap();
        let p = f.to_str().unwrap();

        for tool in ["Edit", "Write", "MultiEdit", "NotebookEdit"] {
            assert!(
                read_before_edit_block(tool, &json!({ "file_path": p }), &seen(tmp.path(), &[p]), tmp.path())
                    .is_none(),
                "{tool} must run once the file has been read"
            );
        }
    }

    /// Creating a file is not overwriting one. Requiring a Read of something
    /// that does not exist would be unsatisfiable.
    #[test]
    fn creating_a_new_file_needs_no_prior_read() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("brand_new.rs");
        assert!(read_before_edit_block(
            "Write",
            &json!({ "file_path": p.to_str().unwrap() }),
            &seen(tmp.path(), &[]),
            tmp.path()
        )
        .is_none());
    }

    #[test]
    fn read_only_tools_are_never_blocked() {
        let tmp = tempfile::tempdir().unwrap();
        let f = tmp.path().join("a.rs");
        std::fs::write(&f, "x\n").unwrap();
        for tool in ["Read", "Grep", "Glob", "Bash", "CodeSearch"] {
            assert!(read_before_edit_block(
                tool,
                &json!({ "file_path": f.to_str().unwrap() }),
                &seen(tmp.path(), &[]),
                tmp.path()
            )
            .is_none());
        }
    }

    /// ApplyPatch hides its targets in the patch body, and names them relative
    /// to the working directory while Read is given absolute paths. Both
    /// spellings must resolve to the same file, or every patch following a read
    /// would be refused.
    #[test]
    fn apply_patch_targets_come_from_the_patch_body() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("a.rs"), "x\n").unwrap();
        let patch = json!({
            "patch": "--- a/a.rs\n+++ a.rs\n@@ -1 +1 @@\n-x\n+y\n"
        });

        assert_eq!(write_targets("ApplyPatch", &patch), vec!["a.rs".to_string()]);
        assert!(
            read_before_edit_block("ApplyPatch", &patch, &seen(tmp.path(), &[]), tmp.path()).is_some(),
            "an unread patched file must be refused"
        );
        let abs = tmp.path().join("a.rs").to_string_lossy().to_string();
        assert!(
            read_before_edit_block("ApplyPatch", &patch, &seen(tmp.path(), &[&abs]), tmp.path()).is_none(),
            "reading the absolute path must satisfy a relative patch target"
        );
    }

    /// A `Read` and an `Edit` of the same file in ONE parallel batch: the edit
    /// is still refused.
    ///
    /// Nothing orders the two calls — they are dispatched together via
    /// `join_all` — so the read has not completed when the edit would run.
    /// Letting it through because a read "is in flight" would reintroduce
    /// exactly the blind overwrite the guard exists to prevent.
    #[test]
    fn a_read_in_the_same_batch_does_not_unlock_the_edit() {
        let tmp = tempfile::tempdir().unwrap();
        let f = tmp.path().join("a.rs");
        std::fs::write(&f, "existing\n").unwrap();
        let p = f.to_str().unwrap().to_string();

        let batch = vec![
            ("id_read".to_string(), "Read".to_string(), json!({ "file_path": p })),
            ("id_edit".to_string(), "Edit".to_string(), json!({ "file_path": p })),
        ];

        let refusals = refusals_for_batch(&batch, &seen(tmp.path(), &[]), tmp.path());
        assert!(
            refusals.contains_key("id_edit"),
            "the edit must be refused: a concurrent read has not landed yet"
        );
        assert!(
            !refusals.contains_key("id_read"),
            "reads are never refused"
        );
    }

    /// Refusals are keyed by tool_use id, so one bad call in a batch cannot
    /// suppress its siblings.
    #[test]
    fn refusal_is_per_call_not_per_batch() {
        let tmp = tempfile::tempdir().unwrap();
        let known = tmp.path().join("known.rs");
        let unknown = tmp.path().join("unknown.rs");
        std::fs::write(&known, "a\n").unwrap();
        std::fs::write(&unknown, "b\n").unwrap();
        let known_p = known.to_str().unwrap().to_string();

        let batch = vec![
            ("ok".to_string(), "Edit".to_string(), json!({ "file_path": known_p })),
            ("bad".to_string(), "Edit".to_string(), json!({ "file_path": unknown.to_str().unwrap() })),
        ];

        let refusals = refusals_for_batch(&batch, &seen(tmp.path(), &[&known_p]), tmp.path());
        assert_eq!(refusals.len(), 1, "only the unread target may be refused");
        assert!(refusals.contains_key("bad"));
    }

    /// The guard must resolve a patch target to the SAME path `apply_patch.rs`
    /// will write, or it silently fails open.
    ///
    /// `apply_patch.rs` strips a tab-separated timestamp and a git-style `b/`
    /// prefix before joining against the working directory. A guard that
    /// skipped those normalisations looked up `<wd>/b/a.rs`, found nothing,
    /// concluded "new file, no read required", and waved through an overwrite
    /// of an unread `<wd>/a.rs`.
    #[test]
    fn patch_targets_are_normalised_the_same_way_apply_patch_normalises_them() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("a.rs"), "x\n").unwrap();

        for header in [
            "+++ b/a.rs",                      // git-style prefix
            "+++ a.rs\t2024-01-01 00:00:00",   // trailing timestamp
            "+++ b/a.rs\t2024-01-01 00:00:00", // both
        ] {
            let patch = json!({ "patch": format!("--- a/a.rs\n{header}\n@@ -1 +1 @@\n-x\n+y\n") });
            assert_eq!(
                write_targets("ApplyPatch", &patch),
                vec!["a.rs".to_string()],
                "header {header:?} must resolve to the path apply_patch writes"
            );
            assert!(
                read_before_edit_block("ApplyPatch", &patch, &seen(tmp.path(), &[]), tmp.path()).is_some(),
                "header {header:?}: guard failed open on an unread file"
            );
        }
    }

    /// A file the model just wrote is a file the model knows.
    ///
    /// Recording only reads meant `Write` could create a file and then never
    /// touch it again: it existed on disk, was absent from the seen set, and
    /// every later write was refused as a blind overwrite of content the model
    /// had authored itself one turn earlier.
    #[test]
    fn a_successful_write_counts_as_having_seen_the_file() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("new.rs");
        let ps = p.to_str().unwrap().to_string();

        // Turn 1: creating it is allowed.
        assert!(
            read_before_edit_block("Write", &json!({ "file_path": ps }), &seen(tmp.path(), &[]), tmp.path())
                .is_none()
        );
        std::fs::write(&p, "v1\n").unwrap();

        // The runner records write targets the same way it records reads.
        let mut have = seen(tmp.path(), &[]);
        for t in write_targets("Write", &json!({ "file_path": ps })) {
            have.insert(resolve_path(tmp.path(), &t));
        }

        // Turn 2: revising it must not be refused.
        for tool in ["Write", "Edit", "MultiEdit"] {
            assert!(
                read_before_edit_block(tool, &json!({ "file_path": ps }), &have, tmp.path())
                    .is_none(),
                "{tool} refused a file this session created"
            );
        }
    }

    /// The seen-set is keyed by resolved path, so the spelling used for the
    /// read need not match the spelling used for the write.
    #[test]
    fn path_spelling_does_not_decide_whether_a_file_counts_as_read() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("m.rs"), "x\n").unwrap();
        let abs = tmp.path().join("m.rs").to_string_lossy().to_string();

        // Read relative, write absolute.
        let have = seen(tmp.path(), &["m.rs"]);
        assert!(
            read_before_edit_block("Edit", &json!({ "file_path": abs }), &have, tmp.path())
                .is_none(),
            "a relative Read must satisfy an absolute Edit of the same file"
        );

        // Read absolute, write relative — and via a redundant './'.
        let have = seen(tmp.path(), &[&abs]);
        for spelling in ["m.rs", "./m.rs"] {
            assert!(
                read_before_edit_block(
                    "Edit",
                    &json!({ "file_path": spelling }),
                    &have,
                    tmp.path()
                )
                .is_none(),
                "an absolute Read must satisfy {spelling:?}"
            );
        }
    }

    /// F-06: the advice may not promise an intervention the runtime does not
    /// perform. Nothing blocks a tool on repeated failure, so nothing may say
    /// it will.
    #[test]
    fn repeated_failure_advice_never_claims_a_limit_it_cannot_enforce() {
        for count in 1..=(MAX_TOOL_ERRORS_PER_TOOL + 4) {
            let note = error_budget_note("Bash", count);
            assert!(note.contains(&count.to_string()), "{note}");
            assert!(
                !note.contains("remaining") && !note.contains("left"),
                "counting down to a limit that never binds: {note}"
            );
            assert!(
                !note.contains("attempts remaining"),
                "the removed claim came back: {note}"
            );
        }
        // It does get blunter once the streak is long.
        assert!(error_budget_note("Bash", MAX_TOOL_ERRORS_PER_TOOL).contains("different tool"));
    }

    /// F-07: error results bypassed `cap_tool_result`, so an unbounded failure
    /// body (a compiler dump, a stack trace) landed in history in full and was
    /// re-sent on every subsequent turn.
    #[test]
    fn oversized_error_results_are_capped() {
        let huge = (0..5_000)
            .map(|i| format!("error line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let capped = cap_tool_result(&huge);
        assert!(
            capped.len() < huge.len(),
            "a 5000-line failure must not enter history whole"
        );
        assert!(capped.contains("lines omitted"), "{capped}");
    }
}
