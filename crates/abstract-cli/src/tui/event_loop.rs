//! Main TUI event loop — multiplexes agent streaming, terminal input, mouse, and paste.

use crate::config::AppConfig;
use crate::tui::{
    app::{AppState, Overlay, SidePanelTab, ToolCall, ToolStatus},
    layout,
    theme::Theme,
    widgets::{footer, header, input, messages, overlay, side_panel, status},
    Terminal,
};
use cersei::events::{AgentEvent, AgentStream};
use cersei::Agent;
use cersei_memory::manager::MemoryManager;
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyModifiers};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio_util::sync::CancellationToken;

const TICK_RATE: Duration = Duration::from_millis(16); // ~62 FPS

pub async fn run(
    terminal: &mut Terminal,
    agent: Arc<Agent>,
    config: &AppConfig,
    _memory_manager: &MemoryManager,
    session_id: &str,
    cancel_token: CancellationToken,
    shared_mode: crate::permissions::SharedPermissionMode,
    mut permission_rx: tokio::sync::mpsc::Receiver<crate::permissions::TuiPermissionRequest>,
) -> anyhow::Result<()> {
    let theme = Theme::from_name(&config.theme);
    let mut state = AppState::new(&config.model, session_id, &config.effort);
    state.set_shared_mode(shared_mode);
    let mut agent_stream: Option<AgentStream> = None;

    // Initial render
    draw(terminal, &mut state, &theme)?;

    loop {
        if state.should_quit {
            break;
        }

        tokio::select! {
            // ── Permission requests from agent ──────────────────────────
            Some(perm_req) = permission_rx.recv() => {
                state.overlay = Overlay::Permission(crate::tui::app::PermissionOverlay {
                    tool_name: perm_req.tool_name,
                    description: perm_req.description,
                    selected: 0,
                });
                state.pending_permission_tx = Some(perm_req.response_tx);
                state.dirty = true;
            }

            // ── Agent stream events ─────────────────────────────────────
            event = poll_agent_stream(&mut agent_stream) => {
                match event {
                    Some(agent_event) => {
                        handle_agent_event(&mut state, agent_event);
                    }
                    None => {
                        if state.is_streaming {
                            state.commit_turn();
                            state.is_streaming = false;
                        }
                        agent_stream = None;
                    }
                }
                state.dirty = true;
            }

            // ── Terminal events + tick ───────────────────────────────────
            _ = tokio::time::sleep(TICK_RATE) => {
                // Drain pending events (cap at 50 to prevent infinite loop on resize storms)
                let mut event_count = 0u32;
                while event_count < 50 && event::poll(Duration::ZERO)? {
                    event_count += 1;
                    match event::read()? {
                        Event::Key(key) => {
                            if let Some(prompt) = handle_key(&mut state, key, config, &cancel_token) {
                                state.push_user(&prompt);
                                state.is_streaming = true;
                                state.stream_start = Some(Instant::now());
                                state.scroll.scroll_to_bottom();
                                agent_stream = Some(agent.run_stream(&prompt));
                            }
                            state.dirty = true;
                        }
                        Event::Mouse(_) => {
                            // Mouse capture disabled to allow native text selection
                        }
                        Event::Paste(text) => {
                            if !state.is_streaming {
                                state.input.insert_str(state.cursor_pos, &text);
                                state.cursor_pos += text.len();
                                state.dirty = true;
                            }
                        }
                        Event::Resize(_, _) => {
                            // Re-push keyboard enhancement after resize
                            // (some terminals drop the protocol on resize)
                            let _ = crossterm::execute!(
                                std::io::stdout(),
                                crossterm::event::PushKeyboardEnhancementFlags(
                                    crossterm::event::KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
                                )
                            );
                            state.dirty = true;
                        }
                        _ => {}
                    }
                }
                state.frame_count += 1;
            }
        }

        if state.dirty || state.is_streaming {
            draw(terminal, &mut state, &theme)?;
            state.dirty = false;
        }
    }

    Ok(())
}

async fn poll_agent_stream(stream: &mut Option<AgentStream>) -> Option<AgentEvent> {
    match stream {
        Some(ref mut s) => s.next().await,
        None => std::future::pending().await,
    }
}

fn draw(terminal: &mut Terminal, state: &mut AppState, theme: &Theme) -> anyhow::Result<()> {
    // Compute input height with terminal width
    let term_width = terminal.size()?.width;
    let input_h = input::desired_height(&state.input, term_width.saturating_sub(4));

    terminal.draw(|f| {
        let layout = layout::compute(f.area(), input_h, state.side_panel_open);

        header::render(f, layout.main.header, state, theme);
        messages::render(f, layout.main.messages, state, theme);
        status::render(f, layout.main.status, state, theme);
        input::render(f, layout.main.input, state, theme);
        footer::render(f, layout.main.footer, state.is_streaming, state.side_panel_open, state.side_panel_focused, theme);

        // Side panel
        if let Some(panel_area) = layout.side_panel {
            side_panel::render(f, panel_area, state, theme);
        }

        // Overlay on top
        overlay::render(f, state, theme);
    })?;
    Ok(())
}

/// Handle a key event. Returns Some(prompt) if the user submitted input.
fn handle_key(
    state: &mut AppState,
    key: KeyEvent,
    config: &AppConfig,
    cancel_token: &CancellationToken,
) -> Option<String> {
    // Handle overlay-specific keys first
    if state.overlay != Overlay::None {
        handle_overlay_key(state, key);
        return None;
    }

    // ── Side panel focused: j/k scroll, Tab switches tabs, Esc returns focus ──
    if state.side_panel_focused {
        match key.code {
            KeyCode::Char('j') => { state.side_panel_scroll.scroll_down(1); }
            KeyCode::Char('k') => { state.side_panel_scroll.scroll_up(1); }
            KeyCode::Char('d') => { state.side_panel_scroll.page_down(); }
            KeyCode::Char('u') => { state.side_panel_scroll.page_up(); }
            KeyCode::Char('g') => { state.side_panel_scroll.scroll_up(state.side_panel_scroll.content_height); }
            KeyCode::Char('G') => { state.side_panel_scroll.scroll_to_bottom(); }
            KeyCode::Tab => { state.side_panel_tab = state.side_panel_tab.toggle(); }
            KeyCode::Char('r') => { side_panel::refresh_content(state, &config.working_dir); }
            KeyCode::Esc | KeyCode::Char('q') => {
                state.side_panel_focused = false;
            }
            _ => {
                // Global keys still work
                match (key.modifiers, key.code) {
                    (KeyModifiers::CONTROL, KeyCode::Char('b')) => {
                        state.side_panel_open = false;
                        state.side_panel_focused = false;
                    }
                    (KeyModifiers::CONTROL, KeyCode::Char('d')) => { state.should_quit = true; }
                    (KeyModifiers::CONTROL, KeyCode::Char('c')) => {
                        if state.is_streaming {
                            cancel_token.cancel();
                            state.is_streaming = false;
                            state.commit_turn();
                        }
                    }
                    _ => {}
                }
            }
        }
        return None;
    }

    match (key.modifiers, key.code) {
        // Ctrl+D — exit
        (KeyModifiers::CONTROL, KeyCode::Char('d')) => {
            state.should_quit = true;
        }

        // Ctrl+C — cancel or quit
        (KeyModifiers::CONTROL, KeyCode::Char('c')) => {
            if state.is_streaming {
                cancel_token.cancel();
                state.is_streaming = false;
                state.commit_turn();
            } else if state.input.is_empty() {
                state.should_quit = true;
            } else {
                state.input.clear();
                state.cursor_pos = 0;
            }
        }

        // Ctrl+B — toggle side panel (and focus it)
        (KeyModifiers::CONTROL, KeyCode::Char('b')) => {
            if state.side_panel_open && !state.side_panel_focused {
                // Panel open but not focused — focus it
                state.side_panel_focused = true;
            } else if state.side_panel_focused {
                // Focused — close panel
                state.side_panel_open = false;
                state.side_panel_focused = false;
            } else {
                // Closed — open and focus
                state.side_panel_open = true;
                state.side_panel_focused = true;
                side_panel::refresh_content(state, &config.working_dir);
            }
        }

        // Shift+Tab — cycle permission mode
        (KeyModifiers::SHIFT, KeyCode::BackTab) => {
            state.cycle_permission_mode();
        }

        // Tab — switch side panel tabs (when panel open but not focused)
        (_, KeyCode::Tab) if state.side_panel_open => {
            state.side_panel_tab = state.side_panel_tab.toggle();
        }

        // Scroll messages
        (_, KeyCode::PageUp) => {
            state.scroll.page_up();
        }
        (_, KeyCode::PageDown) => {
            state.scroll.page_down();
        }
        (_, KeyCode::Home) => {
            state.scroll.scroll_up(state.scroll.content_height);
        }
        (_, KeyCode::End) => {
            state.scroll.scroll_to_bottom();
        }

        // Alt+Enter / Ctrl+J / Shift+Enter — insert newline
        (KeyModifiers::ALT, KeyCode::Enter) if !state.is_streaming => {
            state.input.insert(state.cursor_pos, '\n');
            state.cursor_pos += 1;
        }
        (KeyModifiers::SHIFT, KeyCode::Enter) if !state.is_streaming => {
            state.input.insert(state.cursor_pos, '\n');
            state.cursor_pos += 1;
        }
        (KeyModifiers::CONTROL, KeyCode::Char('j')) if !state.is_streaming => {
            state.input.insert(state.cursor_pos, '\n');
            state.cursor_pos += 1;
        }

        // Enter — submit input
        (_, KeyCode::Enter) if !state.is_streaming => {
            let input_text = state.input.trim().to_string();
            if input_text.is_empty() {
                return None;
            }

            state.input.clear();
            state.cursor_pos = 0;

            if input_text.starts_with('/') {
                handle_slash_command(state, &input_text, config);
                return None;
            }

            state.input_history.push(input_text.clone());
            state.history_index = None;
            return Some(input_text);
        }

        // Backspace
        (_, KeyCode::Backspace) if !state.is_streaming => {
            if state.cursor_pos > 0 {
                state.cursor_pos -= 1;
                state.input.remove(state.cursor_pos);
            }
        }

        // Delete
        (_, KeyCode::Delete) if !state.is_streaming => {
            if state.cursor_pos < state.input.len() {
                state.input.remove(state.cursor_pos);
            }
        }

        // Left arrow
        (_, KeyCode::Left) if !state.is_streaming => {
            if state.cursor_pos > 0 {
                state.cursor_pos -= 1;
            }
        }

        // Right arrow
        (_, KeyCode::Right) if !state.is_streaming => {
            if state.cursor_pos < state.input.len() {
                state.cursor_pos += 1;
            }
        }

        // Up arrow — scroll if input empty, else history
        (_, KeyCode::Up) if !state.is_streaming => {
            if state.input.is_empty() && state.history_index.is_none() {
                state.scroll.scroll_up(1);
            } else if !state.input_history.is_empty() {
                let idx = state
                    .history_index
                    .map(|i| i.saturating_sub(1))
                    .unwrap_or(state.input_history.len() - 1);
                state.history_index = Some(idx);
                state.input = state.input_history[idx].clone();
                state.cursor_pos = state.input.len();
            }
        }
        (_, KeyCode::Up) if state.is_streaming => {
            state.scroll.scroll_up(1);
        }

        // Down arrow — scroll if input empty, else history
        (_, KeyCode::Down) if !state.is_streaming => {
            if let Some(idx) = state.history_index {
                if idx + 1 < state.input_history.len() {
                    let new_idx = idx + 1;
                    state.history_index = Some(new_idx);
                    state.input = state.input_history[new_idx].clone();
                    state.cursor_pos = state.input.len();
                } else {
                    state.history_index = None;
                    state.input.clear();
                    state.cursor_pos = 0;
                }
            } else if state.input.is_empty() {
                state.scroll.scroll_down(1);
            }
        }
        (_, KeyCode::Down) if state.is_streaming => {
            state.scroll.scroll_down(1);
        }

        // Esc
        (_, KeyCode::Esc) => {
            if state.overlay != Overlay::None {
                state.overlay = Overlay::None;
            }
        }

        // Character input
        (_, KeyCode::Char(c)) if !state.is_streaming => {
            state.input.insert(state.cursor_pos, c);
            state.cursor_pos += 1;
        }

        _ => {}
    }

    None
}

fn handle_agent_event(state: &mut AppState, event: AgentEvent) {
    match event {
        AgentEvent::TextDelta(text) => {
            state.streaming_text.push_str(&text);
        }
        AgentEvent::ThinkingDelta(text) => {
            state.streaming_thinking.push_str(&text);
        }
        AgentEvent::ToolStart { name, id: _, input, .. } => {
            let summary = tool_input_summary(&name, &input);
            state.active_tools.push(ToolCall {
                name,
                input_summary: summary,
                status: ToolStatus::Running,
                output_preview: None,
                started_at: Instant::now(),
                duration_ms: None,
            });
            state.tool_count += 1;
        }
        AgentEvent::ToolEnd { name, id: _, result, is_error, duration } => {
            if let Some(tool) = state.active_tools.iter_mut().rev().find(|t| t.name == name) {
                tool.status = if is_error { ToolStatus::Error } else { ToolStatus::Done };
                tool.duration_ms = Some(duration.as_millis() as u64);
                tool.output_preview = Some(result.chars().take(200).collect());
            }
        }
        AgentEvent::CostUpdate { cumulative_cost, input_tokens, output_tokens, .. } => {
            state.input_tokens = input_tokens;
            state.output_tokens = output_tokens;
            // Use reported cost or estimate from model pricing
            state.cost_usd = if cumulative_cost > 0.0 {
                cumulative_cost
            } else {
                cersei_tools::estimate_cost(&state.model, input_tokens, output_tokens)
            };
        }
        AgentEvent::TurnComplete { usage, .. } => {
            state.input_tokens = usage.input_tokens;
            state.output_tokens = usage.output_tokens;
            state.cost_usd = usage.cost_usd
                .filter(|c| *c > 0.0)
                .unwrap_or_else(|| cersei_tools::estimate_cost(&state.model, usage.input_tokens, usage.output_tokens));
        }
        AgentEvent::TokenWarning { pct_used, .. } => {
            state.context_pct = pct_used;
        }
        AgentEvent::Error(msg) => {
            state.commit_turn();
            state.is_streaming = false;
            state.turns.push(crate::tui::app::Turn {
                role: crate::tui::app::TurnRole::System,
                content: format!("Error: {msg}"),
                tools: Vec::new(),
                thinking: None,
            });
        }
        AgentEvent::Complete(_) => {
            state.commit_turn();
            state.is_streaming = false;
        }
        _ => {}
    }
}

fn handle_overlay_key(state: &mut AppState, key: KeyEvent) {
    use cersei_tools::permissions::PermissionDecision;

    match key.code {
        KeyCode::Esc => {
            // Dismiss overlay — for permissions, send Deny
            if matches!(state.overlay, Overlay::Permission(_)) {
                if let Some(tx) = state.pending_permission_tx.take() {
                    let _ = tx.send(PermissionDecision::Deny("User cancelled".into()));
                }
            }
            state.overlay = Overlay::None;
        }
        KeyCode::Up => match &mut state.overlay {
            Overlay::Permission(p) => { p.selected = p.selected.saturating_sub(1); }
            Overlay::Recovery(r) => { r.selected = r.selected.saturating_sub(1); }
            Overlay::Graph(g) => { g.select_prev(); }
            _ => {}
        },
        KeyCode::Down => match &mut state.overlay {
            Overlay::Permission(p) => { p.selected = (p.selected + 1).min(3); }
            Overlay::Recovery(r) => {
                if !r.options.is_empty() {
                    r.selected = (r.selected + 1).min(r.options.len() - 1);
                }
            }
            Overlay::Graph(g) => { g.select_next(); }
            _ => {}
        },
        KeyCode::Left => {
            if let Overlay::Graph(g) = &mut state.overlay { g.pan_x -= 3; }
        }
        KeyCode::Right => {
            if let Overlay::Graph(g) = &mut state.overlay { g.pan_x += 3; }
        }
        KeyCode::Enter => {
            // For permission overlays, send the selected decision
            if let Overlay::Permission(ref p) = state.overlay {
                if let Some(tx) = state.pending_permission_tx.take() {
                    let decision = match p.selected {
                        0 => PermissionDecision::AllowOnce,
                        1 => PermissionDecision::AllowForSession,
                        2 => PermissionDecision::Allow, // Always
                        _ => PermissionDecision::Deny("User denied".into()),
                    };
                    let _ = tx.send(decision);
                }
            }
            state.overlay = Overlay::None;
        }
        _ => {}
    }
}

fn handle_slash_command(state: &mut AppState, input: &str, config: &AppConfig) {
    let cmd = input.trim_start_matches('/').split_whitespace().next().unwrap_or("");
    match cmd {
        "help" | "h" | "?" => { state.overlay = Overlay::Help; }
        "clear" => {
            state.turns.clear();
            state.streaming_text.clear();
            state.streaming_thinking.clear();
            state.active_tools.clear();
        }
        "exit" | "quit" | "q" => { state.should_quit = true; }
        "panel" => {
            state.side_panel_open = !state.side_panel_open;
            if state.side_panel_open {
                side_panel::refresh_content(state, &config.working_dir);
            }
        }
        "graph" => {
            use crate::tui::widgets::graph::{GraphOverlayState, MemoryGraphData, MemoryNode};
            // Build graph data from available info
            let mut data = MemoryGraphData::default();
            // Add LSP servers as nodes
            for server in cersei_lsp::config::builtin_servers() {
                if which::which(&server.command).is_ok() {
                    data.lsp_servers.push(server.name.clone());
                }
            }
            let graph_state = GraphOverlayState::from_memory_stats(&data);
            state.overlay = Overlay::Graph(graph_state);
        }
        "diff" => {
            state.side_panel_open = true;
            state.side_panel_focused = true;
            state.side_panel_tab = SidePanelTab::GitDiff;
            side_panel::refresh_content(state, &config.working_dir);
        }
        "files" | "tree" => {
            state.side_panel_open = true;
            state.side_panel_focused = true;
            state.side_panel_tab = SidePanelTab::FileTree;
            side_panel::refresh_content(state, &config.working_dir);
        }
        "rewind" => {
            // Remove the last assistant turn (rewind one step)
            if let Some(pos) = state.turns.iter().rposition(|t| t.role == crate::tui::app::TurnRole::Assistant) {
                let removed = state.turns.len() - pos;
                state.turns.truncate(pos);
                state.turns.push(crate::tui::app::Turn {
                    role: crate::tui::app::TurnRole::System,
                    content: format!("Rewound {removed} turn(s). You can now re-send your last message."),
                    tools: Vec::new(),
                    thinking: None,
                });
            } else {
                state.turns.push(crate::tui::app::Turn {
                    role: crate::tui::app::TurnRole::System,
                    content: "Nothing to rewind.".into(),
                    tools: Vec::new(),
                    thinking: None,
                });
            }
        }
        "undo" => {
            // Undo last file modification via snapshot manager
            let snapshots = cersei_tools::file_snapshot::session_snapshots(&state.session_id);
            let mut mgr = snapshots.lock();
            let files = mgr.modified_files();
            if let Some(last_file) = files.last() {
                let path = last_file.display().to_string();
                if mgr.undo_last(std::path::Path::new(&path)).is_some() {
                    state.turns.push(crate::tui::app::Turn {
                        role: crate::tui::app::TurnRole::System,
                        content: format!("Undid last change to {path}"),
                        tools: Vec::new(),
                        thinking: None,
                    });
                } else {
                    state.turns.push(crate::tui::app::Turn {
                        role: crate::tui::app::TurnRole::System,
                        content: "Failed to undo.".into(),
                        tools: Vec::new(),
                        thinking: None,
                    });
                }
            } else {
                state.turns.push(crate::tui::app::Turn {
                    role: crate::tui::app::TurnRole::System,
                    content: "No file changes to undo.".into(),
                    tools: Vec::new(),
                    thinking: None,
                });
            }
        }
        "memory" | "mem" => {
            state.turns.push(crate::tui::app::Turn {
                role: crate::tui::app::TurnRole::System,
                content: "Memory is injected into the system prompt automatically.\nUse AGENTS.md or .abstract/instructions.md in your project for persistent instructions.".into(),
                tools: Vec::new(),
                thinking: None,
            });
        }
        "sessions" | "session" | "ls" => {
            state.turns.push(crate::tui::app::Turn {
                role: crate::tui::app::TurnRole::System,
                content: format!("Current session: {}\nUse `abstract sessions list` from the terminal to manage sessions.", state.session_id),
                tools: Vec::new(),
                thinking: None,
            });
        }
        "model" => {
            state.turns.push(crate::tui::app::Turn {
                role: crate::tui::app::TurnRole::System,
                content: format!("Current model: {}\nChange with: abstract --model <provider/model>", state.model),
                tools: Vec::new(),
                thinking: None,
            });
        }
        "cost" => {
            // Estimate cost if provider didn't report it
            let cost = if state.cost_usd > 0.0 {
                state.cost_usd
            } else {
                cersei_tools::estimate_cost(&state.model, state.input_tokens, state.output_tokens)
            };
            state.turns.push(crate::tui::app::Turn {
                role: crate::tui::app::TurnRole::System,
                content: format!(
                    "Session cost: ${:.4}\nInput: {} tokens | Output: {} tokens\nTurns: {} | Tools: {}",
                    cost, state.input_tokens, state.output_tokens,
                    state.turn_count, state.tool_count
                ),
                tools: Vec::new(),
                thinking: None,
            });
        }
        "compact" => {
            state.turns.push(crate::tui::app::Turn {
                role: crate::tui::app::TurnRole::System,
                content: "Compaction will run automatically at 90% context usage.".into(),
                tools: Vec::new(),
                thinking: None,
            });
        }
        _ => {
            state.turns.push(crate::tui::app::Turn {
                role: crate::tui::app::TurnRole::System,
                content: format!("Unknown command: /{cmd}. Type /help for commands."),
                tools: Vec::new(),
                thinking: None,
            });
        }
    }
}

fn tool_input_summary(name: &str, input: &serde_json::Value) -> String {
    match name {
        "Bash" | "bash" => input.get("command").and_then(|v| v.as_str()).map(|s| truncate(s, 60)).unwrap_or_default(),
        "Read" | "Write" | "Edit" => input.get("file_path").and_then(|v| v.as_str()).unwrap_or("").to_string(),
        "Glob" => input.get("pattern").and_then(|v| v.as_str()).unwrap_or("").to_string(),
        "Grep" => input.get("pattern").and_then(|v| v.as_str()).map(|s| truncate(s, 40)).unwrap_or_default(),
        "LSP" => {
            let action = input.get("action").and_then(|v| v.as_str()).unwrap_or("?");
            let file = input.get("file").and_then(|v| v.as_str()).unwrap_or("?");
            format!("{action} {file}")
        }
        _ => truncate(&serde_json::to_string(input).unwrap_or_default(), 60),
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max { s.to_string() } else { format!("{}...", &s[..max]) }
}
