//! Graph schema versioning and migration engine.
//!
//! Stores a `(:SchemaVersion)` node inside the graph. On open, the code checks
//! the graph version against `CURRENT_SCHEMA_VERSION` and runs sequential
//! migrations if the graph is behind. Each migration is idempotent.
//!
//! ## Version History
//! - v0: Pre-versioning (no SchemaVersion node)
//! - v1: Stamp version node (current schema, no data changes)
//! - v2: Add `last_validated_at`, `decay_rate`, `embedding_model_version` to Memory nodes

#[cfg(feature = "graph")]
use grafeo::GrafeoDB;

/// Current schema version that this code expects.
pub const CURRENT_SCHEMA_VERSION: u32 = 2;

/// Result of a schema version check.
#[derive(Debug, PartialEq)]
pub enum VersionCheck {
    /// Graph is at the expected version.
    UpToDate,
    /// Graph needs migration from `from` to `to`.
    NeedsMigration { from: u32, to: u32 },
    /// Graph is ahead of this code (opened by newer code previously).
    CodeBehind {
        graph_version: u32,
        code_version: u32,
    },
}

// ─── GQL queries for version management ────────────────────────────────────

mod queries {
    pub const READ_VERSION: &str = "MATCH (v:SchemaVersion) RETURN v.version";

    pub fn insert_version(version: u32, now: &str, code_ver: &str) -> String {
        format!(
            "INSERT (:SchemaVersion {{singleton: 'schema_version', version: {}, migrated_at: '{}', code_version: '{}'}})",
            version, now, code_ver
        )
    }

    /// Migration v1→v2: add decay and embedding fields to Memory nodes that lack them.
    /// Grafeo is schema-less, so we SET properties on existing nodes.
    /// Idempotent: only targets nodes where last_validated_at is not already set.
    ///
    /// Since Grafeo may not support `WHERE ... IS NULL` or `SET` in a single query,
    /// we do this in Rust by iterating. See `migrate_v1_to_v2`.
    pub const MATCH_ALL_MEMORIES: &str = "MATCH (m:Memory) RETURN m.id, m.created_at";
}

// ─── Version check ─────────────────────────────────────────────────────────

/// Check the graph's schema version against the code's expected version.
#[cfg(feature = "graph")]
pub fn check_version(db: &GrafeoDB) -> VersionCheck {
    let session = db.session();

    match session.execute(queries::READ_VERSION) {
        Ok(result) => {
            if let Some(row) = result.iter().next() {
                // Try to extract version as i64 then cast
                let graph_ver = row
                    .first()
                    .and_then(|v| format!("{}", v).parse::<u32>().ok())
                    .unwrap_or(0);

                if graph_ver == CURRENT_SCHEMA_VERSION {
                    VersionCheck::UpToDate
                } else if graph_ver < CURRENT_SCHEMA_VERSION {
                    VersionCheck::NeedsMigration {
                        from: graph_ver,
                        to: CURRENT_SCHEMA_VERSION,
                    }
                } else {
                    VersionCheck::CodeBehind {
                        graph_version: graph_ver,
                        code_version: CURRENT_SCHEMA_VERSION,
                    }
                }
            } else {
                // No SchemaVersion node → pre-versioning (v0)
                VersionCheck::NeedsMigration {
                    from: 0,
                    to: CURRENT_SCHEMA_VERSION,
                }
            }
        }
        Err(_) => {
            // Query failed → treat as v0
            VersionCheck::NeedsMigration {
                from: 0,
                to: CURRENT_SCHEMA_VERSION,
            }
        }
    }
}

/// Fallback when graph feature is disabled.
#[cfg(not(feature = "graph"))]
pub fn check_version(_db: &()) -> VersionCheck {
    VersionCheck::UpToDate
}

// ─── Migration runner ──────────────────────────────────────────────────────

/// Run all necessary migrations from `from` to `to`.
/// Each step is idempotent. Updates the SchemaVersion node on completion.
#[cfg(feature = "graph")]
pub fn run_migrations(db: &GrafeoDB, from: u32, to: u32) -> cersei_types::Result<()> {
    tracing::info!("Migrating graph schema from v{} to v{}", from, to);

    let mut current = from;
    while current < to {
        match current {
            0 => migrate_v0_to_v1(db)?,
            1 => migrate_v1_to_v2(db)?,
            _ => {
                return Err(cersei_types::CerseiError::Config(format!(
                    "Unknown migration: v{} → v{}",
                    current,
                    current + 1
                )));
            }
        }
        current += 1;
    }

    // Stamp the final version
    stamp_version(db, to)?;

    tracing::info!("Graph schema migration complete: v{}", to);
    Ok(())
}

/// Fallback when graph feature is disabled.
#[cfg(not(feature = "graph"))]
pub fn run_migrations(_db: &(), _from: u32, _to: u32) -> cersei_types::Result<()> {
    Ok(())
}

// ─── Version stamping ──────────────────────────────────────────────────────

#[cfg(feature = "graph")]
fn stamp_version(db: &GrafeoDB, version: u32) -> cersei_types::Result<()> {
    let session = db.session();
    let now = chrono::Utc::now().to_rfc3339();
    let code_ver = env!("CARGO_PKG_VERSION");

    // Delete old version node if exists, then insert fresh one.
    // This is simpler than trying to UPDATE which Grafeo may not support.
    let _ = session.execute("MATCH (v:SchemaVersion) DELETE v");
    session
        .execute(&queries::insert_version(version, &now, code_ver))
        .map_err(|e| {
            cersei_types::CerseiError::Config(format!("Failed to stamp schema version: {}", e))
        })?;

    Ok(())
}

// ─── Individual migrations ─────────────────────────────────────────────────

/// v0 → v1: Stamp initial schema version. No data changes needed —
/// v1 IS the pre-existing schema.
#[cfg(feature = "graph")]
fn migrate_v0_to_v1(db: &GrafeoDB) -> cersei_types::Result<()> {
    tracing::debug!("Running migration v0 → v1 (stamp version, no data changes)");
    // Nothing to change — v1 is the original schema.
    // The stamp_version call after all migrations handles creating the node.
    Ok(())
}

/// v1 → v2: Add confidence decay and embedding fields to all Memory nodes.
///
/// New fields on :Memory:
/// - last_validated_at (String, defaults to created_at)
/// - decay_rate (f32, defaults to 0.01)
/// - embedding_model_version (String, defaults to "")
///
/// Idempotent: re-running is safe because we only modify nodes that exist.
/// Since the INSERT creates new nodes with these fields, and old nodes
/// just don't have them, the forward-compatible read helpers handle the gap.
#[cfg(feature = "graph")]
fn migrate_v1_to_v2(db: &GrafeoDB) -> cersei_types::Result<()> {
    tracing::debug!("Running migration v1 → v2 (add decay/embedding fields)");

    // Grafeo is schema-less. "Adding fields" means new INSERT statements include them.
    // Old nodes simply lack these properties — the read-time helpers in graph.rs
    // (effective_confidence, prop_or_default) handle missing fields with defaults.
    //
    // We don't iterate and SET each existing node because:
    // 1. Grafeo may not support MATCH ... SET syntax
    // 2. The read-time defaults produce identical behavior
    // 3. Iterating thousands of nodes in a migration is slow and fragile
    //
    // The tradeoff: old nodes never get the physical properties, but they
    // behave identically through the API because defaults are applied at read time.

    Ok(())
}

// ─── Confidence decay ──────────────────────────────────────────────────────

/// Calculate effective confidence with time-based decay.
///
/// `effective = base_confidence - (decay_rate * days_since_validation)`
///
/// Clamped to [0.0, 1.0]. If `last_validated_at` is invalid or missing,
/// returns `base_confidence` unchanged (no decay).
pub fn effective_confidence(base: f32, decay_rate: f32, last_validated_at: &str) -> f32 {
    if last_validated_at.is_empty() || decay_rate <= 0.0 {
        return base.clamp(0.0, 1.0);
    }

    let validated = match chrono::DateTime::parse_from_rfc3339(last_validated_at) {
        Ok(dt) => dt.with_timezone(&chrono::Utc),
        Err(_) => return base.clamp(0.0, 1.0),
    };

    let days = (chrono::Utc::now() - validated).num_days().max(0) as f32;
    (base - decay_rate * days).clamp(0.0, 1.0)
}

// ─── Tests ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_effective_confidence_no_decay() {
        assert_eq!(effective_confidence(0.9, 0.0, ""), 0.9);
        assert_eq!(effective_confidence(0.9, 0.0, "2024-01-01T00:00:00Z"), 0.9);
    }

    #[test]
    fn test_effective_confidence_with_decay() {
        // Use a date far in the past for deterministic test
        let old = "2020-01-01T00:00:00Z";
        let result = effective_confidence(0.9, 0.01, old);
        // Should be significantly decayed (>2000 days * 0.01 = >20, clamped to 0.0)
        assert_eq!(result, 0.0);
    }

    #[test]
    fn test_effective_confidence_recent() {
        let now = chrono::Utc::now().to_rfc3339();
        let result = effective_confidence(0.9, 0.01, &now);
        // Just validated — should be ~0.9 (0 days decay)
        assert!((result - 0.9).abs() < 0.02);
    }

    #[test]
    fn test_effective_confidence_invalid_date() {
        assert_eq!(effective_confidence(0.8, 0.01, "not-a-date"), 0.8);
    }

    #[test]
    fn test_effective_confidence_clamps() {
        assert_eq!(effective_confidence(1.5, 0.0, ""), 1.0);
        assert_eq!(effective_confidence(-0.5, 0.0, ""), 0.0);
    }

    #[cfg(feature = "graph")]
    #[test]
    fn test_check_version_fresh_graph() {
        let db = GrafeoDB::new_in_memory();
        let check = check_version(&db);
        assert_eq!(
            check,
            VersionCheck::NeedsMigration {
                from: 0,
                to: CURRENT_SCHEMA_VERSION
            }
        );
    }

    #[cfg(feature = "graph")]
    #[test]
    fn test_migration_and_recheck() {
        let db = GrafeoDB::new_in_memory();

        // Fresh graph → needs migration
        let check = check_version(&db);
        assert_eq!(check, VersionCheck::NeedsMigration { from: 0, to: 2 });

        // Run migrations
        run_migrations(&db, 0, 2).unwrap();

        // Now should be up to date
        let check = check_version(&db);
        assert_eq!(check, VersionCheck::UpToDate);
    }

    #[cfg(feature = "graph")]
    #[test]
    fn test_migration_idempotent() {
        let db = GrafeoDB::new_in_memory();

        // Run migrations twice — should not fail
        run_migrations(&db, 0, 2).unwrap();
        run_migrations(&db, 0, 2).unwrap();

        let check = check_version(&db);
        assert_eq!(check, VersionCheck::UpToDate);
    }
}
