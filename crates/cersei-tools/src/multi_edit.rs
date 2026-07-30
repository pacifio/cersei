//! MultiEdit tool: apply several string replacements to one file atomically.
//!
//! Weaker models bungle refactors that require N separate `Edit` calls (e.g. a
//! variable rename touching several distinct lines): each call re-reads stale
//! context and the sequence drifts. `MultiEdit` takes an ordered list of edits,
//! applies them **sequentially in memory** (each edit sees the result of the
//! previous one), and writes **all-or-nothing** — if any edit fails to match,
//! the file is left untouched and the failing edit is named. Every edit routes
//! through the same tolerant [`crate::tool_primitives::replace`] ladder as
//! `Edit`, so it inherits whitespace/indentation tolerance and the
//! destructive-match guard.

use super::*;
use crate::tool_primitives::replace::{replace, ReplaceError};

pub struct MultiEditTool;

#[async_trait]
impl Tool for MultiEditTool {
    fn name(&self) -> &str {
        "MultiEdit"
    }
    fn description(&self) -> &str {
        "Apply multiple string replacements to a single file in one atomic operation. \
         Edits are applied in order, each against the result of the previous one, and \
         the file is written only if every edit succeeds. Prefer this over many separate \
         Edit calls when refactoring (e.g. renames) a single file. Each edit tolerates \
         leading/trailing whitespace and indentation differences."
    }
    fn permission_level(&self) -> PermissionLevel {
        PermissionLevel::Write
    }
    fn category(&self) -> ToolCategory {
        ToolCategory::FileSystem
    }

    fn input_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "file_path": { "type": "string", "description": "Absolute path to the file" },
                "edits": {
                    "type": "array",
                    "description": "Edits applied in sequence, each against the prior result",
                    "items": {
                        "type": "object",
                        "properties": {
                            "old_string": { "type": "string", "description": "The text to replace" },
                            "new_string": { "type": "string", "description": "The replacement text" },
                            "replace_all": { "type": "boolean", "description": "Replace all occurrences of old_string", "default": false }
                        },
                        "required": ["old_string", "new_string"]
                    }
                }
            },
            "required": ["file_path", "edits"]
        })
    }

    async fn execute(&self, input: Value, _ctx: &ToolContext) -> ToolResult {
        let (file_path, edits) = match coerce_input(input) {
            Ok(v) => v,
            Err(e) => return ToolResult::error(e),
        };

        if edits.is_empty() {
            return ToolResult::error("'edits' is empty — provide at least one edit.");
        }

        let path = std::path::Path::new(&file_path);
        let before = match tokio::fs::read_to_string(path).await {
            Ok(c) => c,
            Err(e) => return ToolResult::error(format!("Failed to read {file_path}: {e}")),
        };

        // Apply every edit in memory first (all-or-nothing).
        let mut content = before.clone();
        for (i, edit) in edits.iter().enumerate() {
            match replace(&content, &edit.old_string, &edit.new_string, edit.replace_all) {
                Ok(updated) => content = updated,
                Err(err) => {
                    return ToolResult::error(edit_error_message(i, edits.len(), &file_path, &err));
                }
            }
        }

        if content == before {
            return ToolResult::error(
                "No changes were produced by the edits (the file is unchanged).",
            );
        }

        if let Err(e) = tokio::fs::write(path, &content).await {
            return ToolResult::error(format!("Failed to write {file_path}: {e}"));
        }

        let diff = crate::tool_primitives::diff::unified_diff(&before, &content, 2);
        let diff_preview = if diff.lines().count() > 30 {
            let truncated: String = diff.lines().take(25).collect::<Vec<_>>().join("\n");
            format!("{}\n... ({} more lines)", truncated, diff.lines().count() - 25)
        } else {
            diff
        };

        ToolResult::success(format!(
            "The file {} has been updated with {} edit(s).\n{}",
            file_path,
            edits.len(),
            diff_preview
        ))
    }
}

/// A single coerced edit operation.
struct EditOp {
    old_string: String,
    new_string: String,
    replace_all: bool,
}

/// Map a [`ReplaceError`] from edit `i` to a corrective, model-facing message.
fn edit_error_message(i: usize, total: usize, file_path: &str, err: &ReplaceError) -> String {
    let pos = format!("edit {} of {}", i + 1, total);
    match err {
        ReplaceError::NotFound => format!(
            "{pos} failed: old_string not found in {file_path}. Note edits apply in order — \
             this edit runs against the result of the earlier edits, so its old_string must \
             match the file *after* those changes (and earlier edits may have already changed \
             this text). The matcher tolerates whitespace/indentation, so a mismatch means the \
             text itself differs; re-read the file and copy old_string verbatim. No changes \
             were written."
        ),
        ReplaceError::Ambiguous { count } => format!(
            "{pos} failed: old_string is not unique ({count} occurrences) in {file_path}. \
             Add surrounding lines to identify exactly one location, or set replace_all=true \
             for this edit. No changes were written."
        ),
        ReplaceError::NoChange => format!(
            "{pos} failed: old_string and new_string are identical, so it would do nothing. \
             No changes were written."
        ),
        ReplaceError::EmptyOldString => format!(
            "{pos} failed: old_string is empty but {file_path} is not — an empty anchor is \
             unsafe. No changes were written."
        ),
    }
}

/// Coerce a loosely-shaped MultiEdit call into a path + edit list.
///
/// Mirrors the leniency of the single `Edit` tool: accept near-miss field names
/// and stringified booleans so a weak model's near-correct call still applies.
fn coerce_input(input: Value) -> std::result::Result<(String, Vec<EditOp>), String> {
    let obj = input
        .as_object()
        .ok_or_else(|| "Invalid input: expected a JSON object".to_string())?;

    let get_str = |obj: &serde_json::Map<String, Value>, keys: &[&str]| -> Option<String> {
        for k in keys {
            match obj.get(*k) {
                Some(Value::String(s)) => return Some(s.clone()),
                Some(Value::Number(n)) => return Some(n.to_string()),
                Some(Value::Bool(b)) => return Some(b.to_string()),
                _ => {}
            }
        }
        None
    };

    let get_bool = |obj: &serde_json::Map<String, Value>, keys: &[&str]| -> bool {
        for k in keys {
            match obj.get(*k) {
                Some(Value::Bool(b)) => return *b,
                Some(Value::String(s)) => {
                    return matches!(s.trim().to_ascii_lowercase().as_str(), "true" | "1" | "yes")
                }
                Some(Value::Number(n)) => return n.as_i64().map(|v| v != 0).unwrap_or(false),
                _ => {}
            }
        }
        false
    };

    let file_path = get_str(obj, &["file_path", "filePath", "path", "file"])
        .ok_or_else(|| "Invalid input: missing 'file_path'".to_string())?;

    let edits_val = obj
        .get("edits")
        .or_else(|| obj.get("changes"))
        .or_else(|| obj.get("replacements"))
        .ok_or_else(|| "Invalid input: missing 'edits' array".to_string())?;
    let edits_arr = edits_val
        .as_array()
        .ok_or_else(|| "Invalid input: 'edits' must be an array".to_string())?;

    let mut edits = Vec::with_capacity(edits_arr.len());
    for (i, e) in edits_arr.iter().enumerate() {
        let eo = e
            .as_object()
            .ok_or_else(|| format!("Invalid input: edit {} is not an object", i + 1))?;
        let old_string = get_str(eo, &["old_string", "oldString", "old_str", "old", "search"])
            .ok_or_else(|| format!("Invalid input: edit {} is missing 'old_string'", i + 1))?;
        // A missing new_string is a deletion.
        let new_string =
            get_str(eo, &["new_string", "newString", "new_str", "new", "replace"]).unwrap_or_default();
        let replace_all = get_bool(eo, &["replace_all", "replaceAll", "all"]);
        edits.push(EditOp {
            old_string,
            new_string,
            replace_all,
        });
    }

    Ok((file_path, edits))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::permissions::AllowAll;
    use std::sync::Arc;

    fn test_ctx() -> ToolContext {
        ToolContext {
            working_dir: std::env::temp_dir(),
            session_id: "multiedit-test".into(),
            permissions: Arc::new(AllowAll),
            cost_tracker: Arc::new(CostTracker::new()),
            mcp_manager: None,
            extensions: Extensions::default(),
        }
    }

    #[tokio::test]
    async fn applies_multiple_edits_in_order() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("f.rs");
        std::fs::write(&path, "let a = 1;\nlet b = 2;\nlet c = 3;\n").unwrap();

        let res = MultiEditTool
            .execute(
                serde_json::json!({
                    "file_path": path.to_str().unwrap(),
                    "edits": [
                        {"old_string": "let a = 1;", "new_string": "let a = 10;"},
                        {"old_string": "let c = 3;", "new_string": "let c = 30;"}
                    ]
                }),
                &test_ctx(),
            )
            .await;

        assert!(!res.is_error, "got: {}", res.content);
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "let a = 10;\nlet b = 2;\nlet c = 30;\n"
        );
    }

    #[tokio::test]
    async fn rename_via_replace_all() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("f.rs");
        std::fs::write(&path, "foo(); foo(); let x = foo;\n").unwrap();

        let res = MultiEditTool
            .execute(
                serde_json::json!({
                    "file_path": path.to_str().unwrap(),
                    "edits": [
                        {"old_string": "foo", "new_string": "bar", "replace_all": true}
                    ]
                }),
                &test_ctx(),
            )
            .await;

        assert!(!res.is_error, "got: {}", res.content);
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "bar(); bar(); let x = bar;\n"
        );
    }

    #[tokio::test]
    async fn sequential_edits_see_prior_result() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("f.txt");
        std::fs::write(&path, "alpha\n").unwrap();

        // Second edit only matches if the first one already ran.
        let res = MultiEditTool
            .execute(
                serde_json::json!({
                    "file_path": path.to_str().unwrap(),
                    "edits": [
                        {"old_string": "alpha", "new_string": "beta"},
                        {"old_string": "beta", "new_string": "gamma"}
                    ]
                }),
                &test_ctx(),
            )
            .await;

        assert!(!res.is_error, "got: {}", res.content);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "gamma\n");
    }

    #[tokio::test]
    async fn atomic_rollback_on_failure() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("f.txt");
        let original = "keep me\n";
        std::fs::write(&path, original).unwrap();

        let res = MultiEditTool
            .execute(
                serde_json::json!({
                    "file_path": path.to_str().unwrap(),
                    "edits": [
                        {"old_string": "keep me", "new_string": "changed"},
                        {"old_string": "does-not-exist", "new_string": "x"}
                    ]
                }),
                &test_ctx(),
            )
            .await;

        assert!(res.is_error);
        assert!(res.content.contains("edit 2 of 2"));
        // File must be untouched because the second edit failed.
        assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
    }

    #[tokio::test]
    async fn tolerates_indentation_drift_per_edit() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("f.rs");
        std::fs::write(&path, "fn main() {\n        let x = 1;\n}\n").unwrap();

        let res = MultiEditTool
            .execute(
                serde_json::json!({
                    "file_path": path.to_str().unwrap(),
                    "edits": [
                        {"old_string": "let x = 1;", "new_string": "let x = 2;"}
                    ]
                }),
                &test_ctx(),
            )
            .await;

        assert!(!res.is_error, "got: {}", res.content);
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "fn main() {\n        let x = 2;\n}\n"
        );
    }
}
