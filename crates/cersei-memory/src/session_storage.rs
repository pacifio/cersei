//! Session storage: append-only JSONL transcript persistence.
//!
//! Compatible with Claude Code's session transcript format.
//! Each session is a `.jsonl` file with one entry per line.
//!
//! When a session file exceeds `MAX_SESSION_SIZE` (50MB), writes automatically
//! fork to a new part file (`session_part2.jsonl`, `_part3.jsonl`, etc.).
//! Loading stitches all parts together transparently.

use cersei_types::*;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

// ─── Constants ───────────────────────────────────────────────────────────────

const MAX_SESSION_SIZE: u64 = 50_000_000; // 50MB per part
const MAX_TOTAL_SESSION_SIZE: u64 = 200_000_000; // 200MB across all parts

// ─── Types ───────────────────────────────────────────────────────────────────

/// A single entry in the session transcript.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum TranscriptEntry {
    User(TranscriptMessage),
    Assistant(TranscriptMessage),
    System(TranscriptMessage),
    Summary(SummaryEntry),
    Tombstone(TombstoneEntry),
    #[serde(other)]
    Unknown,
}

/// A conversation message entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TranscriptMessage {
    pub uuid: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_uuid: Option<String>,
    pub timestamp: String,
    pub session_id: String,
    #[serde(default)]
    pub cwd: String,
    pub message: Message,
    #[serde(default)]
    pub is_sidechain: bool,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub extra: HashMap<String, serde_json::Value>,
}

/// A compaction summary entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SummaryEntry {
    pub uuid: String,
    pub timestamp: String,
    pub session_id: String,
    pub summary: String,
    pub messages_compacted: usize,
}

/// A soft-delete marker.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TombstoneEntry {
    pub deleted_uuid: String,
    pub timestamp: String,
}

// ─── Path resolution ─────────────────────────────────────────────────────────

/// Compute the base transcript file path for a session.
pub fn transcript_path(project_root: &Path, session_id: &str) -> PathBuf {
    let sanitized = super::memdir::sanitize_path_component(&project_root.display().to_string());
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    home.join(".claude")
        .join("projects")
        .join(sanitized)
        .join(format!("{}.jsonl", session_id))
}

// ─── Multi-part helpers ─────────────────────────────────────────────────────

/// Find the current (latest) part file for writing.
/// Returns the base path if it doesn't exist yet or is under the size limit,
/// or the highest existing `_partN` path that's still under the limit.
fn current_write_path(base_path: &Path) -> PathBuf {
    let stem = match base_path.file_stem().and_then(|s| s.to_str()) {
        Some(s) => s.to_string(),
        None => return base_path.to_path_buf(),
    };
    let dir = base_path.parent().unwrap_or(Path::new("."));

    // Find the highest existing part
    let mut highest = base_path.to_path_buf();
    let mut n = 2;
    loop {
        let part = dir.join(format!("{}_part{}.jsonl", stem, n));
        if part.exists() {
            highest = part;
            n += 1;
        } else {
            break;
        }
    }

    highest
}

/// Compute the next part file path (the first `_partN` that doesn't exist).
fn next_part_path(base_path: &Path) -> PathBuf {
    let stem = match base_path.file_stem().and_then(|s| s.to_str()) {
        Some(s) => s.to_string(),
        None => return base_path.to_path_buf(),
    };
    let dir = base_path.parent().unwrap_or(Path::new("."));

    let mut n = 2;
    loop {
        let part = dir.join(format!("{}_part{}.jsonl", stem, n));
        if !part.exists() {
            return part;
        }
        n += 1;
    }
}

/// List all part files for a session, in order (base first, then _part2, _part3, ...).
pub fn all_part_paths(base_path: &Path) -> Vec<PathBuf> {
    let mut parts = Vec::new();
    if base_path.exists() {
        parts.push(base_path.to_path_buf());
    }

    let stem = match base_path.file_stem().and_then(|s| s.to_str()) {
        Some(s) => s.to_string(),
        None => return parts,
    };
    let dir = base_path.parent().unwrap_or(Path::new("."));

    let mut n = 2;
    loop {
        let part = dir.join(format!("{}_part{}.jsonl", stem, n));
        if part.exists() {
            parts.push(part);
            n += 1;
        } else {
            break;
        }
    }

    parts
}

/// Total size across all session parts.
pub fn total_session_size(base_path: &Path) -> u64 {
    all_part_paths(base_path)
        .iter()
        .filter_map(|p| std::fs::metadata(p).ok().map(|m| m.len()))
        .sum()
}

// ─── Write ───────────────────────────────────────────────────────────────────

/// Append a transcript entry to the session file.
/// Automatically forks to a new part file if the current one exceeds 50MB.
pub fn write_transcript_entry(path: &Path, entry: &TranscriptEntry) -> std::io::Result<()> {
    use std::io::Write;

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let line = serde_json::to_string(entry)?;
    let mut write_path = current_write_path(path);

    // Check if current part is too large — fork to next part
    let current_size = std::fs::metadata(&write_path).map(|m| m.len()).unwrap_or(0);
    if current_size + line.len() as u64 + 1 > MAX_SESSION_SIZE {
        write_path = next_part_path(path);
        if let Some(parent) = write_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
    }

    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&write_path)?;

    writeln!(file, "{}", line)?;
    Ok(())
}

/// Write a user message entry.
pub fn write_user_entry(
    path: &Path,
    session_id: &str,
    message: Message,
    cwd: &str,
) -> std::io::Result<String> {
    let uuid = uuid::Uuid::new_v4().to_string();
    let entry = TranscriptEntry::User(TranscriptMessage {
        uuid: uuid.clone(),
        parent_uuid: None,
        timestamp: chrono::Utc::now().to_rfc3339(),
        session_id: session_id.to_string(),
        cwd: cwd.to_string(),
        message,
        is_sidechain: false,
        extra: HashMap::new(),
    });
    write_transcript_entry(path, &entry)?;
    Ok(uuid)
}

/// Write an assistant message entry.
pub fn write_assistant_entry(
    path: &Path,
    session_id: &str,
    message: Message,
    cwd: &str,
    parent_uuid: Option<&str>,
) -> std::io::Result<String> {
    let uuid = uuid::Uuid::new_v4().to_string();
    let entry = TranscriptEntry::Assistant(TranscriptMessage {
        uuid: uuid.clone(),
        parent_uuid: parent_uuid.map(String::from),
        timestamp: chrono::Utc::now().to_rfc3339(),
        session_id: session_id.to_string(),
        cwd: cwd.to_string(),
        message,
        is_sidechain: false,
        extra: HashMap::new(),
    });
    write_transcript_entry(path, &entry)?;
    Ok(uuid)
}

/// Write a tombstone (soft-delete) entry.
pub fn tombstone_entry(path: &Path, deleted_uuid: &str) -> std::io::Result<()> {
    let entry = TranscriptEntry::Tombstone(TombstoneEntry {
        deleted_uuid: deleted_uuid.to_string(),
        timestamp: chrono::Utc::now().to_rfc3339(),
    });
    write_transcript_entry(path, &entry)
}

// ─── Read ────────────────────────────────────────────────────────────────────

/// Load a session transcript from all part files, respecting tombstones.
pub fn load_transcript(path: &Path) -> Result<Vec<TranscriptEntry>> {
    let parts = all_part_paths(path);
    if parts.is_empty() {
        if !path.exists() {
            return Ok(Vec::new());
        }
        // Base path doesn't match pattern but was passed directly
        return load_single_transcript(path);
    }

    // Total size check across all parts
    let total: u64 = parts
        .iter()
        .filter_map(|p| std::fs::metadata(p).ok().map(|m| m.len()))
        .sum();
    if total > MAX_TOTAL_SESSION_SIZE {
        return Err(CerseiError::Config(format!(
            "Session too large: {} bytes across {} parts (max {})",
            total,
            parts.len(),
            MAX_TOTAL_SESSION_SIZE
        )));
    }

    // Concatenate all lines from all parts
    let mut content = String::new();
    for part in &parts {
        content.push_str(&std::fs::read_to_string(part)?);
    }

    parse_transcript_content(&content)
}

/// Load from a single file (internal helper).
fn load_single_transcript(path: &Path) -> Result<Vec<TranscriptEntry>> {
    let meta = std::fs::metadata(path)?;
    if meta.len() > MAX_SESSION_SIZE {
        return Err(CerseiError::Config(format!(
            "Session file too large: {} bytes (max {})",
            meta.len(),
            MAX_SESSION_SIZE
        )));
    }
    let content = std::fs::read_to_string(path)?;
    parse_transcript_content(&content)
}

/// Parse transcript content with two-pass tombstone handling.
fn parse_transcript_content(content: &str) -> Result<Vec<TranscriptEntry>> {
    // Pass 1: collect tombstone UUIDs
    let mut tombstones: HashSet<String> = HashSet::new();
    for line in content.lines() {
        if line.trim().is_empty() {
            continue;
        }
        if let Ok(entry) = serde_json::from_str::<TranscriptEntry>(line) {
            if let TranscriptEntry::Tombstone(t) = &entry {
                tombstones.insert(t.deleted_uuid.clone());
            }
        }
    }

    // Pass 2: load entries, skip tombstoned
    let mut entries = Vec::new();
    for line in content.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let entry: TranscriptEntry = match serde_json::from_str(line) {
            Ok(e) => e,
            Err(_) => continue,
        };

        let uuid = match &entry {
            TranscriptEntry::User(m) => Some(&m.uuid),
            TranscriptEntry::Assistant(m) => Some(&m.uuid),
            TranscriptEntry::System(m) => Some(&m.uuid),
            TranscriptEntry::Summary(s) => Some(&s.uuid),
            TranscriptEntry::Tombstone(_) => continue,
            TranscriptEntry::Unknown => None,
        };

        if let Some(uuid) = uuid {
            if tombstones.contains(uuid) {
                continue;
            }
        }

        entries.push(entry);
    }

    Ok(entries)
}

/// Extract API messages from transcript entries.
pub fn messages_from_transcript(entries: &[TranscriptEntry]) -> Vec<Message> {
    entries
        .iter()
        .filter_map(|e| match e {
            TranscriptEntry::User(m) => Some(m.message.clone()),
            TranscriptEntry::Assistant(m) => Some(m.message.clone()),
            TranscriptEntry::System(m) => Some(m.message.clone()),
            _ => None,
        })
        .collect()
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_write_and_load() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("session.jsonl");

        let uuid1 = write_user_entry(&path, "s1", Message::user("Hello"), "/tmp").unwrap();
        let _uuid2 = write_assistant_entry(&path, "s1", Message::assistant("Hi!"), "/tmp", Some(&uuid1)).unwrap();
        write_user_entry(&path, "s1", Message::user("How are you?"), "/tmp").unwrap();

        let entries = load_transcript(&path).unwrap();
        assert_eq!(entries.len(), 3);

        let messages = messages_from_transcript(&entries);
        assert_eq!(messages.len(), 3);
        assert_eq!(messages[0].get_text().unwrap(), "Hello");
        assert_eq!(messages[1].get_text().unwrap(), "Hi!");
    }

    #[test]
    fn test_tombstone() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("session.jsonl");

        let _uuid1 = write_user_entry(&path, "s1", Message::user("Keep"), "/tmp").unwrap();
        let uuid2 = write_user_entry(&path, "s1", Message::user("Delete me"), "/tmp").unwrap();
        let _uuid3 = write_user_entry(&path, "s1", Message::user("Also keep"), "/tmp").unwrap();

        tombstone_entry(&path, &uuid2).unwrap();

        let entries = load_transcript(&path).unwrap();
        assert_eq!(entries.len(), 2);

        let messages = messages_from_transcript(&entries);
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].get_text().unwrap(), "Keep");
        assert_eq!(messages[1].get_text().unwrap(), "Also keep");
    }

    #[test]
    fn test_empty_file() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("empty.jsonl");
        std::fs::write(&path, "").unwrap();

        let entries = load_transcript(&path).unwrap();
        assert!(entries.is_empty());
    }

    #[test]
    fn test_malformed_lines_skipped() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("session.jsonl");

        write_user_entry(&path, "s1", Message::user("Valid"), "/tmp").unwrap();
        {
            use std::io::Write;
            let mut f = std::fs::OpenOptions::new().append(true).open(&path).unwrap();
            writeln!(f, "{{not valid json}}").unwrap();
        }
        write_user_entry(&path, "s1", Message::user("Also valid"), "/tmp").unwrap();

        let entries = load_transcript(&path).unwrap();
        assert_eq!(entries.len(), 2);
    }

    #[test]
    fn test_transcript_path() {
        let path = transcript_path(Path::new("/Users/test/project"), "abc-123");
        assert!(path.to_str().unwrap().contains("abc-123.jsonl"));
        assert!(path.to_str().unwrap().contains(".claude"));
    }

    #[test]
    fn test_summary_entry() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("session.jsonl");

        write_user_entry(&path, "s1", Message::user("Msg 1"), "/tmp").unwrap();
        let summary = TranscriptEntry::Summary(SummaryEntry {
            uuid: "sum-1".into(),
            timestamp: chrono::Utc::now().to_rfc3339(),
            session_id: "s1".into(),
            summary: "User asked about X, assistant did Y.".into(),
            messages_compacted: 5,
        });
        write_transcript_entry(&path, &summary).unwrap();
        write_user_entry(&path, "s1", Message::user("Msg 2"), "/tmp").unwrap();

        let entries = load_transcript(&path).unwrap();
        assert_eq!(entries.len(), 3);
    }

    #[test]
    fn test_auto_fork_on_size_limit() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("big.jsonl");

        // Create a large message (~1KB each)
        let big_text = "x".repeat(1000);

        // Write enough to exceed a small limit — we test the logic
        // by temporarily using the real MAX_SESSION_SIZE (50MB),
        // so instead we test the path helpers directly
        assert_eq!(all_part_paths(&path).len(), 0); // nothing yet

        write_user_entry(&path, "s1", Message::user(&big_text), "/tmp").unwrap();
        assert_eq!(all_part_paths(&path).len(), 1); // base file

        // Verify current_write_path returns base
        assert_eq!(current_write_path(&path), path);

        // next_part_path should be _part2
        let part2 = next_part_path(&path);
        assert!(part2.to_str().unwrap().contains("_part2"));
    }

    #[test]
    fn test_multi_part_load() {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().join("multi.jsonl");
        let part2 = tmp.path().join("multi_part2.jsonl");

        // Write to base
        write_user_entry(&base, "s1", Message::user("Part 1 msg"), "/tmp").unwrap();

        // Write directly to part2 (simulating auto-fork)
        write_user_entry(&part2, "s1", Message::user("Part 2 msg"), "/tmp").unwrap();

        // Load should stitch both
        let entries = load_transcript(&base).unwrap();
        assert_eq!(entries.len(), 2);

        let messages = messages_from_transcript(&entries);
        assert_eq!(messages[0].get_text().unwrap(), "Part 1 msg");
        assert_eq!(messages[1].get_text().unwrap(), "Part 2 msg");
    }

    #[test]
    fn test_tombstone_across_parts() {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().join("tomb.jsonl");
        let part2 = tmp.path().join("tomb_part2.jsonl");

        // Message in part 1
        let uuid1 = write_user_entry(&base, "s1", Message::user("Delete me"), "/tmp").unwrap();
        write_user_entry(&base, "s1", Message::user("Keep"), "/tmp").unwrap();

        // Tombstone in part 2 deletes message from part 1
        tombstone_entry(&part2, &uuid1).unwrap();
        write_user_entry(&part2, "s1", Message::user("Also keep"), "/tmp").unwrap();

        let entries = load_transcript(&base).unwrap();
        assert_eq!(entries.len(), 2);

        let messages = messages_from_transcript(&entries);
        assert_eq!(messages[0].get_text().unwrap(), "Keep");
        assert_eq!(messages[1].get_text().unwrap(), "Also keep");
    }

    #[test]
    fn test_total_session_size() {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().join("sized.jsonl");

        write_user_entry(&base, "s1", Message::user("Hello"), "/tmp").unwrap();
        let size = total_session_size(&base);
        assert!(size > 0);
    }
}
