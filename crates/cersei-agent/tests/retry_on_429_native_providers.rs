//! §10.5 #4: F-02's end-to-end retry coverage for the two native-protocol
//! providers.
//!
//! `retry_on_429.rs` proves a 429 is retried through the real runner on the
//! OpenAI path; the Anthropic and Gemini paths were covered only at the
//! `complete()` boundary (`http_status_is_retryable.rs`). These tests close
//! that gap: a scripted socket answers 429-then-success in each provider's
//! own wire format, `Agent::run` drives the real runner, and the assertions
//! are exactly the OpenAI test's — two requests on the socket, and the
//! retried attempt's content is what the turn returns.

use cersei_agent::Agent;
use cersei_provider::{Anthropic, Gemini};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

// ─── Canned responses (harness pattern from retry_on_429.rs) ─────────────────

struct Canned {
    status_line: &'static str,
    extra_headers: &'static str,
    content_type: &'static str,
    body: String,
}

impl Canned {
    fn rate_limited() -> Self {
        Canned {
            status_line: "429 Too Many Requests",
            extra_headers: "Retry-After: 1\r\n",
            content_type: "application/json",
            body: r#"{"error":{"message":"slow down"}}"#.to_string(),
        }
    }

    /// A minimal but complete Anthropic Messages SSE stream saying `pong`.
    fn anthropic_sse_pong() -> Self {
        let body = concat!(
            "event: message_start\n",
            "data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_test\",\
             \"type\":\"message\",\"role\":\"assistant\",\"model\":\"claude-test\",\
             \"usage\":{\"input_tokens\":3,\"output_tokens\":0}}}\n\n",
            "event: content_block_start\n",
            "data: {\"type\":\"content_block_start\",\"index\":0,\
             \"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
            "event: content_block_delta\n",
            "data: {\"type\":\"content_block_delta\",\"index\":0,\
             \"delta\":{\"type\":\"text_delta\",\"text\":\"pong\"}}\n\n",
            "event: content_block_stop\n",
            "data: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
            "event: message_delta\n",
            "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\
             \"usage\":{\"output_tokens\":1}}\n\n",
            "event: message_stop\n",
            "data: {\"type\":\"message_stop\"}\n\n",
        );
        Canned {
            status_line: "200 OK",
            extra_headers: "",
            content_type: "text/event-stream",
            body: body.to_string(),
        }
    }

    /// A minimal Gemini `streamGenerateContent` SSE stream saying `pong`.
    fn gemini_sse_pong() -> Self {
        let body = concat!(
            "data: {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"pong\"}],\
             \"role\":\"model\"},\"finishReason\":\"STOP\"}],\
             \"usageMetadata\":{\"promptTokenCount\":3,\"candidatesTokenCount\":1}}\n\n",
        );
        Canned {
            status_line: "200 OK",
            extra_headers: "",
            content_type: "text/event-stream",
            body: body.to_string(),
        }
    }
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

fn drain_http_request(sock: &mut TcpStream) {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 4096];
    let header_end = loop {
        match sock.read(&mut tmp) {
            Ok(0) | Err(_) => return,
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
            Ok(0) | Err(_) => return,
            Ok(n) => buf.extend_from_slice(&tmp[..n]),
        }
    }
}

/// Serve `responses` in order, one per connection, counting requests.
fn serve_sequence(responses: Vec<Canned>) -> (String, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().unwrap().port();
    let hits = Arc::new(AtomicUsize::new(0));
    let counter = hits.clone();

    std::thread::spawn(move || {
        let mut scripted = responses.into_iter();
        while let Ok((mut sock, _)) = listener.accept() {
            counter.fetch_add(1, Ordering::SeqCst);
            drain_http_request(&mut sock);
            let payload = match scripted.next() {
                Some(c) => format!(
                    "HTTP/1.1 {}\r\nContent-Type: {}\r\n{}Content-Length: {}\r\n\
                     Connection: close\r\n\r\n{}",
                    c.status_line,
                    c.content_type,
                    c.extra_headers,
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

    (format!("http://127.0.0.1:{port}"), hits)
}

// ─── Cases ───────────────────────────────────────────────────────────────────

/// Anthropic path: a 429 followed by a good Messages SSE stream must complete
/// the turn through the real runner, with exactly one retry.
#[tokio::test]
async fn anthropic_429_is_retried_through_the_runner() {
    let (url, hits) =
        serve_sequence(vec![Canned::rate_limited(), Canned::anthropic_sse_pong()]);
    let agent = Agent::builder()
        .provider(
            Anthropic::builder()
                .api_key("test-key")
                .base_url(&url) // provider appends /v1/messages
                .model("claude-sonnet-5")
                .build()
                .expect("build provider"),
        )
        .model("claude-sonnet-5")
        .max_turns(2)
        .max_tokens(64)
        .build()
        .expect("build agent");

    let out = agent.run("ping").await;

    let requests = hits.load(Ordering::SeqCst);
    let out = out.unwrap_or_else(|e| {
        panic!(
            "anthropic: a 429 then a good response must complete the turn, got \
             Err({e}) after {requests} request(s). requests == 1 means the \
             retry never fired on this path."
        )
    });
    assert_eq!(requests, 2, "expected exactly one retry, saw {requests}");
    assert_eq!(out.text(), "pong");
}

/// Gemini path: same contract, Gemini wire format.
#[tokio::test]
async fn gemini_429_is_retried_through_the_runner() {
    let (url, hits) = serve_sequence(vec![Canned::rate_limited(), Canned::gemini_sse_pong()]);
    let agent = Agent::builder()
        .provider(
            Gemini::builder()
                .api_key("test-key")
                .base_url(&url) // provider appends /models/{model}:streamGenerateContent
                .model("gemini-flash-lite-latest")
                .build()
                .expect("build provider"),
        )
        .model("gemini-flash-lite-latest")
        .max_turns(2)
        .max_tokens(64)
        .build()
        .expect("build agent");

    let out = agent.run("ping").await;

    let requests = hits.load(Ordering::SeqCst);
    let out = out.unwrap_or_else(|e| {
        panic!(
            "gemini: a 429 then a good response must complete the turn, got \
             Err({e}) after {requests} request(s). requests == 1 means the \
             retry never fired on this path."
        )
    });
    assert_eq!(requests, 2, "expected exactly one retry, saw {requests}");
    assert_eq!(out.text(), "pong");
}
