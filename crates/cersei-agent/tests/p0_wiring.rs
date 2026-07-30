//! Wiring tests for the P0 fixes whose *call sites* the mutation audit found
//! unbound (TOOL-CALLING-RELIABILITY.md §10.3–§10.4).
//!
//! The pattern in every gap was the same: the helper had unit tests, the place
//! the runner calls it had none, and the call site was the original bug. These
//! tests therefore drive the **real runner** — `Agent::run` against a scripted
//! OpenAI-compatible SSE socket — and assert on externally observable outcomes:
//! bytes on disk (F-11), the message that enters history (F-07), and the exact
//! request body sent to the provider after compaction (F-04).

use async_trait::async_trait;
use cersei_agent::Agent;
use cersei_provider::OpenAi;
use cersei_tools::{PermissionLevel, Tool, ToolCategory, ToolContext, ToolResult};
use cersei_types::{ContentBlock, Message, MessageContent, ToolResultContent};
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};

// ─── Canned SSE server (recording variant of the retry_on_429 harness) ───────

struct Canned {
    body: String,
}

impl Canned {
    /// A complete SSE stream: one assistant tool call, terminated properly.
    fn sse_tool_call(id: &str, tool: &str, args: &Value) -> Self {
        let first = json!({
            "choices": [{
                "index": 0,
                "delta": {
                    "role": "assistant",
                    "tool_calls": [{
                        "index": 0,
                        "id": id,
                        "type": "function",
                        "function": { "name": tool, "arguments": args.to_string() }
                    }]
                }
            }]
        });
        let last = json!({
            "choices": [{ "index": 0, "delta": {}, "finish_reason": "tool_calls" }],
            "usage": { "prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 15 }
        });
        Canned {
            body: format!("data: {first}\n\ndata: {last}\n\ndata: [DONE]\n\n"),
        }
    }

    /// A complete SSE stream saying `text`, terminated properly.
    fn sse_text(text: &str) -> Self {
        let first = json!({
            "choices": [{ "index": 0, "delta": { "role": "assistant", "content": text } }]
        });
        let last = json!({
            "choices": [{ "index": 0, "delta": {}, "finish_reason": "stop" }],
            "usage": { "prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 15 }
        });
        Canned {
            body: format!("data: {first}\n\ndata: {last}\n\ndata: [DONE]\n\n"),
        }
    }
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// Read one HTTP request fully (headers + declared body) and return the body.
///
/// Draining matters: closing a socket with unread bytes makes the kernel send
/// an RST, which discards the response we just wrote.
fn drain_http_request(sock: &mut TcpStream) -> String {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 4096];
    let header_end = loop {
        match sock.read(&mut tmp) {
            Ok(0) | Err(_) => return String::new(),
            Ok(n) => buf.extend_from_slice(&tmp[..n]),
        }
        if let Some(p) = find_subslice(&buf, b"\r\n\r\n") {
            break p + 4;
        }
    };
    let head = String::from_utf8_lossy(&buf[..header_end]).to_lowercase();
    let content_length = head
        .lines()
        .find_map(|l| l.strip_prefix("content-length:"))
        .and_then(|v| v.trim().parse::<usize>().ok())
        .unwrap_or(0);
    while buf.len() < header_end + content_length {
        match sock.read(&mut tmp) {
            Ok(0) | Err(_) => break,
            Ok(n) => buf.extend_from_slice(&tmp[..n]),
        }
    }
    String::from_utf8_lossy(&buf[header_end..]).to_string()
}

/// Serve `responses` in order, one per connection, recording every request
/// body. Exhausted scripts answer 500 so an unexpected extra request shows up
/// as a test failure rather than a connection error.
fn serve_recording(responses: Vec<Canned>) -> (String, Arc<Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().unwrap().port();
    let bodies = Arc::new(Mutex::new(Vec::new()));
    let recorder = bodies.clone();

    std::thread::spawn(move || {
        let mut scripted = responses.into_iter();
        while let Ok((mut sock, _)) = listener.accept() {
            let body = drain_http_request(&mut sock);
            recorder.lock().unwrap().push(body);
            let payload = match scripted.next() {
                Some(c) => format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\
                     Content-Length: {}\r\nConnection: close\r\n\r\n{}",
                    c.body.len(),
                    c.body
                ),
                None => "HTTP/1.1 500 Internal Server Error\r\nContent-Length: 0\r\n\
                         Connection: close\r\n\r\n"
                    .to_string(),
            };
            let _ = sock.write_all(payload.as_bytes());
            let _ = sock.flush();
            let _ = sock.shutdown(std::net::Shutdown::Write);
        }
    });

    (format!("http://127.0.0.1:{port}/v1"), bodies)
}

fn provider_against(base_url: &str, model: &str) -> OpenAi {
    OpenAi::builder()
        .api_key("test-key")
        .base_url(base_url)
        .model(model)
        .build()
        .expect("build provider")
}

// ─── F-11: the read-before-edit guard must run BEFORE the write lands ────────

/// The audit's finding: 12 unit tests exercise `refusals_for_batch`, yet
/// replacing its call in the dispatch path with an empty map failed nothing.
/// The defect class is silent data corruption, so the load-bearing assertion
/// here is (b): the bytes on disk. Assertion (a) alone would pass even if the
/// guard ran after the write, which is exactly the bug F-11 fixed.
#[tokio::test]
async fn edit_of_unread_file_is_refused_and_the_file_is_untouched() {
    let dir = tempfile::tempdir().expect("tempdir");
    let target = dir.path().join("guarded.txt");
    std::fs::write(&target, "ORIGINAL CONTENT").expect("seed file");

    let (url, _bodies) = serve_recording(vec![
        Canned::sse_tool_call(
            "call_edit",
            "Edit",
            &json!({
                "file_path": target.to_string_lossy(),
                "old_string": "ORIGINAL",
                "new_string": "CLOBBERED",
            }),
        ),
        Canned::sse_text("done"),
        Canned::sse_text("done"), // depth-nudge retry turn
    ]);

    let agent = Agent::builder()
        .provider(provider_against(&url, "gpt-4o"))
        .tools(cersei_tools::coding())
        .working_dir(dir.path())
        .model("gpt-4o")
        .max_turns(4)
        .max_tokens(64)
        .build()
        .expect("build agent");

    let out = agent
        .run("Replace ORIGINAL with CLOBBERED in guarded.txt")
        .await
        .expect("run must complete");

    // (b) — the assertion that binds guard *placement*: nothing reached disk.
    let on_disk = std::fs::read_to_string(&target).expect("read back");
    assert_eq!(
        on_disk, "ORIGINAL CONTENT",
        "the Edit executed before (or despite) the refusal — the guard is \
         wired after dispatch or not at all"
    );

    // (a) — the refusal actually reached the model as an error it can act on.
    let edit_call = out
        .tool_calls
        .iter()
        .find(|c| c.name == "Edit")
        .expect("the scripted Edit call must appear in tool_calls");
    assert!(
        edit_call.is_error,
        "the guard must refuse the unread-file edit, got success: {}",
        edit_call.result
    );
    assert!(
        edit_call.result.contains("Read"),
        "the refusal must tell the model the way forward (Read first): {}",
        edit_call.result
    );
}

// ─── F-07: error results must be capped before entering history ──────────────

/// A tool whose failure output is huge, the way a failing `cargo build` is.
struct HugeFailureTool;

#[async_trait]
impl Tool for HugeFailureTool {
    fn name(&self) -> &str {
        "Boom"
    }
    fn description(&self) -> &str {
        "Always fails with an enormous diagnostic dump."
    }
    fn permission_level(&self) -> PermissionLevel {
        PermissionLevel::None
    }
    fn category(&self) -> ToolCategory {
        ToolCategory::Shell
    }
    fn input_schema(&self) -> Value {
        json!({ "type": "object", "properties": {} })
    }
    async fn execute(&self, _input: Value, _ctx: &ToolContext) -> ToolResult {
        let line = "error[E0308]: mismatched types — expected `&str`, found `String` (padding)";
        let dump: Vec<String> = (0..600).map(|i| format!("{line} #{i}")).collect();
        ToolResult::error(dump.join("\n"))
    }
}

/// The audit's finding: `cap_tool_result` has unit tests, but reverting the
/// `is_error` branch to `result.content.clone()` failed nothing. So this test
/// asserts on the history the next request is built from, via
/// `Agent::messages()` — the exact artifact the fix protects.
#[tokio::test]
async fn oversized_error_result_is_capped_before_entering_history() {
    let (url, _bodies) = serve_recording(vec![
        Canned::sse_tool_call("call_boom", "Boom", &json!({})),
        Canned::sse_text("that failed"),
        Canned::sse_text("that failed"), // depth-nudge retry turn
    ]);

    let agent = Agent::builder()
        .provider(provider_against(&url, "gpt-4o"))
        .tool(HugeFailureTool)
        .model("gpt-4o")
        .max_turns(4)
        .max_tokens(64)
        .build()
        .expect("build agent");

    agent.run("run the Boom tool").await.expect("run completes");

    let history_result = agent
        .messages()
        .iter()
        .find_map(|m| match &m.content {
            MessageContent::Blocks(blocks) => blocks.iter().find_map(|b| match b {
                ContentBlock::ToolResult {
                    content: ToolResultContent::Text(t),
                    is_error: Some(true),
                    ..
                } => Some(t.clone()),
                _ => None,
            }),
            _ => None,
        })
        .expect("the Boom failure must be in history as an error tool_result");

    // 600 lines went in; head+tail capping keeps 160 plus an omission marker.
    let uncapped_len = 600 * 80; // lower bound on what the tool emitted
    assert!(
        history_result.len() < uncapped_len / 2,
        "an error result entered history at {} chars — the is_error branch is \
         bypassing cap_tool_result again",
        history_result.len()
    );
    assert!(
        history_result.contains("omitted"),
        "capping must be announced with the omission marker so the model knows \
         output is missing; got a result with no marker ({} chars)",
        history_result.len()
    );
}

// ─── F-04: the compaction call site must use the pair-aware split ────────────

/// A cheap tool so the compaction turn has a real tool_use/tool_result pair.
struct PingTool;

#[async_trait]
impl Tool for PingTool {
    fn name(&self) -> &str {
        "Ping"
    }
    fn description(&self) -> &str {
        "Returns pong."
    }
    fn permission_level(&self) -> PermissionLevel {
        PermissionLevel::None
    }
    fn category(&self) -> ToolCategory {
        ToolCategory::Shell
    }
    fn input_schema(&self) -> Value {
        json!({ "type": "object", "properties": {} })
    }
    async fn execute(&self, _input: Value, _ctx: &ToolContext) -> ToolResult {
        ToolResult::success("pong".to_string())
    }
}

/// Build a runner-shaped history whose naive `len - KEEP_RECENT_MESSAGES`
/// split lands exactly on a `user[tool_result]`, severing its pair — the F-04
/// parity trap. Layout (17 messages):
///
///   0            user      40k chars of padding (drives the token estimate
///                          over 90% of gpt-4's 8_192 window; the estimator
///                          counts only Text blocks, so padding cannot live in
///                          the tool results)
///   1,3,..,15    assistant tool_use  seed_t1..seed_t8
///   2,4,..,16    user      tool_result seed_t1..seed_t8
///
/// After `run()` adds the prompt and turn 1 adds assistant + results, the
/// history is 20 messages. The naive split is index 10 — `tool_result seed_t5`
/// — whose `tool_use` at index 9 would be discarded. `pair_aware_split` must
/// back off to index 9 instead.
fn history_with_split_landing_mid_pair() -> Vec<Message> {
    let mut msgs = vec![Message::user("PAD ".repeat(10_000))];
    for i in 1..=8 {
        let id = format!("seed_t{i}");
        msgs.push(Message::assistant_blocks(vec![ContentBlock::ToolUse {
            id: id.clone(),
            name: "Ping".into(),
            input: json!({}),
        }]));
        msgs.push(Message::user_blocks(vec![ContentBlock::ToolResult {
            tool_use_id: id,
            content: ToolResultContent::Text(format!("result {i}")),
            is_error: Some(false),
        }]));
    }
    msgs
}

/// Every tool_call_id referenced by a `role:"tool"` message in `body` must be
/// declared by a preceding assistant `tool_calls` entry. This is the exact
/// invariant the provider enforces with a 400.
fn assert_tool_pairs_intact(body: &str, label: &str) -> usize {
    let parsed: Value = serde_json::from_str(body)
        .unwrap_or_else(|e| panic!("{label}: request body is not JSON: {e}"));
    let messages = parsed["messages"]
        .as_array()
        .unwrap_or_else(|| panic!("{label}: no messages array in body"));

    let declared: Vec<String> = messages
        .iter()
        .filter_map(|m| m["tool_calls"].as_array())
        .flatten()
        .filter_map(|tc| tc["id"].as_str().map(String::from))
        .collect();

    let mut tool_messages = 0;
    for m in messages {
        if m["role"].as_str() == Some("tool") {
            tool_messages += 1;
            let id = m["tool_call_id"].as_str().unwrap_or("");
            assert!(
                declared.contains(&id.to_string()),
                "{label}: tool_result '{id}' has no matching tool_use in the \
                 request — the provider would reject this with a 400. \
                 Declared ids: {declared:?}"
            );
        }
    }
    tool_messages
}

/// The audit's finding: `pair_aware_split` has 5 unit tests, yet reverting the
/// compaction call site to the naive `len - KEEP` split failed nothing. This
/// drives compaction through the real runner and asserts on the request body
/// sent *after* it — the artifact the provider actually judges.
#[tokio::test]
async fn compaction_through_the_runner_never_orphans_tool_results() {
    let (url, bodies) = serve_recording(vec![
        // Turn 1: one Ping call, so compaction runs at the end of the turn.
        Canned::sse_tool_call("call_live", "Ping", &json!({})),
        // The compaction summarization request itself.
        Canned::sse_text("COMPACT-SUMMARY-MARKER: earlier padding and pings."),
        // Turn 2, built from the compacted history — the request under test.
        Canned::sse_text("done"),
        // Turn 3 after the depth nudge.
        Canned::sse_text("done"),
    ]);

    // "gpt-4" (not 4o!) resolves to an 8_192-token window, small enough for a
    // seeded history to cross the 90% auto-compact threshold honestly.
    let agent = Agent::builder()
        .provider(provider_against(&url, "gpt-4"))
        .tool(PingTool)
        .model("gpt-4")
        .max_turns(4)
        .max_tokens(64)
        .auto_compact(true)
        .with_messages(history_with_split_landing_mid_pair())
        .build()
        .expect("build agent");

    agent.run("continue the work").await.expect("run completes");

    let bodies = bodies.lock().unwrap();
    assert!(
        bodies.len() >= 3,
        "expected turn-1, compaction, and turn-2 requests, saw {}",
        bodies.len()
    );

    // The request after compaction must carry the summary, proving compaction
    // actually fired — otherwise the pairing assertion below is vacuous.
    let post_compact = &bodies[2];
    assert!(
        post_compact.contains("COMPACT-SUMMARY-MARKER"),
        "turn-2 request does not contain the compaction summary — compaction \
         never fired and this test measured nothing"
    );

    let tool_messages = assert_tool_pairs_intact(post_compact, "post-compaction request");
    assert!(
        tool_messages >= 1,
        "the kept slice should retain recent tool_result messages; none were \
         sent, so the pairing check proved nothing"
    );

    // The pre-compaction request must also be clean (seeded history sanity).
    assert_tool_pairs_intact(&bodies[0], "turn-1 request");
}
