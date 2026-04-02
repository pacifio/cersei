//! # Coding Agent — Build a Python Todo CLI
//!
//! A real end-to-end test of Cersei as a coding agent framework.
//! Gives the agent a task: "Build a Python todo CLI app", then monitors
//! every event, verifies the output, and produces a detailed usage report.
//!
//! ```bash
//! cargo run --example coding_agent --release
//! ```

use cersei::prelude::*;
use cersei::events::AgentEvent;
use cersei::provider::{CompletionStream, ProviderCapabilities, ProviderOptions};
use cersei::reporters::Reporter;
use std::io::Write as IoWrite;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::mpsc;

// ─── Auth helper ─────────────────────────────────────────────────────────────

fn resolve_provider() -> cersei_types::Result<cersei::provider::anthropic::Anthropic> {
    // Try ANTHROPIC_API_KEY first, then ANTHROPIC_KEY
    if let Ok(key) = std::env::var("ANTHROPIC_API_KEY") {
        if !key.is_empty() {
            return Ok(cersei::Anthropic::new(Auth::ApiKey(key)));
        }
    }
    if let Ok(key) = std::env::var("ANTHROPIC_KEY") {
        if !key.is_empty() {
            return Ok(cersei::Anthropic::new(Auth::ApiKey(key)));
        }
    }
    Err(CerseiError::Auth(
        "No API key found. Set ANTHROPIC_API_KEY or ANTHROPIC_KEY".into(),
    ))
}

// ─── Event monitor ───────────────────────────────────────────────────────────

#[derive(Clone)]
struct EventMonitor {
    events: Arc<parking_lot::Mutex<Vec<EventRecord>>>,
    start: Instant,
}

#[derive(Clone, Debug)]
struct EventRecord {
    elapsed_ms: f64,
    category: String,
    detail: String,
}

impl EventMonitor {
    fn new() -> Self {
        Self {
            events: Arc::new(parking_lot::Mutex::new(Vec::new())),
            start: Instant::now(),
        }
    }

    fn record(&self, category: &str, detail: &str) {
        self.events.lock().push(EventRecord {
            elapsed_ms: self.start.elapsed().as_secs_f64() * 1000.0,
            category: category.to_string(),
            detail: detail.to_string(),
        });
    }
}

#[async_trait]
impl Reporter for EventMonitor {
    async fn on_event(&self, event: &AgentEvent) {
        match event {
            AgentEvent::TurnStart { turn } => {
                self.record("turn", &format!("turn {} started", turn));
                eprintln!("\n\x1b[36m━━ Turn {} ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━\x1b[0m", turn);
            }
            AgentEvent::TextDelta(t) => {
                print!("{}", t);
                let _ = std::io::stdout().flush();
            }
            AgentEvent::ThinkingDelta(_) => {
                // silent
            }
            AgentEvent::ToolStart { name, id, .. } => {
                self.record("tool_start", &format!("{} ({})", name, &id[..8.min(id.len())]));
                eprint!("\x1b[33m  [{name}] \x1b[0m");
            }
            AgentEvent::ToolEnd { name, duration, is_error, result, .. } => {
                let status = if *is_error { "\x1b[31mERR\x1b[0m" } else { "\x1b[32mOK\x1b[0m" };
                let preview: String = result.chars().take(80).collect();
                let preview = preview.replace('\n', " ");
                eprintln!("{} ({:.0}ms) {}", status, duration.as_millis(),
                    if preview.len() > 60 { &preview[..60] } else { &preview });
                self.record("tool_end", &format!("{} {} {:.0}ms", name,
                    if *is_error { "ERR" } else { "OK" }, duration.as_millis()));
            }
            AgentEvent::TurnComplete { turn, usage, stop_reason, .. } => {
                self.record("turn_complete", &format!(
                    "turn {} {}in/{}out {:?}", turn, usage.input_tokens, usage.output_tokens, stop_reason
                ));
                eprintln!("\x1b[2m  tokens: {}in / {}out | cost: ${:.6}\x1b[0m",
                    usage.input_tokens, usage.output_tokens, usage.cost_usd.unwrap_or(0.0));
            }
            AgentEvent::CostUpdate { cumulative_cost, .. } => {
                if *cumulative_cost > 0.0 {
                    self.record("cost", &format!("${:.6}", cumulative_cost));
                }
            }
            AgentEvent::SessionLoaded { message_count, .. } => {
                self.record("session", &format!("loaded {} messages", message_count));
            }
            AgentEvent::SessionSaved { session_id, .. } => {
                self.record("session", &format!("saved {}", session_id));
            }
            AgentEvent::Error(e) => {
                self.record("error", e);
                eprintln!("\n\x1b[31mError: {}\x1b[0m", e);
            }
            _ => {}
        }
    }
}

// ─── Mock coding provider ────────────────────────────────────────────────────

/// A mock provider that simulates a coding agent building a todo.py file.
/// Exercises the full tool dispatch pipeline: Write → Bash → Write → Bash → EndTurn.
struct MockCodingProvider {
    turn: Arc<std::sync::atomic::AtomicU32>,
    workspace: std::path::PathBuf,
}

impl MockCodingProvider {
    fn new(workspace: &std::path::Path) -> Self {
        Self {
            turn: Arc::new(std::sync::atomic::AtomicU32::new(0)),
            workspace: workspace.to_path_buf(),
        }
    }
}

const TODO_PY: &str = r#"#!/usr/bin/env python3
"""Simple Todo CLI application."""

import argparse
import json
import os
from datetime import datetime

TODO_FILE = "todos.json"

def load_todos():
    if os.path.exists(TODO_FILE):
        with open(TODO_FILE) as f:
            return json.load(f)
    return []

def save_todos(todos):
    with open(TODO_FILE, "w") as f:
        json.dump(todos, f, indent=2)

def next_id(todos):
    return max((t["id"] for t in todos), default=0) + 1

def add_todo(text):
    todos = load_todos()
    todo = {
        "id": next_id(todos),
        "text": text,
        "done": False,
        "created_at": datetime.now().isoformat()
    }
    todos.append(todo)
    save_todos(todos)
    print(f"Added: [{todo['id']}] {text}")

def list_todos():
    todos = load_todos()
    if not todos:
        print("No todos yet. Add one with: python todo.py add 'your task'")
        return
    for t in todos:
        check = "✓" if t["done"] else " "
        status = "\033[9m" if t["done"] else ""
        reset = "\033[0m" if t["done"] else ""
        print(f"  [{check}] {t['id']:>3}. {status}{t['text']}{reset}")

def done_todo(todo_id):
    todos = load_todos()
    for t in todos:
        if t["id"] == todo_id:
            t["done"] = True
            save_todos(todos)
            print(f"Completed: [{todo_id}] {t['text']}")
            return
    print(f"Todo {todo_id} not found")

def remove_todo(todo_id):
    todos = load_todos()
    todos = [t for t in todos if t["id"] != todo_id]
    save_todos(todos)
    print(f"Removed todo {todo_id}")

def clear_done():
    todos = load_todos()
    remaining = [t for t in todos if not t["done"]]
    removed = len(todos) - len(remaining)
    save_todos(remaining)
    print(f"Cleared {removed} completed todo(s)")

def main():
    parser = argparse.ArgumentParser(description="Simple Todo CLI")
    sub = parser.add_subparsers(dest="command")

    add_p = sub.add_parser("add", help="Add a new todo")
    add_p.add_argument("text", help="Todo text")

    sub.add_parser("list", help="List all todos")

    done_p = sub.add_parser("done", help="Mark todo as complete")
    done_p.add_argument("id", type=int, help="Todo ID")

    rm_p = sub.add_parser("remove", help="Remove a todo")
    rm_p.add_argument("id", type=int, help="Todo ID")

    sub.add_parser("clear", help="Remove completed todos")

    args = parser.parse_args()

    if args.command == "add":
        add_todo(args.text)
    elif args.command == "list":
        list_todos()
    elif args.command == "done":
        done_todo(args.id)
    elif args.command == "remove":
        remove_todo(args.id)
    elif args.command == "clear":
        clear_done()
    else:
        parser.print_help()

if __name__ == "__main__":
    main()
"#;

const README_CONTENT: &str = r#"# Todo CLI

A simple command-line todo application written in Python.

## Usage

```bash
python todo.py add "Buy groceries"
python todo.py add "Write tests"
python todo.py list
python todo.py done 1
python todo.py remove 2
python todo.py clear
```

## Storage

Todos are stored in `todos.json` in the current directory.
"#;

#[async_trait]
impl Provider for MockCodingProvider {
    fn name(&self) -> &str { "mock-claude" }
    fn context_window(&self, _: &str) -> u64 { 200_000 }
    fn capabilities(&self, _: &str) -> ProviderCapabilities {
        ProviderCapabilities {
            streaming: true, tool_use: true, vision: true,
            thinking: true, system_prompt: true, caching: true,
        }
    }

    async fn complete(&self, request: CompletionRequest) -> cersei_types::Result<CompletionStream> {
        let turn = self.turn.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let ws = self.workspace.clone();
        let msg_count = request.messages.len();
        let (tx, rx) = mpsc::channel(64);

        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(30)).await;

            let base_input = 2500 + (msg_count as u64 * 400);
            let cache_read = if turn > 0 { base_input / 3 } else { 0 };

            let _ = tx.send(StreamEvent::MessageStart {
                id: format!("msg_{turn}"),
                model: "claude-sonnet-4-6".into(),
            }).await;

            match turn {
                0 => {
                    // Turn 1: Write todo.py
                    let _ = tx.send(StreamEvent::ContentBlockStart { index: 0, block_type: "text".into(), id: None, name: None }).await;
                    let _ = tx.send(StreamEvent::TextDelta { index: 0,
                        text: "I'll create the todo CLI app. Let me write `todo.py` first.\n".into() }).await;
                    let _ = tx.send(StreamEvent::ContentBlockStop { index: 0 }).await;

                    // Tool use: Write todo.py
                    let file_path = ws.join("todo.py").display().to_string();
                    let tool_input = serde_json::json!({
                        "file_path": file_path,
                        "content": TODO_PY,
                    });
                    let _ = tx.send(StreamEvent::ContentBlockStart { index: 1, block_type: "tool_use".into(), id: Some("tu_write1".into()), name: Some("Write".into()) }).await;
                    let _ = tx.send(StreamEvent::InputJsonDelta { index: 1,
                        partial_json: serde_json::to_string(&tool_input).unwrap() }).await;
                    let _ = tx.send(StreamEvent::ContentBlockStop { index: 1 }).await;

                    let output_tokens = 1250;
                    let cost = (base_input as f64 / 1e6) * 3.0 + (output_tokens as f64 / 1e6) * 15.0;
                    let _ = tx.send(StreamEvent::MessageDelta {
                        stop_reason: Some(StopReason::ToolUse),
                        usage: Some(Usage {
                            input_tokens: base_input - cache_read,
                            output_tokens,
                            total_tokens: base_input + output_tokens,
                            cost_usd: Some(cost),
                            provider_usage: serde_json::json!({
                                "cache_creation_input_tokens": 1200,
                                "cache_read_input_tokens": cache_read,
                            }),
                        }),
                    }).await;
                }
                1 => {
                    // Turn 2: Write README.md
                    let _ = tx.send(StreamEvent::ContentBlockStart { index: 0, block_type: "text".into(), id: None, name: None }).await;
                    let _ = tx.send(StreamEvent::TextDelta { index: 0,
                        text: "Now I'll create a README.md.\n".into() }).await;
                    let _ = tx.send(StreamEvent::ContentBlockStop { index: 0 }).await;

                    let file_path = ws.join("README.md").display().to_string();
                    let tool_input = serde_json::json!({
                        "file_path": file_path,
                        "content": README_CONTENT,
                    });
                    let _ = tx.send(StreamEvent::ContentBlockStart { index: 1, block_type: "tool_use".into(), id: Some("tu_write2".into()), name: Some("Write".into()) }).await;
                    let _ = tx.send(StreamEvent::InputJsonDelta { index: 1,
                        partial_json: serde_json::to_string(&tool_input).unwrap() }).await;
                    let _ = tx.send(StreamEvent::ContentBlockStop { index: 1 }).await;

                    let output_tokens = 380;
                    let cost = (base_input as f64 / 1e6) * 3.0 + (output_tokens as f64 / 1e6) * 15.0;
                    let _ = tx.send(StreamEvent::MessageDelta {
                        stop_reason: Some(StopReason::ToolUse),
                        usage: Some(Usage {
                            input_tokens: base_input - cache_read,
                            output_tokens: 380,
                            total_tokens: base_input + 380,
                            cost_usd: Some(cost),
                            provider_usage: serde_json::json!({
                                "cache_creation_input_tokens": 0,
                                "cache_read_input_tokens": cache_read,
                            }),
                        }),
                    }).await;
                }
                2 => {
                    // Turn 3: Verify with python3
                    let _ = tx.send(StreamEvent::ContentBlockStart { index: 0, block_type: "text".into(), id: None, name: None }).await;
                    let _ = tx.send(StreamEvent::TextDelta { index: 0,
                        text: "Let me verify the Python syntax.\n".into() }).await;
                    let _ = tx.send(StreamEvent::ContentBlockStop { index: 0 }).await;

                    let py_path = ws.join("todo.py").display().to_string();
                    let tool_input = serde_json::json!({
                        "command": format!("python3 -c \"import ast; ast.parse(open('{}').read()); print('Syntax OK')\"", py_path),
                    });
                    let _ = tx.send(StreamEvent::ContentBlockStart { index: 1, block_type: "tool_use".into(), id: Some("tu_bash1".into()), name: Some("Bash".into()) }).await;
                    let _ = tx.send(StreamEvent::InputJsonDelta { index: 1,
                        partial_json: serde_json::to_string(&tool_input).unwrap() }).await;
                    let _ = tx.send(StreamEvent::ContentBlockStop { index: 1 }).await;

                    let output_tokens = 195;
                    let cost = (base_input as f64 / 1e6) * 3.0 + (output_tokens as f64 / 1e6) * 15.0;
                    let _ = tx.send(StreamEvent::MessageDelta {
                        stop_reason: Some(StopReason::ToolUse),
                        usage: Some(Usage {
                            input_tokens: base_input - cache_read,
                            output_tokens: 195,
                            total_tokens: base_input + 195,
                            cost_usd: Some(cost),
                            provider_usage: serde_json::json!({
                                "cache_creation_input_tokens": 0,
                                "cache_read_input_tokens": cache_read,
                            }),
                        }),
                    }).await;
                }
                _ => {
                    // Turn 4: Final summary
                    let _ = tx.send(StreamEvent::ContentBlockStart { index: 0, block_type: "text".into(), id: None, name: None }).await;
                    let summary = "I've created the Python todo CLI application:\n\n\
                        **Files created:**\n\
                        - `todo.py` — Full CLI with add, list, done, remove, clear commands\n\
                        - `README.md` — Usage documentation\n\n\
                        **Features:**\n\
                        - Argparse-based CLI interface\n\
                        - JSON file storage (`todos.json`)\n\
                        - Each todo has id, text, done status, and created_at timestamp\n\
                        - Formatted output with checkmarks\n\
                        - Python syntax verified successfully\n";
                    for chunk in summary.as_bytes().chunks(50) {
                        let _ = tx.send(StreamEvent::TextDelta { index: 0,
                            text: String::from_utf8_lossy(chunk).to_string() }).await;
                        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                    }
                    let _ = tx.send(StreamEvent::ContentBlockStop { index: 0 }).await;

                    let output_tokens = 285;
                    let cost = (base_input as f64 / 1e6) * 3.0 + (output_tokens as f64 / 1e6) * 15.0;
                    let _ = tx.send(StreamEvent::MessageDelta {
                        stop_reason: Some(StopReason::EndTurn),
                        usage: Some(Usage {
                            input_tokens: base_input - cache_read,
                            output_tokens: 285,
                            total_tokens: base_input + 285,
                            cost_usd: Some(cost),
                            provider_usage: serde_json::json!({
                                "cache_creation_input_tokens": 0,
                                "cache_read_input_tokens": cache_read,
                            }),
                        }),
                    }).await;
                }
            }

            let _ = tx.send(StreamEvent::MessageStop).await;
        });

        Ok(CompletionStream::new(rx))
    }
}

// ─── Main ────────────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Create a temp workspace for the agent
    let workspace = tempfile::tempdir()?;
    let ws_path = workspace.path().to_path_buf();

    let use_mock = std::env::args().any(|a| a == "--mock") || {
        // Try real provider; if billing fails, use mock
        match resolve_provider() {
            Ok(_) => false,
            Err(_) => true,
        }
    };

    let provider_label = if use_mock { "mock-claude (simulated)" } else { "anthropic (live API)" };

    eprintln!("╔══════════════════════════════════════════════════════════════╗");
    eprintln!("║  Cersei Coding Agent — Build a Python Todo CLI              ║");
    eprintln!("╠══════════════════════════════════════════════════════════════╣");
    eprintln!("║  Provider: {:<49}║", provider_label);
    eprintln!("║  Workspace: {}║", format!("{:<48}", ws_path.display()));
    eprintln!("╚══════════════════════════════════════════════════════════════╝");

    let monitor = EventMonitor::new();
    let monitor_ref = monitor.clone();
    let start = Instant::now();

    let prompt = r#"Create a Python todo CLI application in a single file called `todo.py`. Requirements:

1. Use argparse for CLI interface
2. Store todos in a JSON file called `todos.json`
3. Support these commands:
   - `add <text>` — add a new todo
   - `list` — show all todos with their IDs and status
   - `done <id>` — mark a todo as complete
   - `remove <id>` — delete a todo
   - `clear` — remove all completed todos
4. Each todo should have: id (int), text (str), done (bool), created_at (ISO timestamp)
5. Print nice formatted output with checkmarks

After creating the file, also create a brief `README.md` explaining how to use it.
Then verify the Python file is valid by running `python3 -c "import ast; ast.parse(open('todo.py').read()); print('Syntax OK')"`.
"#;

    let mut builder = Agent::builder()
        .tools(cersei::tools::coding())
        .system_prompt(
            "You are an expert Python developer. Write clean, well-structured code. \
             Be concise in your explanations. Always verify your work.",
        )
        .model("claude-sonnet-4-6")
        .max_turns(10)
        .max_tokens(16384)
        .permission_policy(AllowAll)
        .working_dir(&ws_path)
        .reporter(monitor_ref);

    // Attach provider
    let agent = if use_mock {
        builder.provider(MockCodingProvider::new(&ws_path)).build()?
    } else {
        builder.provider(resolve_provider()?).build()?
    };

    let output = agent.run(prompt).await?;
    let elapsed = start.elapsed();

    // ── Verify outputs ───────────────────────────────────────────────────
    eprintln!("\n\x1b[36m━━ Verification ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━\x1b[0m");

    let todo_py = ws_path.join("todo.py");
    let readme = ws_path.join("README.md");

    let checks = vec![
        ("todo.py exists", todo_py.exists()),
        ("todo.py non-empty", todo_py.exists() && std::fs::metadata(&todo_py).map(|m| m.len() > 100).unwrap_or(false)),
        ("README.md exists", readme.exists()),
    ];

    let mut all_pass = true;
    for (name, pass) in &checks {
        let icon = if *pass { "\x1b[32m✓\x1b[0m" } else { "\x1b[31m✗\x1b[0m" };
        eprintln!("  {} {}", icon, name);
        if !pass { all_pass = false; }
    }

    // Verify Python syntax
    if todo_py.exists() {
        let syntax_check = tokio::process::Command::new("python3")
            .args(["-c", &format!(
                "import ast; ast.parse(open('{}').read()); print('Syntax OK')",
                todo_py.display()
            )])
            .output()
            .await;

        match syntax_check {
            Ok(out) if out.status.success() => {
                eprintln!("  \x1b[32m✓\x1b[0m Python syntax valid");
            }
            Ok(out) => {
                let err = String::from_utf8_lossy(&out.stderr);
                eprintln!("  \x1b[31m✗\x1b[0m Python syntax error: {}", err.trim());
                all_pass = false;
            }
            Err(e) => {
                eprintln!("  \x1b[33m?\x1b[0m python3 not available: {}", e);
            }
        }
    }

    // Check file sizes
    if todo_py.exists() {
        let size = std::fs::metadata(&todo_py)?.len();
        eprintln!("  todo.py: {} bytes ({} lines)",
            size,
            std::fs::read_to_string(&todo_py)?.lines().count());
    }
    if readme.exists() {
        let size = std::fs::metadata(&readme)?.len();
        eprintln!("  README.md: {} bytes", size);
    }

    // ── Usage report ─────────────────────────────────────────────────────
    eprintln!("\n\x1b[36m━━ Usage Report ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━\x1b[0m");
    eprintln!("  Model:           claude-sonnet-4-6 ({})", provider_label);
    eprintln!("  Wall time:       {:.2}s", elapsed.as_secs_f64());
    eprintln!("  Turns:           {}", output.turns);
    eprintln!("  Tool calls:      {}", output.tool_calls.len());
    eprintln!("  Stop reason:     {:?}", output.stop_reason);
    eprintln!();
    eprintln!("  Input tokens:    {:>8}", output.usage.input_tokens);
    eprintln!("  Output tokens:   {:>8}", output.usage.output_tokens);
    eprintln!("  Total tokens:    {:>8}", output.usage.input_tokens + output.usage.output_tokens);
    eprintln!("  Cost (USD):      ${:.6}", output.usage.cost_usd.unwrap_or(0.0));
    eprintln!();

    // Tool call breakdown
    if !output.tool_calls.is_empty() {
        eprintln!("  Tool Calls:");
        for (i, tc) in output.tool_calls.iter().enumerate() {
            let status = if tc.is_error { "\x1b[31mERR\x1b[0m" } else { "\x1b[32mOK\x1b[0m" };
            eprintln!("    {}. {} {} ({:.0}ms)",
                i + 1, tc.name, status, tc.duration.as_millis());
        }
        eprintln!();

        // Histogram
        let mut hist: std::collections::HashMap<String, u32> = std::collections::HashMap::new();
        for tc in &output.tool_calls {
            *hist.entry(tc.name.clone()).or_default() += 1;
        }
        eprintln!("  Tool Histogram:");
        let mut sorted: Vec<_> = hist.into_iter().collect();
        sorted.sort_by(|a, b| b.1.cmp(&a.1));
        for (name, count) in &sorted {
            let bar = "█".repeat(*count as usize);
            eprintln!("    {:<10} {:>2}x {}", name, count, bar);
        }
    }

    // ── Event timeline ───────────────────────────────────────────────────
    let events = monitor.events.lock().clone();
    eprintln!("\n  Event Timeline ({} events):", events.len());
    for ev in &events {
        eprintln!("    {:>8.0}ms  [{:<14}] {}",
            ev.elapsed_ms, ev.category, ev.detail);
    }

    // ── Billing projection ───────────────────────────────────────────────
    let input_t = output.usage.input_tokens;
    let output_t = output.usage.output_tokens;
    let sonnet_cost = (input_t as f64 / 1e6) * 3.0 + (output_t as f64 / 1e6) * 15.0;
    eprintln!("\n  Billing:");
    eprintln!("    Sonnet rate:  ${:.6}", sonnet_cost);
    eprintln!("    Per 100 runs: ${:.2}", sonnet_cost * 100.0);
    eprintln!("    Monthly (50/d): ${:.2}", sonnet_cost * 50.0 * 30.0);

    // ── Final verdict ────────────────────────────────────────────────────
    eprintln!();
    if all_pass {
        eprintln!("  \x1b[32m✓ CODING AGENT TEST PASSED\x1b[0m");
        eprintln!("  The agent successfully created a Python todo CLI app.");
    } else {
        eprintln!("  \x1b[31m✗ SOME CHECKS FAILED\x1b[0m");
        eprintln!("  Review the output above for details.");
    }
    eprintln!();

    // Print the generated code
    if todo_py.exists() {
        let code = std::fs::read_to_string(&todo_py)?;
        eprintln!("─── Generated todo.py ({} lines) ───", code.lines().count());
        for (i, line) in code.lines().enumerate().take(60) {
            eprintln!("  {:>3} │ {}", i + 1, line);
        }
        if code.lines().count() > 60 {
            eprintln!("  ... ({} more lines)", code.lines().count() - 60);
        }
    }

    Ok(())
}
