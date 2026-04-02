//! Session management: list, show, delete, memory operations.

use crate::config::AppConfig;
use std::path::PathBuf;

/// Get the sessions directory for the current project.
fn sessions_dir(config: &AppConfig) -> PathBuf {
    let sanitized = sanitize_path(&config.working_dir.display().to_string());
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    home.join(".claude").join("projects").join(sanitized)
}

fn sanitize_path(path: &str) -> String {
    path.replace('/', "-")
        .replace('\\', "-")
        .replace(':', "-")
        .trim_matches('-')
        .to_string()
}

/// List all sessions for the current project.
pub fn list(config: &AppConfig) -> anyhow::Result<()> {
    let dir = sessions_dir(config);
    if !dir.exists() {
        println!("No sessions found.");
        return Ok(());
    }

    let mut entries: Vec<(String, std::fs::Metadata)> = Vec::new();

    for entry in std::fs::read_dir(&dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().map(|e| e == "jsonl").unwrap_or(false) {
            let name = path
                .file_stem()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string();
            if let Ok(meta) = entry.metadata() {
                entries.push((name, meta));
            }
        }
    }

    if entries.is_empty() {
        println!("No sessions found.");
        return Ok(());
    }

    // Sort by modification time (newest first)
    entries.sort_by(|a, b| {
        b.1.modified()
            .unwrap_or(std::time::SystemTime::UNIX_EPOCH)
            .cmp(
                &a.1.modified()
                    .unwrap_or(std::time::SystemTime::UNIX_EPOCH),
            )
    });

    println!("{:<40} {:>10} {:>12}", "SESSION ID", "SIZE", "MODIFIED");
    println!("{}", "-".repeat(64));

    for (name, meta) in &entries {
        let size = format_size(meta.len());
        let modified = meta
            .modified()
            .ok()
            .and_then(|t| {
                let dt: chrono::DateTime<chrono::Local> = t.into();
                Some(dt.format("%Y-%m-%d %H:%M").to_string())
            })
            .unwrap_or_else(|| "unknown".into());

        let display_name = if name.len() > 38 {
            format!("{}...", &name[..35])
        } else {
            name.clone()
        };

        println!("{:<40} {:>10} {:>12}", display_name, size, modified);
    }

    println!("\n{} session(s)", entries.len());
    Ok(())
}

/// Show a session transcript.
pub fn show(config: &AppConfig, id: &str) -> anyhow::Result<()> {
    let dir = sessions_dir(config);
    let path = dir.join(format!("{id}.jsonl"));

    if !path.exists() {
        anyhow::bail!("Session '{}' not found", id);
    }

    let content = std::fs::read_to_string(&path)?;
    for line in content.lines() {
        if let Ok(entry) = serde_json::from_str::<serde_json::Value>(line) {
            let entry_type = entry.get("type").and_then(|t| t.as_str()).unwrap_or("?");
            let message = entry.get("message").cloned().unwrap_or_default();
            let role = message
                .get("role")
                .and_then(|r| r.as_str())
                .unwrap_or(entry_type);

            // Extract text content
            let text = message
                .get("content")
                .and_then(|c| {
                    if let Some(s) = c.as_str() {
                        Some(s.to_string())
                    } else if let Some(blocks) = c.as_array() {
                        let texts: Vec<String> = blocks
                            .iter()
                            .filter_map(|b| b.get("text").and_then(|t| t.as_str()))
                            .map(String::from)
                            .collect();
                        if texts.is_empty() {
                            None
                        } else {
                            Some(texts.join("\n"))
                        }
                    } else {
                        None
                    }
                })
                .unwrap_or_default();

            if !text.is_empty() {
                let role_color = match role {
                    "user" => "\x1b[36m",
                    "assistant" => "\x1b[32m",
                    "system" => "\x1b[33m",
                    _ => "\x1b[90m",
                };
                let preview: String = text.chars().take(200).collect();
                println!("{role_color}[{role}]\x1b[0m {preview}");
            }
        }
    }

    Ok(())
}

/// Delete a session.
pub fn delete(config: &AppConfig, id: &str) -> anyhow::Result<()> {
    let dir = sessions_dir(config);
    let path = dir.join(format!("{id}.jsonl"));

    if !path.exists() {
        anyhow::bail!("Session '{}' not found", id);
    }

    std::fs::remove_file(&path)?;
    println!("Deleted session: {id}");
    Ok(())
}

/// Get the most recent session ID.
pub fn last_session_id(config: &AppConfig) -> Option<String> {
    let dir = sessions_dir(config);
    if !dir.exists() {
        return None;
    }

    let mut newest: Option<(String, std::time::SystemTime)> = None;

    if let Ok(entries) = std::fs::read_dir(&dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().map(|e| e == "jsonl").unwrap_or(false) {
                if let Ok(meta) = entry.metadata() {
                    let modified = meta
                        .modified()
                        .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
                    let name = path
                        .file_stem()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .to_string();

                    if newest.as_ref().map(|(_, t)| modified > *t).unwrap_or(true) {
                        newest = Some((name, modified));
                    }
                }
            }
        }
    }

    newest.map(|(id, _)| id)
}

/// Show memory status.
pub fn show_memory(config: &AppConfig) -> anyhow::Result<()> {
    let mm = cersei_memory::manager::MemoryManager::new(&config.working_dir);
    let context = mm.build_context();

    if context.is_empty() {
        println!("No memory content found.");
    } else {
        println!("{context}");
    }

    Ok(())
}

/// Clear all memory.
pub fn clear_memory(config: &AppConfig) -> anyhow::Result<()> {
    let memory_dir = config.working_dir.join(".claude").join("memory");
    if memory_dir.exists() {
        std::fs::remove_dir_all(&memory_dir)?;
        println!("Memory cleared.");
    } else {
        println!("No memory directory found.");
    }
    Ok(())
}

fn format_size(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{bytes}B")
    } else if bytes < 1024 * 1024 {
        format!("{:.1}KB", bytes as f64 / 1024.0)
    } else {
        format!("{:.1}MB", bytes as f64 / (1024.0 * 1024.0))
    }
}
