//! LSP manager: routes files to the correct language server.
//!
//! Maintains a registry of server configs and lazily starts servers
//! on first access for a given file type.

use crate::client::{self, LspClient, LspError, LspResult};
use crate::config::LspServerConfig;
use crate::types::{LspDiagnostic, SymbolInfo};
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use tokio::sync::Mutex;

/// Multi-server manager that routes files to the correct LSP server.
pub struct LspManager {
    configs: Vec<LspServerConfig>,
    clients: HashMap<String, Arc<LspClient>>,
    /// Extension -> server name mapping (built on registration).
    extension_map: HashMap<String, String>,
    /// Files already opened on their server.
    opened_files: std::collections::HashSet<String>,
    /// Working directory for server processes.
    working_dir: std::path::PathBuf,
}

impl LspManager {
    /// Create a new manager for a given working directory.
    pub fn new(working_dir: impl Into<std::path::PathBuf>) -> Self {
        Self {
            configs: Vec::new(),
            clients: HashMap::new(),
            extension_map: HashMap::new(),
            opened_files: std::collections::HashSet::new(),
            working_dir: working_dir.into(),
        }
    }

    /// Register a server config. Builds extension routing table.
    pub fn register_server(&mut self, config: LspServerConfig) {
        // Skip if already registered
        if self.configs.iter().any(|c| c.name == config.name) {
            return;
        }

        // Build extension map
        for ext in config.extension_to_language.keys() {
            self.extension_map.insert(ext.clone(), config.name.clone());
        }
        for pattern in &config.file_patterns {
            if let Some(ext) = pattern.strip_prefix('*') {
                self.extension_map
                    .insert(ext.to_string(), config.name.clone());
            }
        }

        self.configs.push(config);
    }

    /// Register all built-in servers.
    pub fn register_builtins(&mut self) {
        for config in crate::config::builtin_servers() {
            self.register_server(config);
        }
    }

    /// Seed from a list of configs (idempotent).
    pub fn seed_from_configs(&mut self, configs: &[LspServerConfig]) {
        for config in configs {
            self.register_server(config.clone());
        }
    }

    /// Get all registered server configs.
    pub fn servers(&self) -> &[LspServerConfig] {
        &self.configs
    }

    /// Find which server handles a given file extension.
    pub fn server_name_for_file(&self, path: &Path) -> Option<&str> {
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| format!(".{e}"))?;

        self.extension_map.get(&ext).map(|s| s.as_str())
    }

    /// Check if any server is configured for a file.
    pub fn has_server_for(&self, path: &Path) -> bool {
        self.server_name_for_file(path).is_some()
    }

    /// Ensure a server is started and initialized. Returns the client.
    pub async fn ensure_started(&mut self, server_name: &str) -> LspResult<Arc<LspClient>> {
        // Already started?
        if let Some(client) = self.clients.get(server_name) {
            return Ok(Arc::clone(client));
        }

        // Find config
        let config = self
            .configs
            .iter()
            .find(|c| c.name == server_name)
            .cloned()
            .ok_or(LspError::NotStarted)?;

        // Check binary exists
        if which::which(&config.command).is_err() {
            return Err(LspError::SpawnFailed(format!(
                "'{}' not found in PATH. Install the language server or remove it from config.",
                config.command
            )));
        }

        // Start client
        let client = Arc::new(LspClient::new(config));
        client.start(&self.working_dir).await?;
        client.initialize().await?;

        self.clients
            .insert(server_name.to_string(), Arc::clone(&client));
        tracing::info!("LSP server '{}' ready", server_name);
        Ok(client)
    }

    /// Open a file on the appropriate server.
    pub async fn open_file(&mut self, path: &Path) -> LspResult<()> {
        let abs = if path.is_absolute() {
            path.to_path_buf()
        } else {
            self.working_dir.join(path)
        };
        let abs_str = abs.display().to_string();

        if self.opened_files.contains(&abs_str) {
            return Ok(());
        }

        let server_name = self
            .server_name_for_file(&abs)
            .map(String::from)
            .ok_or(LspError::NotStarted)?;

        let client = self.ensure_started(&server_name).await?;
        client.open_document(&abs).await?;
        self.opened_files.insert(abs_str);
        Ok(())
    }

    /// Hover at position.
    pub async fn hover(
        &mut self,
        path: &Path,
        line: u32,
        character: u32,
    ) -> LspResult<Option<String>> {
        self.open_file(path).await?;
        let server = self
            .server_name_for_file(path)
            .map(String::from)
            .ok_or(LspError::NotStarted)?;
        let client = self.clients.get(&server).ok_or(LspError::NotStarted)?;
        client.hover(path, line, character).await
    }

    /// Go to definition.
    pub async fn definition(
        &mut self,
        path: &Path,
        line: u32,
        character: u32,
    ) -> LspResult<Vec<String>> {
        self.open_file(path).await?;
        let server = self
            .server_name_for_file(path)
            .map(String::from)
            .ok_or(LspError::NotStarted)?;
        let client = self.clients.get(&server).ok_or(LspError::NotStarted)?;
        client.definition(path, line, character).await
    }

    /// Find references.
    pub async fn references(
        &mut self,
        path: &Path,
        line: u32,
        character: u32,
    ) -> LspResult<Vec<String>> {
        self.open_file(path).await?;
        let server = self
            .server_name_for_file(path)
            .map(String::from)
            .ok_or(LspError::NotStarted)?;
        let client = self.clients.get(&server).ok_or(LspError::NotStarted)?;
        client.references(path, line, character).await
    }

    /// Document symbols (outline).
    pub async fn document_symbols(&mut self, path: &Path) -> LspResult<Vec<SymbolInfo>> {
        self.open_file(path).await?;
        let server = self
            .server_name_for_file(path)
            .map(String::from)
            .ok_or(LspError::NotStarted)?;
        let client = self.clients.get(&server).ok_or(LspError::NotStarted)?;
        client.document_symbols(path).await
    }

    /// Get diagnostics for a file.
    pub async fn diagnostics(&mut self, path: &Path) -> LspResult<Vec<LspDiagnostic>> {
        self.open_file(path).await?;
        // Wait briefly for async diagnostics to arrive
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;

        let server = self
            .server_name_for_file(path)
            .map(String::from)
            .ok_or(LspError::NotStarted)?;
        let client = self.clients.get(&server).ok_or(LspError::NotStarted)?;
        Ok(client.get_diagnostics(path))
    }

    /// Get all diagnostics across all servers.
    pub fn all_diagnostics(&self) -> Vec<LspDiagnostic> {
        self.clients
            .values()
            .flat_map(|c| c.all_diagnostics())
            .collect()
    }

    /// Shut down all servers gracefully.
    pub async fn shutdown_all(&self) {
        for client in self.clients.values() {
            let _ = client.shutdown().await;
        }
    }

    /// Format diagnostics for display.
    pub fn format_diagnostics(diagnostics: &[LspDiagnostic]) -> String {
        client::format_diagnostics(diagnostics)
    }
}

impl Drop for LspManager {
    fn drop(&mut self) {
        // Best-effort shutdown — can't await in drop, but processes will be
        // killed when Child is dropped.
        for (name, _) in &self.clients {
            tracing::debug!("Dropping LSP client '{}'", name);
        }
    }
}

// ─── Global singleton ───────────────────────────────────────────────────────

static GLOBAL_MANAGER: std::sync::OnceLock<Arc<Mutex<LspManager>>> = std::sync::OnceLock::new();

/// Get or create the global LSP manager.
pub fn global_lsp_manager(working_dir: &Path) -> Arc<Mutex<LspManager>> {
    Arc::clone(GLOBAL_MANAGER.get_or_init(|| Arc::new(Mutex::new(LspManager::new(working_dir)))))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::LspServerConfig;

    #[test]
    fn test_register_and_route() {
        let mut mgr = LspManager::new("/tmp");
        mgr.register_server(LspServerConfig::new(
            "rust-analyzer",
            "rust-analyzer",
            &["*.rs"],
            &[(".rs", "rust")],
        ));

        assert_eq!(
            mgr.server_name_for_file(Path::new("main.rs")),
            Some("rust-analyzer")
        );
        assert!(mgr.server_name_for_file(Path::new("main.py")).is_none());
    }

    #[test]
    fn test_register_builtins() {
        let mut mgr = LspManager::new("/tmp");
        mgr.register_builtins();
        assert!(mgr.has_server_for(Path::new("main.rs")));
        assert!(mgr.has_server_for(Path::new("main.py")));
        assert!(mgr.has_server_for(Path::new("main.go")));
        assert!(mgr.has_server_for(Path::new("main.ts")));
    }

    #[test]
    fn test_idempotent_registration() {
        let mut mgr = LspManager::new("/tmp");
        mgr.register_builtins();
        let count = mgr.configs.len();
        mgr.register_builtins();
        assert_eq!(mgr.configs.len(), count);
    }
}
