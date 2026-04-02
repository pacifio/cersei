//! Modular system prompt assembly with caching support.
//!
//! The system prompt is split into cacheable (static) sections that go before
//! `SYSTEM_PROMPT_DYNAMIC_BOUNDARY` and volatile (dynamic) sections that follow.
//! This enables provider-level prompt caching for the static parts.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

// ─── Dynamic boundary marker ────────────────────────────────────────────────

/// Marker splitting cached vs dynamic parts of the system prompt.
/// Everything before this marker can be prompt-cached by the provider.
pub const SYSTEM_PROMPT_DYNAMIC_BOUNDARY: &str = "__SYSTEM_PROMPT_DYNAMIC_BOUNDARY__";

// ─── Section cache ──────────────────────────────────────────────────────────

fn section_cache() -> &'static Mutex<HashMap<String, Option<String>>> {
    static CACHE: OnceLock<Mutex<HashMap<String, Option<String>>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Clear all cached system prompt sections (e.g., on compact or clear).
pub fn clear_system_prompt_sections() {
    if let Ok(mut cache) = section_cache().lock() {
        cache.clear();
    }
}

// ─── Section ─────────────────────────────────────────────────────────────────

/// A single named section of the system prompt.
#[derive(Debug, Clone)]
pub struct SystemPromptSection {
    /// Identifier for cache lookups and invalidation.
    pub tag: String,
    /// Computed content (None = section absent/disabled this turn).
    pub content: Option<String>,
    /// If true, this section is volatile and must not be prompt-cached.
    pub cache_break: bool,
}

impl SystemPromptSection {
    /// Create a cacheable (static) section.
    pub fn cached(tag: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            tag: tag.into(),
            content: Some(content.into()),
            cache_break: false,
        }
    }

    /// Create a volatile (dynamic) section.
    pub fn uncached(tag: impl Into<String>, content: Option<String>) -> Self {
        Self {
            tag: tag.into(),
            content,
            cache_break: true,
        }
    }
}

// ─── Output style ────────────────────────────────────────────────────────────

/// Output style that affects the system prompt tone.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum OutputStyle {
    #[default]
    Default,
    Explanatory,
    Learning,
    Concise,
    Formal,
    Casual,
}

impl OutputStyle {
    /// Returns the system-prompt suffix for this style, or None for Default.
    pub fn prompt_suffix(self) -> Option<&'static str> {
        match self {
            OutputStyle::Explanatory => Some(
                "When explaining code or concepts, be thorough and educational. \
                Include reasoning, alternatives considered, and potential pitfalls. \
                Err on the side of over-explaining.",
            ),
            OutputStyle::Learning => Some(
                "This user is learning. Explain concepts as you implement them. \
                Point out patterns, best practices, and why you made each decision. \
                Use analogies when helpful.",
            ),
            OutputStyle::Concise => Some(
                "Be maximally concise. Skip preamble, summaries, and filler. \
                Lead with the answer. One sentence is better than three.",
            ),
            OutputStyle::Formal => Some(
                "Maintain a formal, professional tone. Use precise technical language.",
            ),
            OutputStyle::Casual => Some("Use a casual, conversational tone."),
            OutputStyle::Default => None,
        }
    }

    /// Parse from a string (case-insensitive).
    pub fn from_str(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "explanatory" => Self::Explanatory,
            "learning" => Self::Learning,
            "concise" => Self::Concise,
            "formal" => Self::Formal,
            "casual" => Self::Casual,
            _ => Self::Default,
        }
    }
}

// ─── Prefix ──────────────────────────────────────────────────────────────────

/// Context in which the agent is running. Determines the opening attribution.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SystemPromptPrefix {
    /// Standard interactive session.
    Interactive,
    /// Running as an SDK-embedded agent.
    Sdk,
    /// SDK agent with custom system prompt appended.
    SdkPreset,
    /// Running as a sub-agent spawned by another agent.
    SubAgent,
}

impl SystemPromptPrefix {
    /// Detect from context.
    pub fn detect(is_non_interactive: bool, has_append_system_prompt: bool) -> Self {
        if is_non_interactive {
            if has_append_system_prompt {
                return Self::SdkPreset;
            }
            return Self::Sdk;
        }
        Self::Interactive
    }

    /// The opening attribution string.
    pub fn attribution_text(self) -> &'static str {
        match self {
            Self::Interactive => {
                "You are a coding agent built with the Cersei SDK."
            }
            Self::SdkPreset => {
                "You are a coding agent built with the Cersei SDK, \
                running with custom instructions."
            }
            Self::Sdk => {
                "You are an agent built on the Cersei SDK."
            }
            Self::SubAgent => {
                "You are a specialized sub-agent."
            }
        }
    }
}

// ─── Build options ───────────────────────────────────────────────────────────

/// All options controlling what goes into the assembled system prompt.
#[derive(Debug, Clone, Default)]
pub struct SystemPromptOptions {
    /// Override auto-detected prefix.
    pub prefix: Option<SystemPromptPrefix>,
    /// Whether the session is non-interactive (SDK / pipe mode).
    pub is_non_interactive: bool,
    /// Whether append_system_prompt is set.
    pub has_append_system_prompt: bool,
    /// Output style to inject.
    pub output_style: OutputStyle,
    /// Optional custom output-style prompt from config.
    pub custom_output_style_prompt: Option<String>,
    /// Absolute path to the working directory.
    pub working_directory: Option<String>,
    /// Pre-built memory content from memdir.
    pub memory_content: String,
    /// Custom system prompt (replaces default if replace_system_prompt is true).
    pub custom_system_prompt: Option<String>,
    /// Additional text appended after everything.
    pub append_system_prompt: Option<String>,
    /// If true and custom_system_prompt is set, replaces the entire default prompt.
    pub replace_system_prompt: bool,
    /// Inject the coordinator-mode section.
    pub coordinator_mode: bool,
    /// Additional custom sections to inject (before boundary).
    pub extra_cached_sections: Vec<(String, String)>,
    /// Additional custom sections to inject (after boundary).
    pub extra_dynamic_sections: Vec<(String, String)>,
}

// ─── Main assembly ───────────────────────────────────────────────────────────

/// Build the complete system prompt string.
///
/// The returned string contains `SYSTEM_PROMPT_DYNAMIC_BOUNDARY` as an
/// internal marker. Callers split on this marker to determine which
/// portions are eligible for prompt caching.
pub fn build_system_prompt(opts: &SystemPromptOptions) -> String {
    // Replace mode: skip all default sections
    if opts.replace_system_prompt {
        if let Some(custom) = &opts.custom_system_prompt {
            return format!("{}\n\n{}", custom, SYSTEM_PROMPT_DYNAMIC_BOUNDARY);
        }
    }

    let prefix = opts.prefix.unwrap_or_else(|| {
        SystemPromptPrefix::detect(opts.is_non_interactive, opts.has_append_system_prompt)
    });

    let mut parts: Vec<String> = Vec::new();

    // ── CACHEABLE sections (before boundary) ─────────────────────────────

    // 1. Attribution
    parts.push(prefix.attribution_text().to_string());

    // 2. Core capabilities
    parts.push(CORE_CAPABILITIES.to_string());

    // 3. Tool use guidelines
    parts.push(TOOL_USE_GUIDELINES.to_string());

    // 4. Actions with care
    parts.push(ACTIONS_SECTION.to_string());

    // 5. Safety guidelines
    parts.push(SAFETY_GUIDELINES.to_string());

    // 6. Security
    parts.push(SECURITY_SECTION.to_string());

    // 7. Output style
    if let Some(style_text) = opts
        .custom_output_style_prompt
        .as_deref()
        .filter(|s| !s.trim().is_empty())
        .or_else(|| opts.output_style.prompt_suffix())
    {
        parts.push(format!("\n## Output Style\n{}", style_text));
    }

    // 8. Coordinator mode
    if opts.coordinator_mode {
        parts.push(COORDINATOR_SECTION.to_string());
    }

    // 9. Custom system prompt (cacheable)
    if let Some(custom) = &opts.custom_system_prompt {
        parts.push(format!(
            "\n<custom_instructions>\n{}\n</custom_instructions>",
            custom
        ));
    }

    // 10. Extra cached sections
    for (tag, content) in &opts.extra_cached_sections {
        parts.push(format!("\n<{}>\n{}\n</{}>", tag, content, tag));
    }

    // Dynamic boundary
    parts.push(SYSTEM_PROMPT_DYNAMIC_BOUNDARY.to_string());

    // ── DYNAMIC sections (after boundary) ────────────────────────────────

    // 11. Working directory
    if let Some(cwd) = &opts.working_directory {
        parts.push(format!("\n<working_directory>{}</working_directory>", cwd));
    }

    // 12. Memory
    if !opts.memory_content.is_empty() {
        parts.push(format!(
            "\n<memory>\n{}\n</memory>",
            opts.memory_content
        ));
    }

    // 13. Extra dynamic sections
    for (tag, content) in &opts.extra_dynamic_sections {
        parts.push(format!("\n<{}>\n{}\n</{}>", tag, content, tag));
    }

    // 14. Appended system prompt
    if let Some(append) = &opts.append_system_prompt {
        parts.push(format!("\n{}", append));
    }

    parts.join("\n")
}

// ─── Static sections ─────────────────────────────────────────────────────────

const CORE_CAPABILITIES: &str = r#"
## Capabilities

You have access to powerful tools for software engineering tasks:
- **Read/Write files**: Read any file, write new files, edit existing files with precise diffs
- **Execute commands**: Run bash commands, PowerShell scripts, background processes
- **Search**: Glob patterns, regex grep, web search, file content search
- **Web**: Fetch URLs, search the internet
- **Agents**: Spawn parallel sub-agents for complex multi-step work
- **Memory**: Persistent notes across sessions via the memory system
- **MCP servers**: Connect to external tools and APIs via Model Context Protocol
- **Jupyter notebooks**: Read and edit notebook cells

## How to approach tasks

1. **Understand before acting**: Read relevant files before making changes
2. **Minimal changes**: Only modify what's needed. Don't refactor unrequested code.
3. **Verify**: Check your work with tests or by reading the result
4. **Communicate blockers**: If stuck, ask the user rather than guessing
"#;

const TOOL_USE_GUIDELINES: &str = r#"
## Tool use guidelines

- Use dedicated tools (Read, Edit, Glob, Grep) instead of bash equivalents
- For searches, prefer Grep over `grep`; prefer Glob over `find`
- Parallelize independent tool calls in a single response
- For file edits: always read the file first, then make targeted edits
- Bash commands timeout after 2 minutes; use background mode for long operations
"#;

const ACTIONS_SECTION: &str = r#"
## Executing actions with care

Carefully consider the reversibility and blast radius of actions. For actions
that are hard to reverse, affect shared systems, or could be risky or
destructive, check with the user before proceeding. Authorization stands for
the scope specified, not beyond. Match the scope of your actions to what was
actually requested.
"#;

const SAFETY_GUIDELINES: &str = r#"
## Safety guidelines

- Never delete files without explicit user confirmation
- Don't modify protected files (.gitconfig, .bashrc, .zshrc)
- Be careful with destructive operations (rm -rf, DROP TABLE, etc.)
- Don't commit secrets, credentials, or API keys
- For ambiguous destructive actions, ask before proceeding
"#;

const SECURITY_SECTION: &str = r#"
## Security

You are authorized to assist with security research, CTF challenges, penetration testing
with explicit authorization, defensive security, and educational security content. Do not
assist with creating malware, unauthorized access, denial-of-service attacks, or any
destructive security techniques without clear legitimate purpose.
"#;

const COORDINATOR_SECTION: &str = r#"
## Coordinator Mode

You are operating as an orchestrator. Spawn parallel worker agents using the Agent tool.
Each worker prompt must be fully self-contained. Synthesize findings before delegating
follow-up work. Use TaskCreate/TaskUpdate to track parallel work.
"#;

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn default_opts() -> SystemPromptOptions {
        SystemPromptOptions::default()
    }

    #[test]
    fn test_default_prompt_contains_boundary() {
        let prompt = build_system_prompt(&default_opts());
        assert!(prompt.contains(SYSTEM_PROMPT_DYNAMIC_BOUNDARY));
    }

    #[test]
    fn test_default_prompt_contains_attribution() {
        let prompt = build_system_prompt(&default_opts());
        assert!(prompt.contains("Cersei SDK"));
    }

    #[test]
    fn test_replace_system_prompt() {
        let opts = SystemPromptOptions {
            custom_system_prompt: Some("Custom only.".to_string()),
            replace_system_prompt: true,
            ..Default::default()
        };
        let prompt = build_system_prompt(&opts);
        assert!(prompt.starts_with("Custom only."));
        assert!(!prompt.contains("Capabilities"));
        assert!(prompt.contains(SYSTEM_PROMPT_DYNAMIC_BOUNDARY));
    }

    #[test]
    fn test_working_directory_in_dynamic_section() {
        let opts = SystemPromptOptions {
            working_directory: Some("/home/user/project".to_string()),
            ..Default::default()
        };
        let prompt = build_system_prompt(&opts);
        let boundary_pos = prompt.find(SYSTEM_PROMPT_DYNAMIC_BOUNDARY).unwrap();
        let cwd_pos = prompt.find("/home/user/project").unwrap();
        assert!(cwd_pos > boundary_pos);
    }

    #[test]
    fn test_memory_content_in_dynamic_section() {
        let opts = SystemPromptOptions {
            memory_content: "- [test.md](test.md) -- a test memory".to_string(),
            ..Default::default()
        };
        let prompt = build_system_prompt(&opts);
        let boundary_pos = prompt.find(SYSTEM_PROMPT_DYNAMIC_BOUNDARY).unwrap();
        let mem_pos = prompt.find("test.md").unwrap();
        assert!(mem_pos > boundary_pos);
    }

    #[test]
    fn test_output_style_concise() {
        let opts = SystemPromptOptions {
            output_style: OutputStyle::Concise,
            ..Default::default()
        };
        let prompt = build_system_prompt(&opts);
        assert!(prompt.contains("maximally concise"));
    }

    #[test]
    fn test_output_style_default_no_suffix() {
        let prompt = build_system_prompt(&default_opts());
        assert!(!prompt.contains("maximally concise"));
        assert!(!prompt.contains("This user is learning"));
    }

    #[test]
    fn test_coordinator_mode() {
        let opts = SystemPromptOptions {
            coordinator_mode: true,
            ..Default::default()
        };
        let prompt = build_system_prompt(&opts);
        assert!(prompt.contains("Coordinator Mode"));
        assert!(prompt.contains("orchestrator"));
    }

    #[test]
    fn test_output_style_from_str() {
        assert_eq!(OutputStyle::from_str("concise"), OutputStyle::Concise);
        assert_eq!(OutputStyle::from_str("FORMAL"), OutputStyle::Formal);
        assert_eq!(OutputStyle::from_str("unknown"), OutputStyle::Default);
    }

    #[test]
    fn test_sdk_prefix() {
        let prefix = SystemPromptPrefix::detect(true, false);
        assert_eq!(prefix, SystemPromptPrefix::Sdk);
    }

    #[test]
    fn test_sdk_preset_prefix() {
        let prefix = SystemPromptPrefix::detect(true, true);
        assert_eq!(prefix, SystemPromptPrefix::SdkPreset);
    }

    #[test]
    fn test_extra_sections() {
        let opts = SystemPromptOptions {
            extra_cached_sections: vec![("rules".into(), "no swearing".into())],
            extra_dynamic_sections: vec![("context".into(), "today is Monday".into())],
            ..Default::default()
        };
        let prompt = build_system_prompt(&opts);
        let boundary = prompt.find(SYSTEM_PROMPT_DYNAMIC_BOUNDARY).unwrap();
        let rules_pos = prompt.find("no swearing").unwrap();
        let context_pos = prompt.find("today is Monday").unwrap();
        assert!(rules_pos < boundary);
        assert!(context_pos > boundary);
    }

    #[test]
    fn test_clear_section_cache() {
        {
            let mut cache = section_cache().lock().unwrap();
            cache.insert("test".to_string(), Some("content".to_string()));
        }
        clear_system_prompt_sections();
        let cache = section_cache().lock().unwrap();
        assert!(cache.is_empty());
    }
}
