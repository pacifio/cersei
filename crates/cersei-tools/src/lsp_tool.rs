//! LSP tool: query language servers for code intelligence.
//!
//! Supports 5 operations:
//! - `hover`: Get type/documentation info at a position
//! - `definition`: Go to where a symbol is defined
//! - `references`: Find all references to a symbol
//! - `symbols`: List all symbols in a file (outline)
//! - `diagnostics`: Get compiler errors/warnings for a file

use super::*;
use cersei_lsp::{LspManager, LspServerConfig};
use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::Mutex;

pub struct LspTool {
    manager: Arc<Mutex<LspManager>>,
}

impl LspTool {
    pub fn new(working_dir: &Path) -> Self {
        let mut mgr = LspManager::new(working_dir);
        mgr.register_builtins();
        Self {
            manager: Arc::new(Mutex::new(mgr)),
        }
    }

    pub fn with_configs(working_dir: &Path, extra_configs: &[LspServerConfig]) -> Self {
        let mut mgr = LspManager::new(working_dir);
        mgr.register_builtins();
        mgr.seed_from_configs(extra_configs);
        Self {
            manager: Arc::new(Mutex::new(mgr)),
        }
    }
}

#[async_trait]
impl Tool for LspTool {
    fn name(&self) -> &str {
        "LSP"
    }

    fn description(&self) -> &str {
        "Query a language server for code intelligence. Supports hover (type info), \
         definition (go-to-def), references (find usages), symbols (file outline), \
         and diagnostics (compiler errors). Language servers are auto-detected \
         and started on demand based on file extension."
    }

    fn permission_level(&self) -> PermissionLevel {
        PermissionLevel::ReadOnly
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::FileSystem
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["hover", "definition", "references", "symbols", "diagnostics"],
                    "description": "The LSP operation to perform"
                },
                "file": {
                    "type": "string",
                    "description": "Absolute or relative file path"
                },
                "line": {
                    "type": "integer",
                    "description": "1-based line number (required for hover, definition, references)"
                },
                "column": {
                    "type": "integer",
                    "description": "1-based column number (required for hover, definition, references)"
                }
            },
            "required": ["action", "file"]
        })
    }

    async fn execute(&self, input: serde_json::Value, ctx: &ToolContext) -> ToolResult {
        #[derive(Deserialize)]
        struct Input {
            action: String,
            file: String,
            line: Option<u32>,
            column: Option<u32>,
        }

        let input: Input = match serde_json::from_value(input) {
            Ok(i) => i,
            Err(e) => return ToolResult::error(format!("Invalid input: {e}")),
        };

        // Resolve path
        let path = if Path::new(&input.file).is_absolute() {
            PathBuf::from(&input.file)
        } else {
            ctx.working_dir.join(&input.file)
        };

        if !path.exists() {
            return ToolResult::error(format!("File not found: {}", path.display()));
        }

        let mut mgr = self.manager.lock().await;

        // Check if we have a server for this file type
        if !mgr.has_server_for(&path) {
            let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("?");
            return ToolResult::error(format!(
                "No language server configured for .{ext} files. \
                 Available servers: {}",
                mgr.servers()
                    .iter()
                    .map(|s| s.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }

        // Convert 1-based to 0-based for LSP
        let line = input.line.unwrap_or(1).saturating_sub(1);
        let col = input.column.unwrap_or(1).saturating_sub(1);

        match input.action.as_str() {
            "hover" => match mgr.hover(&path, line, col).await {
                Ok(Some(text)) => ToolResult::success(text),
                Ok(None) => ToolResult::success("No hover information available at this position."),
                Err(e) => ToolResult::error(format!("Hover failed: {e}")),
            },

            "definition" => match mgr.definition(&path, line, col).await {
                Ok(locations) => {
                    if locations.is_empty() {
                        ToolResult::success("No definition found at this position.")
                    } else {
                        ToolResult::success(locations.join("\n"))
                    }
                }
                Err(e) => ToolResult::error(format!("Definition lookup failed: {e}")),
            },

            "references" => match mgr.references(&path, line, col).await {
                Ok(locations) => {
                    if locations.is_empty() {
                        ToolResult::success("No references found at this position.")
                    } else {
                        let count = locations.len();
                        let mut result = locations.join("\n");
                        result.push_str(&format!("\n\n{count} reference(s) found."));
                        ToolResult::success(result)
                    }
                }
                Err(e) => ToolResult::error(format!("References lookup failed: {e}")),
            },

            "symbols" => match mgr.document_symbols(&path).await {
                Ok(symbols) => {
                    if symbols.is_empty() {
                        ToolResult::success("No symbols found in this file.")
                    } else {
                        let output: String = symbols.iter().map(|s| s.format(0)).collect();
                        ToolResult::success(output.trim_end())
                    }
                }
                Err(e) => ToolResult::error(format!("Symbol extraction failed: {e}")),
            },

            "diagnostics" => match mgr.diagnostics(&path).await {
                Ok(diags) => {
                    ToolResult::success(LspManager::format_diagnostics(&diags))
                }
                Err(e) => ToolResult::error(format!("Diagnostics failed: {e}")),
            },

            other => ToolResult::error(format!(
                "Unknown action: '{other}'. Use: hover, definition, references, symbols, diagnostics"
            )),
        }
    }
}
