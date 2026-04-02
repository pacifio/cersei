//! Unified Memory Manager: composes all memory layers into a single API.
//!
//! Layers (in query order):
//! 1. Graph (optional) — Grafeo for relationship-aware recall
//! 2. Memdir — flat file scanning for MEMORY.md and topic files
//! 3. CLAUDE.md — hierarchical instruction loading
//! 4. Session storage — JSONL transcript persistence
//!
//! The manager delegates to the appropriate layer for each operation.

use crate::claudemd::{self};
use crate::graph::{GraphMemory, GraphStats};
use crate::memdir::{self, MemoryFile, MemoryFileMeta, MemoryType};
use crate::session_storage;
use cersei_types::*;
use std::path::{Path, PathBuf};

/// Unified memory manager composing all layers.
pub struct MemoryManager {
    /// Project root for resolving paths.
    project_root: PathBuf,
    /// Memory directory path.
    memory_dir: PathBuf,
    /// Session storage directory.
    sessions_dir: PathBuf,
    /// Optional graph memory layer.
    graph: Option<GraphMemory>,
}

impl MemoryManager {
    /// Create a new memory manager for a project.
    pub fn new(project_root: &Path) -> Self {
        let memory_dir = memdir::auto_memory_path(project_root);
        let sanitized = memdir::sanitize_path_component(&project_root.display().to_string());
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
        let sessions_dir = home.join(".claude").join("projects").join(&sanitized);

        Self {
            project_root: project_root.to_path_buf(),
            memory_dir,
            sessions_dir,
            graph: None,
        }
    }

    /// Enable graph memory at a given path.
    /// Requires the `graph` feature.
    pub fn with_graph(mut self, path: &Path) -> Result<Self> {
        self.graph = Some(GraphMemory::open(path)?);
        Ok(self)
    }

    /// Enable in-memory graph (no persistence).
    /// Requires the `graph` feature.
    pub fn with_graph_in_memory(mut self) -> Result<Self> {
        self.graph = Some(GraphMemory::open_in_memory()?);
        Ok(self)
    }

    /// Set a custom memory directory.
    pub fn with_memory_dir(mut self, dir: PathBuf) -> Self {
        self.memory_dir = dir;
        self
    }

    /// Set a custom sessions directory.
    pub fn with_sessions_dir(mut self, dir: PathBuf) -> Self {
        self.sessions_dir = dir;
        self
    }

    // ─── Context building ────────────────────────────────────────────────

    /// Build the complete memory context for the system prompt.
    /// Includes MEMORY.md index + CLAUDE.md hierarchy.
    pub fn build_context(&self) -> String {
        let mut parts = Vec::new();

        // CLAUDE.md hierarchy
        let claude_files = claudemd::load_all_memory_files(&self.project_root);
        let claude_prompt = claudemd::build_memory_prompt(&claude_files);
        if !claude_prompt.is_empty() {
            parts.push(claude_prompt);
        }

        // MEMORY.md index
        let memdir_content = memdir::build_memory_prompt_content(&self.memory_dir);
        if !memdir_content.is_empty() {
            parts.push(memdir_content);
        }

        parts.join("\n\n")
    }

    // ─── Memory operations ───────────────────────────────────────────────

    /// Scan memory directory for all memory file metadata.
    pub fn scan(&self) -> Vec<MemoryFileMeta> {
        memdir::scan_memory_dir(&self.memory_dir)
    }

    /// Load a specific memory file by path.
    pub fn load_file(&self, path: &Path) -> Option<MemoryFile> {
        memdir::load_memory_file(path)
    }

    /// Store a memory (writes to graph if available, always returns success).
    pub fn store_memory(
        &self,
        content: &str,
        mem_type: MemoryType,
        confidence: f32,
    ) -> Option<String> {
        if let Some(graph) = &self.graph {
            graph.store_memory(content, mem_type, confidence).ok()
        } else {
            None
        }
    }

    /// Recall memories matching a query.
    /// Uses graph if available, falls back to memdir scan + text matching.
    pub fn recall(&self, query: &str, limit: usize) -> Vec<String> {
        // Try graph first
        if let Some(graph) = &self.graph {
            let results = graph.recall(query, limit);
            if !results.is_empty() {
                return results;
            }
        }

        // Fallback: scan memdir and text-match
        let query_lower = query.to_lowercase();
        let metas = self.scan();
        let mut results = Vec::new();

        for meta in metas.iter().take(limit * 2) {
            if let Some(file) = memdir::load_memory_file(&meta.path) {
                if file.content.to_lowercase().contains(&query_lower)
                    || meta.name.as_deref().unwrap_or("").to_lowercase().contains(&query_lower)
                    || meta.description.as_deref().unwrap_or("").to_lowercase().contains(&query_lower)
                {
                    results.push(file.content);
                    if results.len() >= limit {
                        break;
                    }
                }
            }
        }

        results
    }

    /// Get memories by type (graph only, returns empty without graph).
    pub fn by_type(&self, mem_type: MemoryType) -> Vec<String> {
        if let Some(graph) = &self.graph {
            graph.by_type(mem_type)
        } else {
            Vec::new()
        }
    }

    /// Get memories by topic (graph only).
    pub fn by_topic(&self, topic: &str) -> Vec<String> {
        if let Some(graph) = &self.graph {
            graph.by_topic(topic)
        } else {
            Vec::new()
        }
    }

    // ─── Session operations ──────────────────────────────────────────────

    /// Get the transcript path for a session.
    pub fn session_path(&self, session_id: &str) -> PathBuf {
        self.sessions_dir.join(format!("{}.jsonl", session_id))
    }

    /// Write a user message to the session transcript.
    pub fn write_user_message(
        &self,
        session_id: &str,
        message: Message,
    ) -> std::io::Result<String> {
        let path = self.session_path(session_id);
        let cwd = self.project_root.display().to_string();
        session_storage::write_user_entry(&path, session_id, message, &cwd)
    }

    /// Write an assistant message to the session transcript.
    pub fn write_assistant_message(
        &self,
        session_id: &str,
        message: Message,
        parent_uuid: Option<&str>,
    ) -> std::io::Result<String> {
        let path = self.session_path(session_id);
        let cwd = self.project_root.display().to_string();
        session_storage::write_assistant_entry(&path, session_id, message, &cwd, parent_uuid)
    }

    /// Load a session's messages.
    pub fn load_session_messages(&self, session_id: &str) -> Result<Vec<Message>> {
        let path = self.session_path(session_id);
        if !path.exists() {
            return Ok(Vec::new());
        }
        let entries = session_storage::load_transcript(&path)?;
        Ok(session_storage::messages_from_transcript(&entries))
    }

    /// List all session files.
    pub fn list_sessions(&self) -> Vec<SessionInfo> {
        let mut sessions = Vec::new();
        let entries = match std::fs::read_dir(&self.sessions_dir) {
            Ok(e) => e,
            Err(_) => return sessions,
        };

        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                continue;
            }
            let id = path.file_stem().and_then(|s| s.to_str()).unwrap_or("").to_string();
            let created_at = std::fs::metadata(&path)
                .and_then(|m| m.created())
                .ok()
                .and_then(|t| {
                    let d = t.duration_since(std::time::UNIX_EPOCH).ok()?;
                    chrono::DateTime::from_timestamp(d.as_secs() as i64, 0)
                })
                .unwrap_or_else(chrono::Utc::now);

            sessions.push(SessionInfo {
                id,
                created_at,
                message_count: 0, // would need to parse to count
                model: None,
            });
        }

        sessions
    }

    // ─── Graph operations ────────────────────────────────────────────────

    /// Check if graph memory is available.
    pub fn has_graph(&self) -> bool {
        self.graph.is_some()
    }

    /// Get graph statistics (returns default if no graph).
    pub fn graph_stats(&self) -> GraphStats {
        self.graph.as_ref().map(|g| g.stats()).unwrap_or_default()
    }

    /// Tag a memory in the graph (no-op without graph).
    pub fn tag_memory(&self, memory_id: &str, topic: &str) {
        if let Some(graph) = &self.graph {
            let _ = graph.tag_memory(memory_id, topic);
        }
    }

    /// Link two memories in the graph (no-op without graph).
    pub fn link_memories(&self, from_id: &str, to_id: &str, relationship: &str) {
        if let Some(graph) = &self.graph {
            let _ = graph.link_memories(from_id, to_id, relationship);
        }
    }

    /// Access paths.
    pub fn memory_dir(&self) -> &Path { &self.memory_dir }
    pub fn sessions_dir(&self) -> &Path { &self.sessions_dir }
    pub fn project_root(&self) -> &Path { &self.project_root }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_manager_basic() {
        let tmp = tempfile::tempdir().unwrap();
        let manager = MemoryManager::new(tmp.path())
            .with_memory_dir(tmp.path().join("memory"))
            .with_sessions_dir(tmp.path().join("sessions"));

        assert!(!manager.has_graph());
        assert_eq!(manager.graph_stats().memory_count, 0);
    }

    #[test]
    fn test_manager_context_with_claude_md() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("CLAUDE.md"), "# Project Rules\nUse Rust only.").unwrap();

        let mem_dir = tmp.path().join("memory");
        std::fs::create_dir_all(&mem_dir).unwrap();
        std::fs::write(mem_dir.join("MEMORY.md"), "- [pref](pref.md) — user prefs").unwrap();

        let manager = MemoryManager::new(tmp.path())
            .with_memory_dir(mem_dir);

        let context = manager.build_context();
        assert!(context.contains("Use Rust only"));
        assert!(context.contains("user prefs"));
    }

    #[test]
    fn test_manager_session_write_load() {
        let tmp = tempfile::tempdir().unwrap();
        let manager = MemoryManager::new(tmp.path())
            .with_sessions_dir(tmp.path().join("sessions"));

        let uuid = manager.write_user_message("s1", Message::user("Hello")).unwrap();
        manager.write_assistant_message("s1", Message::assistant("Hi!"), Some(&uuid)).unwrap();

        let messages = manager.load_session_messages("s1").unwrap();
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].get_text().unwrap(), "Hello");
        assert_eq!(messages[1].get_text().unwrap(), "Hi!");
    }

    #[test]
    fn test_manager_recall_fallback() {
        let tmp = tempfile::tempdir().unwrap();
        let mem_dir = tmp.path().join("memory");
        std::fs::create_dir_all(&mem_dir).unwrap();
        std::fs::write(mem_dir.join("rust_tips.md"), "---\nname: Rust Tips\n---\n\nAlways use clippy for linting.").unwrap();
        std::fs::write(mem_dir.join("python_tips.md"), "---\nname: Python Tips\n---\n\nUse ruff for linting.").unwrap();

        let manager = MemoryManager::new(tmp.path())
            .with_memory_dir(mem_dir);

        let results = manager.recall("clippy", 10);
        assert_eq!(results.len(), 1);
        assert!(results[0].contains("clippy"));

        let results = manager.recall("linting", 10);
        assert_eq!(results.len(), 2);
    }

    #[test]
    fn test_manager_scan() {
        let tmp = tempfile::tempdir().unwrap();
        let mem_dir = tmp.path().join("memory");
        std::fs::create_dir_all(&mem_dir).unwrap();
        std::fs::write(mem_dir.join("a.md"), "content a").unwrap();
        std::fs::write(mem_dir.join("b.md"), "content b").unwrap();
        std::fs::write(mem_dir.join("MEMORY.md"), "index").unwrap();

        let manager = MemoryManager::new(tmp.path())
            .with_memory_dir(mem_dir);

        let metas = manager.scan();
        assert_eq!(metas.len(), 2); // excludes MEMORY.md
    }

    #[test]
    fn test_manager_list_sessions() {
        let tmp = tempfile::tempdir().unwrap();
        let sessions_dir = tmp.path().join("sessions");
        std::fs::create_dir_all(&sessions_dir).unwrap();
        std::fs::write(sessions_dir.join("s1.jsonl"), "{}").unwrap();
        std::fs::write(sessions_dir.join("s2.jsonl"), "{}").unwrap();
        std::fs::write(sessions_dir.join("not-a-session.txt"), "x").unwrap();

        let manager = MemoryManager::new(tmp.path())
            .with_sessions_dir(sessions_dir);

        let sessions = manager.list_sessions();
        assert_eq!(sessions.len(), 2);
    }
}
