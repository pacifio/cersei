//! Grep tool: search file contents with regex.

use super::*;
use crate::tool_primitives::search as psearch;
use serde::Deserialize;

pub struct GrepTool;

#[async_trait]
impl Tool for GrepTool {
    fn name(&self) -> &str {
        "Grep"
    }
    fn description(&self) -> &str {
        "Recursively search file contents by regex, in-process (ripgrep-powered). \
         Respects .gitignore and skips hidden/binary files by default. Prefer this \
         over running `rg`/`grep` in Bash — no external tools are required."
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
                "pattern": { "type": "string", "description": "Regex pattern to search for" },
                "path": { "type": "string", "description": "File or directory to search in (defaults to the working directory)" },
                "glob": { "type": "string", "description": "Whitelist glob to filter files, e.g. *.rs" },
                "case_insensitive": { "type": "boolean", "description": "Case-insensitive matching", "default": false },
                "hidden": { "type": "boolean", "description": "Include hidden files/directories", "default": false }
            },
            "required": ["pattern"]
        })
    }

    async fn execute(&self, input: Value, ctx: &ToolContext) -> ToolResult {
        #[derive(Deserialize)]
        struct Input {
            pattern: String,
            path: Option<String>,
            glob: Option<String>,
            #[serde(default)]
            case_insensitive: bool,
            #[serde(default)]
            hidden: bool,
        }

        let input: Input = match serde_json::from_value(input) {
            Ok(i) => i,
            Err(e) => return ToolResult::error(format!("Invalid input: {}", e)),
        };

        let search_path = input
            .path
            .unwrap_or_else(|| ctx.working_dir.display().to_string());

        let opts = psearch::GrepOptions {
            glob_filter: input.glob,
            max_results: Some(250),
            case_insensitive: input.case_insensitive,
            no_ignore: false,
            hidden: input.hidden,
        };

        match psearch::grep(&input.pattern, std::path::Path::new(&search_path), opts).await {
            Ok(matches) => {
                if matches.is_empty() {
                    ToolResult::success("No matches found.")
                } else {
                    let output: Vec<String> = matches
                        .iter()
                        .map(|m| {
                            format!("{}:{}:{}", m.file.display(), m.line_number, m.line_content)
                        })
                        .collect();
                    ToolResult::success(output.join("\n"))
                }
            }
            Err(e) => ToolResult::error(format!("Search failed: {}", e)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::permissions::AllowAll;
    use std::fs;
    use std::sync::Arc;

    fn ctx_in(dir: &std::path::Path) -> ToolContext {
        ToolContext {
            working_dir: dir.to_path_buf(),
            session_id: "grep-test".into(),
            permissions: Arc::new(AllowAll),
            cost_tracker: Arc::new(CostTracker::new()),
            mcp_manager: None,
            extensions: Extensions::default(),
        }
    }

    #[tokio::test]
    async fn emits_file_line_content_format() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join("a.txt"), "one\nTARGET two\nthree\n").unwrap();

        let res = GrepTool
            .execute(
                serde_json::json!({ "pattern": "TARGET", "path": tmp.path().to_str().unwrap() }),
                &ctx_in(tmp.path()),
            )
            .await;

        assert!(!res.is_error, "got: {}", res.content);
        // Format is `file:line:content`.
        let line = res.content.lines().next().unwrap();
        assert!(line.ends_with("a.txt:2:TARGET two"), "line was: {line}");
    }

    #[tokio::test]
    async fn defaults_path_to_working_dir() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join("a.txt"), "FINDME\n").unwrap();

        // No `path` provided — should search ctx.working_dir.
        let res = GrepTool
            .execute(
                serde_json::json!({ "pattern": "FINDME" }),
                &ctx_in(tmp.path()),
            )
            .await;

        assert!(!res.is_error, "got: {}", res.content);
        assert!(res.content.contains("a.txt"));
    }

    #[tokio::test]
    async fn reports_no_matches() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join("a.txt"), "nothing here\n").unwrap();

        let res = GrepTool
            .execute(
                serde_json::json!({ "pattern": "ABSENT", "path": tmp.path().to_str().unwrap() }),
                &ctx_in(tmp.path()),
            )
            .await;

        assert!(!res.is_error);
        assert_eq!(res.content, "No matches found.");
    }

    #[tokio::test]
    async fn case_insensitive_flag_is_honored() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join("a.txt"), "Hello World\n").unwrap();
        let path = tmp.path().to_str().unwrap();

        let off = GrepTool
            .execute(
                serde_json::json!({ "pattern": "hello", "path": path }),
                &ctx_in(tmp.path()),
            )
            .await;
        assert_eq!(off.content, "No matches found.");

        let on = GrepTool
            .execute(
                serde_json::json!({ "pattern": "hello", "path": path, "case_insensitive": true }),
                &ctx_in(tmp.path()),
            )
            .await;
        assert!(on.content.contains("Hello World"));
    }

    #[tokio::test]
    async fn glob_filter_restricts_files() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join("a.rs"), "MATCH\n").unwrap();
        fs::write(tmp.path().join("b.txt"), "MATCH\n").unwrap();

        let res = GrepTool
            .execute(
                serde_json::json!({
                    "pattern": "MATCH",
                    "path": tmp.path().to_str().unwrap(),
                    "glob": "*.rs"
                }),
                &ctx_in(tmp.path()),
            )
            .await;

        assert!(!res.is_error);
        assert!(res.content.contains("a.rs"));
        assert!(!res.content.contains("b.txt"));
    }

    #[tokio::test]
    async fn respects_gitignore_unless_hidden_requested() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join(".gitignore"), "build/\n").unwrap();
        fs::create_dir(tmp.path().join("build")).unwrap();
        fs::write(tmp.path().join("src.rs"), "TOKEN\n").unwrap();
        fs::write(tmp.path().join("build/gen.rs"), "TOKEN\n").unwrap();

        let res = GrepTool
            .execute(
                serde_json::json!({ "pattern": "TOKEN", "path": tmp.path().to_str().unwrap() }),
                &ctx_in(tmp.path()),
            )
            .await;

        assert!(!res.is_error);
        // The gitignored build/ dir is skipped; only src.rs matches.
        assert!(res.content.contains("src.rs"));
        assert!(!res.content.contains("gen.rs"), "got: {}", res.content);
    }

    #[tokio::test]
    async fn invalid_regex_is_reported_as_error() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join("a.txt"), "x\n").unwrap();

        let res = GrepTool
            .execute(
                serde_json::json!({ "pattern": "(unclosed", "path": tmp.path().to_str().unwrap() }),
                &ctx_in(tmp.path()),
            )
            .await;

        assert!(res.is_error);
        assert!(res.content.contains("Search failed"));
    }
}
