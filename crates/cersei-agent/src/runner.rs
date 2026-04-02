//! Agent runner: the core agentic loop.

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

// ─── Tool result budget ──────────────────────────────────────────────────────

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
    // Load session history
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

    // Add user prompt
    agent.messages.lock().push(Message::user(prompt));

    let mut tool_calls: Vec<ToolCallRecord> = Vec::new();
    let mut turn: u32 = 0;
    let mut last_stop_reason = StopReason::EndTurn;
    let mut _last_usage = Usage::default();

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

        // Build completion request
        let messages = agent.messages.lock().clone();
        let tool_defs: Vec<ToolDefinition> = agent.tools.iter().map(|t| t.to_definition()).collect();

        let model = agent
            .model
            .clone()
            .unwrap_or_else(|| "claude-sonnet-4-6".to_string());

        let mut options = ProviderOptions::default();
        if let Some(budget) = agent.thinking_budget {
            options.set("thinking_budget", budget);
        }

        let request = CompletionRequest {
            model: model.clone(),
            messages: messages.clone(),
            system: agent.system_prompt.clone(),
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

        // Send to provider
        let stream = agent.provider.complete(request).await?;
        let mut rx = stream.into_receiver();
        let mut accumulator = StreamAccumulator::new();

        let _ = event_tx
            .send(AgentEvent::ModelResponseStart {
                turn,
                model: model.clone(),
            })
            .await;

        // Process stream events
        while let Some(event) = rx.recv().await {
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

        // Convert accumulated response
        let response = accumulator.into_response()?;
        last_stop_reason = response.stop_reason.clone();
        _last_usage = response.usage.clone();

        // Update cumulative usage
        agent.cumulative_usage.lock().merge(&response.usage);
        agent.cost_tracker.add(&response.usage);

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
            return Err(CerseiError::Provider(format!("Blocked by hook: {}", reason)));
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
            StopReason::EndTurn => break,
            StopReason::ToolUse => {
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

                let mut result_blocks: Vec<ContentBlock> = Vec::new();

                for (tool_id, tool_name, tool_input) in tool_use_blocks {
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

                    let start = Instant::now();

                    // Find the tool
                    let tool = agent.tools.iter().find(|t| t.name() == tool_name);

                    let result = if let Some(tool) = tool {
                        // Check permissions
                        let perm_req = PermissionRequest {
                            tool_name: tool_name.clone(),
                            tool_input: tool_input.clone(),
                            permission_level: tool.permission_level(),
                            description: format!("Execute tool '{}'", tool_name),
                            id: tool_id.clone(),
                        };

                        let decision = agent.permission_policy.check(&perm_req).await;

                        match decision {
                            PermissionDecision::Allow
                            | PermissionDecision::AllowOnce
                            | PermissionDecision::AllowForSession => {
                                // Fire PreToolUse hooks
                                let hook_ctx = HookContext {
                                    event: HookEvent::PreToolUse,
                                    tool_name: Some(tool_name.clone()),
                                    tool_input: Some(tool_input.clone()),
                                    tool_result: None,
                                    tool_is_error: None,
                                    turn,
                                    cumulative_cost_usd: cumulative.cost_usd.unwrap_or(0.0),
                                    message_count: agent.messages.lock().len(),
                                };
                                let hook_action =
                                    cersei_hooks::run_hooks(&agent.hooks, &hook_ctx).await;

                                match hook_action {
                                    HookAction::Block(reason) => {
                                        ToolResult::error(format!("Blocked by hook: {}", reason))
                                    }
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

                    tool_calls.push(ToolCallRecord {
                        name: tool_name.clone(),
                        id: tool_id.clone(),
                        input: tool_input,
                        result: result.content.clone(),
                        is_error: result.is_error,
                        duration,
                    });

                    result_blocks.push(ContentBlock::ToolResult {
                        tool_use_id: tool_id,
                        content: ToolResultContent::Text(result.content),
                        is_error: Some(result.is_error),
                    });
                }

                // Add tool results as user message
                agent
                    .messages
                    .lock()
                    .push(Message::user_blocks(result_blocks));
            }
            StopReason::MaxTokens => {
                // Inject continuation message
                agent
                    .messages
                    .lock()
                    .push(Message::user("Continue from where you left off."));
            }
            _ => break,
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
