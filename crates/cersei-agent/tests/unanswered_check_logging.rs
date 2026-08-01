//! §10.5 #3: the mirror-rule check must actually be consulted.
//!
//! `compact::find_unanswered_tool_uses` covers the direction
//! `find_orphaned_tool_results` never did: an assistant `tool_use` whose
//! `tool_result` is missing from the request — the same unretryable provider
//! 400, arrived at from the other side of the pair. As with F-04b, the
//! check's entire output is a `tracing::error!` line, so the test binds the
//! runner call site by capturing tracing output while the real runner sends a
//! request whose history carries an unanswered call.
//!
//! This lives in its own integration-test binary because it installs a global
//! tracing subscriber, which can only be done once per process.

use cersei_agent::Agent;
use cersei_provider::OpenAi;
use cersei_types::{ContentBlock, Message};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};

// ─── Minimal canned SSE server (see orphan_check_logging.rs) ─────────────────

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

fn serve_text_replies() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        let body = concat!(
            "data: {\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"ok\"}}]}\n\n",
            "data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}],\
             \"usage\":{\"prompt_tokens\":3,\"completion_tokens\":1,\"total_tokens\":4}}\n\n",
            "data: [DONE]\n\n",
        );
        while let Ok((mut sock, _)) = listener.accept() {
            drain_http_request(&mut sock);
            let payload = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\
                 Content-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            let _ = sock.write_all(payload.as_bytes());
            let _ = sock.flush();
            let _ = sock.shutdown(std::net::Shutdown::Write);
        }
    });
    format!("http://127.0.0.1:{port}/v1")
}

// ─── Tracing capture ──────────────────────────────────────────────────────────

#[derive(Clone)]
struct SharedWriter(Arc<Mutex<Vec<u8>>>);

impl Write for SharedWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[tokio::test]
async fn a_request_carrying_an_unanswered_tool_use_is_reported_before_send() {
    let captured = Arc::new(Mutex::new(Vec::new()));
    let writer = SharedWriter(captured.clone());
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::ERROR)
        .with_writer(move || writer.clone())
        .with_ansi(false)
        .init();

    let url = serve_text_replies();
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
        // A history whose assistant tool_use was never answered — what a
        // compaction slice that severed the *following* user message produces.
        .with_messages(vec![
            Message::user("read the file"),
            Message::assistant_blocks(vec![ContentBlock::ToolUse {
                id: "ghost_call".into(),
                name: "Read".into(),
                input: serde_json::json!({ "file_path": "/x" }),
            }]),
            Message::user("actually, never mind"),
        ])
        .build()
        .expect("build agent");

    let _ = agent.run("hello").await;

    let logs = String::from_utf8_lossy(&captured.lock().unwrap()).to_string();
    assert!(
        logs.contains("no matching tool_result"),
        "the mirror-rule check never fired: a request carrying an unanswered \
         tool_use went out unreported. Captured logs:\n{logs}"
    );
    assert!(
        logs.contains("ghost_call"),
        "the log must name the offending tool_use id so the cause is \
         diagnosable from the log alone:\n{logs}"
    );
}
