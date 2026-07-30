//! F-02: a 429 must be *retried*, not merely typed correctly.
//!
//! Providers used to report a non-2xx from inside the `tokio::spawn`ed SSE
//! reader, so `complete()` returned `Ok(CompletionStream)` even for a 429. The
//! retry loop in `runner.rs` guards `provider.complete()` and nothing else, so
//! it never observed one; the status was handled in the event loop *below* that
//! loop, which could only `return Err(..)`. One 429 killed the session.
//!
//! A type-only assertion (`err.is_retryable()`) cannot catch that: it passes
//! whether or not a retry ever happens. So this test drives the **real runner**
//! against a socket that answers 429 once and then succeeds, and asserts on the
//! observable outcome — the turn completes, and the provider was called twice.

use cersei_agent::Agent;
use cersei_provider::OpenAi;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

/// One canned HTTP response.
struct Canned {
    status_line: &'static str,
    /// Extra header lines, each `\r\n`-terminated. May be empty.
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

    fn bad_request() -> Self {
        Canned {
            status_line: "400 Bad Request",
            extra_headers: "",
            content_type: "application/json",
            body: r#"{"error":{"message":"unknown model"}}"#.to_string(),
        }
    }

    /// A minimal but complete OpenAI chat-completions SSE stream whose only
    /// content is the text `pong`.
    fn sse_saying_pong() -> Self {
        let body = concat!(
            "data: {\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"\"}}]}\n\n",
            "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"pong\"}}]}\n\n",
            "data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}],\
             \"usage\":{\"prompt_tokens\":3,\"completion_tokens\":1,\"total_tokens\":4}}\n\n",
            "data: [DONE]\n\n",
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

/// Read the request headers *and* the declared body.
///
/// Draining the body matters: closing a socket that still has unread bytes in
/// its receive buffer makes the kernel send an RST, which discards the response
/// we just wrote and turns this into a flaky connection error rather than the
/// status code the test is about.
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

/// Serve `responses` in order, one per connection. Returns the base URL and a
/// live counter of how many requests actually arrived.
///
/// After the scripted responses are exhausted the listener stays up and answers
/// 500, so an unexpected extra request shows up as a wrong count rather than as
/// a connection-refused error that would look like an unrelated failure.
fn serve_sequence(responses: Vec<Canned>) -> (String, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().unwrap().port();
    let hits = Arc::new(AtomicUsize::new(0));
    let counter = hits.clone();

    std::thread::spawn(move || {
        let mut scripted = responses.into_iter();
        while let Ok((mut sock, _)) = listener.accept() {
            drain_http_request(&mut sock);
            counter.fetch_add(1, Ordering::SeqCst);
            let payload = match scripted.next() {
                Some(c) => format!(
                    "HTTP/1.1 {}\r\nContent-Type: {}\r\nContent-Length: {}\r\n\
                     Connection: close\r\n{}\r\n{}",
                    c.status_line,
                    c.content_type,
                    c.body.len(),
                    c.extra_headers,
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

    (format!("http://127.0.0.1:{port}/v1"), hits)
}

fn agent_against(base_url: &str) -> Agent {
    Agent::builder()
        .provider(
            OpenAi::builder()
                .api_key("test-key")
                .base_url(base_url)
                .model("gpt-4o")
                .build()
                .expect("build provider"),
        )
        .model("gpt-4o")
        .max_turns(2)
        .max_tokens(64)
        .build()
        .expect("build agent")
}

/// The load-bearing test. Before F-02 was fixed properly this failed twice
/// over: `run` returned `Err(Provider error 429 …)` and the socket saw exactly
/// one request.
#[tokio::test]
async fn a_429_is_retried_and_the_turn_still_succeeds() {
    let (url, hits) = serve_sequence(vec![Canned::rate_limited(), Canned::sse_saying_pong()]);
    let agent = agent_against(&url);

    let out = agent.run("ping").await;

    let requests = hits.load(Ordering::SeqCst);
    let out = out.unwrap_or_else(|e| {
        panic!(
            "a 429 followed by a good response must complete the turn, got Err({e}) \
             after {requests} request(s). If requests == 1 the retry loop never fired."
        )
    });

    assert_eq!(
        requests, 2,
        "expected exactly one retry (2 requests total), saw {requests}"
    );
    assert_eq!(
        out.text(),
        "pong",
        "the retried attempt's content must be what the turn returns"
    );
}

/// Accept connections and never answer, holding each socket open. Models a
/// provider that has stopped responding — no timeout is configured on any
/// provider's `reqwest::Client`, so this await is unbounded.
fn serve_nothing() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        let mut held = Vec::new();
        while let Ok((sock, _)) = listener.accept() {
            held.push(sock); // keep it open; never write a response
        }
    });
    format!("http://127.0.0.1:{port}/v1")
}

/// Regression guard for the cost of F-02's restructure.
///
/// Moving `client.execute()` inside `complete()` is what makes a 429 retryable,
/// but it also moved an unbounded network await out of the stream loop — the
/// only place the runner polls `cancel_token` — and into the retry loop, which
/// does not. Before this guard existed, cancelling during time-to-first-byte
/// against a server that never replies hung forever.
#[tokio::test]
async fn cancel_is_honoured_while_waiting_on_response_headers() {
    let url = serve_nothing();
    let token = tokio_util::sync::CancellationToken::new();
    let agent = Agent::builder()
        .provider(
            OpenAi::builder()
                .api_key("test-key")
                .base_url(&url)
                .model("gpt-4o")
                .build()
                .expect("build provider"),
        )
        .model("gpt-4o")
        .max_turns(2)
        .max_tokens(64)
        .cancel_token(token.clone())
        .build()
        .expect("build agent");

    let canceller = tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        token.cancel();
    });

    let started = std::time::Instant::now();
    let outcome = tokio::time::timeout(std::time::Duration::from_secs(5), agent.run("ping")).await;
    let elapsed = started.elapsed();
    let _ = canceller.await;

    let result = outcome.unwrap_or_else(|_| {
        panic!(
            "cancel was requested at 200ms but run() was still blocked {elapsed:?} later — \
             the await on response headers is outside any cancellation branch"
        )
    });
    let err = result.expect_err("a cancelled run must not report success");
    assert!(
        matches!(err, cersei_types::CerseiError::Cancelled),
        "expected Cancelled, got {err:?}"
    );
}

/// The same hole, reached the other way: retry backoff sleeps up to ~31s across
/// five attempts, and that sleep became reachable for the first time in F-02.
#[tokio::test]
async fn cancel_is_honoured_during_retry_backoff() {
    // Every attempt is rate limited, so the loop is guaranteed to be sleeping.
    let (url, _hits) = serve_sequence(vec![
        Canned::rate_limited(),
        Canned::rate_limited(),
        Canned::rate_limited(),
    ]);
    let token = tokio_util::sync::CancellationToken::new();
    let agent = Agent::builder()
        .provider(
            OpenAi::builder()
                .api_key("test-key")
                .base_url(&url)
                .model("gpt-4o")
                .build()
                .expect("build provider"),
        )
        .model("gpt-4o")
        .max_turns(2)
        .max_tokens(64)
        .cancel_token(token.clone())
        .build()
        .expect("build agent");

    let canceller = tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        token.cancel();
    });

    let started = std::time::Instant::now();
    let outcome = tokio::time::timeout(std::time::Duration::from_secs(5), agent.run("ping")).await;
    let elapsed = started.elapsed();
    let _ = canceller.await;

    let result = outcome.unwrap_or_else(|_| {
        panic!("cancel at 200ms was still unhonoured {elapsed:?} into the backoff")
    });
    let err = result.expect_err("a cancelled run must not report success");
    assert!(
        matches!(err, cersei_types::CerseiError::Cancelled),
        "expected Cancelled, got {err:?}"
    );
}

/// The other half of the contract: a deterministic 4xx must not be retried, or
/// the backoff just burns quota on a request that can never succeed.
#[tokio::test]
async fn a_400_is_not_retried() {
    let (url, hits) = serve_sequence(vec![Canned::bad_request(), Canned::sse_saying_pong()]);
    let agent = agent_against(&url);

    let err = agent
        .run("ping")
        .await
        .expect_err("a 400 must surface as an error, not be papered over");

    let requests = hits.load(Ordering::SeqCst);
    assert_eq!(
        requests, 1,
        "a 400 is deterministic — it must be attempted exactly once, saw {requests}"
    );
    assert!(
        err.to_string().contains("unknown model"),
        "the response body is the only diagnostic the user gets: {err}"
    );
}
