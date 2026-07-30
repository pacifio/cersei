//! ConfigTool: read and modify agent configuration.

use super::*;
use serde::Deserialize;

/// In-memory config store (session-scoped).
static CONFIG_STORE: once_cell::sync::Lazy<dashmap::DashMap<String, serde_json::Value>> =
    once_cell::sync::Lazy::new(dashmap::DashMap::new);

pub fn get_config(key: &str) -> Option<serde_json::Value> {
    CONFIG_STORE.get(key).map(|v| v.clone())
}

pub fn set_config(key: &str, value: serde_json::Value) {
    CONFIG_STORE.insert(key.to_string(), value);
}

pub struct ConfigTool;

#[async_trait]
impl Tool for ConfigTool {
    fn name(&self) -> &str {
        "Config"
    }
    fn description(&self) -> &str {
        "Read or modify configuration values."
    }
    fn permission_level(&self) -> PermissionLevel {
        PermissionLevel::None
    }
    fn category(&self) -> ToolCategory {
        ToolCategory::Custom
    }

    fn input_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "action": { "type": "string", "enum": ["get", "set", "list"], "description": "Action to perform" },
                "key": { "type": "string", "description": "Config key (for get/set)" },
                "value": { "description": "Value to set (for set action)" }
            },
            "required": ["action"]
        })
    }

    async fn execute(&self, input: Value, _ctx: &ToolContext) -> ToolResult {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Input {
            action: String,
            key: Option<String>,
            value: Option<Value>,
        }

        let input: Input = match crate::tool_feedback::parse_input(self, &input) {
            Ok(i) => i,
            Err(e) => return e,
        };

        match input.action.as_str() {
            "get" => {
                let key = input.key.unwrap_or_default();
                match get_config(&key) {
                    Some(v) => ToolResult::success(format!("{} = {}", key, v)),
                    // Still a success (an unset key is not an error), but the
                    // model gets the keys that DO exist instead of a dead end
                    // — the same list the "list" action prints below (F-A15).
                    None => {
                        let keys: Vec<String> =
                            CONFIG_STORE.iter().map(|e| e.key().clone()).collect();
                        let mut msg = format!("{} is not set.", key);
                        if keys.is_empty() {
                            msg.push_str(" No configuration keys are set at all.");
                        } else {
                            if let Some(best) = crate::tool_feedback::closest(
                                &key,
                                &keys.iter().map(String::as_str).collect::<Vec<_>>(),
                            ) {
                                msg.push_str(&format!(" Did you mean '{best}'?"));
                            }
                            msg.push_str(&format!(
                                "\n\nKeys that are set ({}): {}",
                                keys.len(),
                                keys.join(", ")
                            ));
                        }
                        ToolResult::success(msg)
                    }
                }
            }
            "set" => {
                let key = input.key.unwrap_or_default();
                let value = input.value.unwrap_or(Value::Null);
                set_config(&key, value.clone());
                ToolResult::success(format!("{} = {}", key, value))
            }
            "list" => {
                let entries: Vec<String> = CONFIG_STORE
                    .iter()
                    .map(|e| format!("  {} = {}", e.key(), e.value()))
                    .collect();
                if entries.is_empty() {
                    ToolResult::success("No configuration values set.")
                } else {
                    ToolResult::success(entries.join("\n"))
                }
            }
            other => {
                ToolResult::error(format!("Unknown action: {}. Use get, set, or list.", other))
            }
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
            session_id: "cfg-test".into(),
            permissions: Arc::new(AllowAll),
            cost_tracker: Arc::new(CostTracker::new()),
            mcp_manager: None,
            extensions: Extensions::default(),
        }
    }

    #[tokio::test]
    async fn test_config_set_get() {
        let tool = ConfigTool;
        tool.execute(
            serde_json::json!({"action": "set", "key": "theme", "value": "dark"}),
            &test_ctx(),
        )
        .await;
        let result = tool
            .execute(
                serde_json::json!({"action": "get", "key": "theme"}),
                &test_ctx(),
            )
            .await;
        assert!(result.content.contains("dark"));
    }
}
