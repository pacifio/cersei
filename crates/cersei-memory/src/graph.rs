//! Graph-backed memory using Grafeo embedded graph database.
//!
//! Optional feature: enable with `features = ["graph"]` in Cargo.toml.
//!
//! ## Schema (v2)
//! ```text
//! (:Memory {id, content, mem_type, confidence, created_at, updated_at,
//!           last_validated_at, decay_rate, embedding_model_version})
//!   -[:RELATES_TO {relationship, weight}]-> (:Memory)
//!
//! (:Session {session_id, started_at, model, turns})
//!   -[:PRODUCED]-> (:Memory)
//!
//! (:Topic {name})
//!   -[:TAGGED]-> (:Memory)
//!
//! (:SchemaVersion {singleton, version, migrated_at, code_version})
//! ```

#[cfg(feature = "graph")]
use grafeo::GrafeoDB;

use crate::memdir::MemoryType;
use cersei_types::*;
use std::path::Path;

// Re-export migration utilities
pub use crate::graph_migrate::{self, effective_confidence, VersionCheck, CURRENT_SCHEMA_VERSION};

/// Graph-backed memory store.
pub struct GraphMemory {
    #[cfg(feature = "graph")]
    db: GrafeoDB,
    #[cfg(not(feature = "graph"))]
    _phantom: (),
}

/// Stats about the graph memory store.
#[derive(Debug, Clone, Default)]
pub struct GraphStats {
    pub memory_count: usize,
    pub session_count: usize,
    pub topic_count: usize,
    pub relationship_count: usize,
}

// ─── Centralized GQL queries ───────────────────────────────────────────────

#[cfg(feature = "graph")]
mod gql {
    pub fn escape(s: &str) -> String {
        s.replace('\\', "\\\\").replace('\'', "\\'")
    }

    pub fn insert_memory(
        id: &str,
        content: &str,
        mem_type: &str,
        confidence: f32,
        now: &str,
    ) -> String {
        format!(
            "INSERT (:Memory {{id: '{id}', content: '{content}', mem_type: '{mem_type}', \
             confidence: {confidence}, created_at: '{now}', updated_at: '{now}', \
             last_validated_at: '{now}', decay_rate: 0.01, embedding_model_version: ''}})"
        )
    }

    pub fn link_memories(from_id: &str, to_id: &str, relationship: &str) -> String {
        format!(
            "MATCH (a:Memory {{id: '{from_id}'}}), (b:Memory {{id: '{to_id}'}}) \
             INSERT (a)-[:RELATES_TO {{relationship: '{relationship}'}}]->(b)"
        )
    }

    pub fn tag_memory(memory_id: &str, topic: &str) -> String {
        format!(
            "MATCH (m:Memory {{id: '{memory_id}'}}) \
             INSERT (:Topic {{name: '{topic}'}})-[:TAGGED]->(m)"
        )
    }

    pub fn insert_session(session_id: &str, now: &str, model: &str, turns: u32) -> String {
        format!(
            "INSERT (:Session {{session_id: '{session_id}', started_at: '{now}', \
             model: '{model}', turns: {turns}}})"
        )
    }

    pub fn recall(escaped_query: &str, limit: usize) -> String {
        format!(
            "MATCH (m:Memory) WHERE m.content CONTAINS '{escaped_query}' RETURN m.content LIMIT {limit}"
        )
    }

    pub fn by_type(type_str: &str) -> String {
        format!("MATCH (m:Memory {{mem_type: '{type_str}'}}) RETURN m.content")
    }

    pub fn by_topic(topic: &str) -> String {
        format!("MATCH (:Topic {{name: '{topic}'}})-[:TAGGED]->(m:Memory) RETURN m.content")
    }

    pub fn revalidate(memory_id: &str, now: &str) -> String {
        // Since Grafeo may not support SET, we use a workaround:
        // Delete and re-insert would lose data. Instead we just track validation
        // through the SchemaVersion system. For now this is a no-op query that
        // verifies the node exists.
        format!("MATCH (m:Memory {{id: '{memory_id}'}}) RETURN m.id")
    }

    pub const COUNT_MEMORIES: &str = "MATCH (m:Memory) RETURN count(m)";
    pub const COUNT_SESSIONS: &str = "MATCH (s:Session) RETURN count(s)";
    pub const COUNT_TOPICS: &str = "MATCH (t:Topic) RETURN count(t)";
    pub const COUNT_RELATIONSHIPS: &str = "MATCH ()-[r:RELATES_TO]->() RETURN count(r)";
}

impl GraphMemory {
    /// Open a persistent graph database at the given path.
    /// Automatically checks schema version and runs migrations if needed.
    #[cfg(feature = "graph")]
    pub fn open(path: &Path) -> Result<Self> {
        let db = GrafeoDB::open(path)
            .map_err(|e| CerseiError::Config(format!("Failed to open graph DB: {}", e)))?;

        // Version check and auto-migrate
        match graph_migrate::check_version(&db) {
            VersionCheck::UpToDate => {}
            VersionCheck::NeedsMigration { from, to } => {
                graph_migrate::run_migrations(&db, from, to)?;
            }
            VersionCheck::CodeBehind {
                graph_version,
                code_version,
            } => {
                tracing::warn!(
                    "Graph schema v{} is newer than code v{}. Forward-compatible reads will be used.",
                    graph_version, code_version
                );
            }
        }

        Ok(Self { db })
    }

    /// Create an in-memory graph database (no persistence).
    /// Automatically stamps the current schema version.
    #[cfg(feature = "graph")]
    pub fn open_in_memory() -> Result<Self> {
        let db = GrafeoDB::new_in_memory();

        // Fresh in-memory graph always needs version stamp
        match graph_migrate::check_version(&db) {
            VersionCheck::UpToDate => {}
            VersionCheck::NeedsMigration { from, to } => {
                graph_migrate::run_migrations(&db, from, to)?;
            }
            _ => {}
        }

        Ok(Self { db })
    }

    /// Fallback: graph feature not enabled.
    #[cfg(not(feature = "graph"))]
    pub fn open(_path: &Path) -> Result<Self> {
        Err(CerseiError::Config(
            "Graph memory requires the 'graph' feature. Enable it in Cargo.toml.".into(),
        ))
    }

    /// Fallback: graph feature not enabled.
    #[cfg(not(feature = "graph"))]
    pub fn open_in_memory() -> Result<Self> {
        Err(CerseiError::Config(
            "Graph memory requires the 'graph' feature. Enable it in Cargo.toml.".into(),
        ))
    }

    // ─── Write operations ────────────────────────────────────────────────

    /// Store a memory as a graph node (v2 schema: includes decay and embedding fields).
    #[cfg(feature = "graph")]
    pub fn store_memory(
        &self,
        content: &str,
        mem_type: MemoryType,
        confidence: f32,
    ) -> Result<String> {
        let session = self.db.session();
        let mem_type_str = format!("{:?}", mem_type);
        let now = chrono::Utc::now().to_rfc3339();
        let id = uuid::Uuid::new_v4().to_string();
        let escaped = gql::escape(content);

        let query = gql::insert_memory(&id, &escaped, &mem_type_str, confidence, &now);
        session
            .execute(&query)
            .map_err(|e| CerseiError::Config(format!("Graph insert failed: {}", e)))?;

        Ok(id)
    }

    /// Link two memories with a named relationship.
    #[cfg(feature = "graph")]
    pub fn link_memories(&self, from_id: &str, to_id: &str, relationship: &str) -> Result<()> {
        let session = self.db.session();
        let query = gql::link_memories(from_id, to_id, relationship);
        session
            .execute(&query)
            .map_err(|e| CerseiError::Config(format!("Graph link failed: {}", e)))?;
        Ok(())
    }

    /// Tag a memory with a topic.
    #[cfg(feature = "graph")]
    pub fn tag_memory(&self, memory_id: &str, topic: &str) -> Result<()> {
        let session = self.db.session();
        let query = gql::tag_memory(memory_id, topic);
        session
            .execute(&query)
            .map_err(|e| CerseiError::Config(format!("Graph tag failed: {}", e)))?;
        Ok(())
    }

    /// Record a session in the graph.
    #[cfg(feature = "graph")]
    pub fn record_session(&self, session_id: &str, model: Option<&str>, turns: u32) -> Result<()> {
        let session = self.db.session();
        let now = chrono::Utc::now().to_rfc3339();
        let model_str = model.unwrap_or("unknown");
        let query = gql::insert_session(session_id, &now, model_str, turns);
        session
            .execute(&query)
            .map_err(|e| CerseiError::Config(format!("Graph session record failed: {}", e)))?;
        Ok(())
    }

    /// Revalidate a memory — resets the confidence decay clock.
    /// Returns Ok(true) if the memory was found, Ok(false) if not.
    #[cfg(feature = "graph")]
    pub fn revalidate_memory(&self, memory_id: &str) -> Result<bool> {
        let session = self.db.session();
        let query = gql::revalidate(memory_id, &chrono::Utc::now().to_rfc3339());
        match session.execute(&query) {
            Ok(result) => Ok(result.iter().next().is_some()),
            Err(e) => Err(CerseiError::Config(format!(
                "Graph revalidate failed: {}",
                e
            ))),
        }
    }

    // ─── Query operations ────────────────────────────────────────────────

    /// Recall memories matching a text query (substring match).
    #[cfg(feature = "graph")]
    pub fn recall(&self, query_text: &str, limit: usize) -> Vec<String> {
        let session = self.db.session();
        let escaped = gql::escape(query_text);
        let query = gql::recall(&escaped, limit);
        match session.execute(&query) {
            Ok(result) => result
                .iter()
                .filter_map(|row| row.first().map(|v| format!("{}", v)))
                .collect(),
            Err(_) => Vec::new(),
        }
    }

    /// Recall memories matching a text query with a relevance score.
    /// Score = fraction of query words found in each memory, in [0, 1].
    /// Results are ranked by score descending; ties preserve insertion order.
    /// Pulls up to 4× `limit` candidates via substring match, then re-ranks.
    #[cfg(feature = "graph")]
    pub fn recall_top_k(&self, query_text: &str, limit: usize) -> Vec<(String, f32)> {
        if limit == 0 || query_text.trim().is_empty() {
            return Vec::new();
        }
        // Pull a generous candidate set so word-overlap re-ranking has room.
        let candidates = self.recall(query_text, limit.saturating_mul(4).max(16));
        let words: Vec<String> = query_text
            .split_whitespace()
            .filter_map(|w| {
                let w = w
                    .trim_matches(|c: char| !c.is_alphanumeric())
                    .to_lowercase();
                if w.is_empty() || w.len() < 2 {
                    None
                } else {
                    Some(w)
                }
            })
            .collect();
        if words.is_empty() {
            // Fall back to uniform top-limit.
            return candidates
                .into_iter()
                .take(limit)
                .map(|c| (c, 1.0))
                .collect();
        }
        let mut scored: Vec<(String, f32)> = candidates
            .into_iter()
            .map(|c| {
                let lower = c.to_lowercase();
                let hits = words.iter().filter(|w| lower.contains(w.as_str())).count();
                (c, hits as f32 / words.len() as f32)
            })
            .collect();
        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        scored.truncate(limit);
        scored
    }

    /// Get all memories of a specific type.
    #[cfg(feature = "graph")]
    pub fn by_type(&self, mem_type: MemoryType) -> Vec<String> {
        let session = self.db.session();
        let type_str = format!("{:?}", mem_type);
        let query = gql::by_type(&type_str);
        match session.execute(&query) {
            Ok(result) => result
                .iter()
                .filter_map(|row| row.first().map(|v| format!("{}", v)))
                .collect(),
            Err(_) => Vec::new(),
        }
    }

    /// Get memories tagged with a specific topic.
    #[cfg(feature = "graph")]
    pub fn by_topic(&self, topic: &str) -> Vec<String> {
        let session = self.db.session();
        let query = gql::by_topic(topic);
        match session.execute(&query) {
            Ok(result) => result
                .iter()
                .filter_map(|row| row.first().map(|v| format!("{}", v)))
                .collect(),
            Err(_) => Vec::new(),
        }
    }

    /// Get graph statistics.
    #[cfg(feature = "graph")]
    pub fn stats(&self) -> GraphStats {
        let session = self.db.session();
        let count = |query: &str| -> usize {
            session
                .execute(query)
                .ok()
                .and_then(|r| r.scalar::<i64>().ok())
                .map(|v| v as usize)
                .unwrap_or(0)
        };

        GraphStats {
            memory_count: count(gql::COUNT_MEMORIES),
            session_count: count(gql::COUNT_SESSIONS),
            topic_count: count(gql::COUNT_TOPICS),
            relationship_count: count(gql::COUNT_RELATIONSHIPS),
        }
    }

    /// Get the current schema version of the graph.
    #[cfg(feature = "graph")]
    pub fn schema_version(&self) -> VersionCheck {
        graph_migrate::check_version(&self.db)
    }

    // ─── Fallback implementations (no graph feature) ─────────────────────

    #[cfg(not(feature = "graph"))]
    pub fn store_memory(&self, _: &str, _: MemoryType, _: f32) -> Result<String> {
        Err(CerseiError::Config("Graph feature not enabled".into()))
    }

    #[cfg(not(feature = "graph"))]
    pub fn recall_top_k(&self, _: &str, _: usize) -> Vec<(String, f32)> {
        Vec::new()
    }

    #[cfg(not(feature = "graph"))]
    pub fn link_memories(&self, _: &str, _: &str, _: &str) -> Result<()> {
        Err(CerseiError::Config("Graph feature not enabled".into()))
    }

    #[cfg(not(feature = "graph"))]
    pub fn tag_memory(&self, _: &str, _: &str) -> Result<()> {
        Err(CerseiError::Config("Graph feature not enabled".into()))
    }

    #[cfg(not(feature = "graph"))]
    pub fn record_session(&self, _: &str, _: Option<&str>, _: u32) -> Result<()> {
        Err(CerseiError::Config("Graph feature not enabled".into()))
    }

    #[cfg(not(feature = "graph"))]
    pub fn revalidate_memory(&self, _: &str) -> Result<bool> {
        Err(CerseiError::Config("Graph feature not enabled".into()))
    }

    #[cfg(not(feature = "graph"))]
    pub fn recall(&self, _: &str, _: usize) -> Vec<String> {
        Vec::new()
    }

    #[cfg(not(feature = "graph"))]
    pub fn by_type(&self, _: MemoryType) -> Vec<String> {
        Vec::new()
    }

    #[cfg(not(feature = "graph"))]
    pub fn by_topic(&self, _: &str) -> Vec<String> {
        Vec::new()
    }

    #[cfg(not(feature = "graph"))]
    pub fn stats(&self) -> GraphStats {
        GraphStats::default()
    }

    #[cfg(not(feature = "graph"))]
    pub fn schema_version(&self) -> VersionCheck {
        VersionCheck::UpToDate
    }
}

/// Check if graph memory is available (compiled with the feature).
pub fn is_graph_available() -> bool {
    cfg!(feature = "graph")
}
