//! F-02, provider half: a non-2xx must come back as a typed `Err` **from
//! `complete()` itself**.
//!
//! That "from `complete()`" is the whole point, and it is what an earlier
//! attempt got wrong. `CerseiError::ProviderStatus` / `RateLimit` were being
//! constructed correctly, but from inside the spawned SSE reader as a
//! `StreamEvent`, so `complete()` still returned `Ok(stream)` for a 429. The
//! runner's retry loop guards `provider.complete()` and nothing else, so it
//! never saw one. These tests therefore assert on `complete()`'s return value
//! and deliberately do **not** fall back to draining the stream — a status that
//! only surfaces after the stream is drained is, operationally, not retryable.
//!
//! The companion test `cersei-agent/tests/retry_on_429.rs` covers the other
//! half: that the runner actually retries. Neither test is sufficient alone —
//! this one would pass on a correctly-typed-but-unretried error, and that one
//! would pass without `Retry-After` ever being parsed.
//!
//! Three clients are exercised directly. The fourth, `AnthropicVertex`, is
//! covered transitively: it builds a `reqwest::Request` and hands it to
//! `anthropic::spawn_sse` — the same function the `Anthropic` client uses — so
//! `anthropic_529_overloaded_is_retryable` exercises the identical boundary.
//! Vertex is not driven directly because it mints a GCP bearer token before
//! reaching HTTP, and a test that failed at auth would prove nothing about
//! status handling.

use cersei_provider::{CompletionRequest, Provider};
use cersei_types::*;
use std::io::{Read, Write};
use std::net::TcpListener;

/// Answer exactly one request with `status`, then close. Returns the base URL.
///
/// Hand-rolled rather than pulled from a mock-HTTP crate: the whole contract
/// under test is "the status code survives", and a literal response string is
/// the least indirect way to state that.
fn serve_once(status_line: &'static str, headers: &'static str, body: &'static str) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().unwrap().port();

    std::thread::spawn(move || {
        if let Ok((mut sock, _)) = listener.accept() {
            let mut buf = [0u8; 4096];
            let _ = sock.read(&mut buf); // drain the request
            let payload = format!(
                "HTTP/1.1 {status_line}\r\nContent-Type: application/json\r\n\
                 Content-Length: {}\r\n{headers}\r\n{body}",
                body.len()
            );
            let _ = sock.write_all(payload.as_bytes());
            let _ = sock.flush();
        }
    });

    format!("http://127.0.0.1:{port}/v1")
}

/// Drive `complete()` and require that it is the thing that fails.
///
/// An `Ok(stream)` here is the F-02 regression itself, so it is a panic rather
/// than a fall-through to `collect()`: by the time the stream is drained the
/// runner has already left the only code path that can retry.
async fn error_from(provider: impl Provider, model: &str) -> CerseiError {
    let mut req = CompletionRequest::new(model);
    req.messages = vec![Message::user("hi")];
    req.max_tokens = 16;

    match provider.complete(req).await {
        Err(e) => e,
        Ok(_) => panic!(
            "complete() returned Ok for a non-2xx response — the runner's retry \
             loop only inspects this Result, so the status is unreachable"
        ),
    }
}

#[tokio::test]
async fn openai_429_is_retryable() {
    let url = serve_once("429 Too Many Requests", "Retry-After: 7\r\n", r#"{"error":"slow down"}"#);
    let p = cersei_provider::OpenAi::builder()
        .api_key("k")
        .base_url(&url)
        .model("gpt-4o")
        .build()
        .unwrap();

    let err = error_from(p, "gpt-4o").await;
    assert!(
        err.is_retryable(),
        "a 429 must be retryable, got non-retryable: {err}"
    );
    match err {
        CerseiError::RateLimit { retry_after, .. } => assert_eq!(
            retry_after,
            Some(std::time::Duration::from_secs(7)),
            "Retry-After must be carried through so the backoff can honour it"
        ),
        other => panic!("429 should map to RateLimit, got {other:?}"),
    }
}

#[tokio::test]
async fn anthropic_529_overloaded_is_retryable() {
    let url = serve_once("529 Overloaded", "", r#"{"type":"overloaded_error"}"#);
    let p = cersei_provider::Anthropic::builder()
        .api_key("k")
        .base_url(&url)
        .model("claude-sonnet-5")
        .build()
        .unwrap();

    let err = error_from(p, "claude-sonnet-5").await;
    assert!(err.is_retryable(), "529 Overloaded must be retryable: {err}");
    assert!(
        matches!(err, CerseiError::ProviderStatus { status: 529, .. }),
        "got {err:?}"
    );
}

#[tokio::test]
async fn a_400_is_reported_but_not_retried() {
    let url = serve_once("400 Bad Request", "", r#"{"error":"bad model"}"#);
    let p = cersei_provider::OpenAi::builder()
        .api_key("k")
        .base_url(&url)
        .model("nope")
        .build()
        .unwrap();

    let err = error_from(p, "nope").await;
    assert!(
        !err.is_retryable(),
        "a 400 is deterministic — retrying it just burns quota: {err}"
    );
    match err {
        CerseiError::ProviderStatus { status: 400, message } => assert!(
            message.contains("bad model"),
            "the body is the only diagnostic the user gets: {message}"
        ),
        other => panic!("400 should map to ProviderStatus, got {other:?}"),
    }
}

#[tokio::test]
async fn gemini_429_is_retryable() {
    let url = serve_once("429 Too Many Requests", "", r#"{"error":"quota"}"#);
    let p = cersei_provider::Gemini::builder()
        .api_key("k")
        .base_url(&url)
        .model("gemini-2.0-flash")
        .build()
        .unwrap();

    let err = error_from(p, "gemini-2.0-flash").await;
    assert!(err.is_retryable(), "gemini 429 must be retryable: {err}");
}
