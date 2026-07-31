//! B1 wiring tests: the request BODY each provider actually sends carries
//! `adapt_tools` output, not the raw `input_schema`.
//!
//! The P0 lesson (§10.3): a tested helper with untested wiring is the
//! recurring trap. `adapt.rs` has its own unit tests; these tests instead
//! capture the literal HTTP body through the real `complete()` path — the
//! same in-process-socket technique as `sse_pathologies.rs` — so reverting
//! the seam call at either site, or swapping its dialect, fails here.
//!
//! The Anthropic/Vertex site is not in this file: it lives inside the pure
//! `build_anthropic_body`, and is bound by body-shape tests in
//! `anthropic.rs::tests`.

use cersei_provider::{from_model_string, CompletionRequest, Gemini, OpenAi, Provider};
use cersei_types::*;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::mpsc;

// ─── In-process capture server ───────────────────────────────────────────────

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// Read headers + declared body; return only the body bytes. Draining matters:
/// closing with unread bytes queued makes the kernel RST the connection.
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

/// Serve one request: hand its body to the test, answer with `sse` over 200.
fn capture_one(sse: &'static str) -> (String, mpsc::Receiver<Vec<u8>>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().unwrap().port();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        if let Ok((mut sock, _)) = listener.accept() {
            let body = read_request(&mut sock);
            let _ = tx.send(body);
            let payload = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\
                 Connection: close\r\n\r\n{sse}"
            );
            let _ = sock.write_all(payload.as_bytes());
            let _ = sock.flush();
            let _ = sock.shutdown(std::net::Shutdown::Write);
        }
    });
    (format!("http://127.0.0.1:{port}"), rx)
}

// ─── Fixture ─────────────────────────────────────────────────────────────────

/// The schemars-0.8 shape Exp 1 measured Gemini rejecting: `$schema` at the
/// root, a `$ref` into `definitions`, and `additionalProperties` — plus an
/// `enum`, which every dialect must leave alone.
fn schemars_like_request() -> CompletionRequest {
    let mut req = CompletionRequest::new("test-model");
    req.messages = vec![Message::user("go")];
    req.max_tokens = 64;
    req.tools = vec![ToolDefinition {
        name: "Read".to_string(),
        description: "Reads a file".to_string(),
        input_schema: serde_json::json!({
            "$schema": "http://json-schema.org/draft-07/schema#",
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "file_path": { "type": "string" },
                "mode": { "enum": ["full", "head"] },
                "range": { "$ref": "#/definitions/Range" },
            },
            "required": ["file_path"],
            "definitions": {
                "Range": {
                    "type": "object",
                    "properties": { "start": { "type": "integer" } },
                }
            }
        }),
    }];
    req
}

/// True if `key` appears as an object key anywhere in the tree.
fn contains_key(v: &serde_json::Value, key: &str) -> bool {
    match v {
        serde_json::Value::Object(m) => {
            m.contains_key(key) || m.values().any(|v| contains_key(v, key))
        }
        serde_json::Value::Array(items) => items.iter().any(|v| contains_key(v, key)),
        _ => false,
    }
}

fn body_json(rx: &mpsc::Receiver<Vec<u8>>) -> serde_json::Value {
    let bytes = rx
        .recv_timeout(std::time::Duration::from_secs(5))
        .expect("the provider never sent a request");
    serde_json::from_slice(&bytes).expect("request body must be JSON")
}

// ─── OpenAI site: OpenAiLoose ────────────────────────────────────────────────

#[tokio::test]
async fn openai_body_carries_loose_adapted_tools() {
    let (url, rx) = capture_one("data: [DONE]\n\n");
    let provider = OpenAi::builder()
        .api_key("test-key")
        .base_url(format!("{url}/v1"))
        .model("test-model")
        .build()
        .expect("build provider");
    // The response content is irrelevant; only the captured body is asserted.
    let _ = async {
        provider.complete(schemars_like_request()).await?.collect().await
    }
    .await;

    let body = body_json(&rx);
    let params = &body["tools"][0]["function"]["parameters"];
    assert!(!contains_key(params, "$schema"), "{params:#}");
    assert!(!contains_key(params, "$ref"), "{params:#}");
    assert!(!contains_key(params, "definitions"), "{params:#}");
    // Loose ≠ strict: the author's `additionalProperties` and `required`
    // survive untouched, and no `strict` flag is sent until B2 opts in.
    assert_eq!(params["additionalProperties"], serde_json::json!(false));
    assert_eq!(params["required"], serde_json::json!(["file_path"]));
    assert!(body["tools"][0]["function"].get("strict").is_none());
    // The ref was inlined in place, and the enum survived.
    assert_eq!(params["properties"]["range"]["properties"]["start"]["type"], "integer");
    assert!(contains_key(params, "enum"), "{params:#}");
}

/// F-08: `options.tool_choice = "required"` reaches the OpenAI wire as
/// `"tool_choice":"required"` — and the plain request carries no such key
/// (asserted in the test above via `strict`-absence; here we pin the key).
#[tokio::test]
async fn openai_body_carries_forced_tool_choice_only_when_asked() {
    let (url, rx) = capture_one("data: [DONE]\n\n");
    let provider = OpenAi::builder()
        .api_key("test-key")
        .base_url(format!("{url}/v1"))
        .model("test-model")
        .build()
        .expect("build provider");
    let mut req = schemars_like_request();
    req.options.set("tool_choice", "required");
    let _ = async { provider.complete(req).await?.collect().await }.await;

    let body = body_json(&rx);
    assert_eq!(body["tool_choice"], serde_json::json!("required"));
}

#[tokio::test]
async fn openai_body_omits_tool_choice_by_default() {
    let (url, rx) = capture_one("data: [DONE]\n\n");
    let provider = OpenAi::builder()
        .api_key("test-key")
        .base_url(format!("{url}/v1"))
        .model("test-model")
        .build()
        .expect("build provider");
    let _ = async {
        provider.complete(schemars_like_request()).await?.collect().await
    }
    .await;

    let body = body_json(&rx);
    assert!(
        body.get("tool_choice").is_none(),
        "no option ⇒ provider default (auto), no key: {body:#}"
    );
}

/// F-09: a provider flagged `send_num_ctx` puts the runner-supplied window on
/// the wire as `options.num_ctx`; an unflagged one must not — OpenAI proper
/// rejects unknown top-level fields with a 400.
#[tokio::test]
async fn num_ctx_reaches_the_wire_only_on_flagged_providers() {
    // Flagged (the router's Ollama construction): emitted.
    let (url, rx) = capture_one("data: [DONE]\n\n");
    let provider = OpenAi::builder()
        .api_key("no-key")
        .base_url(format!("{url}/v1"))
        .model("qwen2.5-coder")
        .send_num_ctx(true)
        .build()
        .expect("build provider");
    let mut req = schemars_like_request();
    req.options.set("num_ctx", 8_192u64);
    let _ = async { provider.complete(req).await?.collect().await }.await;
    let body = body_json(&rx);
    assert_eq!(body["options"]["num_ctx"], serde_json::json!(8_192));

    // Unflagged (every non-Ollama OpenAI-compatible endpoint): absent.
    let (url, rx) = capture_one("data: [DONE]\n\n");
    let provider = OpenAi::builder()
        .api_key("test-key")
        .base_url(format!("{url}/v1"))
        .model("gpt-4o")
        .build()
        .expect("build provider");
    let mut req = schemars_like_request();
    req.options.set("num_ctx", 8_192u64);
    let _ = async { provider.complete(req).await?.collect().await }.await;
    let body = body_json(&rx);
    assert!(
        body.get("options").is_none(),
        "OpenAI proper 400s on unknown top-level fields: {body:#}"
    );
}

/// F-09, router wiring: the provider `from_model_string("ollama/…")` builds
/// must be the flagged one. This is the call site the flag exists for — a
/// bound builder with an unbound router would repeat the P0 trap.
#[tokio::test]
async fn router_built_ollama_provider_sends_num_ctx() {
    let (url, rx) = capture_one("data: [DONE]\n\n");
    // resolved_api_base honours {ID}_BASE_URL; nothing else in this binary
    // reads it, so the process-global set is safe.
    std::env::set_var("OLLAMA_BASE_URL", format!("{url}/v1"));
    let (provider, model) =
        from_model_string("ollama/qwen2.5-coder:7b").expect("router must build ollama keyless");
    std::env::remove_var("OLLAMA_BASE_URL");
    assert_eq!(model, "qwen2.5-coder:7b");

    let mut req = schemars_like_request();
    req.model = model;
    req.options.set("num_ctx", 8_192u64);
    let _ = async { provider.complete(req).await?.collect().await }.await;

    let body = body_json(&rx);
    assert_eq!(
        body["options"]["num_ctx"],
        serde_json::json!(8_192),
        "build_provider must flag the ollama entry with send_num_ctx: {body:#}"
    );
    // B2: the router-resolved dialect for OpenAI-compatible entries is Loose —
    // no strict flag, and the author's `required` list untouched.
    assert!(
        body["tools"][0]["function"].get("strict").is_none(),
        "quirks must resolve OpenAiLoose here: {body:#}"
    );
    assert_eq!(body["tools"][0]["function"]["parameters"]["required"], serde_json::json!(["file_path"]));
}

/// B2: the serialization site must consume the router-set dialect — a
/// provider built with `OpenAiStrict` puts the strict shape on the wire.
#[tokio::test]
async fn openai_dialect_field_is_load_bearing() {
    let (url, rx) = capture_one("data: [DONE]\n\n");
    let provider = OpenAi::builder()
        .api_key("test-key")
        .base_url(format!("{url}/v1"))
        .model("test-model")
        .dialect(cersei_provider::SchemaDialect::OpenAiStrict)
        .build()
        .expect("build provider");
    let _ = async {
        provider.complete(schemars_like_request()).await?.collect().await
    }
    .await;

    let body = body_json(&rx);
    assert_eq!(body["tools"][0]["function"]["strict"], serde_json::json!(true));
    assert_eq!(
        body["tools"][0]["function"]["parameters"]["additionalProperties"],
        serde_json::json!(false)
    );
}

// ─── Gemini site: GeminiSubset ───────────────────────────────────────────────

#[tokio::test]
async fn gemini_body_strips_every_key_the_api_rejects() {
    let (url, rx) = capture_one(
        "data: {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"ok\"}],\
         \"role\":\"model\"},\"finishReason\":\"STOP\"}]}\n\n",
    );
    let provider = Gemini::builder()
        .api_key("test-key")
        .base_url(url)
        .model("test-model")
        .build()
        .expect("build provider");
    let _ = async {
        provider.complete(schemars_like_request()).await?.collect().await
    }
    .await;

    let body = body_json(&rx);
    let decl = &body["tools"][0]["functionDeclarations"][0];
    assert_eq!(decl["name"], "Read");
    let params = &decl["parameters"];
    // Exp 1/3: any of these four keys 400s the WHOLE request — every tool in
    // the turn — with INVALID_ARGUMENT.
    for key in ["$schema", "$ref", "definitions", "additionalProperties"] {
        assert!(
            !contains_key(params, key),
            "`{key}` reached the Gemini wire body — the request would die: {params:#}"
        );
    }
    // And the measured-accepted constructs are not over-stripped.
    assert!(contains_key(params, "enum"), "{params:#}");
    assert_eq!(params["properties"]["range"]["properties"]["start"]["type"], "integer");
    assert_eq!(params["required"], serde_json::json!(["file_path"]));
    // F-08: no forced tool choice unless asked.
    assert!(body.get("toolConfig").is_none(), "{body:#}");
}

/// F-08: `options.tool_choice = "required"` reaches the Gemini wire as
/// `functionCallingConfig.mode: "ANY"`.
#[tokio::test]
async fn gemini_body_carries_mode_any_when_asked() {
    let (url, rx) = capture_one(
        "data: {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"ok\"}],\
         \"role\":\"model\"},\"finishReason\":\"STOP\"}]}\n\n",
    );
    let provider = Gemini::builder()
        .api_key("test-key")
        .base_url(url)
        .model("test-model")
        .build()
        .expect("build provider");
    let mut req = schemars_like_request();
    req.options.set("tool_choice", "required");
    let _ = async { provider.complete(req).await?.collect().await }.await;

    let body = body_json(&rx);
    assert_eq!(
        body["toolConfig"]["functionCallingConfig"]["mode"],
        serde_json::json!("ANY")
    );
}

// ─── P3: the dynamic-boundary marker never reaches a wire ───────────────────
//
// The Anthropic path SPLITS at the marker (bound in `anthropic.rs::tests`);
// OpenAI and Gemini have automatic caching and just strip it — before P3 the
// literal `__SYSTEM_PROMPT_DYNAMIC_BOUNDARY__` string went to the model.

#[tokio::test]
async fn openai_body_strips_the_dynamic_boundary_marker() {
    let (url, rx) = capture_one("data: [DONE]\n\n");
    let provider = OpenAi::builder()
        .api_key("test-key")
        .base_url(format!("{url}/v1"))
        .model("test-model")
        .build()
        .expect("build provider");
    let mut req = schemars_like_request();
    req.system = Some(format!(
        "stable half\n{}\ndynamic half",
        cersei_types::SYSTEM_PROMPT_DYNAMIC_BOUNDARY
    ));
    let _ = async { provider.complete(req).await?.collect().await }.await;

    let body = body_json(&rx);
    let system = body["messages"][0]["content"].as_str().expect("system message");
    assert!(system.contains("stable half") && system.contains("dynamic half"));
    assert!(
        !system.contains(cersei_types::SYSTEM_PROMPT_DYNAMIC_BOUNDARY),
        "the marker leaked to the OpenAI wire: {system}"
    );
}

#[tokio::test]
async fn gemini_body_strips_the_dynamic_boundary_marker() {
    let (url, rx) = capture_one(
        "data: {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"ok\"}],\
         \"role\":\"model\"},\"finishReason\":\"STOP\"}]}\n\n",
    );
    let provider = Gemini::builder()
        .api_key("test-key")
        .base_url(url)
        .model("test-model")
        .build()
        .expect("build provider");
    let mut req = schemars_like_request();
    req.system = Some(format!(
        "stable half\n{}\ndynamic half",
        cersei_types::SYSTEM_PROMPT_DYNAMIC_BOUNDARY
    ));
    let _ = async { provider.complete(req).await?.collect().await }.await;

    let body = body_json(&rx);
    let text = body["systemInstruction"]["parts"][0]["text"]
        .as_str()
        .expect("systemInstruction text");
    assert!(text.contains("stable half") && text.contains("dynamic half"));
    assert!(
        !text.contains(cersei_types::SYSTEM_PROMPT_DYNAMIC_BOUNDARY),
        "the marker leaked to the Gemini wire: {text}"
    );
}
