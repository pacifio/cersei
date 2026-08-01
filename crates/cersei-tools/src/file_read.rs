//! File read tool.

use super::*;
use crate::tool_primitives::fs as pfs;
use serde::Deserialize;

pub struct FileReadTool;

#[async_trait]
impl Tool for FileReadTool {
    fn name(&self) -> &str {
        "Read"
    }
    fn description(&self) -> &str {
        "Read a file from the filesystem. Use offset/limit to read a slice of a large file. Read a file before you edit it."
    }
    fn permission_level(&self) -> PermissionLevel {
        PermissionLevel::ReadOnly
    }
    fn category(&self) -> ToolCategory {
        ToolCategory::FileSystem
    }

    fn input_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "file_path": { "type": "string", "description": "Absolute path to the file" },
                "offset": { "type": "integer", "description": "Line number to start reading from" },
                "limit": { "type": "integer", "description": "Number of lines to read" }
            },
            "required": ["file_path"]
        })
    }

    async fn execute(&self, input: Value, _ctx: &ToolContext) -> ToolResult {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Input {
            file_path: String,
            offset: Option<usize>,
            limit: Option<usize>,
        }

        let input: Input = match crate::tool_feedback::parse_input(self, &input) {
            Ok(i) => i,
            Err(e) => return e,
        };

        let path = std::path::Path::new(&input.file_path);
        if !path.exists() {
            return not_found_with_siblings(path, &input.file_path);
        }

        let offset = input.offset.unwrap_or(0);
        let limit = input.limit.unwrap_or(2000);

        match pfs::read_file(path, offset, limit).await {
            Ok(fc) => {
                // F-A13: `read_file` computes total_lines/offset/lines_returned
                // and this site used to throw all three away, so a 5,000-line
                // file read under the default 2,000-line cap looked complete.
                // The model then anchored edits to context that does not exist.
                let mut out = fc.content;
                if let Some(notice) = crate::tool_feedback::window_notice(
                    fc.offset,
                    fc.lines_returned,
                    fc.total_lines,
                    "lines",
                    &format!(
                        "To read the rest, call Read again with offset={} (and the same limit).",
                        fc.offset + fc.lines_returned
                    ),
                ) {
                    if !out.is_empty() && !out.ends_with('\n') {
                        out.push('\n');
                    }
                    out.push_str(&notice);
                }
                ToolResult::success(out)
            }
            Err(e) => ToolResult::error(format!("Failed to read file: {}", e)),
        }
    }
}

/// "File not found" with the sibling names that do exist (F-A15).
///
/// There is no registry to enumerate here, but the parent directory is
/// derivable from the path the model sent, and a near-miss filename is the
/// single most common cause of this error.
fn not_found_with_siblings(path: &std::path::Path, requested: &str) -> ToolResult {
    let parent = path.parent().filter(|p| !p.as_os_str().is_empty());
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");

    let Some(parent) = parent else {
        return ToolResult::error(format!(
            "File not found: {requested}\n\nCheck the path and retry. Use Glob or LS to find the correct absolute path."
        ));
    };
    if !parent.is_dir() {
        return ToolResult::error(format!(
            "File not found: {requested}\n\nIts parent directory {} does not exist either, so the path is wrong above the filename. Use Glob to locate the file, then Read the path Glob returns.",
            parent.display()
        ));
    }

    let mut siblings: Vec<String> = std::fs::read_dir(parent)
        .map(|rd| {
            rd.flatten()
                .filter_map(|e| e.file_name().into_string().ok())
                .collect()
        })
        .unwrap_or_default();
    siblings.sort();

    let mut msg = format!("File not found: {requested}");
    let refs: Vec<&str> = siblings.iter().map(String::as_str).collect();
    if let Some(best) = crate::tool_feedback::closest(name, &refs) {
        msg.push_str(&format!(
            "\n\nDid you mean: {}?",
            parent.join(best).display()
        ));
    }
    msg.push_str(&format!(
        "\n\nIts directory {} exists and holds {} entr{}. Use Glob or LS to list it, then Read the exact path.",
        parent.display(),
        siblings.len(),
        if siblings.len() == 1 { "y" } else { "ies" }
    ));
    ToolResult::error(msg)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::permissions::AllowAll;
    use std::sync::Arc;

    fn test_ctx() -> ToolContext {
        ToolContext {
            working_dir: std::env::temp_dir(),
            session_id: "read-test".into(),
            permissions: Arc::new(AllowAll),
            cost_tracker: Arc::new(CostTracker::new()),
            mcp_manager: None,
            extensions: Extensions::default(),
        }
    }

    /// The measured Exp-2 regression: `{"path": …}` instead of `{"file_path": …}`.
    /// Weak models recovered 6/12 from the old message and 12/12 from this one.
    #[tokio::test]
    async fn wrong_param_name_tells_the_model_the_real_name() {
        let r = FileReadTool
            .execute(serde_json::json!({ "path": "/x.rs" }), &test_ctx())
            .await;
        assert!(r.is_error);
        assert!(r.content.contains("'Read'"), "{}", r.content);
        assert!(r.content.contains("file_path"), "{}", r.content);
        assert!(r.content.contains("/x.rs"), "{}", r.content);
        assert!(
            !r.content.contains("struct Input"),
            "must not leak a Rust type name: {}",
            r.content
        );
    }

    /// Phase 1 hands malformed wire JSON through as `__parse_error`/`__raw`.
    #[tokio::test]
    async fn wire_parse_failure_reports_the_raw_text() {
        let r = FileReadTool
            .execute(
                serde_json::json!({
                    "__parse_error": "EOF while parsing a string at line 1 column 22",
                    "__raw": "{'file_path': '/x/y.rs",
                }),
                &test_ctx(),
            )
            .await;
        assert!(r.is_error);
        assert!(r.content.contains("not valid JSON"), "{}", r.content);
        assert!(r.content.contains("{'file_path': '/x/y.rs"), "{}", r.content);
        assert!(r.content.contains("double quotes"), "{}", r.content);
    }

    /// F-A13: a 5,000-line file under the 2,000-line default must not look
    /// like a complete file.
    #[tokio::test]
    async fn truncated_read_is_marked() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("big.txt");
        let body: String = (1..=5000).map(|i| format!("line {i}\n")).collect();
        std::fs::write(&path, body).unwrap();

        let r = FileReadTool
            .execute(
                serde_json::json!({ "file_path": path.to_str().unwrap() }),
                &test_ctx(),
            )
            .await;
        assert!(!r.is_error, "{}", r.content);
        assert!(r.content.contains("line 2000"), "window should reach 2000");
        assert!(!r.content.contains("line 2001"), "window must stop at 2000");
        assert!(
            r.content.contains("Showing lines 1-2000 of 5000"),
            "truncation must be visible: {}",
            r.content.lines().last().unwrap_or("")
        );
        assert!(r.content.contains("offset=2000"), "must say how to continue");
    }

    #[tokio::test]
    async fn complete_read_is_not_annotated() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("small.txt");
        std::fs::write(&path, "a\nb\nc\n").unwrap();

        let r = FileReadTool
            .execute(
                serde_json::json!({ "file_path": path.to_str().unwrap() }),
                &test_ctx(),
            )
            .await;
        assert!(!r.is_error);
        assert!(!r.content.contains("Showing lines"), "{}", r.content);
    }

    /// An offset past EOF used to return a blank success.
    #[tokio::test]
    async fn offset_past_eof_is_explained() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("small.txt");
        std::fs::write(&path, "a\nb\nc\n").unwrap();

        let r = FileReadTool
            .execute(
                serde_json::json!({ "file_path": path.to_str().unwrap(), "offset": 900 }),
                &test_ctx(),
            )
            .await;
        assert!(r.content.contains("No lines returned"), "{}", r.content);
        assert!(r.content.contains("only 3 lines"), "{}", r.content);
    }

    #[tokio::test]
    async fn missing_file_suggests_the_sibling_that_exists() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("main.rs"), "fn main() {}").unwrap();

        let r = FileReadTool
            .execute(
                serde_json::json!({
                    "file_path": tmp.path().join("mian.rs").to_str().unwrap()
                }),
                &test_ctx(),
            )
            .await;
        assert!(r.is_error);
        assert!(r.content.contains("File not found"), "{}", r.content);
        assert!(r.content.contains("main.rs"), "{}", r.content);
    }
}
