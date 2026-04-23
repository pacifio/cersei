//! Session memory extraction: extract key facts from conversations.
//!
//! After enough conversation activity (≥20 messages, ≥3 tool calls since
//! last extraction), the extractor calls the LLM to identify reusable facts
//! and persists them to the memory directory.

use cersei_types::*;
use serde::{Deserialize, Serialize};
use std::path::Path;

// ─── Constants ───────────────────────────────────────────────────────────────

const MIN_MESSAGES_TO_EXTRACT: usize = 20;
const MIN_TOOL_CALLS_BETWEEN_EXTRACTIONS: usize = 3;

// ─── Types ───────────────────────────────────────────────────────────────────

/// Categories of extracted memories.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum MemoryCategory {
    UserPreference,
    ProjectFact,
    CodePattern,
    Decision,
    Constraint,
}

impl MemoryCategory {
    pub fn label(&self) -> &'static str {
        match self {
            Self::UserPreference => "preference",
            Self::ProjectFact => "project",
            Self::CodePattern => "pattern",
            Self::Decision => "decision",
            Self::Constraint => "constraint",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "preference" | "userpreference" | "user_preference" => Some(Self::UserPreference),
            "project" | "projectfact" | "project_fact" => Some(Self::ProjectFact),
            "pattern" | "codepattern" | "code_pattern" => Some(Self::CodePattern),
            "decision" => Some(Self::Decision),
            "constraint" => Some(Self::Constraint),
            _ => None,
        }
    }
}

/// A single extracted memory fact.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtractedMemory {
    pub content: String,
    pub category: MemoryCategory,
    pub confidence: f32,
}

/// Tracks extraction progress to avoid re-extracting.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SessionMemoryState {
    pub last_extracted_message_index: usize,
    pub tool_calls_since_last: usize,
    pub extraction_count: u32,
}

// ─── Gate checks ─────────────────────────────────────────────────────────────

/// Check if enough conversation has happened to warrant extraction.
pub fn should_extract(messages: &[Message], state: &SessionMemoryState) -> bool {
    // Need enough messages
    if messages.len() < MIN_MESSAGES_TO_EXTRACT {
        return false;
    }

    // Need enough tool calls since last extraction
    if state.extraction_count > 0
        && state.tool_calls_since_last < MIN_TOOL_CALLS_BETWEEN_EXTRACTIONS
    {
        return false;
    }

    // Don't extract if the last assistant message has pending tool calls
    if let Some(last) = messages.iter().rev().find(|m| m.role == Role::Assistant) {
        if last.has_tool_use() {
            return false;
        }
    }

    true
}

/// Count tool calls in messages since a given index.
pub fn count_tool_calls_since(messages: &[Message], since_index: usize) -> usize {
    messages[since_index..]
        .iter()
        .map(|m| m.get_tool_use_blocks().len())
        .sum()
}

// ─── Extraction prompt ──────────────────────────────────────────────────────

/// Build the extraction system prompt.
pub fn extraction_prompt() -> &'static str {
    "You are a memory extraction system. Read the conversation and extract \
    key facts worth remembering for future sessions.\n\n\
    For each fact, output one line in this exact format:\n\
    MEMORY: <category> | <confidence 0-10> | <fact>\n\n\
    Categories: preference, project, pattern, decision, constraint\n\n\
    Only extract facts that would be genuinely useful in future sessions. \
    Don't extract trivial or ephemeral information. Be specific and actionable."
}

/// Parse extraction output into structured memories.
pub fn parse_extraction_output(output: &str) -> Vec<ExtractedMemory> {
    output
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            if !line.starts_with("MEMORY:") {
                return None;
            }
            let rest = line.strip_prefix("MEMORY:")?.trim();
            let parts: Vec<&str> = rest.splitn(3, '|').collect();
            if parts.len() != 3 {
                return None;
            }

            let category = MemoryCategory::from_str(parts[0].trim())?;
            let confidence = parts[1].trim().parse::<f32>().ok()? / 10.0;
            let content = parts[2].trim().to_string();

            if content.is_empty() || confidence < 0.0 {
                return None;
            }

            Some(ExtractedMemory {
                content,
                category,
                confidence: confidence.clamp(0.0, 1.0),
            })
        })
        .collect()
}

// ─── Persistence ─────────────────────────────────────────────────────────────

/// Persist extracted memories to a file under `## Auto-extracted memories`.
pub fn persist_memories(memories: &[ExtractedMemory], target_path: &Path) -> std::io::Result<()> {
    if memories.is_empty() {
        return Ok(());
    }

    if let Some(parent) = target_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let existing = std::fs::read_to_string(target_path).unwrap_or_default();

    let date = chrono::Utc::now().format("%Y-%m-%d").to_string();
    let section_header = "## Auto-extracted memories";
    let date_header = format!("### Session memories — {}", date);

    let mut new_entries = String::new();
    for mem in memories {
        new_entries.push_str(&format!(
            "- **[{}]** {} *(confidence: {:.0}%)*\n",
            mem.category.label(),
            mem.content,
            mem.confidence * 100.0,
        ));
    }

    let output = if existing.contains(section_header) {
        // Append under existing section
        if existing.contains(&date_header) {
            // Append to existing date block
            existing.replace(&date_header, &format!("{}\n{}", date_header, new_entries))
        } else {
            // Add new date block at end of section
            let insert_pos = existing.find(section_header).unwrap() + section_header.len();
            let (before, after) = existing.split_at(insert_pos);
            format!("{}\n\n{}\n{}\n{}", before, date_header, new_entries, after)
        }
    } else {
        // Create section
        if existing.is_empty() {
            format!("{}\n\n{}\n{}", section_header, date_header, new_entries)
        } else {
            format!(
                "{}\n\n{}\n\n{}\n{}",
                existing.trim(),
                section_header,
                date_header,
                new_entries
            )
        }
    };

    std::fs::write(target_path, output)
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_messages(n: usize) -> Vec<Message> {
        (0..n)
            .map(|i| {
                if i % 2 == 0 {
                    Message::user(format!("Msg {}", i))
                } else {
                    Message::assistant(format!("Response {}", i))
                }
            })
            .collect()
    }

    #[test]
    fn test_should_extract_below_threshold() {
        let msgs = make_messages(10);
        let state = SessionMemoryState::default();
        assert!(!should_extract(&msgs, &state));
    }

    #[test]
    fn test_should_extract_above_threshold() {
        let msgs = make_messages(25);
        let state = SessionMemoryState::default();
        assert!(should_extract(&msgs, &state));
    }

    #[test]
    fn test_should_extract_cooldown() {
        let msgs = make_messages(25);
        let state = SessionMemoryState {
            extraction_count: 1,
            tool_calls_since_last: 1, // < 3
            ..Default::default()
        };
        assert!(!should_extract(&msgs, &state));
    }

    #[test]
    fn test_parse_extraction_output() {
        let output = "\
MEMORY: preference | 8 | User prefers Rust over Python
MEMORY: project | 9 | The API uses REST with JSON responses
MEMORY: decision | 7 | We chose PostgreSQL for the database
not a memory line
MEMORY: invalid | 5 | this category won't parse
";
        let memories = parse_extraction_output(output);
        assert_eq!(memories.len(), 3);
        assert_eq!(memories[0].content, "User prefers Rust over Python");
        assert!((memories[0].confidence - 0.8).abs() < 0.01);
        assert_eq!(memories[1].content, "The API uses REST with JSON responses");
    }

    #[test]
    fn test_persist_memories() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("memories.md");

        let memories = vec![
            ExtractedMemory {
                content: "User prefers dark mode".into(),
                category: MemoryCategory::UserPreference,
                confidence: 0.9,
            },
            ExtractedMemory {
                content: "Project uses Cersei SDK".into(),
                category: MemoryCategory::ProjectFact,
                confidence: 0.95,
            },
        ];

        persist_memories(&memories, &path).unwrap();
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("Auto-extracted memories"));
        assert!(content.contains("dark mode"));
        assert!(content.contains("Cersei SDK"));
        assert!(content.contains("preference"));
        assert!(content.contains("90%"));

        // Persist more — should append
        let more = vec![ExtractedMemory {
            content: "Tests use tokio".into(),
            category: MemoryCategory::CodePattern,
            confidence: 0.7,
        }];
        persist_memories(&more, &path).unwrap();
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("tokio"));
        assert!(content.contains("dark mode")); // original preserved
    }
}
