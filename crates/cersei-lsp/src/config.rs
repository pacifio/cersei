//! LSP server configuration and built-in server definitions.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Configuration for a single LSP server.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LspServerConfig {
    /// Display name (e.g. "rust-analyzer").
    pub name: String,
    /// Command to spawn (e.g. "rust-analyzer").
    pub command: String,
    /// Arguments to pass.
    #[serde(default)]
    pub args: Vec<String>,
    /// File glob patterns this server handles (e.g. ["*.rs", "*.toml"]).
    #[serde(default)]
    pub file_patterns: Vec<String>,
    /// Map file extension to LSP language ID (e.g. ".rs" -> "rust").
    #[serde(default)]
    pub extension_to_language: HashMap<String, String>,
    /// Extra environment variables for the server process.
    #[serde(default)]
    pub env: HashMap<String, String>,
    /// Initialization options sent during LSP initialize.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub initialization_options: Option<serde_json::Value>,
}

impl LspServerConfig {
    /// Create a simple server config.
    pub fn new(
        name: impl Into<String>,
        command: impl Into<String>,
        patterns: &[&str],
        lang_map: &[(&str, &str)],
    ) -> Self {
        Self {
            name: name.into(),
            command: command.into(),
            args: vec![],
            file_patterns: patterns.iter().map(|s| s.to_string()).collect(),
            extension_to_language: lang_map
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            env: HashMap::new(),
            initialization_options: None,
        }
    }

    /// Check if this server handles a given file extension.
    pub fn matches_extension(&self, ext: &str) -> bool {
        // Check extension_to_language map
        if self.extension_to_language.contains_key(ext) {
            return true;
        }
        // Check glob patterns
        let ext_pattern = format!("*{ext}");
        self.file_patterns.iter().any(|p| p == &ext_pattern)
    }

    /// Get the language ID for a file extension.
    pub fn language_id(&self, ext: &str) -> String {
        self.extension_to_language
            .get(ext)
            .cloned()
            .unwrap_or_else(|| ext.trim_start_matches('.').to_string())
    }
}

// ─── Built-in server configurations ─────────────────────────────────────────

/// Returns built-in LSP server configs for common languages.
/// Users can override or extend via config.
pub fn builtin_servers() -> Vec<LspServerConfig> {
    vec![
        // Rust
        LspServerConfig::new(
            "rust-analyzer",
            "rust-analyzer",
            &["*.rs"],
            &[(".rs", "rust")],
        ),
        // Python
        LspServerConfig::new(
            "pyright",
            "pyright-langserver",
            &["*.py", "*.pyi"],
            &[(".py", "python"), (".pyi", "python")],
        ),
        // TypeScript / JavaScript
        {
            let mut cfg = LspServerConfig::new(
                "typescript-language-server",
                "typescript-language-server",
                &["*.ts", "*.tsx", "*.js", "*.jsx", "*.mjs", "*.cjs"],
                &[
                    (".ts", "typescript"),
                    (".tsx", "typescriptreact"),
                    (".js", "javascript"),
                    (".jsx", "javascriptreact"),
                    (".mjs", "javascript"),
                    (".cjs", "javascript"),
                ],
            );
            cfg.args = vec!["--stdio".to_string()];
            cfg
        },
        // Go
        LspServerConfig::new("gopls", "gopls", &["*.go"], &[(".go", "go")]),
        // C / C++
        LspServerConfig::new(
            "clangd",
            "clangd",
            &["*.c", "*.h", "*.cpp", "*.hpp", "*.cc", "*.cxx"],
            &[
                (".c", "c"),
                (".h", "c"),
                (".cpp", "cpp"),
                (".hpp", "cpp"),
                (".cc", "cpp"),
                (".cxx", "cpp"),
            ],
        ),
        // Ruby
        {
            let mut cfg = LspServerConfig::new(
                "ruby-lsp",
                "ruby-lsp",
                &["*.rb"],
                &[(".rb", "ruby")],
            );
            cfg.args = vec!["--stdio".to_string()];
            cfg
        },
        // PHP
        {
            let mut cfg = LspServerConfig::new(
                "phpactor",
                "phpactor",
                &["*.php"],
                &[(".php", "php")],
            );
            cfg.args = vec!["language-server".to_string()];
            cfg
        },
        // Lua
        {
            let mut cfg = LspServerConfig::new(
                "lua-language-server",
                "lua-language-server",
                &["*.lua"],
                &[(".lua", "lua")],
            );
            cfg.args = vec!["--stdio".to_string()];
            cfg
        },
        // Bash / Shell
        {
            let mut cfg = LspServerConfig::new(
                "bash-language-server",
                "bash-language-server",
                &["*.sh", "*.bash"],
                &[(".sh", "shellscript"), (".bash", "shellscript")],
            );
            cfg.args = vec!["start".to_string()];
            cfg
        },
        // Swift
        LspServerConfig::new(
            "sourcekit-lsp",
            "sourcekit-lsp",
            &["*.swift"],
            &[(".swift", "swift")],
        ),
        // C#
        {
            let mut cfg = LspServerConfig::new(
                "omnisharp",
                "OmniSharp",
                &["*.cs"],
                &[(".cs", "csharp")],
            );
            cfg.args = vec!["-lsp".to_string()];
            cfg
        },
        // Java
        LspServerConfig::new("jdtls", "jdtls", &["*.java"], &[(".java", "java")]),
        // Zig
        LspServerConfig::new("zls", "zls", &["*.zig"], &[(".zig", "zig")]),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_builtin_servers() {
        let servers = builtin_servers();
        assert!(servers.len() >= 10);
        assert!(servers.iter().any(|s| s.name == "rust-analyzer"));
        assert!(servers.iter().any(|s| s.name == "pyright"));
        assert!(servers.iter().any(|s| s.name == "gopls"));
    }

    #[test]
    fn test_matches_extension() {
        let cfg = LspServerConfig::new("test", "test", &["*.rs"], &[(".rs", "rust")]);
        assert!(cfg.matches_extension(".rs"));
        assert!(!cfg.matches_extension(".py"));
    }

    #[test]
    fn test_language_id() {
        let cfg = LspServerConfig::new("test", "test", &[], &[(".rs", "rust")]);
        assert_eq!(cfg.language_id(".rs"), "rust");
        assert_eq!(cfg.language_id(".unknown"), "unknown");
    }
}
