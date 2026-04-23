//! Auto-dream: background memory consolidation daemon.
//!
//! Three-gate system (cheapest first):
//! 1. Time gate: ≥24 hours since last consolidation
//! 2. Session gate: ≥5 new sessions since last
//! 3. Lock gate: no concurrent consolidation (stale after 1 hour)

use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

// ─── Constants ───────────────────────────────────────────────────────────────

const MIN_HOURS_DEFAULT: f64 = 24.0;
const MIN_SESSIONS_DEFAULT: usize = 5;
const LOCK_STALE_SECS: u64 = 3600; // 1 hour

// ─── Types ───────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ConsolidationState {
    pub last_consolidated_at: Option<u64>,
    pub lock_etag: Option<String>,
}

#[derive(Debug, Clone)]
pub struct AutoDreamConfig {
    pub min_hours: f64,
    pub min_sessions: usize,
}

impl Default for AutoDreamConfig {
    fn default() -> Self {
        Self {
            min_hours: MIN_HOURS_DEFAULT,
            min_sessions: MIN_SESSIONS_DEFAULT,
        }
    }
}

pub struct AutoDream {
    pub memory_dir: PathBuf,
    pub conversations_dir: PathBuf,
    pub config: AutoDreamConfig,
}

impl AutoDream {
    pub fn new(memory_dir: PathBuf, conversations_dir: PathBuf) -> Self {
        Self {
            memory_dir,
            conversations_dir,
            config: AutoDreamConfig::default(),
        }
    }

    pub fn with_config(mut self, config: AutoDreamConfig) -> Self {
        self.config = config;
        self
    }

    // ─── State persistence ───────────────────────────────────────────────

    fn state_path(&self) -> PathBuf {
        self.memory_dir.join(".consolidation_state.json")
    }

    fn lock_path(&self) -> PathBuf {
        self.memory_dir.join(".consolidation_lock")
    }

    pub fn load_state(&self) -> ConsolidationState {
        let path = self.state_path();
        std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn save_state(&self, state: &ConsolidationState) -> std::io::Result<()> {
        let path = self.state_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, serde_json::to_string_pretty(state)?)
    }

    pub fn update_state(&self) -> std::io::Result<()> {
        let now = now_secs();
        let state = ConsolidationState {
            last_consolidated_at: Some(now),
            lock_etag: None,
        };
        self.save_state(&state)
    }

    // ─── Gate checks ─────────────────────────────────────────────────────

    /// Gate 1: Has enough time passed since last consolidation?
    pub fn time_gate_passes(&self, state: &ConsolidationState) -> bool {
        match state.last_consolidated_at {
            None => true, // never consolidated
            Some(last) => {
                let now = now_secs();
                let hours_elapsed = (now - last) as f64 / 3600.0;
                hours_elapsed >= self.config.min_hours
            }
        }
    }

    /// Gate 2: Are there enough new sessions?
    pub fn session_gate_passes(&self, state: &ConsolidationState) -> bool {
        let since = state.last_consolidated_at.unwrap_or(0);

        let entries = match std::fs::read_dir(&self.conversations_dir) {
            Ok(e) => e,
            Err(_) => return false,
        };

        let mut count = 0;
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                continue;
            }
            let mtime = std::fs::metadata(&path)
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map(|d| d.as_secs())
                .unwrap_or(0);

            if mtime > since {
                count += 1;
                if count >= self.config.min_sessions {
                    return true; // early exit
                }
            }
        }

        false
    }

    /// Gate 3: Is there no active consolidation lock?
    pub fn lock_gate_passes(&self) -> bool {
        let lock = self.lock_path();
        if !lock.exists() {
            return true;
        }

        // Check if lock is stale
        let mtime = std::fs::metadata(&lock)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_secs())
            .unwrap_or(0);

        let now = now_secs();
        (now - mtime) > LOCK_STALE_SECS
    }

    /// Check all gates in order (cheapest first).
    pub fn should_consolidate(&self) -> bool {
        let state = self.load_state();
        self.time_gate_passes(&state) && self.session_gate_passes(&state) && self.lock_gate_passes()
    }

    // ─── Lock management ─────────────────────────────────────────────────

    pub fn acquire_lock(&self) -> std::io::Result<()> {
        let lock = self.lock_path();
        if let Some(parent) = lock.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&lock, now_secs().to_string())
    }

    pub fn release_lock(&self) -> std::io::Result<()> {
        let lock = self.lock_path();
        if lock.exists() {
            std::fs::remove_file(&lock)?;
        }
        Ok(())
    }

    // ─── Consolidation prompt ────────────────────────────────────────────

    pub fn consolidation_prompt(&self) -> String {
        format!(
            "You are a memory consolidation agent. Your job is to organize and \
            prune the memory directory at {}.\n\n\
            Follow these phases:\n\
            1. **Orient**: ls the memory directory, read MEMORY.md, skim topic files\n\
            2. **Gather**: Read recent session logs, identify new facts\n\
            3. **Consolidate**: Merge new signal into existing files, convert relative dates to absolute\n\
            4. **Prune**: Keep MEMORY.md under 200 lines / 25KB, remove contradicted facts\n\n\
            Only use read-only tools: ls, find, grep, cat, stat, wc, head, tail.\n\
            Write changes to memory files using Write and Edit tools.",
            self.memory_dir.display()
        )
    }
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> (tempfile::TempDir, AutoDream) {
        let tmp = tempfile::tempdir().unwrap();
        let mem_dir = tmp.path().join("memory");
        let conv_dir = tmp.path().join("conversations");
        std::fs::create_dir_all(&mem_dir).unwrap();
        std::fs::create_dir_all(&conv_dir).unwrap();

        let dream = AutoDream::new(mem_dir, conv_dir);
        (tmp, dream)
    }

    #[test]
    fn test_time_gate_never_consolidated() {
        let (_tmp, dream) = setup();
        let state = ConsolidationState::default();
        assert!(dream.time_gate_passes(&state));
    }

    #[test]
    fn test_time_gate_recent() {
        let (_tmp, dream) = setup();
        let state = ConsolidationState {
            last_consolidated_at: Some(now_secs() - 3600), // 1 hour ago
            ..Default::default()
        };
        assert!(!dream.time_gate_passes(&state)); // need 24 hours
    }

    #[test]
    fn test_time_gate_old() {
        let (_tmp, dream) = setup();
        let state = ConsolidationState {
            last_consolidated_at: Some(now_secs() - 90_000), // 25 hours ago
            ..Default::default()
        };
        assert!(dream.time_gate_passes(&state));
    }

    #[test]
    fn test_session_gate_no_sessions() {
        let (_tmp, dream) = setup();
        let state = ConsolidationState::default();
        assert!(!dream.session_gate_passes(&state));
    }

    #[test]
    fn test_session_gate_enough_sessions() {
        let (tmp, dream) = setup();
        let conv_dir = tmp.path().join("conversations");

        // Create 6 session files
        for i in 0..6 {
            std::fs::write(conv_dir.join(format!("session-{}.jsonl", i)), "{}").unwrap();
        }

        let state = ConsolidationState {
            last_consolidated_at: Some(now_secs() - 86400), // yesterday
            ..Default::default()
        };
        assert!(dream.session_gate_passes(&state));
    }

    #[test]
    fn test_lock_gate_no_lock() {
        let (_tmp, dream) = setup();
        assert!(dream.lock_gate_passes());
    }

    #[test]
    fn test_lock_gate_active_lock() {
        let (_tmp, dream) = setup();
        dream.acquire_lock().unwrap();
        assert!(!dream.lock_gate_passes()); // lock is fresh
    }

    #[test]
    fn test_lock_acquire_release() {
        let (_tmp, dream) = setup();
        dream.acquire_lock().unwrap();
        assert!(dream.lock_path().exists());
        dream.release_lock().unwrap();
        assert!(!dream.lock_path().exists());
    }

    #[test]
    fn test_state_persistence() {
        let (_tmp, dream) = setup();

        // Initially empty
        let state = dream.load_state();
        assert!(state.last_consolidated_at.is_none());

        // Update
        dream.update_state().unwrap();
        let state = dream.load_state();
        assert!(state.last_consolidated_at.is_some());
        assert!(state.last_consolidated_at.unwrap() > now_secs() - 10);
    }

    #[test]
    fn test_consolidation_prompt() {
        let (_tmp, dream) = setup();
        let prompt = dream.consolidation_prompt();
        assert!(prompt.contains("memory consolidation"));
        assert!(prompt.contains("Orient"));
        assert!(prompt.contains("Prune"));
    }
}
