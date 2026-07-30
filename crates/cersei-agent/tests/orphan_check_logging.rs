//! F-04b: the pre-request orphan check must actually be consulted.
//!
//! `compact::find_orphaned_tool_results` has its own unit tests, but the
//! mutation audit (TOOL-CALLING-RELIABILITY.md §10.3) showed the runner call
//! site — the `tracing::error!` fired before every request — could be deleted
//! with the suite green. The check's entire output is that log line, so the
//! test binds it by capturing tracing output while the real runner sends a
//! request whose history carries a severed pair.
//!
//! This lives in its own integration-test binary because it installs a global
//! tracing subscriber, which can only be done once per process.

use cersei_agent::Agent;
use cersei_provider::OpenAi;
use cersei_types::{ContentBlock, Message, ToolResultContent};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};

// ─── Minimal canned SSE server (see retry_on_429.rs for the pattern) ─────────

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
async fn a_request_carrying_an_orphaned_tool_result_is_reported_before_send() {
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
        // A history whose tool_result answers a tool_use that no longer
        // exists — what a bad compaction slice produces.
        .with_messages(vec![
            Message::user("earlier question"),
            Message::assistant("earlier answer"),
            Message::user_blocks(vec![ContentBlock::ToolResult {
                tool_use_id: "ghost_tool_use".into(),
                content: ToolResultContent::Text("stale result".into()),
                is_error: Some(false),
            }]),
        ])
        .build()
        .expect("build agent");

    let _ = agent.run("hello").await;

    let logs = String::from_utf8_lossy(&captured.lock().unwrap()).to_string();
    assert!(
        logs.contains("no matching tool_use"),
        "the pre-request orphan check never fired: a request carrying a \
         severed tool_result went out unreported. Captured ERROR logs:\n{logs}"
    );
    assert!(
        logs.contains("ghost_tool_use"),
        "the report must name the orphaned id so the cause is debuggable \
         from the log alone. Captured:\n{logs}"
    );
}
