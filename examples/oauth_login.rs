//! # Anthropic OAuth Login
//!
//! Native OAuth 2.0 PKCE flow that opens the browser and connects to a
//! Claude Code account. After login, uses the credential to run a simple
//! agent task proving the token works.
//!
//! Supports both auth paths:
//!   - Claude.ai (default) — Bearer token with `user:inference` scope
//!   - Console (`--console`) — exchanges for an API key (`sk-ant-...`)
//!
//! ```bash
//! cargo run --example oauth_login --release
//! cargo run --example oauth_login --release -- --console
//! ```

use cersei::prelude::*;
use std::io::Write as _;
use std::time::Duration;

// ─── OAuth constants ─────────────────────────────────────────────────────────

const CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";
const CONSOLE_AUTHORIZE_URL: &str = "https://platform.claude.com/oauth/authorize";
const CLAUDE_AI_AUTHORIZE_URL: &str = "https://claude.com/cai/oauth/authorize";
const TOKEN_URL: &str = "https://platform.claude.com/v1/oauth/token";
const API_KEY_URL: &str = "https://api.anthropic.com/api/oauth/claude_cli/create_api_key";
const MANUAL_REDIRECT_URL: &str = "https://platform.claude.com/oauth/code/callback";
const SUCCESS_URL: &str = "https://platform.claude.com/oauth/code/success?app=claude-code";

const ALL_SCOPES: &[&str] = &[
    "org:create_api_key",
    "user:profile",
    "user:inference",
    "user:sessions:claude_code",
    "user:mcp_servers",
    "user:file_upload",
];

const INFERENCE_SCOPE: &str = "user:inference";

// ─── Token types ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct OAuthTokens {
    access_token: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    refresh_token: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    expires_at_ms: Option<i64>,
    scopes: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    account_uuid: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    email: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    organization_uuid: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    api_key: Option<String>,
}

impl OAuthTokens {
    fn uses_bearer(&self) -> bool {
        self.scopes.iter().any(|s| s == INFERENCE_SCOPE)
    }

    fn credential(&self) -> Option<(&str, bool)> {
        if self.uses_bearer() {
            Some((&self.access_token, true))
        } else {
            self.api_key.as_deref().map(|k| (k, false))
        }
    }

    fn is_expired(&self) -> bool {
        self.expires_at_ms
            .map(|exp| chrono::Utc::now().timestamp_millis() + 300_000 >= exp)
            .unwrap_or(false)
    }

    fn token_path() -> std::path::PathBuf {
        dirs::home_dir()
            .unwrap_or_else(|| ".".into())
            .join(".claude")
            .join("oauth_tokens.json")
    }

    async fn save(&self) -> anyhow::Result<()> {
        let path = Self::token_path();
        if let Some(p) = path.parent() {
            tokio::fs::create_dir_all(p).await?;
        }
        tokio::fs::write(&path, serde_json::to_string_pretty(self)?).await?;
        Ok(())
    }

    async fn load() -> Option<Self> {
        let path = Self::token_path();
        let content = tokio::fs::read_to_string(&path).await.ok()?;
        serde_json::from_str(&content).ok()
    }
}

#[derive(serde::Deserialize)]
struct TokenExchangeResponse {
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
    expires_in: u64,
    #[serde(default)]
    scope: Option<String>,
    #[serde(default)]
    account: Option<serde_json::Value>,
    #[serde(default)]
    organization: Option<serde_json::Value>,
}

#[derive(serde::Deserialize)]
struct ApiKeyResponse {
    raw_key: Option<String>,
}

// ─── PKCE helpers ────────────────────────────────────────────────────────────

fn generate_verifier() -> String {
    use base64::Engine;
    let mut bytes = [0u8; 32];
    let u1 = uuid::Uuid::new_v4();
    let u2 = uuid::Uuid::new_v4();
    bytes[..16].copy_from_slice(u1.as_bytes());
    bytes[16..].copy_from_slice(u2.as_bytes());
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

fn generate_challenge(verifier: &str) -> String {
    use base64::Engine;
    use sha2::{Digest, Sha256};
    let hash = Sha256::digest(verifier.as_bytes());
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(hash)
}

fn generate_state() -> String {
    generate_verifier() // same algorithm
}

fn build_auth_url(base: &str, challenge: &str, state: &str, port: u16, manual: bool) -> String {
    let redirect = if manual {
        MANUAL_REDIRECT_URL.to_string()
    } else {
        format!("http://localhost:{}/callback", port)
    };
    let scope = ALL_SCOPES.join(" ");

    let mut u = url::Url::parse(base).expect("valid OAuth base URL");
    {
        let mut q = u.query_pairs_mut();
        q.append_pair("code", "true");
        q.append_pair("client_id", CLIENT_ID);
        q.append_pair("response_type", "code");
        q.append_pair("redirect_uri", &redirect);
        q.append_pair("scope", &scope);
        q.append_pair("code_challenge", challenge);
        q.append_pair("code_challenge_method", "S256");
        q.append_pair("state", state);
    }
    u.to_string()
}

// ─── Callback server ─────────────────────────────────────────────────────────

async fn run_callback_server(
    listener: tokio::net::TcpListener,
    expected_state: &str,
) -> anyhow::Result<String> {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    let (mut socket, _) = tokio::time::timeout(Duration::from_secs(120), listener.accept())
        .await
        .map_err(|_| anyhow::anyhow!("Timeout waiting for browser redirect (120s)"))?
        .map_err(|e| anyhow::anyhow!("Accept failed: {}", e))?;

    let (reader, mut writer) = socket.split();
    let mut reader = BufReader::new(reader);

    // Read request line
    let mut request_line = String::new();
    reader.read_line(&mut request_line).await?;

    // Drain headers
    loop {
        let mut header = String::new();
        reader.read_line(&mut header).await?;
        if header.trim().is_empty() {
            break;
        }
    }

    // Parse: GET /callback?code=XXX&state=YYY HTTP/1.1
    let path = request_line
        .split_whitespace()
        .nth(1)
        .unwrap_or("")
        .to_string();
    let parsed = url::Url::parse(&format!("http://localhost{}", path))?;

    let code = parsed
        .query_pairs()
        .find(|(k, _)| k == "code")
        .map(|(_, v)| v.to_string());
    let recv_state = parsed
        .query_pairs()
        .find(|(k, _)| k == "state")
        .map(|(_, v)| v.to_string());

    // Redirect browser to success page
    let response = format!(
        "HTTP/1.1 302 Found\r\nLocation: {}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        SUCCESS_URL
    );
    writer.write_all(response.as_bytes()).await?;

    // Validate state
    if recv_state.as_deref() != Some(expected_state) {
        anyhow::bail!("OAuth state mismatch (possible CSRF)");
    }

    code.ok_or_else(|| anyhow::anyhow!("No authorization code in callback"))
}

// ─── Token exchange ──────────────────────────────────────────────────────────

async fn exchange_code(
    code: &str,
    state: &str,
    verifier: &str,
    port: u16,
) -> anyhow::Result<TokenExchangeResponse> {
    let redirect_uri = format!("http://localhost:{}/callback", port);

    let body = serde_json::json!({
        "grant_type": "authorization_code",
        "code": code,
        "redirect_uri": redirect_uri,
        "client_id": CLIENT_ID,
        "code_verifier": verifier,
        "state": state,
    });

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()?;
    let resp = client
        .post(TOKEN_URL)
        .header("content-type", "application/json")
        .json(&body)
        .send()
        .await?;

    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        anyhow::bail!("Token exchange failed ({}): {}", status, text);
    }

    Ok(resp.json().await?)
}

async fn create_api_key(access_token: &str) -> anyhow::Result<String> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()?;
    let resp = client
        .post(API_KEY_URL)
        .header("authorization", format!("Bearer {}", access_token))
        .send()
        .await?;

    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        anyhow::bail!("API key creation failed ({}): {}", status, text);
    }

    let data: ApiKeyResponse = resp.json().await?;
    data.raw_key
        .ok_or_else(|| anyhow::anyhow!("No API key in response"))
}

// ─── Token refresh ───────────────────────────────────────────────────────────

async fn refresh_token(tokens: &OAuthTokens) -> anyhow::Result<OAuthTokens> {
    let rt = tokens
        .refresh_token
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("No refresh token"))?;

    let body = serde_json::json!({
        "grant_type": "refresh_token",
        "refresh_token": rt,
        "client_id": CLIENT_ID,
        "scope": ALL_SCOPES.join(" "),
    });

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()?;
    let resp = client.post(TOKEN_URL).json(&body).send().await?;

    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        anyhow::bail!("Token refresh failed ({}): {}", status, text);
    }

    let data: TokenExchangeResponse = resp.json().await?;
    let expires_at_ms = chrono::Utc::now().timestamp_millis() + (data.expires_in as i64 * 1000);
    let scopes: Vec<String> = data
        .scope
        .as_deref()
        .unwrap_or("")
        .split_whitespace()
        .map(String::from)
        .collect();

    let mut updated = tokens.clone();
    updated.access_token = data.access_token;
    if let Some(new_rt) = data.refresh_token {
        updated.refresh_token = Some(new_rt);
    }
    updated.expires_at_ms = Some(expires_at_ms);
    updated.scopes = scopes;
    updated.save().await?;
    Ok(updated)
}

// ─── Browser opener ──────────────────────────────────────────────────────────

fn open_browser(url: &str) {
    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("open")
            .arg(url)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
    }
    #[cfg(target_os = "windows")]
    {
        let ps = format!("Start-Process '{}'", url.replace('\'', "''"));
        let _ = std::process::Command::new("powershell")
            .args(["-NoProfile", "-NonInteractive", "-Command", &ps])
            .spawn();
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        let _ = std::process::Command::new("xdg-open").arg(url).spawn();
    }
}

// ─── Login flow ──────────────────────────────────────────────────────────────

async fn login(use_claude_ai: bool) -> anyhow::Result<OAuthTokens> {
    // 1. PKCE
    let verifier = generate_verifier();
    let challenge = generate_challenge(&verifier);
    let state = generate_state();

    // 2. Bind callback server
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let port = listener.local_addr()?.port();

    // 3. Build URLs
    let base = if use_claude_ai {
        CLAUDE_AI_AUTHORIZE_URL
    } else {
        CONSOLE_AUTHORIZE_URL
    };
    let manual_url = build_auth_url(base, &challenge, &state, port, true);
    let auto_url = build_auth_url(base, &challenge, &state, port, false);

    // 4. Open browser
    let mode = if use_claude_ai {
        "Claude.ai"
    } else {
        "Anthropic Console"
    };
    println!("\n  Opening browser for {} authentication...", mode);
    println!("  If the browser did not open, visit:\n");
    println!("  {}\n", manual_url);
    open_browser(&auto_url);

    // 5. Wait for auth code (browser redirect OR manual paste)
    let state_clone = state.clone();
    let (cb_tx, cb_rx) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        let _ = cb_tx.send(run_callback_server(listener, &state_clone).await);
    });

    print!("  Or paste authorization code here: ");
    std::io::stdout().flush().ok();
    let (paste_tx, paste_rx) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        let mut line = String::new();
        if tokio::io::AsyncBufReadExt::read_line(
            &mut tokio::io::BufReader::new(tokio::io::stdin()),
            &mut line,
        )
        .await
        .is_ok()
        {
            let trimmed = line.trim().to_string();
            if !trimmed.is_empty() {
                let _ = paste_tx.send(trimmed);
            }
        }
    });

    let auth_code = tokio::select! {
        result = cb_rx => result??,
        code = paste_rx => code.map_err(|_| anyhow::anyhow!("stdin closed"))?,
        _ = tokio::time::sleep(Duration::from_secs(120)) => {
            anyhow::bail!("Authentication timed out after 120 seconds");
        }
    };

    println!("\n  Authorization code received.");

    // 6. Exchange for tokens
    print!("  Exchanging code for tokens...");
    std::io::stdout().flush().ok();
    let token_resp = exchange_code(&auth_code, &state, &verifier, port).await?;
    println!(" done.");

    let expires_at_ms =
        chrono::Utc::now().timestamp_millis() + (token_resp.expires_in as i64 * 1000);
    let scopes: Vec<String> = token_resp
        .scope
        .as_deref()
        .unwrap_or("")
        .split_whitespace()
        .map(String::from)
        .collect();

    let account_uuid = token_resp
        .account
        .as_ref()
        .and_then(|a| a["uuid"].as_str().map(String::from));
    let email = token_resp
        .account
        .as_ref()
        .and_then(|a| a["email_address"].as_str().map(String::from));
    let org_uuid = token_resp
        .organization
        .as_ref()
        .and_then(|o| o["uuid"].as_str().map(String::from));
    let uses_bearer = scopes.iter().any(|s| s == INFERENCE_SCOPE);

    // 7. Console flow: create API key
    let api_key = if !uses_bearer {
        print!("  Creating API key...");
        std::io::stdout().flush().ok();
        match create_api_key(&token_resp.access_token).await {
            Ok(key) => {
                println!(" done.");
                Some(key)
            }
            Err(e) => {
                println!(" failed: {}", e);
                None
            }
        }
    } else {
        None
    };

    // 8. Build and save tokens
    let tokens = OAuthTokens {
        access_token: token_resp.access_token,
        refresh_token: token_resp.refresh_token,
        expires_at_ms: Some(expires_at_ms),
        scopes,
        account_uuid,
        email: email.clone(),
        organization_uuid: org_uuid,
        api_key,
    };
    tokens.save().await?;

    println!(
        "\n  Authenticated as: {}",
        email.as_deref().unwrap_or("(unknown)")
    );
    println!(
        "  Auth mode: {}",
        if uses_bearer {
            "Bearer (Claude.ai)"
        } else {
            "API Key (Console)"
        }
    );
    println!("  Token saved to: {}", OAuthTokens::token_path().display());
    println!(
        "  Expires: {}",
        chrono::DateTime::from_timestamp_millis(expires_at_ms)
            .map(|dt| dt.to_rfc3339())
            .unwrap_or_else(|| "unknown".into())
    );

    Ok(tokens)
}

// ─── Main ────────────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let use_console = std::env::args().any(|a| a == "--console");
    let force_login = std::env::args().any(|a| a == "--force");

    println!("╔══════════════════════════════════════════════╗");
    println!("║  Cersei — Anthropic OAuth Login              ║");
    println!("╚══════════════════════════════════════════════╝");

    // Check for existing tokens
    let tokens = if force_login {
        None
    } else {
        OAuthTokens::load().await
    };

    let tokens = match tokens {
        Some(mut t) if !t.is_expired() => {
            println!("\n  Found valid cached tokens.");
            if let Some(email) = &t.email {
                println!("  Logged in as: {}", email);
            }
            t
        }
        Some(t) if t.refresh_token.is_some() => {
            println!("\n  Token expired, refreshing...");
            match refresh_token(&t).await {
                Ok(refreshed) => {
                    println!("  Token refreshed successfully.");
                    refreshed
                }
                Err(e) => {
                    println!("  Refresh failed ({}), starting new login.", e);
                    login(!use_console).await?
                }
            }
        }
        _ => login(!use_console).await?,
    };

    // Build provider from the obtained credential
    let (credential, uses_bearer) = tokens
        .credential()
        .ok_or_else(|| anyhow::anyhow!("No usable credential in tokens"))?;

    let auth = if uses_bearer {
        Auth::Bearer(credential.to_string())
    } else {
        Auth::ApiKey(credential.to_string())
    };

    println!("\n  Running a quick agent task to verify the token works...\n");

    // Run a simple agent task to prove it works
    let output = Agent::builder()
        .provider(cersei::Anthropic::new(auth))
        .tools(cersei::tools::filesystem())
        .system_prompt("You are a helpful assistant. Be very brief — one sentence max.")
        .model("claude-sonnet-4-6")
        .max_turns(2)
        .permission_policy(AllowAll)
        .working_dir(".")
        .on_event(|e| {
            if let cersei::events::AgentEvent::TextDelta(t) = e {
                print!("{}", t);
                std::io::stdout().flush().ok();
            }
        })
        .run_with("What is 2+2? Just say the number.")
        .await;

    match output {
        Ok(out) => {
            println!("\n");
            println!("  Agent response: {}", out.text().trim());
            println!(
                "  Tokens used: {}in / {}out",
                out.usage.input_tokens, out.usage.output_tokens
            );
            println!("  Turns: {}", out.turns);
            println!("\n  Authentication verified successfully.");
        }
        Err(e) => {
            println!("\n\n  Agent task failed: {}", e);
            println!("  The token was obtained but may not have sufficient permissions.");
            println!("  Try: cargo run --example oauth_login -- --force");
        }
    }

    Ok(())
}
