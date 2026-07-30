//! File edit tool: tolerant string replacement (see [`crate::tool_primitives::replace`]).

use super::*;
use crate::tool_primitives::fs as pfs;

pub struct FileEditTool;

#[async_trait]
impl Tool for FileEditTool {
    fn name(&self) -> &str {
        "Edit"
    }
    fn description(&self) -> &str {
        "Replace a string in a file. Prefers an exact match of old_string but \
         tolerates leading/trailing whitespace and indentation differences. \
         old_string must uniquely identify the target unless replace_all is set."
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
                "old_string": { "type": "string", "description": "The text to replace" },
                "new_string": { "type": "string", "description": "The replacement text" },
                "replace_all": { "type": "boolean", "description": "Replace all occurrences", "default": false }
            },
            "required": ["file_path", "old_string", "new_string"]
        })
    }

    async fn execute(&self, input: Value, _ctx: &ToolContext) -> ToolResult {
        let input = match coerce_input(input) {
            Ok(i) => i,
            Err(e) => return ToolResult::error(e),
        };

        let path = std::path::Path::new(&input.file_path);

        // Capture content before edit for diff
        let before_content = tokio::fs::read_to_string(path).await.unwrap_or_default();

        match pfs::edit_file(path, &input.old_string, &input.new_string, input.replace_all).await {
            Ok(result) => {
                // Generate a compact inline diff
                let after_content = tokio::fs::read_to_string(path).await.unwrap_or_default();
                let diff = crate::tool_primitives::diff::unified_diff(
                    &before_content, &after_content, 2,
                );

                // Include diff in result (truncated for large changes)
                let diff_preview = if diff.lines().count() > 30 {
                    let truncated: String = diff.lines().take(25).collect::<Vec<_>>().join("\n");
                    format!("{}\n... ({} more lines)", truncated, diff.lines().count() - 25)
                } else {
                    diff
                };

                ToolResult::success(format!(
                    "The file {} has been updated. {} replacement(s) made.\n{}",
                    input.file_path, result.replacements_made, diff_preview
                ))
            }
            Err(pfs::EditError::NotFound) => ToolResult::error(format!(
                "old_string not found in {}. The editor already tolerates leading/trailing \
                 whitespace and indentation differences, so a mismatch here means the text \
                 itself differs. Re-read the file with the Read tool and copy old_string \
                 verbatim from the current contents (watch for typos, hidden characters, or \
                 stale content from an earlier edit).",
                input.file_path
            )),
            Err(pfs::EditError::AmbiguousMatch { count }) => ToolResult::error(format!(
                "old_string is not unique ({} occurrences) in {}. Either add surrounding lines \
                 to old_string so it identifies exactly one location, or set replace_all=true \
                 to change every occurrence.",
                count, input.file_path
            )),
            Err(pfs::EditError::NoChange) => ToolResult::error(
                "old_string and new_string are identical, so the edit would do nothing. \
                 Make new_string the intended replacement.".to_string(),
            ),
            Err(pfs::EditError::Io(e)) => ToolResult::error(format!(
                "Failed to edit file {}: {}", input.file_path, e
            )),
        }
    }
}

/// Parsed and coerced edit request.
struct EditInput {
    file_path: String,
    old_string: String,
    new_string: String,
    replace_all: bool,
}

/// Coerce a loosely-shaped tool call into a valid [`EditInput`].
///
/// Weaker models frequently emit near-miss field names (`path`, `oldText`,
/// `old`), stringified booleans (`"true"`), or numeric values where strings are
/// expected. Rather than rejecting the whole edit on a strict deserialize, we
/// accept these common variants — the cost of an unrecoverable round-trip on a
/// slow model is far higher than a little leniency here.
fn coerce_input(input: Value) -> std::result::Result<EditInput, String> {
    let obj = input
        .as_object()
        .ok_or_else(|| "Invalid input: expected a JSON object".to_string())?;

    // Pull a string field, trying a list of accepted aliases, coercing numbers
    // and bools to their string form.
    let get_str = |keys: &[&str]| -> Option<String> {
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

    let file_path = get_str(&["file_path", "filePath", "path", "file"])
        .ok_or_else(|| "Invalid input: missing 'file_path'".to_string())?;

    let old_string = get_str(&["old_string", "oldString", "old_str", "old", "search"])
        .ok_or_else(|| {
            "Invalid input: missing 'old_string' (the exact text to replace)".to_string()
        })?;

    // new_string may legitimately be an empty string (a deletion); treat a
    // missing field as empty so deletions don't fail on omission.
    let new_string =
        get_str(&["new_string", "newString", "new_str", "new", "replace"]).unwrap_or_default();

    let replace_all = match obj
        .get("replace_all")
        .or_else(|| obj.get("replaceAll"))
        .or_else(|| obj.get("all"))
    {
        Some(Value::Bool(b)) => *b,
        Some(Value::String(s)) => matches!(s.trim().to_ascii_lowercase().as_str(), "true" | "1" | "yes"),
        Some(Value::Number(n)) => n.as_i64().map(|v| v != 0).unwrap_or(false),
        _ => false,
    };

    Ok(EditInput {
        file_path,
        old_string,
        new_string,
        replace_all,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::permissions::AllowAll;
    use std::sync::Arc;

    fn test_ctx() -> ToolContext {
        ToolContext {
            working_dir: std::env::temp_dir(),
            session_id: "edit-test".into(),
            permissions: Arc::new(AllowAll),
            cost_tracker: Arc::new(CostTracker::new()),
            mcp_manager: None,
            extensions: Extensions::default(),
        }
    }

    #[tokio::test]
    async fn tolerant_edit_survives_indentation_drift() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("f.rs");
        // File is indented; the model supplies old_string without indentation.
        std::fs::write(&path, "fn main() {\n        let x = 1;\n}\n").unwrap();

        let tool = FileEditTool;
        let res = tool
            .execute(
                serde_json::json!({
                    "file_path": path.to_str().unwrap(),
                    "old_string": "let x = 1;",
                    "new_string": "let x = 2;"
                }),
                &test_ctx(),
            )
            .await;

        assert!(!res.is_error, "expected success, got: {:?}", res.content);
        let after = std::fs::read_to_string(&path).unwrap();
        assert_eq!(after, "fn main() {\n        let x = 2;\n}\n");
    }

    #[tokio::test]
    async fn coerces_aliased_field_names() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("f.txt");
        std::fs::write(&path, "hello world").unwrap();

        let tool = FileEditTool;
        // Model emitted `path`/`old`/`new` instead of the canonical names.
        let res = tool
            .execute(
                serde_json::json!({
                    "path": path.to_str().unwrap(),
                    "old": "world",
                    "new": "there"
                }),
                &test_ctx(),
            )
            .await;

        assert!(!res.is_error, "expected success, got: {:?}", res.content);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "hello there");
    }

    #[tokio::test]
    async fn coerces_stringified_replace_all() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("f.txt");
        std::fs::write(&path, "a a a").unwrap();

        let tool = FileEditTool;
        let res = tool
            .execute(
                serde_json::json!({
                    "file_path": path.to_str().unwrap(),
                    "old_string": "a",
                    "new_string": "b",
                    "replace_all": "true"
                }),
                &test_ctx(),
            )
            .await;

        assert!(!res.is_error, "expected success, got: {:?}", res.content);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "b b b");
    }

    #[tokio::test]
    async fn ambiguous_match_gives_corrective_message() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("f.txt");
        std::fs::write(&path, "a a a").unwrap();

        let tool = FileEditTool;
        let res = tool
            .execute(
                serde_json::json!({
                    "file_path": path.to_str().unwrap(),
                    "old_string": "a",
                    "new_string": "b"
                }),
                &test_ctx(),
            )
            .await;

        assert!(res.is_error);
        let msg = res.content;
        assert!(msg.contains("not unique"));
        assert!(msg.contains("replace_all"));
    }
}
