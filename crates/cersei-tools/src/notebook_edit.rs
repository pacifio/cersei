//! NotebookEdit tool: edit Jupyter/IPython notebook cells.

use super::*;
use serde::Deserialize;

pub struct NotebookEditTool;

#[async_trait]
impl Tool for NotebookEditTool {
    fn name(&self) -> &str { "NotebookEdit" }
    fn description(&self) -> &str {
        "Edit a Jupyter notebook (.ipynb) cell by index. Can replace cell source or change cell type."
    }
    fn permission_level(&self) -> PermissionLevel { PermissionLevel::Write }
    fn category(&self) -> ToolCategory { ToolCategory::FileSystem }

    fn input_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "file_path": { "type": "string", "description": "Path to .ipynb file" },
                "cell_index": { "type": "integer", "description": "0-based cell index to edit" },
                "new_source": { "type": "string", "description": "New cell source content" },
                "cell_type": { "type": "string", "description": "Optional: 'code' or 'markdown'" }
            },
            "required": ["file_path", "cell_index", "new_source"]
        })
    }

    async fn execute(&self, input: Value, _ctx: &ToolContext) -> ToolResult {
        #[derive(Deserialize)]
        struct Input {
            file_path: String,
            cell_index: usize,
            new_source: String,
            cell_type: Option<String>,
        }

        let input: Input = match serde_json::from_value(input) {
            Ok(i) => i,
            Err(e) => return ToolResult::error(format!("Invalid input: {}", e)),
        };

        let content = match tokio::fs::read_to_string(&input.file_path).await {
            Ok(c) => c,
            Err(e) => return ToolResult::error(format!("Failed to read notebook: {}", e)),
        };

        let mut notebook: Value = match serde_json::from_str(&content) {
            Ok(n) => n,
            Err(e) => return ToolResult::error(format!("Invalid notebook JSON: {}", e)),
        };

        let cells = match notebook.get_mut("cells").and_then(|c| c.as_array_mut()) {
            Some(c) => c,
            None => return ToolResult::error("Notebook has no 'cells' array"),
        };

        if input.cell_index >= cells.len() {
            return ToolResult::error(format!(
                "Cell index {} out of range (notebook has {} cells)",
                input.cell_index,
                cells.len()
            ));
        }

        // Update cell source (notebooks store source as array of lines)
        let source_lines: Vec<Value> = input
            .new_source
            .lines()
            .enumerate()
            .map(|(i, line)| {
                if i < input.new_source.lines().count() - 1 {
                    Value::String(format!("{}\n", line))
                } else {
                    Value::String(line.to_string())
                }
            })
            .collect();

        cells[input.cell_index]["source"] = Value::Array(source_lines);

        if let Some(ct) = &input.cell_type {
            cells[input.cell_index]["cell_type"] = Value::String(ct.clone());
        }

        // Clear outputs for code cells
        if cells[input.cell_index]["cell_type"].as_str() == Some("code") {
            cells[input.cell_index]["outputs"] = Value::Array(vec![]);
            cells[input.cell_index]["execution_count"] = Value::Null;
        }

        let output = serde_json::to_string_pretty(&notebook).unwrap_or_default();
        match tokio::fs::write(&input.file_path, output).await {
            Ok(()) => ToolResult::success(format!(
                "Updated cell {} in {}",
                input.cell_index, input.file_path
            )),
            Err(e) => ToolResult::error(format!("Failed to write notebook: {}", e)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::permissions::AllowAll;
    use std::sync::Arc;

    fn test_ctx() -> ToolContext {
        ToolContext {
            working_dir: std::env::temp_dir(),
            session_id: "nb-test".into(),
            permissions: Arc::new(AllowAll),
            cost_tracker: Arc::new(CostTracker::new()),
            mcp_manager: None,
            extensions: Extensions::default(),
        }
    }

    #[tokio::test]
    async fn test_notebook_edit() {
        let tmp = tempfile::tempdir().unwrap();
        let nb_path = tmp.path().join("test.ipynb");
        let notebook = serde_json::json!({
            "nbformat": 4,
            "nbformat_minor": 5,
            "metadata": {},
            "cells": [
                {"cell_type": "code", "source": ["print('hello')\n"], "outputs": [], "metadata": {}},
                {"cell_type": "markdown", "source": ["# Title\n"], "metadata": {}}
            ]
        });
        std::fs::write(&nb_path, serde_json::to_string(&notebook).unwrap()).unwrap();

        let tool = NotebookEditTool;
        let result = tool.execute(serde_json::json!({
            "file_path": nb_path.display().to_string(),
            "cell_index": 0,
            "new_source": "print('updated')"
        }), &test_ctx()).await;

        assert!(!result.is_error);
        assert!(result.content.contains("Updated cell 0"));

        // Verify
        let content: Value = serde_json::from_str(&std::fs::read_to_string(&nb_path).unwrap()).unwrap();
        let source = content["cells"][0]["source"][0].as_str().unwrap();
        assert!(source.contains("updated"));
    }
}
