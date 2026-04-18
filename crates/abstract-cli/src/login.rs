//! Login and authentication flows.
//!
//! Supports:
//!   - `abstract login`           — Auto-detect or interactive chooser
//!   - `abstract login claude`    — Anthropic OAuth PKCE (opens browser, no API key)
//!   - `abstract login openai`    — Prompt for OpenAI API key
//!   - `abstract login key`       — Prompt for any API key (auto-detect provider)
//!   - `abstract login status`    — Show current auth status
//!   - `abstract logout`          — Remove saved credentials

use std::collections::BTreeMap;
use std::io::{self, Write};
use std::path::PathBuf;
use std::time::Duration;

// ─── OAuth constants ────────────────────────────────────────────────────────

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

// ─── Credential storage ────────────────────────────────────────────────────

fn credentials_path() -> PathBuf {
    crate::config::global_config_dir().join("credentials.json")
}

fn oauth_tokens_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| ".".into())
        .join(".claude")
        .join("oauth_tokens.json")
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, Default)]
pub struct Credentials {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub anthropic_api_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub openai_api_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub anthropic_oauth: Option<OAuthTokenData>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_provider: Option<String>,
    /// API keys for any registered provider, keyed by provider id
    /// (`"google"`, `"groq"`, `"deepseek"`, …). Populated by
    /// `abstract login <provider>`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub provider_keys: BTreeMap<String, String>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct OAuthTokenData {
    pub access_token: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub refresh_token: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at_ms: Option<i64>,
    pub scopes: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub account_uuid: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub organization_uuid: Option<String>,
}

impl OAuthTokenData {
    fn is_expired(&self) -> bool {
        self.expires_at_ms
            .map(|exp| chrono::Utc::now().timestamp_millis() + 300_000 >= exp)
            .unwrap_or(false)
    }

    fn uses_bearer(&self) -> bool {
        self.scopes.iter().any(|s| s == INFERENCE_SCOPE)
    }

    fn usable_key(&self) -> Option<&str> {
        if self.uses_bearer() {
            Some(&self.access_token)
        } else {
            self.api_key.as_deref()
        }
    }
}

impl Credentials {
    pub fn load() -> Self {
        let path = credentials_path();
        if path.exists() {
            std::fs::read_to_string(&path)
                .ok()
                .and_then(|s| serde_json::from_str(&s).ok())
                .unwrap_or_default()
        } else {
            Self::default()
        }
    }

    pub fn save(&self) -> anyhow::Result<()> {
        let path = credentials_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let content = serde_json::to_string_pretty(self)?;
        std::fs::write(&path, content)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
        }
        Ok(())
    }

    pub fn clear(&self) -> anyhow::Result<()> {
        let path = credentials_path();
        if path.exists() {
            std::fs::remove_file(&path)?;
        }
        Ok(())
    }

    /// Resolve the best Anthropic key (for display/non-auth purposes).
    pub fn resolve_anthropic_key(&self) -> Option<String> {
        // OAuth with user:inference takes top priority (Max/Pro plans)
        if let Some(oauth) = &self.anthropic_oauth {
            if !oauth.is_expired() && oauth.uses_bearer() {
                return Some(oauth.access_token.clone());
            }
        }
        if let Some(oauth) = load_external_oauth() {
            if !oauth.is_expired() && oauth.uses_bearer() {
                return Some(oauth.access_token.clone());
            }
        }
        // Then env vars (API key path)
        for var in &["ANTHROPIC_API_KEY", "ANTHROPIC_KEY"] {
            if let Ok(key) = std::env::var(var) {
                if !key.is_empty() {
                    return Some(key);
                }
            }
        }
        // Saved OAuth (non-bearer, i.e. has api_key)
        if let Some(oauth) = &self.anthropic_oauth {
            if !oauth.is_expired() {
                if let Some(key) = oauth.usable_key() {
                    return Some(key.to_string());
                }
            }
        }
        self.anthropic_api_key.clone()
    }

    pub fn resolve_openai_key(&self) -> Option<String> {
        if let Ok(key) = std::env::var("OPENAI_API_KEY") {
            if !key.is_empty() {
                return Some(key);
            }
        }
        self.openai_api_key.clone()
    }

    pub fn has_any_auth(&self) -> bool {
        self.resolve_anthropic_key().is_some() || self.resolve_openai_key().is_some()
    }

    /// Resolve the best Anthropic auth method.
    /// OAuth bearer (Max/Pro plan) > env API key > saved credentials.
    pub fn resolve_anthropic_auth(&self) -> Option<cersei_provider::Auth> {
        // 1. OAuth bearer tokens (Max/Pro subscription — no API credits needed)
        if let Some(oauth) = &self.anthropic_oauth {
            if !oauth.is_expired() && oauth.uses_bearer() {
                return Some(cersei_provider::Auth::Bearer(oauth.access_token.clone()));
            }
        }
        // Also check external OAuth tokens
        if let Some(oauth) = load_external_oauth() {
            if !oauth.is_expired() && oauth.uses_bearer() {
                return Some(cersei_provider::Auth::Bearer(oauth.access_token.clone()));
            }
        }

        // 2. OAuth with API key (Console flow)
        if let Some(oauth) = &self.anthropic_oauth {
            if !oauth.is_expired() {
                if let Some(key) = &oauth.api_key {
                    return Some(cersei_provider::Auth::ApiKey(key.clone()));
                }
            }
        }

        // 3. Environment variables
        for var in &["ANTHROPIC_API_KEY", "ANTHROPIC_KEY"] {
            if let Ok(key) = std::env::var(var) {
                if !key.is_empty() {
                    return Some(cersei_provider::Auth::ApiKey(key));
                }
            }
        }

        // 4. Saved API key
        if let Some(key) = &self.anthropic_api_key {
            return Some(cersei_provider::Auth::ApiKey(key.clone()));
        }

        None
    }

    /// Get the OAuth account UUID (needed for Max/Pro plan metadata).
    pub fn oauth_account_uuid(&self) -> Option<String> {
        if let Some(oauth) = &self.anthropic_oauth {
            return oauth.account_uuid.clone();
        }
        if let Some(oauth) = load_external_oauth() {
            return oauth.account_uuid.clone();
        }
        None
    }
}

/// Try to load external OAuth tokens from ~/.claude/oauth_tokens.json
fn load_external_oauth() -> Option<OAuthTokenData> {
    let path = oauth_tokens_path();
    let content = std::fs::read_to_string(&path).ok()?;
    let v: serde_json::Value = serde_json::from_str(&content).ok()?;

    Some(OAuthTokenData {
        access_token: v.get("access_token")?.as_str()?.to_string(),
        refresh_token: v.get("refresh_token").and_then(|v| v.as_str()).map(String::from),
        expires_at_ms: v.get("expires_at_ms").and_then(|v| v.as_i64()),
        scopes: v
            .get("scopes")
            .and_then(|v| v.as_array())
            .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
            .unwrap_or_default(),
        email: v.get("email").and_then(|v| v.as_str()).map(String::from),
        api_key: v.get("api_key").and_then(|v| v.as_str()).map(String::from),
        account_uuid: v.get("account_uuid").and_then(|v| v.as_str()).map(String::from),
        organization_uuid: v.get("organization_uuid").and_then(|v| v.as_str()).map(String::from),
    })
}

// ─── PKCE helpers ──────────────────────────────────────────────────────────

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

// ─── OAuth callback server ─────────────────────────────────────────────────

async fn run_callback_server(
    listener: tokio::net::TcpListener,
    expected_state: &str,
) -> anyhow::Result<String> {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    let (socket, _) = tokio::time::timeout(Duration::from_secs(120), listener.accept())
        .await
        .map_err(|_| anyhow::anyhow!("Timeout waiting for browser redirect (120s)"))?
        .map_err(|e| anyhow::anyhow!("Accept failed: {e}"))?;

    let (reader, mut writer) = socket.into_split();
    let mut reader = BufReader::new(reader);

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

    let path = request_line.split_whitespace().nth(1).unwrap_or("").to_string();
    let parsed = url::Url::parse(&format!("http://localhost{}", path))?;

    let code = parsed.query_pairs().find(|(k, _)| k == "code").map(|(_, v)| v.to_string());
    let recv_state = parsed.query_pairs().find(|(k, _)| k == "state").map(|(_, v)| v.to_string());

    let response = format!(
        "HTTP/1.1 302 Found\r\nLocation: {}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        SUCCESS_URL
    );
    writer.write_all(response.as_bytes()).await?;

    if recv_state.as_deref() != Some(expected_state) {
        anyhow::bail!("OAuth state mismatch (possible CSRF)");
    }

    code.ok_or_else(|| anyhow::anyhow!("No authorization code in callback"))
}

// ─── Token exchange ────────────────────────────────────────────────────────

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

    let client = reqwest::Client::builder().timeout(Duration::from_secs(30)).build()?;
    let resp = client
        .post(TOKEN_URL)
        .header("content-type", "application/json")
        .json(&body)
        .send()
        .await?;

    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        anyhow::bail!("Token exchange failed ({status}): {text}");
    }

    Ok(resp.json().await?)
}

async fn create_api_key(access_token: &str) -> anyhow::Result<String> {
    let client = reqwest::Client::builder().timeout(Duration::from_secs(30)).build()?;
    let resp = client
        .post(API_KEY_URL)
        .header("authorization", format!("Bearer {access_token}"))
        .send()
        .await?;

    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        anyhow::bail!("API key creation failed ({status}): {text}");
    }

    let data: ApiKeyResponse = resp.json().await?;
    data.raw_key.ok_or_else(|| anyhow::anyhow!("No API key in response"))
}

async fn refresh_oauth_token(oauth: &OAuthTokenData) -> anyhow::Result<OAuthTokenData> {
    let rt = oauth
        .refresh_token
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("No refresh token"))?;

    let body = serde_json::json!({
        "grant_type": "refresh_token",
        "refresh_token": rt,
        "client_id": CLIENT_ID,
        "scope": ALL_SCOPES.join(" "),
    });

    let client = reqwest::Client::builder().timeout(Duration::from_secs(30)).build()?;
    let resp = client.post(TOKEN_URL).json(&body).send().await?;

    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        anyhow::bail!("Token refresh failed ({status}): {text}");
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

    let mut updated = oauth.clone();
    updated.access_token = data.access_token;
    if let Some(new_rt) = data.refresh_token {
        updated.refresh_token = Some(new_rt);
    }
    updated.expires_at_ms = Some(expires_at_ms);
    updated.scopes = scopes;
    Ok(updated)
}

// ─── Login flows ───────────────────────────────────────────────────────────

pub async fn run_login(provider: Option<&str>) -> anyhow::Result<()> {
    match provider {
        Some("claude") | Some("anthropic") => login_anthropic_oauth().await,
        Some("key") => login_api_key(),
        Some("status") => show_status(),
        Some(other) => {
            // Any registered provider from the registry (google, groq, deepseek, …).
            if let Some(entry) = cersei_provider::registry::lookup(other) {
                if !entry.requires_key() {
                    eprintln!(
                        "\x1b[36m{}\x1b[0m is a local provider — no API key required.",
                        entry.name
                    );
                    eprintln!(
                        "\x1b[90mPoint at it with: abstract --model {}/<model>\x1b[0m",
                        entry.id
                    );
                    return Ok(());
                }
                return login_api_key_for(entry.id);
            }
            let known: Vec<&str> = cersei_provider::registry::all()
                .iter()
                .map(|e| e.id)
                .collect();
            anyhow::bail!(
                "Unknown provider: '{other}'\n\nKnown providers: {}\n\nUsage:\n  abstract login              Interactive chooser\n  abstract login claude       Anthropic OAuth (opens browser)\n  abstract login <provider>   Enter API key for any registered provider\n  abstract login key          Enter any API key (auto-detects provider)\n  abstract login status       Show auth status",
                known.join(", ")
            );
        }
        None => login_interactive().await,
    }
}

/// Export saved provider keys into environment variables so the cersei-provider
/// registry's `api_key_from_env()` lookups find them during this process run.
///
/// Only sets a var if it is currently unset or empty — an explicit env var
/// always wins over a saved credential.
pub fn export_saved_keys_to_env() {
    let creds = Credentials::load();

    // Legacy fields first
    if let Some(key) = &creds.anthropic_api_key {
        set_if_empty("ANTHROPIC_API_KEY", key);
    }
    if let Some(key) = &creds.openai_api_key {
        set_if_empty("OPENAI_API_KEY", key);
    }

    // Generic map: use the provider's first env_key as the target var.
    for (provider_id, key) in &creds.provider_keys {
        if let Some(entry) = cersei_provider::registry::lookup(provider_id) {
            if let Some(env_var) = entry.env_keys.first() {
                set_if_empty(env_var, key);
            }
        }
    }
}

fn set_if_empty(var: &str, value: &str) {
    let existing = std::env::var(var).ok().filter(|v| !v.is_empty());
    if existing.is_none() {
        std::env::set_var(var, value);
    }
}

pub fn run_logout() -> anyhow::Result<()> {
    let creds = Credentials::load();
    creds.clear()?;
    eprintln!("\x1b[32mLogged out. Credentials removed.\x1b[0m");

    let claude_path = oauth_tokens_path();
    if claude_path.exists() {
        eprintln!("\x1b[90mNote: External OAuth tokens at {} were not removed.\x1b[0m", claude_path.display());
    }
    Ok(())
}

fn show_status() -> anyhow::Result<()> {
    eprintln!("\x1b[36;1mAuthentication Status\x1b[0m\n");

    // Show all providers from the registry
    for entry in cersei_provider::registry::all() {
        let status = if !entry.requires_key() {
            if entry.is_reachable() {
                "\x1b[32mavailable (local)\x1b[0m".to_string()
            } else {
                "\x1b[90mnot running\x1b[0m".to_string()
            }
        } else if let Some(key) = entry.api_key_from_env() {
            let env_name = entry.env_keys.iter()
                .find(|v| std::env::var(v).ok().filter(|k| !k.is_empty()).is_some())
                .unwrap_or(&"?");
            format!("\x1b[32mENV\x1b[0m ({}={})", env_name, mask_key(&key))
        } else {
            "\x1b[90mnot configured\x1b[0m".to_string()
        };

        eprintln!("  {:<14} {}", format!("{}:", entry.id), status);
    }

    eprintln!("\n  Credentials: {}", credentials_path().display());
    if oauth_tokens_path().exists() {
        eprintln!("  OAuth tokens: {}", oauth_tokens_path().display());
    }
    Ok(())
}

async fn login_interactive() -> anyhow::Result<()> {
    let creds = Credentials::load();
    if creds.has_any_auth() {
        show_status()?;
        eprintln!("\n\x1b[90mAlready authenticated. Use 'abstract login <provider>' to re-auth.\x1b[0m");
        return Ok(());
    }

    eprintln!("\x1b[36;1mAbstract — Login\x1b[0m\n");
    eprintln!("  1. \x1b[36mClaude OAuth\x1b[0m — opens browser, no API key needed");
    eprintln!("  2. \x1b[36mAnthropic API key\x1b[0m — enter sk-ant-... key");
    eprintln!("  3. \x1b[36mOpenAI API key\x1b[0m — enter sk-... key");
    eprintln!();
    eprint!("  Choice [1/2/3]: ");
    io::stderr().flush()?;

    let mut input = String::new();
    io::stdin().read_line(&mut input)?;

    match input.trim() {
        "1" | "" => login_anthropic_oauth().await,
        "2" => login_api_key_for("anthropic"),
        "3" => login_api_key_for("openai"),
        _ => {
            eprintln!("\x1b[33mInvalid choice.\x1b[0m");
            Ok(())
        }
    }
}

/// Full Anthropic OAuth PKCE flow — opens browser, no API key needed.
async fn login_anthropic_oauth() -> anyhow::Result<()> {
    // Check for existing valid tokens first
    let mut creds = Credentials::load();
    if let Some(oauth) = &creds.anthropic_oauth {
        if !oauth.is_expired() {
            let email = oauth.email.as_deref().unwrap_or("unknown");
            eprintln!("\x1b[32mAlready authenticated as {email} (OAuth).\x1b[0m");
            eprintln!("\x1b[90mUse 'abstract logout' first to re-authenticate.\x1b[0m");
            return Ok(());
        }
        // Try refresh
        if oauth.refresh_token.is_some() {
            eprint!("  Token expired, refreshing... ");
            io::stderr().flush()?;
            match refresh_oauth_token(oauth).await {
                Ok(refreshed) => {
                    eprintln!("\x1b[32mdone.\x1b[0m");
                    creds.anthropic_oauth = Some(refreshed);
                    creds.default_provider = Some("anthropic".into());
                    creds.save()?;
                    return Ok(());
                }
                Err(e) => {
                    eprintln!("\x1b[33mfailed ({e}). Starting new login.\x1b[0m");
                }
            }
        }
    }

    // Also try importing from external OAuth store
    if let Some(cc_oauth) = load_external_oauth() {
        if !cc_oauth.is_expired() && cc_oauth.usable_key().is_some() {
            let email = cc_oauth.email.as_deref().unwrap_or("unknown");
            eprintln!("\x1b[32mImported external OAuth credentials ({email}).\x1b[0m");
            creds.anthropic_oauth = Some(cc_oauth);
            creds.default_provider = Some("anthropic".into());
            creds.save()?;
            return Ok(());
        }
    }

    // ── Full PKCE flow ──────────────────────────────────────────────────

    eprintln!("\x1b[36;1mAnthropic OAuth Login\x1b[0m\n");

    // 1. Generate PKCE params
    let verifier = generate_verifier();
    let challenge = generate_challenge(&verifier);
    let state = generate_verifier(); // reuse same algo for state

    // 2. Bind callback server
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let port = listener.local_addr()?.port();

    // 3. Build URLs
    let manual_url = build_auth_url(CLAUDE_AI_AUTHORIZE_URL, &challenge, &state, port, true);
    let auto_url = build_auth_url(CLAUDE_AI_AUTHORIZE_URL, &challenge, &state, port, false);

    // 4. Open browser
    eprintln!("  Opening browser for Claude.ai authentication...");
    eprintln!("  If the browser did not open, visit:\n");
    eprintln!("  {manual_url}\n");
    open_browser(&auto_url);

    // 5. Wait for callback or manual paste
    let state_clone = state.clone();
    let (cb_tx, cb_rx) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        let _ = cb_tx.send(run_callback_server(listener, &state_clone).await);
    });

    eprint!("  Or paste authorization code here: ");
    io::stdout().flush()?;
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

    eprintln!("\n  Authorization code received.");

    // 6. Exchange for tokens
    eprint!("  Exchanging code for tokens... ");
    io::stderr().flush()?;
    let token_resp = exchange_code(&auth_code, &state, &verifier, port).await?;
    eprintln!("\x1b[32mdone.\x1b[0m");

    let expires_at_ms = chrono::Utc::now().timestamp_millis() + (token_resp.expires_in as i64 * 1000);
    let scopes: Vec<String> = token_resp
        .scope
        .as_deref()
        .unwrap_or("")
        .split_whitespace()
        .map(String::from)
        .collect();

    let account_uuid = token_resp.account.as_ref().and_then(|a| a["uuid"].as_str().map(String::from));
    let email = token_resp.account.as_ref().and_then(|a| a["email_address"].as_str().map(String::from));
    let org_uuid = token_resp.organization.as_ref().and_then(|o| o["uuid"].as_str().map(String::from));
    let uses_bearer = scopes.iter().any(|s| s == INFERENCE_SCOPE);

    // 7. Console path: create API key from access token
    let api_key = if !uses_bearer {
        eprint!("  Creating API key... ");
        io::stderr().flush()?;
        match create_api_key(&token_resp.access_token).await {
            Ok(key) => {
                eprintln!("\x1b[32mdone.\x1b[0m");
                Some(key)
            }
            Err(e) => {
                eprintln!("\x1b[33mfailed ({e})\x1b[0m");
                None
            }
        }
    } else {
        None
    };

    // 8. Save to credentials
    let oauth_data = OAuthTokenData {
        access_token: token_resp.access_token,
        refresh_token: token_resp.refresh_token,
        expires_at_ms: Some(expires_at_ms),
        scopes,
        email: email.clone(),
        api_key,
        account_uuid,
        organization_uuid: org_uuid,
    };

    // Also save to ~/.claude/oauth_tokens.json for cross-tool compatibility
    if let Ok(json) = serde_json::to_string_pretty(&oauth_data) {
        let cc_path = oauth_tokens_path();
        if let Some(parent) = cc_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(&cc_path, &json);
    }

    creds.anthropic_oauth = Some(oauth_data);
    creds.default_provider = Some("anthropic".into());
    creds.save()?;

    eprintln!();
    eprintln!("  \x1b[32;1mAuthenticated!\x1b[0m");
    eprintln!("  Account: {}", email.as_deref().unwrap_or("unknown"));
    eprintln!("  Mode:    {}", if uses_bearer { "Bearer (Claude.ai)" } else { "API Key (Console)" });
    eprintln!("  Expires: {}", chrono::DateTime::from_timestamp_millis(expires_at_ms)
        .map(|dt| dt.format("%Y-%m-%d %H:%M UTC").to_string())
        .unwrap_or_else(|| "unknown".into()));
    eprintln!();
    eprintln!("  You can now use: abstract \"your prompt\"");

    Ok(())
}

fn login_api_key() -> anyhow::Result<()> {
    eprint!("  Enter API key: ");
    io::stderr().flush()?;
    let key = read_line_trimmed()?;
    if key.is_empty() {
        anyhow::bail!("No key entered.");
    }

    let mut creds = Credentials::load();
    if key.starts_with("sk-ant-") {
        creds.anthropic_api_key = Some(key);
        creds.default_provider = Some("anthropic".into());
        creds.save()?;
        eprintln!("\n\x1b[32mAnthropic API key saved.\x1b[0m");
    } else if key.starts_with("sk-") {
        creds.openai_api_key = Some(key);
        creds.default_provider = Some("openai".into());
        creds.save()?;
        eprintln!("\n\x1b[32mOpenAI API key saved.\x1b[0m");
    } else {
        creds.anthropic_api_key = Some(key);
        creds.default_provider = Some("anthropic".into());
        creds.save()?;
        eprintln!("\n\x1b[32mAPI key saved (defaulting to Anthropic).\x1b[0m");
    }
    Ok(())
}

fn login_api_key_for(provider: &str) -> anyhow::Result<()> {
    let entry = cersei_provider::registry::lookup(provider)
        .ok_or_else(|| anyhow::anyhow!("Unknown provider: {provider}"))?;

    let hint = match provider {
        "anthropic" => "sk-ant-...".to_string(),
        "openai" => "sk-...".to_string(),
        _ => entry
            .env_keys
            .first()
            .map(|k| format!("read from ${k}"))
            .unwrap_or_else(|| "API key".into()),
    };
    eprint!("  Enter {} API key ({hint}): ", entry.name);
    io::stderr().flush()?;
    let key = read_line_trimmed()?;
    if key.is_empty() {
        anyhow::bail!("No key entered.");
    }

    let mut creds = Credentials::load();
    match provider {
        "anthropic" => {
            creds.anthropic_api_key = Some(key.clone());
        }
        "openai" => {
            creds.openai_api_key = Some(key.clone());
        }
        _ => {}
    }
    // Store in the generic map too so every provider goes through one code path
    // for env-var injection at startup.
    creds.provider_keys.insert(provider.to_string(), key);
    creds.default_provider = Some(provider.to_string());
    creds.save()?;
    eprintln!("\n\x1b[32m{} API key saved.\x1b[0m", entry.name);
    Ok(())
}

fn read_line_trimmed() -> anyhow::Result<String> {
    let mut line = String::new();
    io::stdin().read_line(&mut line)?;
    Ok(line.trim().to_string())
}

fn mask_key(key: &str) -> String {
    if key.len() <= 8 {
        "****".into()
    } else {
        format!("{}...{}", &key[..4], &key[key.len() - 4..])
    }
}

fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        None => String::new(),
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
    }
}
