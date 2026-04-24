//! Agent runner: the core agentic loop.

use crate::compact;
use crate::events::{AgentControl, AgentEvent};
use crate::{Agent, AgentOutput, ToolCallRecord};
use cersei_hooks::{HookAction, HookContext, HookEvent};
use cersei_provider::{CompletionRequest, ProviderOptions, StreamAccumulator};
use cersei_tools::permissions::{PermissionDecision, PermissionRequest};
use cersei_tools::{ToolContext, ToolResult};
use cersei_types::*;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::mpsc;

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
        let head_chars = MAX_SINGLE_RESULT_CHARS * 70 / 100;
        let tail_chars = MAX_SINGLE_RESULT_CHARS * 20 / 100;
        let omitted = content.len().saturating_sub(head_chars + tail_chars);
        return format!(
            "{}\n\n[... {omitted} chars omitted ...]\n\n{}",
            &content[..head_chars],
            &content[content.len().saturating_sub(tail_chars)..]
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

/// Run the agent without streaming (blocking until complete).
pub async fn run_agent(agent: &Agent, prompt: &str) -> Result<AgentOutput> {
    let (event_tx, _event_rx) = mpsc::channel(512);
    let (_control_tx, control_rx) = mpsc::channel(64);

    let prompt = prompt.to_string();

    // Run in a background task and collect events
    let result = run_agent_streaming(agent, &prompt, event_tx, control_rx).await;

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

/// Core agentic loop with streaming events.
pub async fn run_agent_streaming(
    agent: &Agent,
    prompt: &str,
    event_tx: mpsc::Sender<AgentEvent>,
    _control_rx: mpsc::Receiver<AgentControl>,
) -> Result<AgentOutput> {
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
    let mut benchmark_retries: u32 = 0;
    const BENCHMARK_MAX_RETRIES: u32 = 4;
    let mut doom_loop_warned = false;
    let mut completion_verified = false;

    // Runtime guards
    let mut files_read: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut tool_error_counts: std::collections::HashMap<String, u32> =
        std::collections::HashMap::new();
    const MAX_TOOL_ERRORS_PER_TOOL: u32 = 3;

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
        extensions: cersei_tools::Extensions::default(),
    };

    // Agentic loop
    loop {
        turn += 1;
        if turn > agent.max_turns {
            break;
        }

        // Check cancellation
        if agent.cancel_token.is_cancelled() {
            return Err(CerseiError::Cancelled);
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
            match agent.provider.complete(req_clone).await {
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
                    tokio::time::sleep(std::time::Duration::from_millis(actual_delay)).await;
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
                _ = agent.cancel_token.cancelled() => {
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
                let exec_futures: Vec<_> = tool_use_blocks
                    .iter()
                    .map(|(tool_id, tool_name, tool_input)| {
                        let tool_name = tool_name.clone();
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

                            let result = if let Some(idx) = tool_idx {
                                let tool = &agent.tools[idx];
                                // Check permissions
                                let perm_req = PermissionRequest {
                                    tool_name: tool_name.clone(),
                                    tool_input: tool_input.clone(),
                                    permission_level: tool.permission_level(),
                                    description: format!("Execute tool '{}'", tool_name),
                                    id: tool_id.clone(),
                                };

                                let decision = permission_policy.check(&perm_req).await;

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
                                ToolResult::error(format!("Unknown tool: {}", tool_name))
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
                    // ── Guard: Read-before-edit ──
                    // Track files that have been read; block edits to unread files
                    if (tool_name == "Read" || tool_name == "read") && !result.is_error {
                        if let Some(path) = tool_input.get("file_path").and_then(|v| v.as_str()) {
                            files_read.insert(path.to_string());
                        }
                    }
                    if (tool_name == "Edit" || tool_name == "edit") && !result.is_error {
                        if let Some(path) = tool_input.get("file_path").and_then(|v| v.as_str()) {
                            if !files_read.contains(path) {
                                // Check if file exists — new files don't need prior read
                                let file_exists = std::path::Path::new(path).exists()
                                    || tool_ctx.working_dir.join(path).exists();
                                if file_exists {
                                    result = ToolResult::error(
                                        format!("You must Read '{}' before editing it. Read the file first to understand its current contents.", path)
                                    );
                                }
                            }
                        }
                    }

                    // ── Guard: Per-tool error counter with reflection ──
                    if result.is_error {
                        let count = tool_error_counts.entry(tool_name.clone()).or_insert(0);
                        *count += 1;
                        let remaining = MAX_TOOL_ERRORS_PER_TOOL.saturating_sub(*count);
                        result.content = format!(
                            "{}\n\n[Tool '{}' failed {} time(s). {} attempts remaining. Analyze the error and try a different approach.]",
                            result.content, tool_name, count, remaining
                        );
                    } else {
                        tool_error_counts.remove(&tool_name);
                    }

                    let _ = event_tx
                        .send(AgentEvent::ToolEnd {
                            name: tool_name.clone(),
                            id: tool_id.clone(),
                            result: result.content.clone(),
                            is_error: result.is_error,
                            duration,
                        })
                        .await;
                    agent.emit(AgentEvent::ToolEnd {
                        name: tool_name.clone(),
                        id: tool_id.clone(),
                        result: result.content.clone(),
                        is_error: result.is_error,
                        duration,
                    });

                    let capped_content = if result.is_error {
                        result.content.clone()
                    } else {
                        let level = *agent.compression_level.lock();
                        let compressed = cersei_compression::compress_tool_output(
                            &tool_name,
                            &tool_input,
                            &result.content,
                            level,
                        );
                        cap_tool_result(&compressed)
                    };

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
                        let split_idx = msgs.len().saturating_sub(compact::KEEP_RECENT_MESSAGES);
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
