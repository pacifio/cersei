//! F-08 wiring: a prose-only first turn with tools available must trigger the
//! no-tool-call nudge — once, ungated by benchmark_mode — and the retry turn
//! must carry the forced tool choice on the wire.
//!
//! These drive the real runner (`Agent::run` against a scripted SSE socket,
//! the `p0_wiring.rs` harness pattern) and assert on the literal request
//! bodies, because F-08's original defect was exactly a wiring gate: the
//! nudge existed but only fired when `had_tool_use || benchmark_mode`.

use async_trait::async_trait;
use cersei_agent::Agent;
use cersei_provider::OpenAi;
use cersei_tools::{PermissionLevel, Tool, ToolCategory, ToolContext, ToolResult};
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};

// ─── Canned SSE server (recording variant, as in p0_wiring.rs) ───────────────

struct Canned {
    body: String,
}

impl Canned {
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

/// Serve `responses` in order, recording every request body. Exhausted scripts
/// answer 500 so an unexpected extra request fails the test loudly.
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

/// A trivial registered tool, so `tools_available` is true.
struct PingTool;

#[async_trait]
impl Tool for PingTool {
    fn name(&self) -> &str {
        "Ping"
    }
    fn description(&self) -> &str {
        "Replies with pong."
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
        ToolResult::success("pong")
    }
}

// ─── Cases ───────────────────────────────────────────────────────────────────

/// The load-bearing case: turn 1 is prose, tools exist. The runner must nudge
/// exactly once — the second request carries the nudge message AND the forced
/// tool choice — and a second prose answer ends the session (no third call).
#[tokio::test]
async fn prose_only_turn_is_nudged_once_with_forced_tool_choice() {
    let (url, bodies) = serve_recording(vec![
        Canned::sse_text("The auth logic is in src/auth.rs and uses JWT."),
        Canned::sse_text("Still the same answer."),
    ]);

    let agent = Agent::builder()
        .provider(provider_against(&url, "gpt-4o"))
        .tool(PingTool)
        .model("gpt-4o")
        .max_turns(6)
        .max_tokens(64)
        .build()
        .expect("build agent");

    agent
        .run("Where is the authentication logic?")
        .await
        .expect("run must complete");

    let bodies = bodies.lock().unwrap();
    assert_eq!(
        bodies.len(),
        2,
        "exactly one nudge retry: a third request would mean the once-per-\
         session gate failed, one request that it never fired"
    );
    assert!(
        !bodies[0].contains("tool_choice"),
        "the first request must run with the provider default: {}",
        bodies[0]
    );
    assert!(
        bodies[1].contains("answered without using any tools"),
        "the retry must carry the nudge message: {}",
        bodies[1]
    );
    assert!(
        bodies[1].contains("\"tool_choice\":\"required\""),
        "the retry must force a tool call where the provider supports it: {}",
        bodies[1]
    );
}

/// Without tools, a prose answer is the only possible answer — nudging would
/// loop for nothing. One request, no retry.
#[tokio::test]
async fn prose_answer_without_tools_is_not_nudged() {
    let (url, bodies) = serve_recording(vec![Canned::sse_text("Hello!")]);

    let agent = Agent::builder()
        .provider(provider_against(&url, "gpt-4o"))
        .model("gpt-4o")
        .max_turns(6)
        .max_tokens(64)
        .build()
        .expect("build agent");

    agent.run("hi").await.expect("run must complete");

    assert_eq!(
        bodies.lock().unwrap().len(),
        1,
        "an agent with no tools must not be nudged to use them"
    );
}
