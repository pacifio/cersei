//! `cersei-envd` — tiny in-VM JSON-RPC daemon.
//!
//! Listens on a Unix-domain socket that the host bind-mounts into the
//! container. Each accepted connection is line-delimited JSON-RPC 2.0
//! (one request per line, one response per line). Concurrent connections
//! and concurrent in-flight requests on the same connection are both
//! supported.
//!
//! Used both by the `cersei-envd` binary (running inside containers) and
//! by the `LocalProcessRuntime` for end-to-end testing of the protocol
//! without needing Docker.

pub mod handlers;
pub mod protocol;

use crate::error::{Result, VmError};
use std::path::Path;

#[cfg(unix)]
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

#[cfg(unix)]
use tokio::net::{UnixListener, UnixStream};

/// Run the envd server on a Unix socket at `socket_path` until shutdown.
#[cfg(unix)]
pub async fn run<P: AsRef<Path>>(socket_path: P) -> Result<()> {
    let path = socket_path.as_ref();
    if path.exists() {
        std::fs::remove_file(path)?;
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let listener = UnixListener::bind(path)
        .map_err(|e| VmError::Transport(format!("bind {}: {e}", path.display())))?;
    tracing::info!(path = %path.display(), "cersei-envd listening");

    loop {
        let (stream, _addr) = listener
            .accept()
            .await
            .map_err(|e| VmError::Transport(format!("accept: {e}")))?;
        tokio::spawn(async move {
            if let Err(e) = handle_conn(stream).await {
                tracing::warn!(%e, "envd connection error");
            }
        });
    }
}

#[cfg(not(unix))]
pub async fn run<P: AsRef<Path>>(_socket_path: P) -> Result<()> {
    Err(VmError::Transport(
        "cersei-envd requires Unix-domain socket support".to_string(),
    ))
}

#[cfg(unix)]
async fn handle_conn(stream: UnixStream) -> Result<()> {
    let (read_half, mut write_half) = stream.into_split();
    let mut reader = BufReader::new(read_half).lines();
    while let Ok(Some(line)) = reader.next_line().await {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let resp = match serde_json::from_str::<protocol::Request>(trimmed) {
            Ok(req) => handlers::dispatch(req).await,
            Err(e) => protocol::Response::error(
                serde_json::Value::Null,
                -32700,
                format!("parse error: {e}"),
            ),
        };
        let mut buf = serde_json::to_vec(&resp)?;
        buf.push(b'\n');
        write_half.write_all(&buf).await?;
    }
    Ok(())
}
