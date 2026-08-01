//! F-09 wiring: the runner supplies `options.num_ctx` from
//! `context_window_for_model` on every request, and a provider flagged for it
//! (the router's Ollama construction) puts it on the wire.
//!
//! This drives the real runner against a scripted SSE socket (the
//! `p0_wiring.rs` harness pattern) and asserts on the literal request body,
//! binding three things at once: the runner's `options.set("num_ctx", …)`
//! call, the conservative 8_192 catch-all for unknown Ollama-style tags, and
//! the provider's flagged emission.

use cersei_agent::Agent;
use cersei_provider::OpenAi;
use serde_json::json;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};

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

/// Serve one prose SSE reply, recording the request body.
fn serve_one_text(text: &str) -> (String, Arc<Mutex<Vec<String>>>) {
    let first = json!({
        "choices": [{ "index": 0, "delta": { "role": "assistant", "content": text } }]
    });
    let last = json!({
        "choices": [{ "index": 0, "delta": {}, "finish_reason": "stop" }],
        "usage": { "prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 15 }
    });
    let sse = format!("data: {first}\n\ndata: {last}\n\ndata: [DONE]\n\n");

    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().unwrap().port();
    let bodies = Arc::new(Mutex::new(Vec::new()));
    let recorder = bodies.clone();
    std::thread::spawn(move || {
        while let Ok((mut sock, _)) = listener.accept() {
            let body = drain_http_request(&mut sock);
            recorder.lock().unwrap().push(body);
            let payload = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\
                 Content-Length: {}\r\nConnection: close\r\n\r\n{}",
                sse.len(),
                sse
            );
            let _ = sock.write_all(payload.as_bytes());
            let _ = sock.flush();
            let _ = sock.shutdown(std::net::Shutdown::Write);
        }
    });
    (format!("http://127.0.0.1:{port}/v1"), bodies)
}

/// An unknown Ollama-style tag gets the conservative window, and the flagged
/// provider sends it as `options.num_ctx` — through the real runner.
#[tokio::test]
async fn runner_sends_conservative_num_ctx_on_the_ollama_path() {
    let (url, bodies) = serve_one_text("hello");

    let provider = OpenAi::builder()
        .api_key("no-key")
        .base_url(&url)
        .model("qwen2.5-coder:7b")
        .send_num_ctx(true) // what the router sets for entry.id == "ollama"
        .build()
        .expect("build provider");

    let agent = Agent::builder()
        .provider(provider)
        .model("qwen2.5-coder:7b")
        .max_turns(2)
        .max_tokens(64)
        .build()
        .expect("build agent");

    agent.run("hi").await.expect("run must complete");

    let bodies = bodies.lock().unwrap();
    assert!(!bodies.is_empty(), "at least one request must have been sent");
    assert!(
        bodies[0].contains("\"num_ctx\":8192"),
        "F-09: the runner's window truth (conservative 8_192 for an unknown \
         tag) must reach the Ollama wire as options.num_ctx: {}",
        bodies[0]
    );
}
