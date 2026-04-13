//! ApplyPatch tool: apply unified diff patches to files.

use super::*;
use serde::Deserialize;
use std::path::PathBuf;

pub struct ApplyPatchTool;

#[async_trait]
impl Tool for ApplyPatchTool {
    fn name(&self) -> &str { "ApplyPatch" }

    fn description(&self) -> &str {
        "Apply a unified diff patch to one or more files. The patch should be in standard \
         unified diff format (as produced by `diff -u` or `git diff`). Supports creating \
         new files and deleting files."
    }

    fn permission_level(&self) -> PermissionLevel { PermissionLevel::Write }
    fn category(&self) -> ToolCategory { ToolCategory::FileSystem }

    fn input_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "patch": {
                    "type": "string",
                    "description": "Unified diff patch content"
                }
            },
            "required": ["patch"]
        })
    }

    async fn execute(&self, input: Value, ctx: &ToolContext) -> ToolResult {
        #[derive(Deserialize)]
        struct Input {
            patch: String,
        }

        let input: Input = match serde_json::from_value(input) {
            Ok(i) => i,
            Err(e) => return ToolResult::error(format!("Invalid input: {e}")),
        };

        match apply_unified_patch(&input.patch, &ctx.working_dir) {
            Ok(files) => {
                if files.is_empty() {
                    ToolResult::success("Patch applied (no files changed).")
                } else {
                    ToolResult::success(format!(
                        "Patch applied to {} file(s):\n{}",
                        files.len(),
                        files.iter().map(|f| format!("  {}", f.display())).collect::<Vec<_>>().join("\n")
                    ))
                }
            }
            Err(e) => ToolResult::error(format!("Failed to apply patch: {e}")),
        }
    }
}

/// Apply a unified diff patch. Returns list of modified files.
fn apply_unified_patch(patch: &str, working_dir: &std::path::Path) -> std::result::Result<Vec<PathBuf>, String> {
    let mut modified = Vec::new();
    let mut current_file: Option<PathBuf> = None;
    let mut original_lines: Vec<String> = Vec::new();
    let mut hunks: Vec<Hunk> = Vec::new();

    // Parse patch into files and hunks
    let lines: Vec<&str> = patch.lines().collect();
    let mut i = 0;

    while i < lines.len() {
        let line = lines[i];

        if line.starts_with("--- ") {
            // Flush previous file
            if let Some(ref file) = current_file {
                apply_hunks(file, &original_lines, &hunks)?;
                modified.push(file.clone());
            }

            // Parse file paths
            i += 1;
            if i >= lines.len() || !lines[i].starts_with("+++ ") {
                return Err("Expected +++ line after ---".into());
            }

            let target = lines[i].strip_prefix("+++ ").unwrap_or(lines[i]);
            let target = target.split('\t').next().unwrap_or(target); // Strip timestamp
            let target = target.strip_prefix("b/").unwrap_or(target); // Strip git prefix

            let file_path = working_dir.join(target);
            original_lines = if file_path.exists() {
                std::fs::read_to_string(&file_path)
                    .map_err(|e| format!("Cannot read {}: {e}", file_path.display()))?
                    .lines()
                    .map(String::from)
                    .collect()
            } else {
                Vec::new() // New file
            };

            current_file = Some(file_path);
            hunks.clear();
            i += 1;
            continue;
        }

        if line.starts_with("@@ ") {
            if let Some(hunk) = parse_hunk_header(line) {
                let mut hunk_lines = Vec::new();
                i += 1;
                while i < lines.len()
                    && !lines[i].starts_with("@@ ")
                    && !lines[i].starts_with("--- ")
                    && !lines[i].starts_with("diff ")
                {
                    hunk_lines.push(lines[i].to_string());
                    i += 1;
                }
                hunks.push(Hunk {
                    old_start: hunk.0,
                    old_count: hunk.1,
                    new_start: hunk.2,
                    new_count: hunk.3,
                    lines: hunk_lines,
                });
                continue;
            }
        }

        i += 1;
    }

    // Flush last file
    if let Some(ref file) = current_file {
        apply_hunks(file, &original_lines, &hunks)?;
        modified.push(file.clone());
    }

    Ok(modified)
}

struct Hunk {
    old_start: usize,
    old_count: usize,
    new_start: usize,
    new_count: usize,
    lines: Vec<String>,
}

fn parse_hunk_header(line: &str) -> Option<(usize, usize, usize, usize)> {
    // @@ -old_start,old_count +new_start,new_count @@
    let line = line.strip_prefix("@@ -")?;
    let (old, rest) = line.split_once(' ')?;
    let rest = rest.strip_prefix('+')?;
    let (new, _) = rest.split_once(' ').unwrap_or((rest.trim_end_matches(" @@"), ""));
    let new = new.trim_end_matches(" @@");

    let parse_range = |s: &str| -> (usize, usize) {
        if let Some((start, count)) = s.split_once(',') {
            (start.parse().unwrap_or(1), count.parse().unwrap_or(0))
        } else {
            (s.parse().unwrap_or(1), 1)
        }
    };

    let (os, oc) = parse_range(old);
    let (ns, nc) = parse_range(new);
    Some((os, oc, ns, nc))
}

fn apply_hunks(file: &std::path::Path, original: &[String], hunks: &[Hunk]) -> std::result::Result<(), String> {
    let mut result = original.to_vec();
    let mut offset: isize = 0;

    for hunk in hunks {
        let start = ((hunk.old_start as isize - 1) + offset).max(0) as usize;
        let mut new_lines = Vec::new();
        let mut old_removed = 0usize;

        for line in &hunk.lines {
            if let Some(content) = line.strip_prefix('+') {
                new_lines.push(content.to_string());
            } else if let Some(_) = line.strip_prefix('-') {
                old_removed += 1;
            } else if let Some(content) = line.strip_prefix(' ') {
                new_lines.push(content.to_string());
                old_removed += 1; // context line replaces itself
            } else {
                // No prefix = context line
                new_lines.push(line.to_string());
                old_removed += 1;
            }
        }

        // Replace old lines with new lines
        let end = (start + old_removed).min(result.len());
        result.splice(start..end, new_lines.iter().cloned());
        offset += new_lines.len() as isize - old_removed as isize;
    }

    // Write result
    if let Some(parent) = file.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("Cannot create directory: {e}"))?;
    }
    std::fs::write(file, result.join("\n") + "\n")
        .map_err(|e| format!("Cannot write {}: {e}", file.display()))?;

    Ok(())
}
