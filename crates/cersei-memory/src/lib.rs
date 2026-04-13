//! cersei-memory: Memory trait and backends for the Cersei SDK.
//!
//! Memory provides session persistence and retrieval, enabling resumable
//! conversations and long-term knowledge storage.
//!
//! ## Modules
//! - `memdir` — Flat file memory scanning
//! - `claudemd` — Hierarchical CLAUDE.md loading
//! - `session_storage` — JSONL transcript persistence

pub mod claudemd;
pub mod graph;
pub mod graph_migrate;
pub mod manager;
pub mod memdir;
pub mod session_storage;

use async_trait::async_trait;
use cersei_types::*;
use std::path::PathBuf;

/// Strip YAML frontmatter from content.
pub fn strip_frontmatter(content: &str) -> String {
    if content.starts_with("---") {
        if let Some(close_pos) = content[3..].find("\n---") {
            return content[3 + close_pos + 4..].trim_start_matches('\n').to_string();
        }
    }
    content.to_string()
}

// ─── Memory trait ────────────────────────────────────────────────────────────

#[async_trait]
pub trait Memory: Send + Sync {
    /// Store conversation messages for a session.
    async fn store(&self, session_id: &str, messages: &[Message]) -> Result<()>;

    /// Load conversation history for a session.
    async fn load(&self, session_id: &str) -> Result<Vec<Message>>;

    /// Search memories relevant to a query (for RAG-style retrieval).
    async fn search(&self, query: &str, limit: usize) -> Result<Vec<MemoryEntry>>;

    /// List available sessions.
    async fn sessions(&self) -> Result<Vec<SessionInfo>>;

    /// Delete a session.
    async fn delete(&self, session_id: &str) -> Result<()>;
}

// ─── JSONL Memory ────────────────────────────────────────────────────────────

/// File-based memory backend using JSONL format.
/// Each session is stored as a `.jsonl` file with one message per line.
pub struct JsonlMemory {
    dir: PathBuf,
}

impl JsonlMemory {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    fn session_path(&self, session_id: &str) -> PathBuf {
        self.dir.join(format!("{}.jsonl", session_id))
    }
}

#[async_trait]
impl Memory for JsonlMemory {
    async fn store(&self, session_id: &str, messages: &[Message]) -> Result<()> {
        tokio::fs::create_dir_all(&self.dir).await?;
        let path = self.session_path(session_id);
        let mut content = String::new();
        for msg in messages {
            let line = serde_json::to_string(msg)?;
            content.push_str(&line);
            content.push('\n');
        }
        tokio::fs::write(&path, content).await?;
        Ok(())
    }

    async fn load(&self, session_id: &str) -> Result<Vec<Message>> {
        let path = self.session_path(session_id);
        if !path.exists() {
            return Ok(Vec::new());
        }
        let content = tokio::fs::read_to_string(&path).await?;
        let mut messages = Vec::new();
        for line in content.lines() {
            if line.trim().is_empty() {
                continue;
            }
            let msg: Message = serde_json::from_str(line)?;
            messages.push(msg);
        }
        Ok(messages)
    }

    async fn search(&self, _query: &str, _limit: usize) -> Result<Vec<MemoryEntry>> {
        // JSONL memory doesn't support semantic search
        Ok(Vec::new())
    }

    async fn sessions(&self) -> Result<Vec<SessionInfo>> {
        let mut sessions = Vec::new();
        if !self.dir.exists() {
            return Ok(sessions);
        }
        let mut entries = tokio::fs::read_dir(&self.dir).await?;
        while let Some(entry) = entries.next_entry().await? {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some("jsonl") {
                let id = path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("")
                    .to_string();
                let metadata = tokio::fs::metadata(&path).await?;
                let created_at = metadata
                    .created()
                    .ok()
                    .and_then(|t| {
                        let dur = t.duration_since(std::time::UNIX_EPOCH).ok()?;
                        chrono::DateTime::from_timestamp(dur.as_secs() as i64, 0)
                    })
                    .unwrap_or_else(chrono::Utc::now);
                let content = tokio::fs::read_to_string(&path).await.unwrap_or_default();
                let message_count = content.lines().filter(|l| !l.trim().is_empty()).count();
                sessions.push(SessionInfo {
                    id,
                    created_at,
                    message_count,
                    model: None,
                });
            }
        }
        Ok(sessions)
    }

    async fn delete(&self, session_id: &str) -> Result<()> {
        let path = self.session_path(session_id);
        if path.exists() {
            tokio::fs::remove_file(&path).await?;
        }
        Ok(())
    }
}

// ─── In-Memory Store ─────────────────────────────────────────────────────────

/// In-memory store for tests and short-lived agents.
pub struct InMemory {
    store: std::sync::Arc<parking_lot::Mutex<std::collections::HashMap<String, Vec<Message>>>>,
}

impl InMemory {
    pub fn new() -> Self {
        Self {
            store: std::sync::Arc::new(parking_lot::Mutex::new(std::collections::HashMap::new())),
        }
    }
}

impl Default for InMemory {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Memory for InMemory {
    async fn store(&self, session_id: &str, messages: &[Message]) -> Result<()> {
        self.store
            .lock()
            .insert(session_id.to_string(), messages.to_vec());
        Ok(())
    }

    async fn load(&self, session_id: &str) -> Result<Vec<Message>> {
        Ok(self
            .store
            .lock()
            .get(session_id)
            .cloned()
            .unwrap_or_default())
    }

    async fn search(&self, _query: &str, _limit: usize) -> Result<Vec<MemoryEntry>> {
        Ok(Vec::new())
    }

    async fn sessions(&self) -> Result<Vec<SessionInfo>> {
        let store = self.store.lock();
        Ok(store
            .iter()
            .map(|(id, msgs)| SessionInfo {
                id: id.clone(),
                created_at: chrono::Utc::now(),
                message_count: msgs.len(),
                model: None,
            })
            .collect())
    }

    async fn delete(&self, session_id: &str) -> Result<()> {
        self.store.lock().remove(session_id);
        Ok(())
    }
}
