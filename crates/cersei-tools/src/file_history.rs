//! File history: track which files were accessed during a session.
//!
//! Used for context injection ("files you've been working on").

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

/// Tracks file access during a session.
#[derive(Debug, Clone, Default)]
pub struct FileHistory {
    entries: HashMap<PathBuf, FileAccess>,
}

#[derive(Debug, Clone)]
pub struct FileAccess {
    pub path: PathBuf,
    pub read_count: u32,
    pub write_count: u32,
    pub edit_count: u32,
    pub last_accessed: u64,
}

impl FileHistory {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn record_read(&mut self, path: &PathBuf) {
        let entry = self
            .entries
            .entry(path.clone())
            .or_insert_with(|| FileAccess {
                path: path.clone(),
                read_count: 0,
                write_count: 0,
                edit_count: 0,
                last_accessed: 0,
            });
        entry.read_count += 1;
        entry.last_accessed = now_secs();
    }

    pub fn record_write(&mut self, path: &PathBuf) {
        let entry = self
            .entries
            .entry(path.clone())
            .or_insert_with(|| FileAccess {
                path: path.clone(),
                read_count: 0,
                write_count: 0,
                edit_count: 0,
                last_accessed: 0,
            });
        entry.write_count += 1;
        entry.last_accessed = now_secs();
    }

    pub fn record_edit(&mut self, path: &PathBuf) {
        let entry = self
            .entries
            .entry(path.clone())
            .or_insert_with(|| FileAccess {
                path: path.clone(),
                read_count: 0,
                write_count: 0,
                edit_count: 0,
                last_accessed: 0,
            });
        entry.edit_count += 1;
        entry.last_accessed = now_secs();
    }

    /// Get all accessed files, sorted by last access (most recent first).
    pub fn all_files(&self) -> Vec<&FileAccess> {
        let mut files: Vec<_> = self.entries.values().collect();
        files.sort_by(|a, b| b.last_accessed.cmp(&a.last_accessed));
        files
    }

    /// Get modified files (written or edited), sorted by recency.
    pub fn modified_files(&self) -> Vec<&FileAccess> {
        let mut files: Vec<_> = self
            .entries
            .values()
            .filter(|f| f.write_count > 0 || f.edit_count > 0)
            .collect();
        files.sort_by(|a, b| b.last_accessed.cmp(&a.last_accessed));
        files
    }

    /// Build a context string for the system prompt.
    pub fn build_context(&self) -> Option<String> {
        let modified = self.modified_files();
        if modified.is_empty() {
            return None;
        }

        let lines: Vec<String> = modified
            .iter()
            .take(20)
            .map(|f| {
                let ops = format!(
                    "{}{}{}",
                    if f.read_count > 0 {
                        format!("r{} ", f.read_count)
                    } else {
                        String::new()
                    },
                    if f.write_count > 0 {
                        format!("w{} ", f.write_count)
                    } else {
                        String::new()
                    },
                    if f.edit_count > 0 {
                        format!("e{}", f.edit_count)
                    } else {
                        String::new()
                    },
                );
                format!("- {} ({})", f.path.display(), ops.trim())
            })
            .collect();

        Some(format!(
            "Files modified this session:\n{}",
            lines.join("\n")
        ))
    }

    pub fn file_count(&self) -> usize {
        self.entries.len()
    }

    pub fn clear(&mut self) {
        self.entries.clear();
    }
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_file_history() {
        let mut history = FileHistory::new();
        let path1 = PathBuf::from("src/main.rs");
        let path2 = PathBuf::from("Cargo.toml");

        history.record_read(&path1);
        history.record_read(&path1);
        history.record_edit(&path1);
        history.record_write(&path2);

        assert_eq!(history.file_count(), 2);
        assert_eq!(history.all_files().len(), 2);
        assert_eq!(history.modified_files().len(), 2);

        let main = history.entries.get(&path1).unwrap();
        assert_eq!(main.read_count, 2);
        assert_eq!(main.edit_count, 1);
    }

    #[test]
    fn test_build_context() {
        let mut history = FileHistory::new();
        history.record_edit(&PathBuf::from("src/lib.rs"));
        history.record_write(&PathBuf::from("README.md"));

        let ctx = history.build_context();
        assert!(ctx.is_some());
        assert!(ctx.unwrap().contains("src/lib.rs"));
    }

    #[test]
    fn test_empty_context() {
        let history = FileHistory::new();
        assert!(history.build_context().is_none());

        // Read-only doesn't count as modified
        let mut history = FileHistory::new();
        history.record_read(&PathBuf::from("file.txt"));
        assert!(history.build_context().is_none());
    }
}
