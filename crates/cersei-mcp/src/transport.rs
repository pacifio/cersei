//! MCP transports: stdio subprocess communication.

use crate::jsonrpc;
use cersei_types::*;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::{mpsc, oneshot, Mutex};

/// Stdio transport: communicates with an MCP server via stdin/stdout of a subprocess.
pub struct StdioTransport {
    child: Child,
    request_tx: mpsc::Sender<(jsonrpc::Request, Option<oneshot::Sender<serde_json::Value>>)>,
    next_id: AtomicU64,
}

impl StdioTransport {
    /// Spawn a subprocess and start the JSON-RPC transport.
    pub async fn spawn(
        command: &str,
        args: &[String],
        env: &HashMap<String, String>,
    ) -> Result<Self> {
        let mut cmd = Command::new(command);
        cmd.args(args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());

        for (k, v) in env {
            cmd.env(k, v);
        }

        let mut child = cmd.spawn().map_err(|e| {
            CerseiError::Mcp(format!("Failed to spawn MCP server '{}': {}", command, e))
        })?;

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| CerseiError::Mcp("Failed to get stdin".into()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| CerseiError::Mcp("Failed to get stdout".into()))?;

        // Pending requests: id → response channel
        let pending: Arc<Mutex<HashMap<u64, oneshot::Sender<serde_json::Value>>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let pending_clone = Arc::clone(&pending);

        // Stdout reader task
        tokio::spawn(async move {
            let mut reader = BufReader::new(stdout);
            let mut line = String::new();

            loop {
                line.clear();
                match reader.read_line(&mut line).await {
                    Ok(0) => break, // EOF
                    Ok(_) => {
                        let trimmed = line.trim();
                        if trimmed.is_empty() {
                            continue;
                        }
                        if let Ok(resp) = serde_json::from_str::<jsonrpc::Response>(trimmed) {
                            if let Some(id) = resp.id.as_ref().and_then(|v| v.as_u64()) {
                                let mut pending = pending_clone.lock().await;
                                if let Some(tx) = pending.remove(&id) {
                                    if let Some(result) = resp.result {
                                        let _ = tx.send(result);
                                    } else if let Some(err) = resp.error {
                                        let _ = tx.send(serde_json::json!({
                                            "error": {"code": err.code, "message": err.message}
                                        }));
                                    }
                                }
                            }
                            // Notifications (no id) are silently consumed
                        }
                    }
                    Err(_) => break,
                }
            }
        });

        // Request sender channel
        let (request_tx, mut request_rx) =
            mpsc::channel::<(jsonrpc::Request, Option<oneshot::Sender<serde_json::Value>>)>(64);

        let pending_for_writer = Arc::clone(&pending);

        // Stdin writer task
        tokio::spawn(async move {
            let mut stdin = stdin;
            while let Some((req, resp_tx)) = request_rx.recv().await {
                // Register pending response handler
                if let (Some(tx), Some(id)) = (resp_tx, req.id.as_u64()) {
                    pending_for_writer.lock().await.insert(id, tx);
                }

                // Write request as JSON line
                let mut json = serde_json::to_string(&req).unwrap_or_default();
                json.push('\n');
                if stdin.write_all(json.as_bytes()).await.is_err() {
                    break;
                }
                if stdin.flush().await.is_err() {
                    break;
                }
            }
        });

        Ok(Self {
            child,
            request_tx,
            next_id: AtomicU64::new(1),
        })
    }

    /// Send a JSON-RPC request and wait for the response.
    pub async fn request(
        &mut self,
        method: &str,
        params: Option<serde_json::Value>,
    ) -> Result<serde_json::Value> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let req = jsonrpc::Request::new(id, method, params);

        let (resp_tx, resp_rx) = oneshot::channel();

        self.request_tx
            .send((req, Some(resp_tx)))
            .await
            .map_err(|_| CerseiError::Mcp("Transport channel closed".into()))?;

        let result = tokio::time::timeout(std::time::Duration::from_secs(30), resp_rx)
            .await
            .map_err(|_| CerseiError::Mcp(format!("MCP request '{}' timed out (30s)", method)))?
            .map_err(|_| CerseiError::Mcp("Response channel dropped".into()))?;

        // Check for error response
        if let Some(err) = result.get("error") {
            let msg = err
                .get("message")
                .and_then(|m| m.as_str())
                .unwrap_or("Unknown error");
            return Err(CerseiError::Mcp(format!("MCP error: {}", msg)));
        }

        Ok(result)
    }

    /// Send a JSON-RPC notification (no response expected).
    pub async fn notify(&mut self, method: &str, params: Option<serde_json::Value>) -> Result<()> {
        let req = jsonrpc::Request::notification(method, params);

        self.request_tx
            .send((req, None))
            .await
            .map_err(|_| CerseiError::Mcp("Transport channel closed".into()))?;

        Ok(())
    }
}

impl Drop for StdioTransport {
    fn drop(&mut self) {
        // Kill the child process when transport is dropped
        let _ = self.child.start_kill();
    }
}
