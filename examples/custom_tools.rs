//! # Custom Tools
//!
//! Shows how to define custom tools using the `#[derive(Tool)]` macro
//! and the raw `Tool` trait, then register them with an agent.
//!
//! ```bash
//! ANTHROPIC_API_KEY=sk-ant-... cargo run --example custom_tools
//! ```

use cersei::prelude::*;
use serde::Deserialize;
use std::collections::HashMap;
use std::sync::Arc;

// ─── Custom tool via derive macro ────────────────────────────────────────────

// NOTE: The derive macro generates the Tool impl automatically from ToolExecute.
// For this example we'll implement Tool directly to avoid needing the macro
// in a dev-dependency context.

/// A tool that counts words in text.
struct WordCountTool;

#[async_trait]
impl Tool for WordCountTool {
    fn name(&self) -> &str { "word_count" }
    fn description(&self) -> &str {
        "Count the number of words, lines, and characters in the given text."
    }
    fn permission_level(&self) -> PermissionLevel { PermissionLevel::None }
    fn category(&self) -> ToolCategory { ToolCategory::Custom }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "text": {
                    "type": "string",
                    "description": "The text to analyze"
                }
            },
            "required": ["text"]
        })
    }

    async fn execute(&self, input: serde_json::Value, _ctx: &ToolContext) -> ToolResult {
        #[derive(Deserialize)]
        struct Input { text: String }

        let input: Input = match serde_json::from_value(input) {
            Ok(i) => i,
            Err(e) => return ToolResult::error(format!("Invalid input: {}", e)),
        };

        let words = input.text.split_whitespace().count();
        let lines = input.text.lines().count();
        let chars = input.text.chars().count();

        ToolResult::success(format!(
            "Words: {}\nLines: {}\nCharacters: {}",
            words, lines, chars
        ))
    }
}

/// A tool that looks up values from a key-value store.
struct KvLookupTool {
    store: Arc<parking_lot::Mutex<HashMap<String, String>>>,
}

#[async_trait]
impl Tool for KvLookupTool {
    fn name(&self) -> &str { "kv_lookup" }
    fn description(&self) -> &str {
        "Look up a value by key from the key-value store. Returns the value or 'not found'."
    }
    fn permission_level(&self) -> PermissionLevel { PermissionLevel::ReadOnly }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "key": { "type": "string", "description": "The key to look up" }
            },
            "required": ["key"]
        })
    }

    async fn execute(&self, input: serde_json::Value, _ctx: &ToolContext) -> ToolResult {
        #[derive(Deserialize)]
        struct Input { key: String }

        let input: Input = match serde_json::from_value(input) {
            Ok(i) => i,
            Err(e) => return ToolResult::error(format!("Invalid input: {}", e)),
        };

        let store = self.store.lock();
        match store.get(&input.key) {
            Some(value) => ToolResult::success(format!("{} = {}", input.key, value)),
            None => ToolResult::success(format!("Key '{}' not found", input.key)),
        }
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Seed the KV store
    let store = Arc::new(parking_lot::Mutex::new(HashMap::new()));
    {
        let mut s = store.lock();
        s.insert("project".into(), "Cersei SDK".into());
        s.insert("version".into(), "0.1.0".into());
        s.insert("language".into(), "Rust".into());
        s.insert("author".into(), "The Cersei Contributors".into());
    }

    let agent = Agent::builder()
        .provider(Anthropic::from_env()?)
        .tool(WordCountTool)
        .tool(KvLookupTool { store: store.clone() })
        .tools(cersei::tools::filesystem()) // also add file tools
        .system_prompt("You are a helpful assistant. Use tools when needed.")
        .max_turns(5)
        .permission_policy(AllowAll)
        .build()?;

    let output = agent
        .run("Look up the 'project' and 'version' keys, then count the words in the combined result.")
        .await?;

    println!("─── Result ───");
    println!("{}", output.text());
    println!("\nTool calls made:");
    for tc in &output.tool_calls {
        println!("  {} → {} ({}ms)", tc.name,
            if tc.is_error { "ERR" } else { "OK" },
            tc.duration.as_millis());
    }

    Ok(())
}
