//! Modular system prompt assembly with conditional components.
//!
//! The system prompt is built from named components, each with an inclusion rule.
//! Static components go before `SYSTEM_PROMPT_DYNAMIC_BOUNDARY` (cacheable).
//! Dynamic components go after (recomputed each turn).

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

// ─── Dynamic boundary marker ────────────────────────────────────────────────

pub const SYSTEM_PROMPT_DYNAMIC_BOUNDARY: &str = "__SYSTEM_PROMPT_DYNAMIC_BOUNDARY__";

// ─── Section cache ──────────────────────────────────────────────────────────

fn section_cache() -> &'static Mutex<HashMap<String, Option<String>>> {
    static CACHE: OnceLock<Mutex<HashMap<String, Option<String>>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

pub fn clear_system_prompt_sections() {
    if let Ok(mut cache) = section_cache().lock() {
        cache.clear();
    }
}

// ─── Section type (kept for backward compat) ────────────────────────────────

#[derive(Debug, Clone)]
pub struct SystemPromptSection {
    pub tag: String,
    pub content: Option<String>,
    pub cache_break: bool,
}

impl SystemPromptSection {
    pub fn cached(tag: impl Into<String>, content: impl Into<String>) -> Self {
        Self { tag: tag.into(), content: Some(content.into()), cache_break: false }
    }
    pub fn uncached(tag: impl Into<String>, content: Option<String>) -> Self {
        Self { tag: tag.into(), content, cache_break: true }
    }
}

// ─── Output style ───────────────────────────────────────────────────────────

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
    pub fn prompt_suffix(self) -> Option<&'static str> {
        match self {
            Self::Explanatory => Some("When explaining code or concepts, be thorough and educational. Include reasoning, alternatives considered, and potential pitfalls. Err on the side of over-explaining."),
            Self::Learning => Some("This user is learning. Explain concepts as you implement them. Point out patterns, best practices, and why you made each decision. Use analogies when helpful."),
            Self::Concise => Some("Be maximally concise. Skip preamble, summaries, and filler. Lead with the answer. One sentence is better than three."),
            Self::Formal => Some("Maintain a formal, professional tone. Use precise technical language."),
            Self::Casual => Some("Use a casual, conversational tone."),
            Self::Default => None,
        }
    }

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

// ─── Prefix ─────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SystemPromptPrefix {
    Interactive,
    Sdk,
    SdkPreset,
    SubAgent,
}

impl SystemPromptPrefix {
    pub fn detect(is_non_interactive: bool, has_append_system_prompt: bool) -> Self {
        if is_non_interactive {
            if has_append_system_prompt { return Self::SdkPreset; }
            return Self::Sdk;
        }
        Self::Interactive
    }

    pub fn attribution_text(self) -> &'static str {
        match self {
            Self::Interactive => "You are a coding agent built with the Cersei SDK.",
            Self::SdkPreset => "You are a coding agent built with the Cersei SDK, running with custom instructions.",
            Self::Sdk => "You are an agent built on the Cersei SDK.",
            Self::SubAgent => "You are a specialized sub-agent.",
        }
    }
}

// ─── Git snapshot ───────────────────────────────────────────────────────────

/// Pre-computed git repository information for the system prompt.
#[derive(Debug, Clone, Default)]
pub struct GitSnapshot {
    pub branch: String,
    pub recent_commits: Vec<String>,
    pub status_lines: Vec<String>,
    pub user: Option<String>,
}

// ─── Build options ──────────────────────────────────────────────────────────

#[derive(Debug, Clone, Default)]
pub struct SystemPromptOptions {
    // ── Existing fields ──
    pub prefix: Option<SystemPromptPrefix>,
    pub is_non_interactive: bool,
    pub has_append_system_prompt: bool,
    pub output_style: OutputStyle,
    pub custom_output_style_prompt: Option<String>,
    pub working_directory: Option<String>,
    pub memory_content: String,
    pub custom_system_prompt: Option<String>,
    pub append_system_prompt: Option<String>,
    pub replace_system_prompt: bool,
    pub coordinator_mode: bool,
    pub extra_cached_sections: Vec<(String, String)>,
    pub extra_dynamic_sections: Vec<(String, String)>,

    // ── New fields for conditional components ──

    /// Tool names available in the agent's tool list (for conditional guidance).
    pub tools_available: Vec<String>,
    /// Whether a memory backend is configured.
    pub has_memory: bool,
    /// Whether auto-compact is enabled.
    pub has_auto_compact: bool,
    /// Pre-computed git repository snapshot.
    pub git_status: Option<GitSnapshot>,
    /// Per-MCP-server instructions: (server_name, instructions).
    pub mcp_instructions: Vec<(String, String)>,
    /// Preferred response language (e.g., "Japanese").
    pub language: Option<String>,
}

// ─── Main assembly ──────────────────────────────────────────────────────────

pub fn build_system_prompt(opts: &SystemPromptOptions) -> String {
    // Replace mode
    if opts.replace_system_prompt {
        if let Some(custom) = &opts.custom_system_prompt {
            return format!("{}\n\n{}", custom, SYSTEM_PROMPT_DYNAMIC_BOUNDARY);
        }
    }

    let prefix = opts.prefix.unwrap_or_else(|| {
        SystemPromptPrefix::detect(opts.is_non_interactive, opts.has_append_system_prompt)
    });

    let mut parts: Vec<String> = Vec::new();

    // ── CACHEABLE sections ──────────────────────────────────────────────

    // 1. Attribution
    parts.push(prefix.attribution_text().to_string());

    // 2. Core capabilities
    parts.push(CORE_CAPABILITIES.to_string());

    // 3. Tool use guidelines
    parts.push(TOOL_USE_GUIDELINES.to_string());

    // 4. Actions with care
    parts.push(ACTIONS_SECTION.to_string());

    // 5. Safety
    parts.push(SAFETY_GUIDELINES.to_string());

    // 6. Security
    parts.push(SECURITY_SECTION.to_string());

    // 7. Output efficiency
    parts.push(OUTPUT_EFFICIENCY.to_string());

    // 8. Summarize tool results
    parts.push(SUMMARIZE_TOOL_RESULTS.to_string());

    // 9. Output style
    if let Some(style_text) = opts
        .custom_output_style_prompt
        .as_deref()
        .filter(|s| !s.trim().is_empty())
        .or_else(|| opts.output_style.prompt_suffix())
    {
        parts.push(format!("\n## Output Style\n{}", style_text));
    }

    // 10. Coordinator mode
    if opts.coordinator_mode {
        parts.push(COORDINATOR_SECTION.to_string());
    }

    // 11. Session guidance: Agent tool
    if opts.tools_available.iter().any(|t| t == "Agent" || t == "TaskCreate") {
        parts.push(SESSION_AGENT_GUIDANCE.to_string());
    }

    // 12. Session guidance: Skills
    if opts.tools_available.iter().any(|t| t == "Skill") {
        parts.push(SESSION_SKILLS_GUIDANCE.to_string());
    }

    // 13. Session guidance: Memory
    if opts.has_memory {
        parts.push(SESSION_MEMORY_GUIDANCE.to_string());
    }

    // 14. Function result clearing warning
    if opts.has_auto_compact {
        parts.push(FUNCTION_RESULT_CLEARING.to_string());
    }

    // 15. Language preference
    if let Some(lang) = &opts.language {
        parts.push(format!(
            "\n## Language\nAlways respond in {lang}. Use {lang} for all explanations, comments, and communications. Technical terms and code identifiers should remain in their original form."
        ));
    }

    // 16. Custom system prompt
    if let Some(custom) = &opts.custom_system_prompt {
        parts.push(format!("\n<custom_instructions>\n{}\n</custom_instructions>", custom));
    }

    // 17. Extra cached sections
    for (tag, content) in &opts.extra_cached_sections {
        parts.push(format!("\n<{}>\n{}\n</{}>", tag, content, tag));
    }

    // ── BOUNDARY ────────────────────────────────────────────────────────
    parts.push(SYSTEM_PROMPT_DYNAMIC_BOUNDARY.to_string());

    // ── DYNAMIC sections ────────────────────────────────────────────────

    // 18. Working directory
    if let Some(cwd) = &opts.working_directory {
        parts.push(format!("\n<working_directory>{}</working_directory>", cwd));
    }

    // 19. Git status snapshot
    if let Some(git) = &opts.git_status {
        let mut git_section = format!("\n<git_status>\nBranch: {}", git.branch);
        if let Some(user) = &git.user {
            git_section.push_str(&format!("\nUser: {}", user));
        }
        if !git.status_lines.is_empty() {
            git_section.push_str("\nStatus:");
            for line in &git.status_lines {
                git_section.push_str(&format!("\n  {}", line));
            }
        }
        if !git.recent_commits.is_empty() {
            git_section.push_str("\nRecent commits:");
            for commit in &git.recent_commits {
                git_section.push_str(&format!("\n  {}", commit));
            }
        }
        git_section.push_str("\n</git_status>");
        parts.push(git_section);
    }

    // 20. Memory
    if !opts.memory_content.is_empty() {
        parts.push(format!("\n<memory>\n{}\n</memory>", opts.memory_content));
    }

    // 21. MCP server instructions
    if !opts.mcp_instructions.is_empty() {
        let mut mcp_section = String::from("\n<mcp_instructions>");
        for (name, instructions) in &opts.mcp_instructions {
            mcp_section.push_str(&format!("\n## {}\n{}", name, instructions));
        }
        mcp_section.push_str("\n</mcp_instructions>");
        parts.push(mcp_section);
    }

    // 22. Extra dynamic sections
    for (tag, content) in &opts.extra_dynamic_sections {
        parts.push(format!("\n<{}>\n{}\n</{}>", tag, content, tag));
    }

    // 23. Appended system prompt
    if let Some(append) = &opts.append_system_prompt {
        parts.push(format!("\n{}", append));
    }

    parts.join("\n")
}

// ─── Static sections ────────────────────────────────────────────────────────

const CORE_CAPABILITIES: &str = r#"
## Capabilities

You have access to powerful tools for software engineering tasks:
- **Read/Write files**: Read any file, write new files, edit existing files with precise diffs
- **Execute commands**: Run bash commands, PowerShell scripts, background processes
- **Search**: Glob patterns, regex grep, web search, file content search
- **LSP**: Language server queries for hover, go-to-definition, references, symbols, diagnostics
- **Web**: Fetch URLs, search the internet
- **Agents**: Spawn parallel sub-agents for complex multi-step work
- **Memory**: Persistent notes across sessions via the memory system
- **MCP servers**: Connect to external tools and APIs via Model Context Protocol
- **Jupyter notebooks**: Read and edit notebook cells

## Task Management

You have access to the TodoWrite tool to help you manage and plan tasks. Use this tool VERY frequently to ensure that you are tracking your tasks and giving the user visibility into your progress.
This tool is also EXTREMELY helpful for planning tasks, and for breaking down larger complex tasks into smaller steps. If you do not use this tool when planning, you may forget to do important tasks - and that is unacceptable.

It is critical that you mark todos as completed as soon as you are done with a task. Do not batch up multiple tasks before marking them as completed.

IMPORTANT: Always use the TodoWrite tool to plan and track tasks throughout the conversation.

## How to approach tasks

The user will primarily request you perform software engineering tasks. For these tasks:
- NEVER propose changes to code you haven't read. Read first, then modify.
- Use the TodoWrite tool to plan the task if required.
- Be careful not to introduce security vulnerabilities.
- Avoid over-engineering. Only make changes that are directly requested or clearly necessary.
- Don't add features, refactor code, or make improvements beyond what was asked.
- ALWAYS verify information about the codebase using tools before answering. Never rely solely on general knowledge or assumptions about how code works.

## Tool usage policy

- When doing file search or research, prefer using Bash (with grep, find) or Grep tool for targeted searches.
- When you need information you don't have, use WebSearch to find it. Do not guess APIs, node types, or library details — search for the current documentation.
- You can call multiple tools in a single response. If you intend to call multiple tools and there are no dependencies between them, make all independent tool calls in parallel. Maximize use of parallel tool calls where possible to increase efficiency.
- If the user specifies running tools in parallel, you MUST send a single response with multiple tool calls.
- Use specialized tools instead of bash when possible: Read for reading files, Edit for editing, Glob for finding files, Grep for searching content.
"#;

const TOOL_USE_GUIDELINES: &str = r#"
## Tool use guidelines

- Use dedicated tools (Read, Edit, Glob, Grep, LSP) instead of bash equivalents
- For searches, prefer Grep over `grep`; prefer Glob over `find`
- For file edits: always read the file first, then make targeted edits
- Bash commands timeout after 2 minutes; use background mode for long operations
- Use Glob for targeted patterns (`src/**/*.rs`), never glob `**/*` at root
- Use LSP tool for semantic understanding: symbols, definitions, references, diagnostics
- Write down key findings in your response — tool results may be cleared from context later
- Old tool results are automatically cleared to free space. Summarize important information.
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

const OUTPUT_EFFICIENCY: &str = r#"
## Output efficiency

Be direct and informative. Lead with the answer, not the reasoning.
- For analysis/explanation: Be thorough and structured. Use tables, lists, and sections.
- For code changes: Be concise. Show what changed and why.
- For status updates: One sentence is enough.
- Never ask "would you like me to investigate more?" — just investigate.
- Never stop at surface-level answers when deeper investigation would give better results.
- Use multiple tool calls in a single response to gather evidence in parallel.
"#;

const SUMMARIZE_TOOL_RESULTS: &str = r#"
## Tool results

When working with tool results, write down any important information you might need later
in your response, as the original tool result may be cleared from context later.
"#;

const COORDINATOR_SECTION: &str = r#"
## Coordinator Mode

You are operating as an orchestrator. Spawn parallel worker agents using the Agent tool.
Each worker prompt must be fully self-contained. Synthesize findings before delegating
follow-up work. Use TaskCreate/TaskUpdate to track parallel work.
"#;

// ─── Conditional sections ───────────────────────────────────────────────────

const SESSION_AGENT_GUIDANCE: &str = r#"
## Sub-agents

Use the Agent tool for complex multi-step tasks that benefit from parallel work or
deep research. Each sub-agent runs independently with its own context window.
- Launch multiple agents in parallel when tasks are independent
- Provide each agent with a complete, self-contained prompt
- The agent's output is not visible to the user — summarize results yourself
- Use TaskCreate/TaskUpdate to track background work
"#;

const SESSION_SKILLS_GUIDANCE: &str = r#"
## Skills

/<skill-name> (e.g., /commit) invokes a skill — a reusable prompt template.
Skills are loaded from .claude/commands/*.md, .claude/skills/*/SKILL.md, or bundled.
Use the Skill tool to execute them. Only use skills that are listed as available.
"#;

const SESSION_MEMORY_GUIDANCE: &str = r#"
## Persistent memory

You have access to persistent memory across sessions. Memory files survive across
conversations and are injected into your context automatically.
- Store facts about the user's preferences, project decisions, and recurring patterns
- Before recommending from memory, verify that files and functions still exist
- Memory records can become stale — if a recalled memory conflicts with current code, trust what you observe now
"#;

const FUNCTION_RESULT_CLEARING: &str = r#"
## Context management

Old tool results will be automatically summarized to free context space when the
conversation grows long. The most recent results are always kept. Write down any
important information from tool results in your response text — the originals may
be cleared in future turns.
"#;

// ─── Tests ──────────────────────────────────────────────────────────────────

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

    // ── New component tests ──

    #[test]
    fn test_agent_guidance_included_when_tools_available() {
        let opts = SystemPromptOptions {
            tools_available: vec!["Agent".into(), "Read".into()],
            ..Default::default()
        };
        let prompt = build_system_prompt(&opts);
        assert!(prompt.contains("Sub-agents"));
    }

    #[test]
    fn test_agent_guidance_excluded_when_no_agent_tool() {
        let opts = SystemPromptOptions {
            tools_available: vec!["Read".into(), "Write".into()],
            ..Default::default()
        };
        let prompt = build_system_prompt(&opts);
        assert!(!prompt.contains("Sub-agents"));
    }

    #[test]
    fn test_skills_guidance_conditional() {
        let with = SystemPromptOptions {
            tools_available: vec!["Skill".into()],
            ..Default::default()
        };
        assert!(build_system_prompt(&with).contains("/<skill-name>"));

        let without = SystemPromptOptions::default();
        assert!(!build_system_prompt(&without).contains("/<skill-name>"));
    }

    #[test]
    fn test_memory_guidance_conditional() {
        let with = SystemPromptOptions {
            has_memory: true,
            ..Default::default()
        };
        assert!(build_system_prompt(&with).contains("Persistent memory"));

        let without = SystemPromptOptions::default();
        assert!(!build_system_prompt(&without).contains("Persistent memory"));
    }

    #[test]
    fn test_auto_compact_warning() {
        let with = SystemPromptOptions {
            has_auto_compact: true,
            ..Default::default()
        };
        assert!(build_system_prompt(&with).contains("Context management"));

        let without = SystemPromptOptions::default();
        assert!(!build_system_prompt(&without).contains("Context management"));
    }

    #[test]
    fn test_git_snapshot() {
        let opts = SystemPromptOptions {
            git_status: Some(GitSnapshot {
                branch: "main".into(),
                recent_commits: vec!["abc1234 Fix bug".into()],
                status_lines: vec!["M src/main.rs".into()],
                user: Some("Dev".into()),
            }),
            ..Default::default()
        };
        let prompt = build_system_prompt(&opts);
        let boundary = prompt.find(SYSTEM_PROMPT_DYNAMIC_BOUNDARY).unwrap();
        let git_pos = prompt.find("Branch: main").unwrap();
        assert!(git_pos > boundary); // dynamic section
        assert!(prompt.contains("abc1234 Fix bug"));
        assert!(prompt.contains("M src/main.rs"));
        assert!(prompt.contains("User: Dev"));
    }

    #[test]
    fn test_mcp_instructions() {
        let opts = SystemPromptOptions {
            mcp_instructions: vec![("db-server".into(), "Use LIMIT clauses".into())],
            ..Default::default()
        };
        let prompt = build_system_prompt(&opts);
        assert!(prompt.contains("db-server"));
        assert!(prompt.contains("Use LIMIT clauses"));
    }

    #[test]
    fn test_language_preference() {
        let opts = SystemPromptOptions {
            language: Some("Japanese".into()),
            ..Default::default()
        };
        let prompt = build_system_prompt(&opts);
        assert!(prompt.contains("Always respond in Japanese"));
    }

    #[test]
    fn test_output_efficiency_always_included() {
        let prompt = build_system_prompt(&default_opts());
        assert!(prompt.contains("Output efficiency"));
    }

    #[test]
    fn test_summarize_tool_results_always_included() {
        let prompt = build_system_prompt(&default_opts());
        assert!(prompt.contains("Tool results"));
    }
}
