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
        let (file_path, edits) = match coerce_input(&input) {
            Ok(v) => v,
            // A top-level shape problem: route through the shared builder for
            // the tool name, an echo of the arguments, and the parameter table
            // (F-05b/F-A14).
            Err(CoerceError::Shape(e)) => {
                return crate::tool_feedback::invalid_input(self, &input, e)
            }
            // A problem *inside* `edits` is already fully phrased. See
            // [`CoerceError::InsideEdits`] for why the shared builder is
            // deliberately bypassed here.
            Err(CoerceError::InsideEdits(msg)) => return ToolResult::error(msg),
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

/// The parameters `MultiEdit` declares, at the top level and per edit.
const KNOWN_PARAMS: &[&str] = &["file_path", "edits"];
const KNOWN_EDIT_PARAMS: &[&str] = &["old_string", "new_string", "replace_all"];

/// Parse a MultiEdit call into a path + edit list, coercing scalar *types* but
/// not parameter *names*.
///
/// Mirrors `Edit`: the name aliases (`path`, `changes`, `oldString`, `all`, …)
/// were removed because accepting them here while `Grep` and `Glob` silently
/// dropped the same misspellings taught models a schema the runtime did not
/// actually honour. Unknown keys are now refused, per edit as well as at the
/// top level — a typo buried in `edits[3]` was previously the easiest way to
/// lose one edit out of several with no indication which.
/// Why a MultiEdit call could not be read.
enum CoerceError {
    /// A top-level shape problem, phrased as a bare reason. The shared failure
    /// builder supplies the tool name, the echo, and the parameter table.
    Shape(String),
    /// A problem *inside* `edits`, already carrying its own instruction.
    ///
    /// The shared builder is bypassed on purpose. Its corrected-call example
    /// models top-level keys only, so for a nested mistake it closed with
    ///
    /// ```text
    /// Retry 'MultiEdit' now, sending exactly this JSON object:
    /// {"edits":"<keep the same value you sent>", "file_path":"…"}
    /// ```
    ///
    /// — telling the model to keep the very edits that were just refused, and
    /// collapsing a required array into a string on the way. That closing
    /// instruction is the one part the builder never truncates, so a model
    /// following it literally resends the failing call and loops.
    InsideEdits(String),
}

/// Model-facing text for an unrecognised key inside one edit.
///
/// Says which edit, which key, what the real name probably is, and to leave the
/// other edits alone — everything the top-level builder cannot express.
fn unknown_edit_key_message(i: usize, total: usize, key: &str) -> String {
    let expected = KNOWN_EDIT_PARAMS
        .iter()
        .map(|k| format!("`{k}`"))
        .collect::<Vec<_>>()
        .join(", ");
    let n = i + 1;
    let mut m = format!(
        "Tool 'MultiEdit' rejected your arguments: edit {n} of {total} uses unknown field \
         `{key}`, expected one of {expected}.\n\n"
    );
    if let Some(best) = crate::tool_feedback::closest(key, KNOWN_EDIT_PARAMS) {
        m.push_str(&format!(
            "You used '{key}' in edit {n}, but that parameter is named '{best}'. Rename it.\n\n"
        ));
    }
    m.push_str(
        "Each entry in 'edits' takes: old_string (string, the exact text to replace), \
         new_string (string, the replacement — omit it for a deletion), \
         replace_all (boolean, optional).\n\n",
    );
    m.push_str(&format!(
        "Send the call again with edit {n} corrected, leaving the other edits as they are. \
         No changes were written.\n"
    ));
    m
}

fn coerce_input(input: &Value) -> std::result::Result<(String, Vec<EditOp>), CoerceError> {
    let obj = input
        .as_object()
        .ok_or_else(|| CoerceError::Shape("the arguments must be a JSON object".to_string()))?;

    crate::tool_feedback::reject_unknown_keys(input, KNOWN_PARAMS).map_err(CoerceError::Shape)?;

    let get_str = |obj: &serde_json::Map<String, Value>, key: &str| -> Option<String> {
        match obj.get(key) {
            Some(Value::String(s)) => Some(s.clone()),
            Some(Value::Number(n)) => Some(n.to_string()),
            Some(Value::Bool(b)) => Some(b.to_string()),
            _ => None,
        }
    };

    let get_bool = |obj: &serde_json::Map<String, Value>, key: &str| -> bool {
        match obj.get(key) {
            Some(Value::Bool(b)) => *b,
            Some(Value::String(s)) => {
                matches!(s.trim().to_ascii_lowercase().as_str(), "true" | "1" | "yes")
            }
            Some(Value::Number(n)) => n.as_i64().map(|v| v != 0).unwrap_or(false),
            _ => false,
        }
    };

    let file_path = get_str(obj, "file_path").ok_or_else(|| {
        CoerceError::Shape("missing 'file_path' (the absolute path of the file to edit)".to_string())
    })?;

    let edits_val = obj.get("edits").ok_or_else(|| {
        CoerceError::Shape(
            "missing 'edits' (an array of {old_string, new_string} objects)".to_string(),
        )
    })?;
    let edits_arr = edits_val.as_array().ok_or_else(|| {
        CoerceError::Shape(
            "'edits' must be a JSON array of {old_string, new_string} objects".to_string(),
        )
    })?;

    let total = edits_arr.len();
    let mut edits = Vec::with_capacity(total);
    for (i, e) in edits_arr.iter().enumerate() {
        let eo = e.as_object().ok_or_else(|| {
            CoerceError::InsideEdits(format!(
                "Tool 'MultiEdit' rejected your arguments: edit {} of {total} is not a JSON \
                 object. Each entry in 'edits' must be an object with old_string and \
                 new_string. No changes were written.\n",
                i + 1
            ))
        })?;
        if let Err(err) = crate::tool_feedback::reject_unknown_keys(e, KNOWN_EDIT_PARAMS) {
            // Recover the offending key from serde's phrasing to name it precisely.
            let key = err
                .split('`')
                .nth(1)
                .map(str::to_string)
                .unwrap_or_else(|| "?".to_string());
            return Err(CoerceError::InsideEdits(unknown_edit_key_message(
                i, total, &key,
            )));
        }
        let old_string = get_str(eo, "old_string").ok_or_else(|| {
            CoerceError::InsideEdits(format!(
                "Tool 'MultiEdit' rejected your arguments: edit {} of {total} is missing \
                 'old_string' (the exact text to replace). Send the call again with edit {} \
                 corrected, leaving the other edits as they are. No changes were written.\n",
                i + 1,
                i + 1
            ))
        })?;
        // A missing new_string is a deletion.
        let new_string = get_str(eo, "new_string").unwrap_or_default();
        let replace_all = get_bool(eo, "replace_all");
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

    /// A typo inside one edit must not produce a "retry with exactly this"
    /// example that still contains the typo.
    ///
    /// The shared failure builder models corrected calls at the *top level*
    /// only. `edits` is a legitimate top-level key, so it was copied through
    /// verbatim — offending entry and all — under an instruction the builder
    /// deliberately never truncates. A model following it literally resends the
    /// identical call and loops until the turn budget runs out.
    #[tokio::test]
    async fn per_edit_typo_is_not_echoed_back_as_the_suggested_retry() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("f.rs");
        std::fs::write(&path, "let a = 1;\nlet c = 3;\n").unwrap();

        let res = MultiEditTool
            .execute(
                serde_json::json!({
                    "file_path": path.to_str().unwrap(),
                    "edits": [
                        { "old_string": "let a = 1;", "new_string": "let b = 2;" },
                        // camelCase typo in the second edit only.
                        { "oldString": "let c = 3;", "new_string": "let d = 4;" }
                    ]
                }),
                &test_ctx(),
            )
            .await;

        assert!(res.is_error, "got: {}", res.content);
        assert!(
            res.content.contains("oldString"),
            "must quote the offending key: {}",
            res.content
        );
        assert!(
            res.content.contains("old_string"),
            "must name the real parameter: {}",
            res.content
        );
        assert!(
            res.content.contains('2'),
            "must say which edit is wrong: {}",
            res.content
        );

        // The load-bearing assertion. The shared builder used to close with
        //   Retry 'MultiEdit' now, sending exactly this JSON object:
        //   {"edits":"<keep the same value you sent>", "file_path": "..."}
        // which tells the model to keep the very edits that were refused, and
        // collapses a required array into a string on the way. Neither may
        // appear.
        assert!(
            !res.content.contains("keep the same value you sent"),
            "the suggested retry tells the model to resend the rejected edits: {}",
            res.content
        );
        assert!(
            !res.content.contains("sending exactly this"),
            "a nested failure must not emit a top-level-only corrected call: {}",
            res.content
        );

        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "let a = 1;\nlet c = 3;\n",
            "a refused MultiEdit must be atomic — no edit may land"
        );
    }


}
