//! HTTP client primitives.
//!
//! GET, POST, and HTML-to-text fetching built on reqwest.

use std::collections::HashMap;
use std::time::Duration;

/// HTTP response.
#[derive(Debug, Clone)]
pub struct HttpResponse {
    pub status: u16,
    pub headers: HashMap<String, String>,
    pub body: String,
    pub content_type: Option<String>,
}

/// Options for HTTP requests.
#[derive(Debug, Clone)]
pub struct HttpOptions {
    pub headers: HashMap<String, String>,
    pub timeout: Option<Duration>,
    pub user_agent: Option<String>,
}

impl Default for HttpOptions {
    fn default() -> Self {
        Self {
            headers: HashMap::new(),
            timeout: Some(Duration::from_secs(30)),
            user_agent: Some("Cersei-Agent/0.1".into()),
        }
    }
}

/// HTTP errors.
#[derive(Debug)]
pub enum HttpError {
    RequestFailed(String),
    Timeout,
    ClientBuild(String),
}

impl std::fmt::Display for HttpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::RequestFailed(msg) => write!(f, "HTTP request failed: {msg}"),
            Self::Timeout => write!(f, "HTTP request timed out"),
            Self::ClientBuild(msg) => write!(f, "failed to build HTTP client: {msg}"),
        }
    }
}

impl std::error::Error for HttpError {}

fn build_client(opts: &HttpOptions) -> Result<reqwest::Client, HttpError> {
    let mut builder = reqwest::Client::builder();

    if let Some(timeout) = opts.timeout {
        builder = builder.timeout(timeout);
    }

    if let Some(ua) = &opts.user_agent {
        builder = builder.user_agent(ua);
    }

    builder
        .build()
        .map_err(|e| HttpError::ClientBuild(e.to_string()))
}

/// Send a GET request.
pub async fn get(url: &str, opts: HttpOptions) -> Result<HttpResponse, HttpError> {
    let client = build_client(&opts)?;
    let mut req = client.get(url);

    for (k, v) in &opts.headers {
        req = req.header(k.as_str(), v.as_str());
    }

    let resp = req.send().await.map_err(|e| {
        if e.is_timeout() {
            HttpError::Timeout
        } else {
            HttpError::RequestFailed(e.to_string())
        }
    })?;

    let status = resp.status().as_u16();
    let content_type = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .map(String::from);
    let headers: HashMap<String, String> = resp
        .headers()
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_str().unwrap_or("").to_string()))
        .collect();
    let body = resp
        .text()
        .await
        .map_err(|e| HttpError::RequestFailed(e.to_string()))?;

    Ok(HttpResponse {
        status,
        headers,
        body,
        content_type,
    })
}

/// Send a POST request with a string body.
pub async fn post(url: &str, body: &str, opts: HttpOptions) -> Result<HttpResponse, HttpError> {
    let client = build_client(&opts)?;
    let mut req = client.post(url).body(body.to_string());

    for (k, v) in &opts.headers {
        req = req.header(k.as_str(), v.as_str());
    }

    let resp = req.send().await.map_err(|e| {
        if e.is_timeout() {
            HttpError::Timeout
        } else {
            HttpError::RequestFailed(e.to_string())
        }
    })?;

    let status = resp.status().as_u16();
    let content_type = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .map(String::from);
    let headers: HashMap<String, String> = resp
        .headers()
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_str().unwrap_or("").to_string()))
        .collect();
    let body = resp
        .text()
        .await
        .map_err(|e| HttpError::RequestFailed(e.to_string()))?;

    Ok(HttpResponse {
        status,
        headers,
        body,
        content_type,
    })
}

/// Fetch a URL and convert HTML to readable plain text.
/// Non-HTML content is returned as-is. Truncated to `max_chars`.
pub async fn fetch_html(
    url: &str,
    max_chars: usize,
    opts: HttpOptions,
) -> Result<String, HttpError> {
    let resp = get(url, opts).await?;

    let is_html = resp
        .content_type
        .as_deref()
        .map(|ct| ct.contains("html"))
        .unwrap_or(false);

    let text = if is_html {
        html2text::from_read(resp.body.as_bytes(), 80)
    } else {
        resp.body
    };

    if text.len() > max_chars {
        Ok(text[..max_chars].to_string())
    } else {
        Ok(text)
    }
}
