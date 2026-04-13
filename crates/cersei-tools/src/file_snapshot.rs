//! File snapshot system: stores before/after content per tool call for undo.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// A snapshot of a file's content before a tool modified it.
#[derive(Debug, Clone)]
pub struct FileSnapshot {
    pub path: PathBuf,
    pub before: String,
    pub after: String,
    pub tool_name: String,
    pub tool_call_id: String,
    pub timestamp: u64,
}

/// Session-level snapshot manager for undo support.
#[derive(Debug, Clone, Default)]
pub struct SnapshotManager {
    /// All snapshots, ordered by time.
    snapshots: Vec<FileSnapshot>,
}

impl SnapshotManager {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a file modification. Call BEFORE writing the new content.
    pub fn record(
        &mut self,
        path: &Path,
        before: &str,
        after: &str,
        tool_name: &str,
        tool_call_id: &str,
    ) {
        self.snapshots.push(FileSnapshot {
            path: path.to_path_buf(),
            before: before.to_string(),
            after: after.to_string(),
            tool_name: tool_name.to_string(),
            tool_call_id: tool_call_id.to_string(),
            timestamp: now_secs(),
        });
    }

    /// Undo the last modification to a file. Returns the restored content.
    pub fn undo_last(&mut self, path: &Path) -> Option<String> {
        // Find the most recent snapshot for this path
        if let Some(idx) = self.snapshots.iter().rposition(|s| s.path == path) {
            let snapshot = self.snapshots.remove(idx);
            // Write the before content back to disk
            if std::fs::write(&snapshot.path, &snapshot.before).is_ok() {
                return Some(snapshot.before);
            }
        }
        None
    }

    /// Undo all changes from a specific tool call ID.
    pub fn undo_tool_call(&mut self, tool_call_id: &str) -> Vec<PathBuf> {
        let mut reverted = Vec::new();
        let matching: Vec<usize> = self.snapshots.iter()
            .enumerate()
            .filter(|(_, s)| s.tool_call_id == tool_call_id)
            .map(|(i, _)| i)
            .collect();

        // Process in reverse order to handle multiple edits to same file
        for idx in matching.into_iter().rev() {
            let snapshot = &self.snapshots[idx];
            if std::fs::write(&snapshot.path, &snapshot.before).is_ok() {
                reverted.push(snapshot.path.clone());
            }
        }

        // Remove the matching snapshots
        self.snapshots.retain(|s| s.tool_call_id != tool_call_id);
        reverted
    }

    /// Undo ALL changes in this session (nuclear option).
    pub fn undo_all(&mut self) -> Vec<PathBuf> {
        let mut reverted = Vec::new();

        // Group by file, restore each to its ORIGINAL state (first snapshot's before)
        let mut first_states: HashMap<PathBuf, String> = HashMap::new();
        for snapshot in &self.snapshots {
            first_states.entry(snapshot.path.clone())
                .or_insert_with(|| snapshot.before.clone());
        }

        for (path, original) in &first_states {
            if std::fs::write(path, original).is_ok() {
                reverted.push(path.clone());
            }
        }

        self.snapshots.clear();
        reverted
    }

    /// Get recent snapshots (newest first).
    pub fn recent(&self, limit: usize) -> Vec<&FileSnapshot> {
        self.snapshots.iter().rev().take(limit).collect()
    }

    /// Get all snapshots for a file.
    pub fn for_file(&self, path: &Path) -> Vec<&FileSnapshot> {
        self.snapshots.iter().filter(|s| s.path == path).collect()
    }

    /// Total number of snapshots.
    pub fn count(&self) -> usize {
        self.snapshots.len()
    }

    /// List of unique files that have been modified.
    pub fn modified_files(&self) -> Vec<PathBuf> {
        let mut files: Vec<PathBuf> = self.snapshots.iter()
            .map(|s| s.path.clone())
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .collect();
        files.sort();
        files
    }
}

/// Global snapshot registry keyed by session_id.
static SNAPSHOT_REGISTRY: once_cell::sync::Lazy<
    dashmap::DashMap<String, std::sync::Arc<parking_lot::Mutex<SnapshotManager>>>,
> = once_cell::sync::Lazy::new(dashmap::DashMap::new);

/// Get or create the snapshot manager for a session.
pub fn session_snapshots(session_id: &str) -> std::sync::Arc<parking_lot::Mutex<SnapshotManager>> {
    SNAPSHOT_REGISTRY
        .entry(session_id.to_string())
        .or_insert_with(|| std::sync::Arc::new(parking_lot::Mutex::new(SnapshotManager::new())))
        .clone()
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn test_record_and_undo() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("test.txt");
        std::fs::write(&file, "original").unwrap();

        let mut mgr = SnapshotManager::new();
        mgr.record(&file, "original", "modified", "Edit", "call-1");
        std::fs::write(&file, "modified").unwrap();

        assert_eq!(mgr.count(), 1);
        let restored = mgr.undo_last(&file);
        assert_eq!(restored, Some("original".to_string()));
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "original");
    }

    #[test]
    fn test_undo_tool_call() {
        let dir = tempfile::tempdir().unwrap();
        let f1 = dir.path().join("a.txt");
        let f2 = dir.path().join("b.txt");
        std::fs::write(&f1, "a-orig").unwrap();
        std::fs::write(&f2, "b-orig").unwrap();

        let mut mgr = SnapshotManager::new();
        mgr.record(&f1, "a-orig", "a-new", "Edit", "call-1");
        mgr.record(&f2, "b-orig", "b-new", "Edit", "call-1");
        std::fs::write(&f1, "a-new").unwrap();
        std::fs::write(&f2, "b-new").unwrap();

        let reverted = mgr.undo_tool_call("call-1");
        assert_eq!(reverted.len(), 2);
        assert_eq!(std::fs::read_to_string(&f1).unwrap(), "a-orig");
        assert_eq!(std::fs::read_to_string(&f2).unwrap(), "b-orig");
    }

    #[test]
    fn test_modified_files() {
        let mut mgr = SnapshotManager::new();
        mgr.record(Path::new("/a.rs"), "x", "y", "Edit", "c1");
        mgr.record(Path::new("/b.rs"), "x", "y", "Write", "c2");
        mgr.record(Path::new("/a.rs"), "y", "z", "Edit", "c3");
        assert_eq!(mgr.modified_files().len(), 2);
    }
}
