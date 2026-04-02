//! TOML configuration with layered loading.
//!
//! Priority (lowest → highest):
//! 1. Hardcoded defaults
//! 2. ~/.abstract/config.toml  (user global)
//! 3. .abstract/config.toml    (project local)
//! 4. Environment variables     (ABSTRACT_MODEL, etc.)
//! 5. CLI flags

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

// ─── Config structs ────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AppConfig {
    pub model: String,
    pub provider: String,
    pub max_turns: u32,
    pub max_tokens: u32,
    pub effort: String,
    pub output_style: String,
    pub theme: String,
    pub auto_compact: bool,
    pub graph_memory: bool,
    pub permissions_mode: String,
    pub working_dir: PathBuf,
    #[serde(default)]
    pub mcp_servers: Vec<McpServerEntry>,
    #[serde(default)]
    pub hooks: Vec<HookEntry>,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            model: "claude-sonnet-4-6".into(),
            provider: "auto".into(),
            max_turns: 20,
            max_tokens: 16384,
            effort: "medium".into(),
            output_style: "default".into(),
            theme: "dark".into(),
            auto_compact: true,
            graph_memory: true,
            permissions_mode: "interactive".into(),
            working_dir: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            mcp_servers: Vec::new(),
            hooks: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServerEntry {
    pub name: String,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: std::collections::HashMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HookEntry {
    pub event: String,
    pub command: String,
}

// ─── Config directories ────────────────────────────────────────────────────

/// ~/.abstract/
pub fn global_config_dir() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".abstract")
}

/// .abstract/ in the current project
pub fn project_config_dir() -> PathBuf {
    std::env::current_dir()
        .unwrap_or_else(|_| PathBuf::from("."))
        .join(".abstract")
}

/// ~/.abstract/config.toml
pub fn global_config_path() -> PathBuf {
    global_config_dir().join("config.toml")
}

/// .abstract/config.toml
pub fn project_config_path() -> PathBuf {
    project_config_dir().join("config.toml")
}

/// ~/.abstract/history
pub fn history_path() -> PathBuf {
    global_config_dir().join("history")
}

/// ~/.abstract/graph.db
pub fn graph_db_path() -> PathBuf {
    global_config_dir().join("graph.db")
}

// ─── Loading ───────────────────────────────────────────────────────────────

/// Load config with layered merging.
pub fn load() -> AppConfig {
    let mut config = AppConfig::default();

    // Layer 2: global config
    if let Some(loaded) = load_toml_file(&global_config_path()) {
        merge(&mut config, loaded);
    }

    // Layer 3: project config
    if let Some(loaded) = load_toml_file(&project_config_path()) {
        merge(&mut config, loaded);
    }

    // Layer 4: environment variables
    apply_env(&mut config);

    config
}

fn load_toml_file(path: &Path) -> Option<AppConfig> {
    let content = std::fs::read_to_string(path).ok()?;
    toml::from_str(&content).ok()
}

fn merge(base: &mut AppConfig, overlay: AppConfig) {
    // Only override non-default values
    if overlay.model != AppConfig::default().model {
        base.model = overlay.model;
    }
    if overlay.provider != AppConfig::default().provider {
        base.provider = overlay.provider;
    }
    if overlay.max_turns != AppConfig::default().max_turns {
        base.max_turns = overlay.max_turns;
    }
    if overlay.max_tokens != AppConfig::default().max_tokens {
        base.max_tokens = overlay.max_tokens;
    }
    if overlay.effort != AppConfig::default().effort {
        base.effort = overlay.effort;
    }
    if overlay.output_style != AppConfig::default().output_style {
        base.output_style = overlay.output_style;
    }
    if overlay.theme != AppConfig::default().theme {
        base.theme = overlay.theme;
    }
    if !overlay.auto_compact && AppConfig::default().auto_compact {
        base.auto_compact = false;
    }
    if !overlay.graph_memory && AppConfig::default().graph_memory {
        base.graph_memory = false;
    }
    if overlay.permissions_mode != AppConfig::default().permissions_mode {
        base.permissions_mode = overlay.permissions_mode;
    }
    if !overlay.mcp_servers.is_empty() {
        base.mcp_servers = overlay.mcp_servers;
    }
    if !overlay.hooks.is_empty() {
        base.hooks = overlay.hooks;
    }
}

fn apply_env(config: &mut AppConfig) {
    if let Ok(v) = std::env::var("ABSTRACT_MODEL") {
        config.model = v;
    }
    if let Ok(v) = std::env::var("ABSTRACT_PROVIDER") {
        config.provider = v;
    }
    if let Ok(v) = std::env::var("ABSTRACT_EFFORT") {
        config.effort = v;
    }
    if let Ok(v) = std::env::var("ABSTRACT_THEME") {
        config.theme = v;
    }
    if let Ok(v) = std::env::var("ABSTRACT_MAX_TURNS") {
        if let Ok(n) = v.parse() {
            config.max_turns = n;
        }
    }
}

/// Save config to a TOML file.
pub fn save_to(config: &AppConfig, path: &Path) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let content = toml::to_string_pretty(config)?;
    std::fs::write(path, content)?;
    Ok(())
}
