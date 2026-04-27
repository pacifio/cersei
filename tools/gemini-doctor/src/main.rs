//! Gemini connectivity doctor.
//!
//! Six checks in order — first failure stops the run. The point is to isolate
//! WHERE network behaviour is breaking down: DNS, TLS, auth, non-stream POST,
//! streaming first-byte, streaming full-body. The agent loop hits the same
//! endpoint with the same headers, so if all six pass we know the bench
//! environment is healthy.
//!
//! Reads `GOOGLE_API_KEY` from env. Never logs the key. Header auth only
//! (`x-goog-api-key`) — never URL query string.

use anyhow::{anyhow, Context, Result};
use clap::Parser;
use futures::StreamExt;
use std::time::{Duration, Instant};

const BASE: &str = "https://generativelanguage.googleapis.com/v1beta";

#[derive(Parser, Debug)]
#[command(name = "gemini-doctor", about = "Check Gemini API health from this machine.")]
struct Cli {
    /// Model to probe.
    #[arg(long, default_value = "gemini-3.1-pro-preview")]
    model: String,

    /// Per-request HTTP timeout in seconds.
    #[arg(long, default_value_t = 60)]
    timeout: u64,

    /// Skip the streaming check (saves ~10s if you've already validated streams).
    #[arg(long, default_value_t = false)]
    no_stream: bool,

    /// Number of streaming probes to run back-to-back. Helps spot intermittent
    /// hangs that only show up on the 2nd or 3rd connection.
    #[arg(long, default_value_t = 1)]
    stream_repeat: u32,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    println!("→ gemini-doctor");
    println!("   base:    {BASE}");
    println!("   model:   {}", cli.model);
    println!("   timeout: {}s", cli.timeout);
    println!();

    let key = std::env::var("GOOGLE_API_KEY")
        .map_err(|_| anyhow!("GOOGLE_API_KEY not set in env (.env is gitignored — `source .env`)"))?;
    println!("✓ env  GOOGLE_API_KEY present (len={}, masked)", key.len());

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(cli.timeout))
        .connect_timeout(Duration::from_secs(20))
        .pool_idle_timeout(Duration::from_secs(60))
        .tcp_keepalive(Duration::from_secs(30))
        .build()
        .context("build http client")?;

    // 1. DNS + TLS + cleartext HEAD on base URL ─────────────────────────────
    let t = Instant::now();
    let resp = client
        .get(format!("{BASE}/models"))
        .header("x-goog-api-key", &key)
        .send()
        .await
        .context("network: list models")?;
    let dt = t.elapsed();
    let status = resp.status();
    let _body = resp.text().await.unwrap_or_default();
    println!(
        "✓ http GET /models                  status={} {:>6}ms",
        status,
        dt.as_millis()
    );
    if !status.is_success() {
        return Err(anyhow!(
            "list models returned non-2xx — most likely API key invalid or VPN blocking"
        ));
    }

    // 2. Single-model lookup (auth roundtrip) ───────────────────────────────
    let t = Instant::now();
    let resp = client
        .get(format!("{BASE}/models/{}", cli.model))
        .header("x-goog-api-key", &key)
        .send()
        .await
        .context("network: model lookup")?;
    let dt = t.elapsed();
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    println!(
        "✓ http GET /models/{}            status={} {:>6}ms",
        truncate(&cli.model, 18),
        status,
        dt.as_millis()
    );
    if !status.is_success() {
        return Err(anyhow!(
            "model lookup returned non-2xx — model name may be wrong or unavailable in your region. body: {}",
            redact(&body, &key)
        ));
    }

    // 3. Non-streaming generateContent ──────────────────────────────────────
    //
    // gemini-3.1-pro-preview reasons internally before emitting visible text;
    // a tiny `maxOutputTokens` budget gets consumed entirely by thinking and
    // no `text` part is ever produced. Allow 1024 so we always see real output
    // and turn thinking off explicitly so the test doesn't get charged for
    // 800+ thinking tokens just to say "PONG".
    let t = Instant::now();
    let req = serde_json::json!({
        "contents": [{
            "role": "user",
            "parts": [{ "text": "Reply with exactly: PONG" }]
        }],
        "generationConfig": { "temperature": 0.0, "maxOutputTokens": 4096 }
    });
    let resp = client
        .post(format!("{BASE}/models/{}:generateContent", cli.model))
        .header("x-goog-api-key", &key)
        .header("content-type", "application/json")
        .json(&req)
        .send()
        .await
        .context("network: generateContent")?;
    let dt = t.elapsed();
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    println!(
        "✓ post     :generateContent          status={} {:>6}ms",
        status,
        dt.as_millis()
    );
    if !status.is_success() {
        return Err(anyhow!(
            "generateContent returned non-2xx. body: {}",
            redact(&body, &key)
        ));
    }
    match extract_text(&body) {
        Some(s) => println!("   model replied: {:?}", truncate(s.trim(), 60)),
        None => {
            println!("⚠  no `text` part in response — full structure:");
            // Pretty-print the candidates section so we can see whether the
            // response is all thought parts, blocked, or shaped differently.
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&body) {
                let pretty = serde_json::to_string_pretty(
                    v.pointer("/candidates").unwrap_or(&v),
                )
                .unwrap_or_else(|_| body.clone());
                for line in pretty.lines().take(40) {
                    println!("   | {}", line);
                }
            } else {
                println!("   | {}", redact(&body, &key));
            }
            return Err(anyhow!(
                "generateContent returned 200 but no visible text — see structure above"
            ));
        }
    }

    if cli.no_stream {
        println!("\n— skipping streaming probe (--no-stream)");
        println!("✓ all checks passed");
        return Ok(());
    }

    // 4-N. Streaming generateContent ────────────────────────────────────────
    for i in 1..=cli.stream_repeat {
        let label = if cli.stream_repeat > 1 {
            format!(" #{i}/{}", cli.stream_repeat)
        } else {
            String::new()
        };
        println!("\n→ streaming probe{label}");
        let t = Instant::now();
        let resp = client
            .post(format!(
                "{BASE}/models/{}:streamGenerateContent?alt=sse",
                cli.model
            ))
            .header("x-goog-api-key", &key)
            .header("content-type", "application/json")
            // Long enough output that, if the VPN is *not* buffering, we'll
            // get many SSE chunks separated in time. If we see ONE big chunk
            // arrive at the end, the VPN/proxy is buffering — that's exactly
            // what stalls the cersei agent loop on long generations.
            .json(&serde_json::json!({
                "contents": [{
                    "role": "user",
                    "parts": [{ "text":
                        "Count from 1 to 50, each number on its own line. Output \
                         only the numbers, nothing else." }]
                }],
                "generationConfig": { "temperature": 0.0, "maxOutputTokens": 4096 }
            }))
            .send()
            .await
            .context("network: streamGenerateContent")?;
        let status = resp.status();
        let connect_ms = t.elapsed().as_millis();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(anyhow!(
                "streamGenerateContent returned non-2xx (status={status} after {connect_ms}ms). body: {}",
                redact(&body, &key)
            ));
        }
        println!(
            "  connect: status={status} {:>6}ms (response headers received)",
            connect_ms
        );

        let mut bytes = resp.bytes_stream();
        let mut chunks = 0usize;
        let mut total_bytes = 0usize;
        let mut first_chunk_ms: Option<u128> = None;
        let stream_started = Instant::now();
        while let Some(chunk) = bytes.next().await {
            let chunk = chunk.context("stream chunk")?;
            if first_chunk_ms.is_none() {
                first_chunk_ms = Some(stream_started.elapsed().as_millis());
            }
            chunks += 1;
            total_bytes += chunk.len();
        }
        let total_ms = stream_started.elapsed().as_millis();
        println!(
            "  first-chunk: {:>6}ms   chunks={chunks}   bytes={total_bytes}   total: {:>6}ms",
            first_chunk_ms.unwrap_or(0),
            total_ms
        );
        if chunks == 0 {
            return Err(anyhow!(
                "stream produced ZERO chunks — VPN / proxy is likely buffering the response and dropping it"
            ));
        }
        if first_chunk_ms.unwrap_or(0) > 30_000 {
            println!(
                "⚠  first chunk took {}ms — VPN likely buffering. Agent thinks it's hung.",
                first_chunk_ms.unwrap()
            );
        }
    }

    println!("\n✓ all checks passed");
    Ok(())
}

fn extract_text(body: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(body).ok()?;
    v.pointer("/candidates/0/content/parts/0/text")
        .and_then(|t| t.as_str())
        .map(|s| s.to_string())
}

/// Char-boundary-safe truncate.
fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        return s.to_string();
    }
    let mut end = 0;
    for (i, _) in s.char_indices().take(n) {
        end = i + s[i..].chars().next().map(|c| c.len_utf8()).unwrap_or(0);
    }
    format!("{}…", &s[..end])
}

/// Replace any literal occurrence of the API key (or URL `key=…` style) with
/// a fixed placeholder. Belt-and-braces — we never put the key in a URL, but
/// echoed error bodies from upstream might.
fn redact(s: &str, key: &str) -> String {
    if key.is_empty() {
        return s.to_string();
    }
    s.replace(key, "<redacted>")
}
