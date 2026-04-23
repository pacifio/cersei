//! LSP client: manages a single language server process.
//!
//! Communicates via JSON-RPC 2.0 over stdio with Content-Length framing.

use crate::config::LspServerConfig;
use crate::jsonrpc::{self, Notification, Request, Response};
use crate::types::*;
use dashmap::DashMap;
use serde_json::Value;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use tokio::io::BufReader;
use tokio::process::{Child, ChildStdin};
use tokio::sync::{oneshot, Mutex};

const REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// Errors from the LSP client.
#[derive(thiserror::Error, Debug)]
pub enum LspError {
    #[error("Server not started")]
    NotStarted,
    #[error("Server process failed to start: {0}")]
    SpawnFailed(String),
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("Request timed out after {0:?}")]
    Timeout(std::time::Duration),
    #[error("RPC error {code}: {message}")]
    Rpc { code: i64, message: String },
    #[error("Server not initialized")]
    NotInitialized,
}

pub type LspResult<T> = Result<T, LspError>;

/// A client connected to a single LSP server process.
pub struct LspClient {
    config: LspServerConfig,
    writer: Arc<Mutex<Option<ChildStdin>>>,
    request_id: AtomicU64,
    pending: Arc<DashMap<u64, oneshot::Sender<Response>>>,
    diagnostics: Arc<DashMap<String, Vec<LspDiagnostic>>>,
    is_initialized: AtomicBool,
    process: Mutex<Option<Child>>,
    root_uri: Mutex<Option<String>>,
}

impl LspClient {
    /// Create a new unstarted client.
    pub fn new(config: LspServerConfig) -> Self {
        Self {
            config,
            writer: Arc::new(Mutex::new(None)),
            request_id: AtomicU64::new(1),
            pending: Arc::new(DashMap::new()),
            diagnostics: Arc::new(DashMap::new()),
            is_initialized: AtomicBool::new(false),
            process: Mutex::new(None),
            root_uri: Mutex::new(None),
        }
    }

    /// Server name.
    pub fn name(&self) -> &str {
        &self.config.name
    }

    /// Whether the server has been initialized.
    pub fn is_initialized(&self) -> bool {
        self.is_initialized.load(Ordering::Relaxed)
    }

    /// Start the server process and the I/O pump.
    pub async fn start(&self, working_dir: &Path) -> LspResult<()> {
        let mut cmd = tokio::process::Command::new(&self.config.command);
        cmd.args(&self.config.args)
            .current_dir(working_dir)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null());

        for (k, v) in &self.config.env {
            cmd.env(k, v);
        }

        let mut child = cmd
            .spawn()
            .map_err(|e| LspError::SpawnFailed(format!("{}: {e}", self.config.command)))?;

        let stdin = child.stdin.take().ok_or(LspError::NotStarted)?;
        let stdout = child.stdout.take().ok_or(LspError::NotStarted)?;

        *self.writer.lock().await = Some(stdin);
        *self.process.lock().await = Some(child);

        // Store root URI
        let uri = path_to_uri(working_dir);
        *self.root_uri.lock().await = Some(uri);

        // Spawn I/O pump to read responses
        let pending = Arc::clone(&self.pending);
        let diagnostics = Arc::clone(&self.diagnostics);
        tokio::spawn(async move {
            let mut reader = BufReader::new(stdout);
            loop {
                match jsonrpc::read_message(&mut reader).await {
                    Ok(Some(data)) => {
                        if let Ok(msg) = serde_json::from_slice::<Response>(&data) {
                            dispatch_incoming(msg, &pending, &diagnostics);
                        }
                    }
                    Ok(None) => break, // EOF
                    Err(_) => break,
                }
            }
        });

        tracing::debug!("LSP server '{}' started", self.config.name);
        Ok(())
    }

    /// Send the LSP `initialize` handshake.
    pub async fn initialize(&self) -> LspResult<Value> {
        let root_uri = self.root_uri.lock().await.clone();
        let params = serde_json::json!({
            "processId": std::process::id(),
            "rootUri": root_uri,
            "workspaceFolders": root_uri.as_ref().map(|uri| {
                vec![serde_json::json!({ "name": "workspace", "uri": uri })]
            }),
            "capabilities": {
                "textDocument": {
                    "synchronization": { "didOpen": true, "didChange": true },
                    "hover": { "contentFormat": ["plaintext", "markdown"] },
                    "definition": {},
                    "references": {},
                    "documentSymbol": { "hierarchicalDocumentSymbolSupport": true },
                    "publishDiagnostics": { "versionSupport": true }
                },
                "workspace": {
                    "workspaceFolders": true
                }
            },
            "initializationOptions": self.config.initialization_options,
        });

        let result = self.send_request("initialize", Some(params)).await?;
        self.send_notification("initialized", Some(serde_json::json!({})))
            .await?;
        self.is_initialized.store(true, Ordering::Relaxed);
        Ok(result)
    }

    /// Notify the server about an opened file.
    pub async fn open_document(&self, path: &Path) -> LspResult<()> {
        let content = tokio::fs::read_to_string(path).await.unwrap_or_default();
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| format!(".{e}"))
            .unwrap_or_default();
        let language_id = self.config.language_id(&ext);

        self.send_notification(
            "textDocument/didOpen",
            Some(serde_json::json!({
                "textDocument": {
                    "uri": path_to_uri(path),
                    "languageId": language_id,
                    "version": 1,
                    "text": content,
                }
            })),
        )
        .await
    }

    /// Get hover information at a position.
    pub async fn hover(&self, path: &Path, line: u32, character: u32) -> LspResult<Option<String>> {
        let result = self
            .send_request(
                "textDocument/hover",
                Some(serde_json::json!({
                    "textDocument": { "uri": path_to_uri(path) },
                    "position": { "line": line, "character": character },
                })),
            )
            .await?;

        if result.is_null() {
            return Ok(None);
        }

        // Extract hover content (can be string, MarkupContent, or MarkedString[])
        let contents = &result["contents"];
        let text = if let Some(s) = contents.as_str() {
            s.to_string()
        } else if let Some(value) = contents.get("value").and_then(|v| v.as_str()) {
            value.to_string()
        } else if let Some(arr) = contents.as_array() {
            arr.iter()
                .filter_map(|item| {
                    item.as_str()
                        .map(String::from)
                        .or_else(|| item.get("value").and_then(|v| v.as_str()).map(String::from))
                })
                .collect::<Vec<_>>()
                .join("\n")
        } else {
            serde_json::to_string_pretty(contents).unwrap_or_default()
        };

        Ok(if text.is_empty() { None } else { Some(text) })
    }

    /// Go to definition.
    pub async fn definition(
        &self,
        path: &Path,
        line: u32,
        character: u32,
    ) -> LspResult<Vec<String>> {
        let result = self
            .send_request(
                "textDocument/definition",
                Some(serde_json::json!({
                    "textDocument": { "uri": path_to_uri(path) },
                    "position": { "line": line, "character": character },
                })),
            )
            .await?;

        Ok(extract_locations(&result))
    }

    /// Find all references.
    pub async fn references(
        &self,
        path: &Path,
        line: u32,
        character: u32,
    ) -> LspResult<Vec<String>> {
        let result = self
            .send_request(
                "textDocument/references",
                Some(serde_json::json!({
                    "textDocument": { "uri": path_to_uri(path) },
                    "position": { "line": line, "character": character },
                    "context": { "includeDeclaration": true },
                })),
            )
            .await?;

        Ok(extract_locations(&result))
    }

    /// Get document symbols (outline).
    pub async fn document_symbols(&self, path: &Path) -> LspResult<Vec<SymbolInfo>> {
        let result = self
            .send_request(
                "textDocument/documentSymbol",
                Some(serde_json::json!({
                    "textDocument": { "uri": path_to_uri(path) },
                })),
            )
            .await?;

        let symbols = if let Some(arr) = result.as_array() {
            arr.iter().map(collect_symbol).collect()
        } else {
            vec![]
        };

        Ok(symbols)
    }

    /// Get cached diagnostics for a file.
    pub fn get_diagnostics(&self, path: &Path) -> Vec<LspDiagnostic> {
        let uri = path_to_uri(path);
        self.diagnostics
            .get(&uri)
            .map(|v| v.value().clone())
            .unwrap_or_default()
    }

    /// Get all cached diagnostics.
    pub fn all_diagnostics(&self) -> Vec<LspDiagnostic> {
        self.diagnostics
            .iter()
            .flat_map(|entry| entry.value().clone())
            .collect()
    }

    /// Gracefully shutdown the server.
    pub async fn shutdown(&self) -> LspResult<()> {
        if self.is_initialized() {
            let _ = self.send_request("shutdown", None).await;
            let _ = self.send_notification("exit", None).await;
        }

        if let Some(mut child) = self.process.lock().await.take() {
            let _ = tokio::time::timeout(std::time::Duration::from_secs(5), child.wait()).await;
            let _ = child.kill().await;
        }

        self.is_initialized.store(false, Ordering::Relaxed);
        tracing::debug!("LSP server '{}' shut down", self.config.name);
        Ok(())
    }

    // ── Internal ────────────────────────────────────────────────────────

    async fn send_request(&self, method: &str, params: Option<Value>) -> LspResult<Value> {
        let id = self.request_id.fetch_add(1, Ordering::Relaxed);
        let req = Request::new(id, method, params);
        let body = serde_json::to_vec(&req)?;

        // Register pending response
        let (tx, rx) = oneshot::channel();
        self.pending.insert(id, tx);

        // Send
        {
            let mut writer_guard = self.writer.lock().await;
            let writer = writer_guard.as_mut().ok_or(LspError::NotStarted)?;
            jsonrpc::send_message(writer, &body).await?;
        }

        // Wait for response with timeout
        let response = tokio::time::timeout(REQUEST_TIMEOUT, rx)
            .await
            .map_err(|_| {
                self.pending.remove(&id);
                LspError::Timeout(REQUEST_TIMEOUT)
            })?
            .map_err(|_| LspError::NotStarted)?;

        if let Some(error) = response.error {
            return Err(LspError::Rpc {
                code: error.code,
                message: error.message,
            });
        }

        Ok(response.result.unwrap_or(Value::Null))
    }

    async fn send_notification(&self, method: &str, params: Option<Value>) -> LspResult<()> {
        let notif = Notification::new(method, params);
        let body = serde_json::to_vec(&notif)?;

        let mut writer_guard = self.writer.lock().await;
        let writer = writer_guard.as_mut().ok_or(LspError::NotStarted)?;
        jsonrpc::send_message(writer, &body).await?;
        Ok(())
    }
}

// ─── Dispatch ───────────────────────────────────────────────────────────────

fn dispatch_incoming(
    msg: Response,
    pending: &DashMap<u64, oneshot::Sender<Response>>,
    diagnostics: &DashMap<String, Vec<LspDiagnostic>>,
) {
    // Response to a request
    if let Some(id) = msg.id {
        if let Some((_, tx)) = pending.remove(&id) {
            let _ = tx.send(msg);
        }
        return;
    }

    // Notification
    if let Some(method) = &msg.method {
        if method == "textDocument/publishDiagnostics" {
            if let Some(params) = &msg.params {
                handle_publish_diagnostics(params, diagnostics);
            }
        }
    }
}

fn handle_publish_diagnostics(params: &Value, store: &DashMap<String, Vec<LspDiagnostic>>) {
    let uri = params["uri"].as_str().unwrap_or_default().to_string();
    let file = uri_to_path(&uri);

    let diags = params["diagnostics"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|d| parse_diagnostic(d, &file))
                .collect()
        })
        .unwrap_or_default();

    store.insert(uri, diags);
}

fn parse_diagnostic(d: &Value, file: &str) -> Option<LspDiagnostic> {
    let range = &d["range"]["start"];
    let line = range["line"].as_u64()? as u32;
    let col = range["character"].as_u64().unwrap_or(0) as u32;
    let severity = d["severity"]
        .as_u64()
        .map(DiagnosticSeverity::from_lsp)
        .unwrap_or(DiagnosticSeverity::Warning);
    let message = d["message"].as_str()?.to_string();
    let source = d["source"].as_str().map(String::from);
    let code = d["code"]
        .as_str()
        .map(String::from)
        .or_else(|| d["code"].as_u64().map(|n| n.to_string()));

    Some(LspDiagnostic {
        file: file.to_string(),
        line,
        col,
        severity,
        message,
        source,
        code,
    })
}

// ─── Helpers ────────────────────────────────────────────────────────────────

/// Extract location(s) from an LSP definition/references result.
fn extract_locations(value: &Value) -> Vec<String> {
    let mut locations = Vec::new();

    let extract_one = |loc: &Value| -> Option<String> {
        let uri = loc["uri"].as_str()?;
        let path = uri_to_path(uri);
        let line = loc["range"]["start"]["line"].as_u64()? + 1;
        let col = loc["range"]["start"]["character"].as_u64().unwrap_or(0) + 1;
        Some(format!("{path}:{line}:{col}"))
    };

    if let Some(arr) = value.as_array() {
        for item in arr {
            if let Some(loc) = extract_one(item) {
                locations.push(loc);
            }
        }
    } else if let Some(loc) = extract_one(value) {
        locations.push(loc);
    }

    locations
}

/// Recursively collect document symbols.
fn collect_symbol(value: &Value) -> SymbolInfo {
    let name = value["name"].as_str().unwrap_or("?").to_string();
    let kind_num = value["kind"].as_u64().unwrap_or(0);
    let kind = symbol_kind_name(kind_num).to_string();

    let range_val = &value["range"];
    let range = Range {
        start: Position {
            line: range_val["start"]["line"].as_u64().unwrap_or(0) as u32,
            character: range_val["start"]["character"].as_u64().unwrap_or(0) as u32,
        },
        end: Position {
            line: range_val["end"]["line"].as_u64().unwrap_or(0) as u32,
            character: range_val["end"]["character"].as_u64().unwrap_or(0) as u32,
        },
    };

    let children = value["children"]
        .as_array()
        .map(|arr| arr.iter().map(collect_symbol).collect())
        .unwrap_or_default();

    SymbolInfo {
        name,
        kind,
        range,
        children,
    }
}

/// Convert a file path to a file:// URI.
pub fn path_to_uri(path: &Path) -> String {
    let abs = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir().unwrap_or_default().join(path)
    };

    #[cfg(windows)]
    {
        format!("file:///{}", abs.display().to_string().replace('\\', "/"))
    }
    #[cfg(not(windows))]
    {
        format!("file://{}", abs.display())
    }
}

/// Convert a file:// URI to a path string.
pub fn uri_to_path(uri: &str) -> String {
    let path = uri.strip_prefix("file://").unwrap_or(uri);

    #[cfg(windows)]
    {
        path.strip_prefix('/').unwrap_or(path).replace('/', "\\")
    }
    #[cfg(not(windows))]
    {
        path.to_string()
    }
}

/// Format diagnostics for display.
pub fn format_diagnostics(diagnostics: &[LspDiagnostic]) -> String {
    if diagnostics.is_empty() {
        return "No diagnostics.".to_string();
    }

    let mut errors = 0u32;
    let mut warnings = 0u32;
    let mut lines = Vec::new();

    for d in diagnostics {
        match d.severity {
            DiagnosticSeverity::Error => errors += 1,
            DiagnosticSeverity::Warning => warnings += 1,
            _ => {}
        }
        lines.push(d.to_string());
    }

    let summary = format!("{errors} error(s), {warnings} warning(s)");
    lines.push(format!("\n{summary}"));
    lines.join("\n")
}
