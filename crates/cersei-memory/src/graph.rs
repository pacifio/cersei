//! Graph-backed memory using Grafeo embedded graph database.
//!
//! Optional feature: enable with `features = ["graph"]` in Cargo.toml.
//!
//! Provides relationship-aware memory storage where memories are nodes,
//! relationships are edges, and queries traverse the graph for context recall.
//!
//! ## Schema
//! ```text
//! (:Memory {content, mem_type, confidence, created_at, updated_at})
//!   -[:RELATES_TO {relationship, weight}]-> (:Memory)
//!
//! (:Session {session_id, started_at, model, turns})
//!   -[:PRODUCED]-> (:Memory)
//!
//! (:Topic {name})
//!   -[:TAGGED]-> (:Memory)
//! ```

#[cfg(feature = "graph")]
use grafeo::GrafeoDB;

use crate::memdir::MemoryType;
use cersei_types::*;
use std::path::Path;

/// Graph-backed memory store.
///
/// When the `graph` feature is enabled, this uses Grafeo for structured
/// memory with relationship tracking. Without the feature, all methods
/// return empty results (no-op fallback).
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

impl GraphMemory {
    /// Open a persistent graph database at the given path.
    #[cfg(feature = "graph")]
    pub fn open(path: &Path) -> Result<Self> {
        let db = GrafeoDB::open(path)
            .map_err(|e| CerseiError::Config(format!("Failed to open graph DB: {}", e)))?;
        Ok(Self { db })
    }

    /// Create an in-memory graph database (no persistence).
    #[cfg(feature = "graph")]
    pub fn open_in_memory() -> Result<Self> {
        let db = GrafeoDB::new_in_memory();
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

    /// Store a memory as a graph node.
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

        // Escape content for GQL (replace single quotes)
        let escaped = content.replace('\'', "\\'").replace('\\', "\\\\");

        let query = format!(
            "INSERT (:Memory {{id: '{}', content: '{}', mem_type: '{}', confidence: {}, created_at: '{}', updated_at: '{}'}})",
            id, escaped, mem_type_str, confidence, now, now
        );

        session.execute(&query)
            .map_err(|e| CerseiError::Config(format!("Graph insert failed: {}", e)))?;

        Ok(id)
    }

    /// Link two memories with a named relationship.
    #[cfg(feature = "graph")]
    pub fn link_memories(
        &self,
        from_id: &str,
        to_id: &str,
        relationship: &str,
    ) -> Result<()> {
        let session = self.db.session();
        let query = format!(
            "MATCH (a:Memory {{id: '{}'}}), (b:Memory {{id: '{}'}}) \
             INSERT (a)-[:RELATES_TO {{relationship: '{}'}}]->(b)",
            from_id, to_id, relationship
        );
        session.execute(&query)
            .map_err(|e| CerseiError::Config(format!("Graph link failed: {}", e)))?;
        Ok(())
    }

    /// Tag a memory with a topic.
    #[cfg(feature = "graph")]
    pub fn tag_memory(&self, memory_id: &str, topic: &str) -> Result<()> {
        let session = self.db.session();
        // Create topic if not exists, then link
        let query = format!(
            "MATCH (m:Memory {{id: '{}'}}) \
             INSERT (:Topic {{name: '{}'}})-[:TAGGED]->(m)",
            memory_id, topic
        );
        session.execute(&query)
            .map_err(|e| CerseiError::Config(format!("Graph tag failed: {}", e)))?;
        Ok(())
    }

    /// Record a session in the graph.
    #[cfg(feature = "graph")]
    pub fn record_session(
        &self,
        session_id: &str,
        model: Option<&str>,
        turns: u32,
    ) -> Result<()> {
        let session = self.db.session();
        let now = chrono::Utc::now().to_rfc3339();
        let model_str = model.unwrap_or("unknown");
        let query = format!(
            "INSERT (:Session {{session_id: '{}', started_at: '{}', model: '{}', turns: {}}})",
            session_id, now, model_str, turns
        );
        session.execute(&query)
            .map_err(|e| CerseiError::Config(format!("Graph session record failed: {}", e)))?;
        Ok(())
    }

    // ─── Query operations ────────────────────────────────────────────────

    /// Recall memories matching a text query (substring match).
    #[cfg(feature = "graph")]
    pub fn recall(&self, query_text: &str, limit: usize) -> Vec<String> {
        let session = self.db.session();
        let escaped = query_text.replace('\'', "\\'");
        let query = format!(
            "MATCH (m:Memory) WHERE m.content CONTAINS '{}' RETURN m.content LIMIT {}",
            escaped, limit
        );
        match session.execute(&query) {
            Ok(result) => {
                result.iter()
                    .filter_map(|row| row.first().map(|v| format!("{}", v)))
                    .collect()
            }
            Err(_) => Vec::new(),
        }
    }

    /// Get all memories of a specific type.
    #[cfg(feature = "graph")]
    pub fn by_type(&self, mem_type: MemoryType) -> Vec<String> {
        let session = self.db.session();
        let type_str = format!("{:?}", mem_type);
        let query = format!(
            "MATCH (m:Memory {{mem_type: '{}'}}) RETURN m.content",
            type_str
        );
        match session.execute(&query) {
            Ok(result) => {
                result.iter()
                    .filter_map(|row| row.first().map(|v| format!("{}", v)))
                    .collect()
            }
            Err(_) => Vec::new(),
        }
    }

    /// Get memories tagged with a specific topic.
    #[cfg(feature = "graph")]
    pub fn by_topic(&self, topic: &str) -> Vec<String> {
        let session = self.db.session();
        let query = format!(
            "MATCH (:Topic {{name: '{}'}})-[:TAGGED]->(m:Memory) RETURN m.content",
            topic
        );
        match session.execute(&query) {
            Ok(result) => {
                result.iter()
                    .filter_map(|row| row.first().map(|v| format!("{}", v)))
                    .collect()
            }
            Err(_) => Vec::new(),
        }
    }

    /// Get graph statistics.
    #[cfg(feature = "graph")]
    pub fn stats(&self) -> GraphStats {
        let session = self.db.session();
        let count = |query: &str| -> usize {
            session.execute(query)
                .ok()
                .and_then(|r| r.scalar::<i64>().ok())
                .map(|v| v as usize)
                .unwrap_or(0)
        };

        GraphStats {
            memory_count: count("MATCH (m:Memory) RETURN count(m)"),
            session_count: count("MATCH (s:Session) RETURN count(s)"),
            topic_count: count("MATCH (t:Topic) RETURN count(t)"),
            relationship_count: count("MATCH ()-[r:RELATES_TO]->() RETURN count(r)"),
        }
    }

    // ─── Fallback implementations (no graph feature) ─────────────────────

    #[cfg(not(feature = "graph"))]
    pub fn store_memory(&self, _: &str, _: MemoryType, _: f32) -> Result<String> {
        Err(CerseiError::Config("Graph feature not enabled".into()))
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
    pub fn recall(&self, _: &str, _: usize) -> Vec<String> { Vec::new() }

    #[cfg(not(feature = "graph"))]
    pub fn by_type(&self, _: MemoryType) -> Vec<String> { Vec::new() }

    #[cfg(not(feature = "graph"))]
    pub fn by_topic(&self, _: &str) -> Vec<String> { Vec::new() }

    #[cfg(not(feature = "graph"))]
    pub fn stats(&self) -> GraphStats { GraphStats::default() }
}

/// Check if graph memory is available (compiled with the feature).
pub fn is_graph_available() -> bool {
    cfg!(feature = "graph")
}
