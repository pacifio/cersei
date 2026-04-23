//! cersei-mcp: Model Context Protocol (MCP) client.
//!
//! Full JSON-RPC 2.0 implementation with stdio transport for connecting
//! to MCP servers. Discovers tools and resources, makes them available
//! as standard Cersei tool definitions.

pub mod jsonrpc;
pub mod transport;

use cersei_types::*;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;

// ─── MCP server config ──────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServerConfig {
    pub name: String,
    pub command: Option<String>,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: HashMap<String, String>,
    pub url: Option<String>,
    #[serde(rename = "type", default = "default_type")]
    pub server_type: String,
}

fn default_type() -> String {
    "stdio".to_string()
}

impl McpServerConfig {
    pub fn stdio(name: impl Into<String>, command: impl Into<String>, args: &[&str]) -> Self {
        Self {
            name: name.into(),
            command: Some(command.into()),
            args: args.iter().map(|s| s.to_string()).collect(),
            env: HashMap::new(),
            url: None,
            server_type: "stdio".to_string(),
        }
    }

    pub fn sse(name: impl Into<String>, url: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            command: None,
            args: Vec::new(),
            env: HashMap::new(),
            url: Some(url.into()),
            server_type: "sse".to_string(),
        }
    }
}

// ─── MCP protocol types ─────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpToolDef {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub input_schema: serde_json::Value,
}

impl From<&McpToolDef> for ToolDefinition {
    fn from(t: &McpToolDef) -> Self {
        ToolDefinition {
            name: t.name.clone(),
            description: t.description.clone().unwrap_or_default(),
            input_schema: t.input_schema.clone(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpResource {
    pub uri: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", rename = "mimeType")]
    pub mime_type: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum McpContent {
    Text {
        text: String,
    },
    Image {
        data: String,
        #[serde(rename = "mimeType")]
        mime_type: String,
    },
    Resource {
        resource: McpResource,
    },
}

// ─── Server status ───────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub enum McpServerStatus {
    Connecting,
    Connected,
    Error(String),
    Disconnected,
}

// ─── MCP client (per-server) ─────────────────────────────────────────────────

/// A client connected to a single MCP server.
pub struct McpClient {
    pub config: McpServerConfig,
    pub status: McpServerStatus,
    pub tools: Vec<McpToolDef>,
    pub resources: Vec<McpResource>,
    transport: Option<transport::StdioTransport>,
}

impl McpClient {
    /// Connect to an MCP server and perform the handshake.
    pub async fn connect(config: McpServerConfig) -> Result<Self> {
        let config_expanded = expand_server_config(&config);

        if config_expanded.server_type == "stdio" {
            let command = config_expanded
                .command
                .as_deref()
                .ok_or_else(|| CerseiError::Mcp("stdio server requires 'command'".into()))?;

            let mut transport = transport::StdioTransport::spawn(
                command,
                &config_expanded.args,
                &config_expanded.env,
            )
            .await?;

            // Initialize handshake
            let init_params = serde_json::json!({
                "protocolVersion": "2024-11-05",
                "capabilities": {
                    "roots": { "listChanged": true }
                },
                "clientInfo": {
                    "name": "cersei",
                    "version": env!("CARGO_PKG_VERSION")
                }
            });

            let init_result = transport.request("initialize", Some(init_params)).await?;
            tracing::debug!("MCP initialize result: {:?}", init_result);

            // Send initialized notification
            transport.notify("notifications/initialized", None).await?;

            // Discover tools
            let tools_result = transport.request("tools/list", None).await?;
            let tools: Vec<McpToolDef> = tools_result
                .get("tools")
                .and_then(|t| serde_json::from_value(t.clone()).ok())
                .unwrap_or_default();

            // Discover resources
            let resources = match transport.request("resources/list", None).await {
                Ok(res) => res
                    .get("resources")
                    .and_then(|r| serde_json::from_value(r.clone()).ok())
                    .unwrap_or_default(),
                Err(_) => Vec::new(), // resources are optional
            };

            tracing::info!(
                server = %config.name,
                tools = tools.len(),
                resources = resources.len(),
                "MCP server connected"
            );

            Ok(Self {
                config,
                status: McpServerStatus::Connected,
                tools,
                resources,
                transport: Some(transport),
            })
        } else {
            // SSE transport placeholder
            Err(CerseiError::Mcp(format!(
                "SSE transport not yet implemented for server '{}'",
                config.name
            )))
        }
    }

    /// Call a tool on this MCP server.
    pub async fn call_tool(
        &mut self,
        tool_name: &str,
        arguments: Option<serde_json::Value>,
    ) -> Result<String> {
        let transport = self
            .transport
            .as_mut()
            .ok_or_else(|| CerseiError::Mcp("Not connected".into()))?;

        let params = serde_json::json!({
            "name": tool_name,
            "arguments": arguments.unwrap_or(serde_json::Value::Object(Default::default())),
        });

        let result = transport.request("tools/call", Some(params)).await?;

        // Parse content array
        let content: Vec<McpContent> = result
            .get("content")
            .and_then(|c| serde_json::from_value(c.clone()).ok())
            .unwrap_or_default();

        let is_error = result
            .get("isError")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let text: String = content
            .iter()
            .filter_map(|c| match c {
                McpContent::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n");

        if is_error {
            Err(CerseiError::Mcp(text))
        } else {
            Ok(text)
        }
    }

    /// Read a resource from this MCP server.
    pub async fn read_resource(&mut self, uri: &str) -> Result<String> {
        let transport = self
            .transport
            .as_mut()
            .ok_or_else(|| CerseiError::Mcp("Not connected".into()))?;

        let params = serde_json::json!({ "uri": uri });
        let result = transport.request("resources/read", Some(params)).await?;

        let contents = result
            .get("contents")
            .and_then(|c| c.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|item| item.get("text").and_then(|t| t.as_str()))
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .unwrap_or_default();

        Ok(contents)
    }

    /// Get tool definitions for the provider.
    pub fn tool_definitions(&self) -> Vec<ToolDefinition> {
        self.tools.iter().map(ToolDefinition::from).collect()
    }
}

// ─── MCP manager (multi-server) ──────────────────────────────────────────────

/// Manages connections to multiple MCP servers.
pub struct McpManager {
    clients: Arc<Mutex<HashMap<String, McpClient>>>,
}

impl McpManager {
    /// Connect to all configured MCP servers.
    pub async fn connect(configs: &[McpServerConfig]) -> Result<Self> {
        let mut clients = HashMap::new();

        for config in configs {
            match McpClient::connect(config.clone()).await {
                Ok(client) => {
                    clients.insert(config.name.clone(), client);
                }
                Err(e) => {
                    tracing::warn!(server = %config.name, error = %e, "Failed to connect MCP server");
                }
            }
        }

        Ok(Self {
            clients: Arc::new(Mutex::new(clients)),
        })
    }

    /// Get all discovered tool definitions across all servers.
    pub async fn tool_definitions(&self) -> Vec<ToolDefinition> {
        let clients = self.clients.lock().await;
        clients
            .values()
            .flat_map(|c| c.tool_definitions())
            .collect()
    }

    /// Call a tool by name (routes to the correct server).
    pub async fn call_tool(
        &self,
        tool_name: &str,
        arguments: Option<serde_json::Value>,
    ) -> Result<String> {
        let mut clients = self.clients.lock().await;

        for client in clients.values_mut() {
            if client.tools.iter().any(|t| t.name == tool_name) {
                return client.call_tool(tool_name, arguments).await;
            }
        }

        Err(CerseiError::Mcp(format!(
            "No MCP server has tool '{}'",
            tool_name
        )))
    }

    /// List all resources across all servers.
    pub async fn list_resources(&self) -> Vec<McpResource> {
        let clients = self.clients.lock().await;
        clients.values().flat_map(|c| c.resources.clone()).collect()
    }

    /// Read a resource by URI (routes to the correct server).
    pub async fn read_resource(&self, uri: &str) -> Result<String> {
        let mut clients = self.clients.lock().await;

        for client in clients.values_mut() {
            if client.resources.iter().any(|r| r.uri == uri) {
                return client.read_resource(uri).await;
            }
        }

        Err(CerseiError::Mcp(format!(
            "No MCP server has resource '{}'",
            uri
        )))
    }

    /// Get the status of all connected servers.
    pub async fn server_statuses(&self) -> HashMap<String, McpServerStatus> {
        let clients = self.clients.lock().await;
        clients
            .iter()
            .map(|(name, client)| (name.clone(), client.status.clone()))
            .collect()
    }

    /// Get server configs.
    pub async fn configs(&self) -> Vec<McpServerConfig> {
        let clients = self.clients.lock().await;
        clients.values().map(|c| c.config.clone()).collect()
    }
}

// ─── Env var expansion ───────────────────────────────────────────────────────

/// Expand `${VAR}` and `${VAR:-default}` patterns.
pub fn expand_env_vars(input: &str) -> String {
    let mut result = input.to_string();
    let mut search_from = 0;
    loop {
        match result[search_from..].find("${") {
            None => break,
            Some(rel_start) => {
                let start = search_from + rel_start;
                match result[start..].find('}') {
                    None => break,
                    Some(rel_end) => {
                        let end = start + rel_end;
                        let inner = &result[start + 2..end];
                        let (var_name, default_value) = if let Some(pos) = inner.find(":-") {
                            (&inner[..pos], Some(&inner[pos + 2..]))
                        } else {
                            (inner, None)
                        };

                        let replacement = match std::env::var(var_name) {
                            Ok(val) => val,
                            Err(_) => match default_value {
                                Some(def) => def.to_string(),
                                None => {
                                    search_from = end + 1;
                                    continue;
                                }
                            },
                        };

                        result =
                            format!("{}{}{}", &result[..start], replacement, &result[end + 1..]);
                        search_from = start + replacement.len();
                    }
                }
            }
        }
    }
    result
}

/// Expand env vars in all string fields of a server config.
pub fn expand_server_config(config: &McpServerConfig) -> McpServerConfig {
    McpServerConfig {
        name: config.name.clone(),
        command: config.command.as_deref().map(expand_env_vars),
        args: config.args.iter().map(|a| expand_env_vars(a)).collect(),
        env: config
            .env
            .iter()
            .map(|(k, v)| (k.clone(), expand_env_vars(v)))
            .collect(),
        url: config.url.as_deref().map(expand_env_vars),
        server_type: config.server_type.clone(),
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_expand_env_vars_simple() {
        std::env::set_var("CERSEI_TEST_VAR", "hello");
        assert_eq!(expand_env_vars("${CERSEI_TEST_VAR}"), "hello");
        std::env::remove_var("CERSEI_TEST_VAR");
    }

    #[test]
    fn test_expand_env_vars_default() {
        assert_eq!(expand_env_vars("${NONEXISTENT_VAR:-fallback}"), "fallback");
    }

    #[test]
    fn test_expand_env_vars_missing_no_default() {
        let result = expand_env_vars("${CERSEI_MISSING_XYZ}");
        assert_eq!(result, "${CERSEI_MISSING_XYZ}"); // left as-is
    }

    #[test]
    fn test_expand_env_vars_multiple() {
        std::env::set_var("CERSEI_A", "one");
        std::env::set_var("CERSEI_B", "two");
        assert_eq!(expand_env_vars("${CERSEI_A}-${CERSEI_B}"), "one-two");
        std::env::remove_var("CERSEI_A");
        std::env::remove_var("CERSEI_B");
    }

    #[test]
    fn test_stdio_config() {
        let config = McpServerConfig::stdio("test", "node", &["server.js"]);
        assert_eq!(config.server_type, "stdio");
        assert_eq!(config.command.as_deref(), Some("node"));
        assert_eq!(config.args, vec!["server.js"]);
    }

    #[test]
    fn test_sse_config() {
        let config = McpServerConfig::sse("remote", "https://mcp.example.com");
        assert_eq!(config.server_type, "sse");
        assert_eq!(config.url.as_deref(), Some("https://mcp.example.com"));
    }

    #[test]
    fn test_tool_def_conversion() {
        let mcp_tool = McpToolDef {
            name: "search".into(),
            description: Some("Search docs".into()),
            input_schema: serde_json::json!({"type": "object"}),
        };
        let tool_def: ToolDefinition = ToolDefinition::from(&mcp_tool);
        assert_eq!(tool_def.name, "search");
        assert_eq!(tool_def.description, "Search docs");
    }

    #[test]
    fn test_expand_server_config() {
        std::env::set_var("CERSEI_MCP_CMD", "/usr/bin/node");
        let config = McpServerConfig {
            name: "test".into(),
            command: Some("${CERSEI_MCP_CMD}".into()),
            args: vec!["${CERSEI_MCP_CMD}".into()],
            env: HashMap::from([("KEY".into(), "${CERSEI_MCP_CMD}".into())]),
            url: None,
            server_type: "stdio".into(),
        };
        let expanded = expand_server_config(&config);
        assert_eq!(expanded.command.as_deref(), Some("/usr/bin/node"));
        assert_eq!(expanded.args[0], "/usr/bin/node");
        assert_eq!(expanded.env["KEY"], "/usr/bin/node");
        std::env::remove_var("CERSEI_MCP_CMD");
    }
}
