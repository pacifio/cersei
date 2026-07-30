//! The seven wire pathologies from `joy/fake_sse_server.py`, as real tests.
//!
//! These drive the actual `openai.rs` + `stream.rs` code paths against an
//! in-process socket — nothing on the Cersei side is stubbed. They exist so the
//! F-03 / F-A2 / F-A3 / H2c / P1 guarantees are covered by `cargo test` rather
//! than only by an out-of-tree probe that has to be started by hand, and so
//! deleting `joy/` costs no coverage.
//!
//! What each case pins down:
//!   normal          CONTROL — two parallel calls, correct `index`, `[DONE]`
//!   no_done         F-03    — identical stream, EOF without the sentinel
//!   no_index        F-A2    — `index` field absent; must not collapse to slot 0
//!   empty_id        F-A3    — empty id/name is unusable and must be rejected
//!   len_stop        H2c     — truncated call + finish_reason "length" -> MaxTokens
//!   len_stop_valid  P1-BLOCKER — a COMPLETE call + "length" must stay ToolUse
//!   mixed_bad       P1-HIGH — one bad sibling must not destroy the good call

use cersei_provider::{CompletionRequest, OpenAi, Provider};
use cersei_types::*;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};

// ─── Wire fixtures (mirror of fake_sse_server.py) ────────────────────────────

fn chunk(delta: &str, finish: Option<&str>, usage: bool) -> String {
    let finish = match finish {
        Some(f) => format!("\"{f}\""),
        None => "null".to_string(),
    };
    let usage = if usage {
        ",\"usage\":{\"prompt_tokens\":10,\"completion_tokens\":5,\"total_tokens\":15}"
    } else {
        ""
    };
    format!(
        "data: {{\"id\":\"chatcmpl-x\",\"object\":\"chat.completion.chunk\",\
         \"model\":\"fake\",\"choices\":[{{\"index\":0,\"delta\":{delta},\
         \"finish_reason\":{finish}}}]{usage}}}\n\n"
    )
}

/// One `tool_calls` delta. `index` is omitted entirely when `with_index` is false.
fn tool_call(index: usize, id: &str, name: &str, args: &str, with_index: bool) -> String {
    let idx = if with_index {
        format!("\"index\":{index},")
    } else {
        String::new()
    };
    // `args` is embedded as a JSON *string*, so its quotes must be escaped.
    let args_escaped = args.replace('\\', "\\\\").replace('"', "\\\"");
    format!(
        "{{\"tool_calls\":[{{{idx}\"id\":\"{id}\",\"type\":\"function\",\
         \"function\":{{\"name\":\"{name}\",\"arguments\":\"{args_escaped}\"}}}}]}}"
    )
}

const DONE: &str = "data: [DONE]\n\n";

fn stream_for(mode: &str) -> String {
    let call_a = tool_call(0, "call_a", "Read", r#"{"file_path":"/a.rs"}"#, true);
    let call_b = tool_call(1, "call_b", "Read", r#"{"file_path":"/b.rs"}"#, true);
    match mode {
        "normal" => format!("{}{}{DONE}", chunk(&call_a, None, false), chunk(&call_b, None, false)),
        // F-03: byte-identical to `normal` minus the sentinel.
        "no_done" => format!("{}{}", chunk(&call_a, None, false), chunk(&call_b, None, false)),
        "no_index" => {
            let a = tool_call(0, "call_a", "Read", r#"{"file_path":"/a.rs"}"#, false);
            let b = tool_call(1, "call_b", "Read", r#"{"file_path":"/b.rs"}"#, false);
            format!("{}{}{DONE}", chunk(&a, None, false), chunk(&b, None, false))
        }
        "empty_id" => {
            let bad = tool_call(0, "", "", r#"{"file_path":"/a.rs"}"#, true);
            format!("{}{DONE}", chunk(&bad, None, false))
        }
        // Arguments cut mid-string: unparseable, so the call is not executable.
        "len_stop" => {
            let truncated = tool_call(0, "call_a", "Read", r#"{"file_path":"/a"#, true);
            format!(
                "{}{}{DONE}",
                chunk(&truncated, None, false),
                chunk("{}", Some("length"), true)
            )
        }
        // Same shape, but the call is COMPLETE — the cap was hit on the next token.
        "len_stop_valid" => format!(
            "{}{}{DONE}",
            chunk(&call_a, None, false),
            chunk("{}", Some("length"), true)
        ),
        "mixed_bad" => {
            let bad = tool_call(1, "", "", r#"{"file_path":"/b.rs"}"#, true);
            format!("{}{}{DONE}", chunk(&call_a, None, false), chunk(&bad, None, false))
        }
        other => panic!("unknown mode {other}"),
    }
}

// ─── In-process server ───────────────────────────────────────────────────────

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// Read headers + declared body and return the body bytes.
///
/// The body must be drained: closing a socket with unread bytes still queued
/// makes the kernel send RST, which would discard the response we just wrote.
fn read_request(sock: &mut TcpStream) -> Vec<u8> {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 4096];
    let head_end = loop {
        match sock.read(&mut tmp) {
            Ok(0) | Err(_) => return Vec::new(),
            Ok(n) => buf.extend_from_slice(&tmp[..n]),
        }
        if let Some(p) = find(&buf, b"\r\n\r\n") {
            break p + 4;
        }
    };
    let head = String::from_utf8_lossy(&buf[..head_end]).to_lowercase();
    let len = head
        .lines()
        .find_map(|l| l.strip_prefix("content-length:"))
        .and_then(|v| v.trim().parse::<usize>().ok())
        .unwrap_or(0);
    while buf.len() < head_end + len {
        match sock.read(&mut tmp) {
            Ok(0) | Err(_) => break,
            Ok(n) => buf.extend_from_slice(&tmp[..n]),
        }
    }
    buf[head_end.min(buf.len())..].to_vec()
}

/// Serve exactly one request, choosing the pathology from the request's `model`
/// field — same contract as the python probe, so the fixtures stay comparable.
///
/// No `Content-Length` and no chunked framing: the body is delimited by the
/// connection close. That is what makes `no_done` a genuine EOF-without-sentinel
/// rather than a truncated-but-framed response.
fn serve_one() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        if let Ok((mut sock, _)) = listener.accept() {
            let body = read_request(&mut sock);
            let text = String::from_utf8_lossy(&body);
            let mode = text
                .split("\"model\":\"")
                .nth(1)
                .and_then(|s| s.split('"').next())
                .unwrap_or("normal")
                .to_string();
            let payload = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\
                 Connection: close\r\n\r\n{}",
                stream_for(&mode)
            );
            let _ = sock.write_all(payload.as_bytes());
            let _ = sock.flush();
            let _ = sock.shutdown(std::net::Shutdown::Write);
        }
    });
    format!("http://127.0.0.1:{port}/v1")
}

/// Drive the real OpenAI client in `mode` and collect the finished response.
async fn run(mode: &str) -> Result<cersei_provider::CompletionResponse> {
    let url = serve_one();
    let provider = OpenAi::builder()
        .api_key("test-key")
        .base_url(&url)
        .model(mode) // the model string selects the pathology
        .build()
        .expect("build provider");
    let mut req = CompletionRequest::new(mode);
    req.messages = vec![Message::user("go")];
    req.max_tokens = 64;
    provider.complete(req).await?.collect().await
}

/// (id, name, input) for each emitted tool call.
fn calls(resp: &cersei_provider::CompletionResponse) -> Vec<(String, String, serde_json::Value)> {
    match &resp.message.content {
        MessageContent::Blocks(blocks) => blocks
            .iter()
            .filter_map(|b| match b {
                ContentBlock::ToolUse { id, name, input } => {
                    Some((id.clone(), name.clone(), input.clone()))
                }
                _ => None,
            })
            .collect(),
        _ => vec![],
    }
}

fn assert_two_reads(resp: &cersei_provider::CompletionResponse, case: &str) {
    let c = calls(resp);
    assert_eq!(c.len(), 2, "{case}: expected 2 tool calls, got {c:?}");
    assert_eq!(c[0].0, "call_a", "{case}: first call id");
    assert_eq!(c[1].0, "call_b", "{case}: second call id");
    assert_eq!(c[0].2, serde_json::json!({"file_path": "/a.rs"}), "{case}");
    assert_eq!(c[1].2, serde_json::json!({"file_path": "/b.rs"}), "{case}");
    assert!(
        matches!(resp.stop_reason, StopReason::ToolUse),
        "{case}: dispatchable calls require ToolUse, got {:?}",
        resp.stop_reason
    );
}

// ─── Cases ───────────────────────────────────────────────────────────────────

/// CONTROL. If this breaks, every other case below is meaningless.
#[tokio::test]
async fn normal_two_parallel_calls_survive() {
    let resp = run("normal").await.expect("control stream must parse");
    assert_two_reads(&resp, "normal");
}

/// F-03: the sentinel is the provider's courtesy, not a guarantee. A stream that
/// ends at EOF must still yield the calls it already delivered — flushing only on
/// `[DONE]` silently discarded them and reported a clean, empty success.
#[tokio::test]
async fn f03_eof_without_done_still_yields_the_calls() {
    let resp = run("no_done").await.expect("EOF without [DONE] must not error");
    assert_two_reads(&resp, "no_done");
}

/// F-A2: a missing `index` must not collapse both calls into slot 0, which
/// merged their argument fragments into one corrupt call.
#[tokio::test]
async fn fa2_missing_index_does_not_merge_calls() {
    let resp = run("no_index").await.expect("absent index must not error");
    assert_two_reads(&resp, "no_index");
}

/// F-A3: a call with no id and no name cannot be dispatched or answered. When it
/// is the only call, the turn must fail loudly rather than report success.
#[tokio::test]
async fn fa3_empty_id_and_name_is_rejected() {
    let err = run("empty_id")
        .await
        .expect_err("an unusable sole tool call must not look like success");
    let msg = err.to_string();
    assert!(
        msg.contains("unusable tool call") && msg.contains("slot 0"),
        "the error must name what was wrong and where: {msg}"
    );
}

/// H2c: arguments cut mid-string are not executable, so the provider's own
/// `length` reason stands — and the raw text plus the parse error survive so the
/// model is told what happened instead of receiving `null`.
#[tokio::test]
async fn h2c_truncated_call_reports_maxtokens_and_keeps_the_raw_args() {
    let resp = run("len_stop").await.expect("truncated args must not kill the stream");
    let c = calls(&resp);
    assert_eq!(c.len(), 1, "expected the one truncated call, got {c:?}");
    assert!(
        matches!(resp.stop_reason, StopReason::MaxTokens),
        "nothing dispatchable came out, so MaxTokens stands: {:?}",
        resp.stop_reason
    );
    assert_eq!(
        c[0].2.get("__raw").and_then(|v| v.as_str()),
        Some(r#"{"file_path":"/a"#),
        "F-05: the raw arguments must survive verbatim, got {:?}",
        c[0].2
    );
    assert!(
        c[0].2.get("__parse_error").is_some(),
        "F-05: the parse error must be reported, got {:?}",
        c[0].2
    );
}

/// P1-BLOCKER. `finish_reason` describes how generation *ended*, not what it
/// produced. A complete call plus `length` is still a dispatchable call; if
/// MaxTokens wins, the runner skips dispatch yet still serialises the assistant
/// message WITH `tool_calls`, so the next request carries a tool_call no
/// `role:"tool"` message answers -> 400 -> not retryable -> conversation wedged.
#[tokio::test]
async fn p1_complete_call_with_length_finish_stays_tooluse() {
    let resp = run("len_stop_valid").await.expect("must parse");
    let c = calls(&resp);
    assert_eq!(c.len(), 1, "expected the one valid call, got {c:?}");
    assert_eq!(c[0].0, "call_a");
    assert_eq!(c[0].2, serde_json::json!({"file_path": "/a.rs"}));
    assert!(
        matches!(resp.stop_reason, StopReason::ToolUse),
        "an executable call must be dispatched, got {:?}",
        resp.stop_reason
    );
}

/// P1-HIGH: rejecting a bad sibling must not take the good call down with it.
/// `into_response` short-circuits on the first stream error and never looks at
/// the accumulated blocks, so raising one here would destroy `call_a` and abort
/// an otherwise fine turn.
#[tokio::test]
async fn p1_one_bad_sibling_does_not_destroy_the_good_call() {
    let resp = run("mixed_bad")
        .await
        .expect("a bad sibling must not turn the whole turn into an Err");
    let c = calls(&resp);
    assert_eq!(c.len(), 1, "call_a must survive alone, got {c:?}");
    assert_eq!(c[0].0, "call_a");
    assert_eq!(c[0].2, serde_json::json!({"file_path": "/a.rs"}));
    assert!(
        matches!(resp.stop_reason, StopReason::ToolUse),
        "the surviving call must still be dispatched, got {:?}",
        resp.stop_reason
    );
    // The loss is reported in-band so the model can re-issue what was dropped.
    let text = resp.message.get_all_text();
    assert!(
        text.contains("dropped") && text.contains("unusable"),
        "the dropped sibling must be reported in-band, got text {text:?}"
    );
}
