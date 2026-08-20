//! Regression tests for the `Agent::run_stream` control path.
//!
//! Every test here drives the **real runner** against a scripted
//! OpenAI-compatible SSE socket (the harness pattern from `p0_wiring.rs`) and
//! asserts on externally observable outcomes: whether a second run reaches the
//! provider at all, whether a cancelled stream terminates, what the next
//! request body carries, and what a registered listener saw.
//!
//! The four defects under test all shared a shape: a documented control
//! surface (`wiki/05-events-streaming.md`) whose messages went into a channel
//! nobody read, or a one-shot token nobody could reset.

use cersei_agent::{Agent, AgentEvent};
use cersei_provider::OpenAi;
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

// ─── Canned SSE server ───────────────────────────────────────────────────────

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

    /// A complete SSE stream that says one word `chunks` times, so the run
    /// produces more events than the agent's internal event channel can hold.
    fn sse_many_deltas(chunks: usize) -> Self {
        let mut body = String::new();
        for _ in 0..chunks {
            let frame = json!({
                "choices": [{ "index": 0, "delta": { "role": "assistant", "content": "x" } }]
            });
            body.push_str(&format!("data: {frame}\n\n"));
        }
        let last = json!({
            "choices": [{ "index": 0, "delta": {}, "finish_reason": "stop" }],
            "usage": { "prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 15 }
        });
        body.push_str(&format!("data: {last}\n\ndata: [DONE]\n\n"));
        Canned { body }
    }

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

/// Accept connections, read each request, then hold the socket open forever
/// without ever answering. Models the "server accepts and goes quiet" case
/// that cancellation has to be able to break out of.
///
/// Returns the base URL and a counter of requests received, so a test can
/// assert that a cancelled run stopped issuing them.
fn serve_hanging() -> (String, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().unwrap().port();
    let seen = Arc::new(AtomicUsize::new(0));
    let counter = seen.clone();

    std::thread::spawn(move || {
        // Held open deliberately: dropping these would send FIN and let the
        // client's read finish, which is the opposite of what we're modelling.
        let mut held: Vec<TcpStream> = Vec::new();
        while let Ok((mut sock, _)) = listener.accept() {
            drain_http_request(&mut sock);
            counter.fetch_add(1, Ordering::SeqCst);
            held.push(sock);
        }
    });

    (format!("http://127.0.0.1:{port}/v1"), seen)
}

fn provider_against(base_url: &str, model: &str) -> OpenAi {
    OpenAi::builder()
        .api_key("test-key")
        .base_url(base_url)
        .model(model)
        .build()
        .expect("build provider")
}

fn agent_against(base_url: &str) -> Agent {
    Agent::builder()
        .provider(provider_against(base_url, "gpt-4o"))
        .working_dir(".")
        .model("gpt-4o")
        .max_turns(4)
        .max_tokens(64)
        .build()
        .expect("build agent")
}

// ─── B1: cancellation must not be permanent ─────────────────────────────────

/// The reported symptom. `Agent::cancel()` used to cancel a one-shot
/// `CancellationToken` that the runner re-checked at the top of every turn.
/// Tokens never un-cancel, so the *second* run returned `Cancelled` before it
/// ever reached the provider — the agent was bricked by its own cancel.
///
/// The load-bearing assertion is the second run's success. Asserting only that
/// `cancel()` returns would pass even with the bug.
#[tokio::test]
async fn agent_survives_a_cancel_and_can_stream_again() {
    let (url, bodies) = serve_recording(vec![Canned::sse_text("one"), Canned::sse_text("two")]);
    let agent = Arc::new(agent_against(&url));

    let first = agent.run_stream("first").collect_text().await;
    assert!(first.is_ok(), "first run should stream: {first:?}");

    // The user hits Ctrl+C / the embedder calls cancel().
    agent.cancel();

    let second = agent.run_stream("second").collect_text().await;
    assert!(
        second.is_ok(),
        "the agent is bricked after one cancel — every later run_stream returns \
         Cancelled without reaching the provider: {second:?}"
    );

    // Externally observable proof the second run actually hit the wire, rather
    // than short-circuiting on a stale token.
    assert_eq!(
        bodies.lock().unwrap().len(),
        2,
        "the second run never reached the provider"
    );
}

/// `run_agent` builds an event channel it never drains, but bound the receiver
/// as `_event_rx` — an underscore-prefixed *binding*, which lives to the end of
/// the scope rather than dropping immediately like a bare `_`. So the channel
/// stayed open with a 512-slot buffer and no reader, and `event_tx.send().await`
/// blocked forever once it filled. Any `run()` producing more than 512 events —
/// a few hundred text deltas — hung.
#[tokio::test]
async fn blocking_run_survives_more_events_than_the_channel_can_buffer() {
    // 512 is the channel capacity; overshoot it well clear of the other
    // per-turn events so the test is about the buffer, not the margin.
    let (url, _bodies) = serve_recording(vec![Canned::sse_many_deltas(2000)]);
    let agent = agent_against(&url);

    let out = tokio::time::timeout(Duration::from_secs(20), agent.run("say a lot"))
        .await
        .expect("run() deadlocked on its own undrained event channel")
        .expect("run should complete");

    assert_eq!(out.text().len(), 2000, "the response was truncated");
}

/// Same defect through the non-streaming entry point, which shares the token.
#[tokio::test]
async fn cancel_does_not_brick_the_blocking_run_path() {
    let (url, _bodies) = serve_recording(vec![Canned::sse_text("one"), Canned::sse_text("two")]);
    let agent = agent_against(&url);

    agent.run("first").await.expect("first run");
    agent.cancel();
    assert!(
        agent.run("second").await.is_ok(),
        "run() is bricked after a cancel"
    );
}

// ─── B2: the control channel must actually be read ──────────────────────────

/// `AgentStream::cancel()` is documented in `wiki/05-events-streaming.md`, but
/// `run_agent_streaming` bound its receiver as `_control_rx` and never read it,
/// so the message died in an undrained 64-slot buffer.
///
/// Pointed at a socket that accepts and never answers, a working `cancel()` is
/// the only thing that can end this stream.
#[tokio::test]
async fn stream_cancel_stops_a_run_that_is_waiting_on_the_provider() {
    let (url, _seen) = serve_hanging();
    let agent = Arc::new(agent_against(&url));
    let mut stream = agent.run_stream("hello");

    // Let the run get as far as awaiting the provider's response headers.
    tokio::time::sleep(Duration::from_millis(300)).await;
    stream.cancel();

    let drained = tokio::time::timeout(Duration::from_secs(5), async {
        while stream.next().await.is_some() {}
    })
    .await;

    assert!(
        drained.is_ok(),
        "AgentStream::cancel() did not stop the run — the control channel is \
         never drained, so the message was dropped"
    );
}

/// `inject_message` has the same dead-channel problem. The observable proof it
/// works is the *next request body*: the injected text has to reach the
/// provider as a user message, not merely be accepted by the setter.
#[tokio::test]
async fn injected_message_reaches_the_provider_on_the_next_turn() {
    let (url, bodies) = serve_recording(vec![
        // Turn 1 asks for a tool, which guarantees a turn 2 we can inspect.
        Canned::sse_tool_call("call_1", "Read", &json!({ "file_path": "Cargo.toml" })),
        Canned::sse_text("done"),
        Canned::sse_text("done"),
    ]);

    let agent = Arc::new(agent_against(&url));
    let mut stream = agent.run_stream("start");

    let mut injected = false;
    while let Some(event) = stream.next().await {
        // Inject as soon as the first turn is under way.
        if !injected {
            if let AgentEvent::TurnStart { .. } = event {
                stream.inject_message("INJECTED_SENTINEL".into());
                injected = true;
            }
        }
        if matches!(event, AgentEvent::Complete(_) | AgentEvent::Error(_)) {
            break;
        }
    }
    assert!(injected, "never saw a TurnStart to inject against");

    let recorded = bodies.lock().unwrap().clone();
    assert!(
        recorded.len() >= 2,
        "expected a second turn to inspect, got {} request(s)",
        recorded.len()
    );
    assert!(
        recorded[1..].iter().any(|b| b.contains("INJECTED_SENTINEL")),
        "the injected message never reached the provider — the control channel \
         swallowed it. Bodies after turn 1: {:?}",
        &recorded[1..]
    );
}

/// `respond_permission` was the third casualty of the dead control channel,
/// and it had a second problem behind it: the runner never emitted
/// `AgentEvent::PermissionRequired` at all, and `StreamDeferredPolicy` just
/// returned `Allow`. So the documented interactive-permission flow allowed
/// everything, silently.
///
/// The load-bearing assertion is the tool *result*: a denial the stream issued
/// has to be the reason the tool did not run.
#[tokio::test]
async fn stream_deferred_permission_denial_actually_blocks_the_tool() {
    let (url, _bodies) = serve_recording(vec![
        Canned::sse_tool_call("call_1", "Read", &json!({ "file_path": "Cargo.toml" })),
        Canned::sse_text("understood"),
        Canned::sse_text("understood"),
    ]);

    let agent = Arc::new(
        Agent::builder()
            .provider(provider_against(&url, "gpt-4o"))
            .tools(cersei_tools::coding())
            .permission_policy(cersei_tools::permissions::StreamDeferredPolicy)
            .working_dir(".")
            .model("gpt-4o")
            .max_turns(4)
            .max_tokens(64)
            .build()
            .expect("build agent"),
    );

    let mut stream = agent.run_stream("read the manifest");
    let mut asked = false;
    let mut denied_result: Option<(String, bool)> = None;

    while let Some(event) = stream.next().await {
        match event {
            AgentEvent::PermissionRequired(req) => {
                asked = true;
                assert_eq!(req.tool_name, "Read");
                stream.respond_permission(
                    req.id,
                    cersei_tools::permissions::PermissionDecision::Deny("nope".into()),
                );
            }
            AgentEvent::ToolEnd {
                name,
                result,
                is_error,
                ..
            } if name == "Read" => {
                denied_result = Some((result, is_error));
            }
            AgentEvent::Complete(_) | AgentEvent::Error(_) => break,
            _ => {}
        }
    }

    assert!(
        asked,
        "the runner never emitted PermissionRequired for a stream-deferred policy"
    );
    let (result, is_error) = denied_result.expect("no ToolEnd for the Read call");
    assert!(is_error, "the denied tool reported success");
    assert!(
        result.contains("Permission denied") && result.contains("nope"),
        "the stream's denial did not reach the tool result: {result}"
    );
}

/// The same policy under the non-streaming `run()`, where nothing can answer.
/// The fallback must stay exactly as it was — allow, and warn — rather than
/// flipping to a deny that would break existing headless callers.
#[tokio::test]
async fn stream_deferred_policy_falls_back_to_allow_without_a_stream() {
    let (url, _bodies) = serve_recording(vec![
        Canned::sse_tool_call("call_1", "Read", &json!({ "file_path": "Cargo.toml" })),
        Canned::sse_text("done"),
        Canned::sse_text("done"),
    ]);

    let out = Agent::builder()
        .provider(provider_against(&url, "gpt-4o"))
        .tools(cersei_tools::coding())
        .permission_policy(cersei_tools::permissions::StreamDeferredPolicy)
        .working_dir(".")
        .model("gpt-4o")
        .max_turns(4)
        .max_tokens(64)
        .build()
        .expect("build agent")
        .run("read the manifest")
        .await
        .expect("run should complete");

    let read = out
        .tool_calls
        .iter()
        .find(|c| c.name == "Read")
        .expect("no Read call recorded");
    assert!(
        !read.result.contains("Permission denied"),
        "the headless fallback started denying: {}",
        read.result
    );
}

// ─── B3: terminal events must reach the agent's own listeners ───────────────

/// `run()` emitted `Complete`/`Error` through `agent.emit`; `run_stream()` sent
/// them only down the mpsc. So `on_event` handlers, `Reporter`s and broadcast
/// subscribers saw every event of a streamed run *except* the one saying it
/// had ended.
#[tokio::test]
async fn streamed_run_emits_its_terminal_event_to_registered_listeners() {
    let (url, _bodies) = serve_recording(vec![Canned::sse_text("hi")]);

    let saw_complete = Arc::new(AtomicUsize::new(0));
    let counter = saw_complete.clone();

    let agent = Arc::new(
        Agent::builder()
            .provider(provider_against(&url, "gpt-4o"))
            .working_dir(".")
            .model("gpt-4o")
            .max_turns(2)
            .max_tokens(64)
            .on_event(move |e| {
                if matches!(e, AgentEvent::Complete(_)) {
                    counter.fetch_add(1, Ordering::SeqCst);
                }
            })
            .build()
            .expect("build agent"),
    );

    agent
        .run_stream("hello")
        .collect()
        .await
        .expect("run should complete");

    // `emit` fans reporters out onto spawned tasks; the on_event handler itself
    // is synchronous, but yield once so the ordering is not load-bearing.
    tokio::task::yield_now().await;

    assert_eq!(
        saw_complete.load(Ordering::SeqCst),
        1,
        "the on_event listener never saw Complete for a streamed run"
    );
}

// ─── B4: dropping the stream must stop the run ──────────────────────────────

/// The task spawned by `run_stream` owns an `Arc<Agent>`. Dropping the stream
/// only closed the receiver, and every send site swallows the failure with
/// `let _ =`, so the agent kept driving the provider — and spending — with
/// nobody listening.
#[tokio::test]
async fn dropping_the_stream_cancels_the_run() {
    let (url, _bodies) = serve_recording(vec![
        Canned::sse_tool_call("call_1", "Read", &json!({ "file_path": "Cargo.toml" })),
        Canned::sse_text("done"),
        Canned::sse_text("done"),
        Canned::sse_text("done"),
    ]);

    let agent = Arc::new(agent_against(&url));
    let token = {
        let mut stream = agent.run_stream("start");
        // Wait until the run is genuinely under way, so the drop lands mid-run.
        let first = tokio::time::timeout(Duration::from_secs(5), stream.next())
            .await
            .expect("first event should arrive");
        assert!(first.is_some(), "stream ended before it started");
        let token = agent.cancel_token();
        drop(stream);
        token
    };

    tokio::time::timeout(Duration::from_secs(5), token.cancelled())
        .await
        .expect("dropping the AgentStream left the run going");
}

/// Drop-cancel plus a reused token is a trap: if two runs share one token, a
/// stale handle dropped after the *next* run started would kill that run. Each
/// run must therefore get its own token.
#[tokio::test]
async fn dropping_an_old_stream_does_not_kill_a_newer_run() {
    let (url, _bodies) = serve_recording(vec![
        Canned::sse_text("one"),
        Canned::sse_tool_call("call_1", "Read", &json!({ "file_path": "Cargo.toml" })),
        Canned::sse_text("two"),
        Canned::sse_text("two"),
    ]);

    let agent = Arc::new(agent_against(&url));

    // First run, driven to completion but with its handle deliberately kept.
    let mut first = agent.run_stream("first");
    while let Some(e) = first.next().await {
        if matches!(e, AgentEvent::Complete(_) | AgentEvent::Error(_)) {
            break;
        }
    }

    // Second run starts while the first handle is still alive...
    let second = agent.run_stream("second");
    // ...and the stale handle is dropped afterwards.
    drop(first);

    let out = tokio::time::timeout(Duration::from_secs(10), second.collect())
        .await
        .expect("second run hung");
    assert!(
        out.is_ok(),
        "dropping the first stream cancelled the second run — the two runs \
         shared a cancellation token: {out:?}"
    );
}

/// A builder-supplied token means *shutdown*: it must stop the run in flight
/// and every run started after it, rather than being swallowed by the per-run
/// token swap.
#[tokio::test]
async fn a_builder_supplied_token_shuts_down_later_runs_too() {
    let (url, _bodies) = serve_recording(vec![Canned::sse_text("one"), Canned::sse_text("two")]);
    let shutdown = tokio_util::sync::CancellationToken::new();

    let agent = Arc::new(
        Agent::builder()
            .provider(provider_against(&url, "gpt-4o"))
            .working_dir(".")
            .model("gpt-4o")
            .max_turns(2)
            .max_tokens(64)
            .cancel_token(shutdown.clone())
            .build()
            .expect("build agent"),
    );

    assert!(agent.run_stream("first").collect().await.is_ok());

    shutdown.cancel();

    assert!(
        agent.run_stream("second").collect().await.is_err(),
        "a cancelled shutdown token did not stop a later run"
    );
}

/// The converse: cancelling a *run* must not cancel the shutdown token, or one
/// Ctrl+C would take the process down with it.
#[tokio::test]
async fn cancelling_a_run_leaves_the_shutdown_token_alone() {
    let (url, _seen) = serve_hanging();
    let shutdown = tokio_util::sync::CancellationToken::new();

    let agent = Arc::new(
        Agent::builder()
            .provider(provider_against(&url, "gpt-4o"))
            .working_dir(".")
            .model("gpt-4o")
            .max_turns(2)
            .max_tokens(64)
            .cancel_token(shutdown.clone())
            .build()
            .expect("build agent"),
    );

    let stream = agent.run_stream("hello");
    tokio::time::sleep(Duration::from_millis(200)).await;
    stream.cancel();

    assert!(
        !shutdown.is_cancelled(),
        "cancelling one run cancelled the process-wide shutdown token"
    );
}

/// `detach()` is the deliberate opt-out from drop-cancel: the run outlives the
/// handle on purpose.
#[tokio::test]
async fn detach_opts_out_of_drop_cancellation() {
    let (url, bodies) = serve_recording(vec![
        Canned::sse_tool_call("call_1", "Read", &json!({ "file_path": "Cargo.toml" })),
        Canned::sse_text("done"),
        Canned::sse_text("done"),
    ]);

    let agent = Arc::new(agent_against(&url));
    let stream = agent.run_stream("start");
    tokio::time::sleep(Duration::from_millis(300)).await;
    stream.detach();

    // The detached run must keep going and reach a second turn on its own.
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert!(
        bodies.lock().unwrap().len() >= 2,
        "a detached run stopped when its handle was dropped"
    );
    assert!(
        !agent.cancel_token().is_cancelled(),
        "detach() still cancelled the run"
    );
}

/// `collect()` consumes the handle and returns once the terminal event lands,
/// so its drop-cancel necessarily fires on an already-finished run. Guard
/// against a future refactor turning that into a truncated result.
#[tokio::test]
async fn collect_returns_the_full_output_despite_drop_cancellation() {
    let (url, _bodies) = serve_recording(vec![
        Canned::sse_tool_call("call_1", "Read", &json!({ "file_path": "Cargo.toml" })),
        Canned::sse_text("all done"),
        Canned::sse_text("all done"),
    ]);

    let agent = Arc::new(agent_against(&url));
    let out = agent
        .run_stream("start")
        .collect()
        .await
        .expect("collect should return the completed output");

    assert!(out.turns >= 2, "expected a multi-turn run, got {}", out.turns);
    assert!(
        out.tool_calls.iter().any(|c| c.name == "Read"),
        "the tool call is missing from the collected output"
    );
}

// ─── E1: ergonomic entry points ─────────────────────────────────────────────

/// `run_stream` takes `self: &Arc<Self>` but `build()` returns `Agent`, so
/// every caller wrote the `Arc::new` dance. `stream_with` is the streaming twin
/// of `run_with`.
#[tokio::test]
async fn builder_stream_with_streams_without_a_manual_arc() {
    let (url, _bodies) = serve_recording(vec![Canned::sse_text("hello from stream_with")]);

    let text = Agent::builder()
        .provider(provider_against(&url, "gpt-4o"))
        .working_dir(".")
        .model("gpt-4o")
        .max_turns(2)
        .max_tokens(64)
        .stream_with("hi")
        .expect("stream_with should build and start the stream")
        .collect_text()
        .await
        .expect("collect_text");

    assert_eq!(text, "hello from stream_with");
}

/// `into_stream` covers the case where the caller already has an owned `Agent`.
#[tokio::test]
async fn into_stream_consumes_an_owned_agent() {
    let (url, _bodies) = serve_recording(vec![Canned::sse_text("owned")]);

    let text = agent_against(&url)
        .into_stream("hi")
        .collect_text()
        .await
        .expect("collect_text");

    assert_eq!(text, "owned");
}

/// `build_shared` is for callers that keep the agent across several streamed
/// turns.
#[tokio::test]
async fn build_shared_returns_a_reusable_arc_agent() {
    let (url, _bodies) = serve_recording(vec![Canned::sse_text("one"), Canned::sse_text("two")]);

    let agent = Agent::builder()
        .provider(provider_against(&url, "gpt-4o"))
        .working_dir(".")
        .model("gpt-4o")
        .max_turns(2)
        .max_tokens(64)
        .build_shared()
        .expect("build_shared");

    assert_eq!(agent.run_stream("a").collect_text().await.unwrap(), "one");
    assert_eq!(agent.run_stream("b").collect_text().await.unwrap(), "two");
}
