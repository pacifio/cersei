//! Session storage: append-only JSONL transcript persistence.
//!
//! Compatible with Claude Code's session transcript format.
//! Each session is a `.jsonl` file with one entry per line.

use cersei_types::*;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

// ─── Constants ───────────────────────────────────────────────────────────────

const MAX_SESSION_SIZE: u64 = 50_000_000; // 50MB

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

/// Compute the transcript file path for a session.
pub fn transcript_path(project_root: &Path, session_id: &str) -> PathBuf {
    let sanitized = super::memdir::sanitize_path_component(&project_root.display().to_string());
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    home.join(".claude")
        .join("projects")
        .join(sanitized)
        .join(format!("{}.jsonl", session_id))
}

// ─── Write ───────────────────────────────────────────────────────────────────

/// Append a single transcript entry to the session file.
pub fn write_transcript_entry(path: &Path, entry: &TranscriptEntry) -> std::io::Result<()> {
    use std::io::Write;

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let line = serde_json::to_string(entry)?;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;

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

/// Load a session transcript, respecting tombstones.
/// Two-pass: first collect tombstone UUIDs, then load non-tombstoned entries.
pub fn load_transcript(path: &Path) -> Result<Vec<TranscriptEntry>> {
    // Size check
    if let Ok(meta) = std::fs::metadata(path) {
        if meta.len() > MAX_SESSION_SIZE {
            return Err(CerseiError::Config(format!(
                "Session file too large: {} bytes (max {})",
                meta.len(),
                MAX_SESSION_SIZE
            )));
        }
    }

    let content = std::fs::read_to_string(path)?;

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
            Err(_) => continue, // skip malformed lines
        };

        // Skip tombstoned entries
        let uuid = match &entry {
            TranscriptEntry::User(m) => Some(&m.uuid),
            TranscriptEntry::Assistant(m) => Some(&m.uuid),
            TranscriptEntry::System(m) => Some(&m.uuid),
            TranscriptEntry::Summary(s) => Some(&s.uuid),
            TranscriptEntry::Tombstone(_) => continue, // don't include tombstones themselves
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
        let uuid2 = write_assistant_entry(&path, "s1", Message::assistant("Hi!"), "/tmp", Some(&uuid1)).unwrap();
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

        let uuid1 = write_user_entry(&path, "s1", Message::user("Keep"), "/tmp").unwrap();
        let uuid2 = write_user_entry(&path, "s1", Message::user("Delete me"), "/tmp").unwrap();
        let uuid3 = write_user_entry(&path, "s1", Message::user("Also keep"), "/tmp").unwrap();

        // Tombstone the second entry
        tombstone_entry(&path, &uuid2).unwrap();

        let entries = load_transcript(&path).unwrap();
        assert_eq!(entries.len(), 2); // 3 - 1 tombstoned

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
        // Append malformed line
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new().append(true).open(&path).unwrap();
        writeln!(f, "{{not valid json}}").unwrap();
        write_user_entry(&path, "s1", Message::user("Also valid"), "/tmp").unwrap();

        let entries = load_transcript(&path).unwrap();
        assert_eq!(entries.len(), 2); // malformed line skipped
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
        assert_eq!(entries.len(), 3); // user + summary + user
    }
}
