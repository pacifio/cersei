//! File watching: detect changes in the project directory.

#[cfg(feature = "file-watch")]
use notify::{Event, RecommendedWatcher, RecursiveMode, Watcher};
use parking_lot::Mutex;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Tracks recently changed files.
#[derive(Debug, Clone, Default)]
pub struct FileChangeTracker {
    changed_files: Vec<PathBuf>,
    max_tracked: usize,
}

impl FileChangeTracker {
    pub fn new(max: usize) -> Self {
        Self {
            changed_files: Vec::new(),
            max_tracked: max,
        }
    }

    pub fn record_change(&mut self, path: PathBuf) {
        if !self.changed_files.contains(&path) {
            self.changed_files.push(path);
            if self.changed_files.len() > self.max_tracked {
                self.changed_files.remove(0);
            }
        }
    }

    pub fn drain(&mut self) -> Vec<PathBuf> {
        std::mem::take(&mut self.changed_files)
    }

    pub fn recent(&self) -> &[PathBuf] {
        &self.changed_files
    }
}

/// Start watching a directory for file changes.
#[cfg(feature = "file-watch")]
pub fn watch_directory(
    root: &Path,
    tracker: Arc<Mutex<FileChangeTracker>>,
) -> Option<RecommendedWatcher> {
    let root = root.to_path_buf();
    let mut watcher = notify::recommended_watcher(move |res: Result<Event, _>| {
        if let Ok(event) = res {
            for path in event.paths {
                // Skip hidden files, target dirs, node_modules
                let path_str = path.display().to_string();
                if path_str.contains("/.git/")
                    || path_str.contains("/target/")
                    || path_str.contains("/node_modules/")
                    || path_str.contains("/__pycache__/")
                {
                    continue;
                }
                tracker.lock().record_change(path);
            }
        }
    })
    .ok()?;

    watcher.watch(&root, RecursiveMode::Recursive).ok()?;
    Some(watcher)
}

#[cfg(not(feature = "file-watch"))]
pub fn watch_directory(_root: &Path, _tracker: Arc<Mutex<FileChangeTracker>>) -> Option<()> {
    None
}
