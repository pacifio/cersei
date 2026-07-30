//! Anthropic-on-Google-Vertex provider (Model Garden).
//!
//! Reuses the direct-Anthropic request body + SSE parser
//! ([`crate::anthropic::build_anthropic_body`] / [`crate::anthropic::spawn_sse`]).
//! Differences from direct Anthropic:
//! - URL: `https://{endpoint}/v1/projects/{project}/locations/{location}/publishers/anthropic/models/{model}:streamRawPredict`
//!   (endpoint = `aiplatform.googleapis.com` for the `global` location, else `{location}-aiplatform.googleapis.com`)
//! - Auth: `Authorization: Bearer <gcp access token>`
//! - Body: Anthropic Messages + `"anthropic_version": "vertex-2023-10-16"`, no `model` field.
//!
//! Auth resolution (in order): a service-account JSON (self-refreshing, via
//! `gcp_auth`) — required for long runs since GCP tokens expire ~1h; a pre-minted
//! `VERTEX_ACCESS_TOKEN`; or a local `gcloud auth print-access-token`.

use crate::anthropic::{build_anthropic_body, spawn_sse};
use crate::*;
use cersei_types::*;
use std::sync::Arc;

const VERTEX_VERSION: &str = "vertex-2023-10-16";
const CLOUD_PLATFORM_SCOPE: &str = "https://www.googleapis.com/auth/cloud-platform";

/// How credentials are obtained. All non-`Static` kinds resolve to a
/// self-refreshing `gcp_auth::TokenProvider` built lazily on first use.
#[derive(Clone)]
enum VertexAuthSpec {
    /// A pre-minted bearer token (no refresh).
    Static(String),
    /// Application Default Credentials — `gcp_auth::provider()` auto-detects
    /// `GOOGLE_APPLICATION_CREDENTIALS` (service-account), the gcloud ADC user
    /// file (`application_default_credentials.json`), or the GCE metadata server.
    Adc,
    /// An explicit service-account JSON file.
    ServiceAccountFile(std::path::PathBuf),
}

pub struct AnthropicVertex {
    auth_spec: VertexAuthSpec,
    token_provider: tokio::sync::OnceCell<Arc<dyn gcp_auth::TokenProvider>>,
    project_id: String,
    location: String,
    endpoint: String,
    default_model: String,
    thinking_budget: Option<u32>,
    client: reqwest::Client,
}

impl AnthropicVertex {
    pub fn new(
        auth_spec: VertexAuthSpec,
        project_id: impl Into<String>,
        location: impl Into<String>,
    ) -> Result<Self> {
        let project_id = project_id.into();
        let location = location.into();
        let endpoint = if location == "global" {
            "aiplatform.googleapis.com".to_string()
        } else {
            format!("{location}-aiplatform.googleapis.com")
        };
        Ok(Self {
            auth_spec,
            token_provider: tokio::sync::OnceCell::new(),
            project_id,
            location,
            endpoint,
            default_model: "claude-opus-4-8".to_string(),
            thinking_budget: None,
            client: reqwest::Client::new(),
        })
    }

    /// Construct from environment: `VERTEX_PROJECT_ID` (required), `VERTEX_LOCATION`
    /// (default `global`), and credentials resolved as: `VERTEX_ACCESS_TOKEN`
    /// → ADC (`GOOGLE_APPLICATION_CREDENTIALS` / gcloud ADC user file / GCE
    /// metadata) → `./service-account.json` → `gcloud auth print-access-token`.
    pub fn from_env() -> Result<Self> {
        let project_id = std::env::var("VERTEX_PROJECT_ID")
            .map_err(|_| CerseiError::Config("VERTEX_PROJECT_ID not set".into()))?;
        let location = std::env::var("VERTEX_LOCATION").unwrap_or_else(|_| "global".to_string());
        Self::new(resolve_auth_spec()?, project_id, location)
    }

    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        self.default_model = model.into();
        self
    }

    async fn token_provider(&self) -> Result<&Arc<dyn gcp_auth::TokenProvider>> {
        self.token_provider
            .get_or_try_init(|| async {
                match &self.auth_spec {
                    VertexAuthSpec::Adc => gcp_auth::provider()
                        .await
                        .map_err(|e| CerseiError::Auth(format!("vertex ADC init failed: {e}"))),
                    VertexAuthSpec::ServiceAccountFile(p) => {
                        let sa = gcp_auth::CustomServiceAccount::from_file(p).map_err(|e| {
                            CerseiError::Auth(format!("vertex service-account load failed: {e}"))
                        })?;
                        Ok(Arc::new(sa) as Arc<dyn gcp_auth::TokenProvider>)
                    }
                    VertexAuthSpec::Static(_) => {
                        Err(CerseiError::Auth("unreachable: static token".into()))
                    }
                }
            })
            .await
    }

    async fn bearer_token(&self) -> Result<String> {
        use gcp_auth::TokenProvider;
        if let VertexAuthSpec::Static(t) = &self.auth_spec {
            return Ok(t.clone());
        }
        let provider = self.token_provider().await?;
        let token = provider
            .token(&[CLOUD_PLATFORM_SCOPE])
            .await
            .map_err(|e| CerseiError::Auth(format!("vertex token mint failed: {e}")))?;
        Ok(token.as_str().to_string())
    }

    fn model_url(&self, model: &str) -> String {
        format!(
            "https://{}/v1/projects/{}/locations/{}/publishers/anthropic/models/{}:streamRawPredict",
            self.endpoint, self.project_id, self.location, model
        )
    }
}

#[async_trait::async_trait]
impl Provider for AnthropicVertex {
    fn name(&self) -> &str {
        "vertex"
    }

    fn context_window(&self, _model: &str) -> u64 {
        200_000
    }

    fn capabilities(&self, _model: &str) -> ProviderCapabilities {
        ProviderCapabilities {
            streaming: true,
            tool_use: true,
            vision: true,
            thinking: true,
            system_prompt: true,
            caching: true,
        }
    }

    async fn complete(&self, request: CompletionRequest) -> Result<CompletionStream> {
        let model = if request.model.is_empty() {
            self.default_model.clone()
        } else {
            request.model.clone()
        };
        let thinking_budget = request
            .options
            .get::<u32>("thinking_budget")
            .or(self.thinking_budget);

        // Vertex: no `model` in body, add the vertex anthropic_version.
        let body = build_anthropic_body(None, &request, thinking_budget, Some(VERTEX_VERSION));

        let token = self.bearer_token().await?;
        let http_request = self
            .client
            .post(self.model_url(&model))
            .header("authorization", format!("Bearer {token}"))
            .header("content-type", "application/json")
            .json(&body)
            .build()
            .map_err(CerseiError::Http)?;

        Ok(spawn_sse(self.client.clone(), http_request))
    }
}

fn adc_available() -> bool {
    // GOOGLE_APPLICATION_CREDENTIALS → a credentials file (service-account or
    // external account) that gcp_auth/ADC will use.
    if let Ok(p) = std::env::var("GOOGLE_APPLICATION_CREDENTIALS") {
        if !p.is_empty() && std::path::Path::new(&p).is_file() {
            return true;
        }
    }
    // gcloud ADC user file: $CLOUDSDK_CONFIG or ~/.config/gcloud.
    let base = std::env::var("CLOUDSDK_CONFIG")
        .ok()
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::var("HOME").ok().map(|h| std::path::Path::new(&h).join(".config/gcloud")));
    if let Some(base) = base {
        if base.join("application_default_credentials.json").is_file() {
            return true;
        }
    }
    false
}

fn resolve_auth_spec() -> Result<VertexAuthSpec> {
    // 1) explicit pre-minted token (escape hatch)
    if let Ok(t) = std::env::var("VERTEX_ACCESS_TOKEN") {
        if !t.trim().is_empty() {
            return Ok(VertexAuthSpec::Static(t));
        }
    }
    // 2) Application Default Credentials (covers GOOGLE_APPLICATION_CREDENTIALS
    //    service-account, the gcloud ADC user login, and GCE metadata) — preferred.
    if adc_available() {
        return Ok(VertexAuthSpec::Adc);
    }
    // 3) a service-account file shipped alongside the agent (e.g. forwarded into
    //    a container at /workspace).
    for cand in ["service-account.json", "/workspace/service-account.json"] {
        if std::path::Path::new(cand).is_file() {
            return Ok(VertexAuthSpec::ServiceAccountFile(cand.into()));
        }
    }
    // 4) last resort: a short-lived gcloud token (no refresh).
    if let Ok(out) = std::process::Command::new("gcloud")
        .args(["auth", "print-access-token"])
        .output()
    {
        if out.status.success() {
            let t = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if !t.is_empty() {
                return Ok(VertexAuthSpec::Static(t));
            }
        }
    }
    Err(CerseiError::Auth(
        "no Vertex credentials: run `gcloud auth application-default login` (ADC), set \
         GOOGLE_APPLICATION_CREDENTIALS, provide ./service-account.json, or set VERTEX_ACCESS_TOKEN"
            .into(),
    ))
}
