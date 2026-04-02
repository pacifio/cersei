//! Context analyzer: token breakdown by category with compaction recommendations.

use cersei_types::*;

// ─── Context categories ─────────────────────────────────────────────────────

/// Token breakdown categories.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContextCategory {
    SystemPrompt,
    ToolDefinitions,
    ConversationHistory,
    ToolResults,
    Attachments,
    Unknown,
}

impl ContextCategory {
    pub fn label(&self) -> &'static str {
        match self {
            Self::SystemPrompt => "System Prompt",
            Self::ToolDefinitions => "Tool Definitions",
            Self::ConversationHistory => "Conversation",
            Self::ToolResults => "Tool Results",
            Self::Attachments => "Attachments",
            Self::Unknown => "Other",
        }
    }
}

// ─── Compaction strategy ─────────────────────────────────────────────────────

/// Recommendation for what to compact.
#[derive(Debug, Clone)]
pub enum CompactionStrategy {
    /// Compact all messages.
    FullCompact { expected_reduction_pct: f64 },
    /// Compact only the oldest N messages.
    PartialCompact {
        messages_to_compact: usize,
        expected_reduction_pct: f64,
    },
    /// Collapse repeated file reads to save space.
    CollapseReads { expected_reduction_pct: f64 },
    /// No compaction needed.
    None,
}

// ─── Context analysis ────────────────────────────────────────────────────────

/// Token count breakdown of the current context.
#[derive(Debug, Clone, Default)]
pub struct ContextAnalysis {
    pub system_prompt_tokens: u64,
    pub tool_definitions_tokens: u64,
    pub conversation_history_tokens: u64,
    pub tool_results_tokens: u64,
    pub attachments_tokens: u64,
    pub total_tokens: u64,
    /// 0.0 (not compressible) to 1.0 (highly compressible).
    pub compressibility: f64,
}

impl ContextAnalysis {
    /// Percentage of context used by a category.
    pub fn category_pct(&self, cat: ContextCategory) -> f64 {
        if self.total_tokens == 0 {
            return 0.0;
        }
        let tokens = match cat {
            ContextCategory::SystemPrompt => self.system_prompt_tokens,
            ContextCategory::ToolDefinitions => self.tool_definitions_tokens,
            ContextCategory::ConversationHistory => self.conversation_history_tokens,
            ContextCategory::ToolResults => self.tool_results_tokens,
            ContextCategory::Attachments => self.attachments_tokens,
            ContextCategory::Unknown => 0,
        };
        tokens as f64 / self.total_tokens as f64
    }
}

// ─── Analysis ────────────────────────────────────────────────────────────────

fn estimate_tokens(text: &str) -> u64 {
    (text.len() as u64) / 4
}

/// Analyze context window usage by category.
pub fn analyze_context(
    system_prompt: Option<&str>,
    tool_defs_json: Option<&str>,
    messages: &[Message],
) -> ContextAnalysis {
    let system_prompt_tokens = system_prompt.map(estimate_tokens).unwrap_or(0);
    let tool_definitions_tokens = tool_defs_json.map(estimate_tokens).unwrap_or(0);

    let mut conversation_tokens: u64 = 0;
    let mut tool_result_tokens: u64 = 0;

    for msg in messages {
        match &msg.content {
            MessageContent::Text(t) => {
                conversation_tokens += estimate_tokens(t);
            }
            MessageContent::Blocks(blocks) => {
                for block in blocks {
                    match block {
                        ContentBlock::ToolResult { content, .. } => {
                            let text = match content {
                                ToolResultContent::Text(t) => t.len(),
                                ToolResultContent::Blocks(b) => {
                                    b.iter()
                                        .map(|bb| {
                                            if let ContentBlock::Text { text } = bb {
                                                text.len()
                                            } else {
                                                50
                                            }
                                        })
                                        .sum()
                                }
                            };
                            tool_result_tokens += (text as u64) / 4;
                        }
                        ContentBlock::Text { text } => {
                            conversation_tokens += estimate_tokens(text);
                        }
                        ContentBlock::ToolUse { input, .. } => {
                            conversation_tokens +=
                                estimate_tokens(&serde_json::to_string(input).unwrap_or_default());
                        }
                        ContentBlock::Thinking { thinking, .. } => {
                            conversation_tokens += estimate_tokens(thinking);
                        }
                        _ => {
                            conversation_tokens += 10; // small overhead for other types
                        }
                    }
                }
            }
        }
    }

    let total = system_prompt_tokens
        + tool_definitions_tokens
        + conversation_tokens
        + tool_result_tokens;

    // Compressibility: tool results are ~90% compressible, conversation is ~50%
    let compressibility = if total > 0 {
        (tool_result_tokens as f64 * 0.9 + conversation_tokens as f64 * 0.5) / total as f64
    } else {
        0.0
    };

    ContextAnalysis {
        system_prompt_tokens,
        tool_definitions_tokens,
        conversation_history_tokens: conversation_tokens,
        tool_results_tokens: tool_result_tokens,
        attachments_tokens: 0,
        total_tokens: total,
        compressibility,
    }
}

/// Suggest a compaction strategy based on the analysis.
pub fn suggest_compaction(analysis: &ContextAnalysis, context_limit: u64) -> CompactionStrategy {
    if context_limit == 0 || analysis.total_tokens == 0 {
        return CompactionStrategy::None;
    }

    let usage_pct = analysis.total_tokens as f64 / context_limit as f64;

    if usage_pct < 0.75 {
        return CompactionStrategy::None;
    }

    // If tool results dominate (>40%) and we're not critical, collapse reads first
    if analysis.category_pct(ContextCategory::ToolResults) > 0.4 && usage_pct < 0.90 {
        return CompactionStrategy::CollapseReads {
            expected_reduction_pct: analysis.category_pct(ContextCategory::ToolResults) * 0.5,
        };
    }

    // Critical: full compact
    if usage_pct >= 0.90 {
        return CompactionStrategy::FullCompact {
            expected_reduction_pct: analysis.compressibility * 0.7,
        };
    }

    // Moderate: partial compact (oldest half)
    CompactionStrategy::PartialCompact {
        messages_to_compact: 0, // caller determines based on message count
        expected_reduction_pct: analysis.compressibility * 0.5,
    }
}

/// Format a human-readable context visualization.
pub fn format_ctx_viz(analysis: &ContextAnalysis, context_limit: u64) -> String {
    let usage_pct = if context_limit > 0 {
        (analysis.total_tokens as f64 / context_limit as f64) * 100.0
    } else {
        0.0
    };

    let bar_width = 40;
    let filled = ((usage_pct / 100.0) * bar_width as f64).min(bar_width as f64) as usize;
    let bar: String = format!(
        "[{}{}]",
        "#".repeat(filled),
        ".".repeat(bar_width - filled)
    );

    let categories = [
        (ContextCategory::SystemPrompt, analysis.system_prompt_tokens),
        (ContextCategory::ToolDefinitions, analysis.tool_definitions_tokens),
        (ContextCategory::ConversationHistory, analysis.conversation_history_tokens),
        (ContextCategory::ToolResults, analysis.tool_results_tokens),
    ];

    let mut lines = vec![
        format!("Context: {} {:.1}% of {} tokens", bar, usage_pct, context_limit),
        String::new(),
    ];

    for (cat, tokens) in &categories {
        if *tokens > 0 {
            let pct = (*tokens as f64 / analysis.total_tokens.max(1) as f64) * 100.0;
            lines.push(format!("  {:<20} {:>8} tokens ({:.1}%)", cat.label(), tokens, pct));
        }
    }

    lines.push(format!("\n  Compressibility: {:.0}%", analysis.compressibility * 100.0));
    lines.join("\n")
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_messages() -> Vec<Message> {
        vec![
            Message::user("Read the file src/main.rs"),
            Message::assistant_blocks(vec![
                ContentBlock::Text { text: "Here's the file:".into() },
                ContentBlock::ToolUse {
                    id: "t1".into(),
                    name: "Read".into(),
                    input: serde_json::json!({"file_path": "src/main.rs"}),
                },
            ]),
            Message::user_blocks(vec![ContentBlock::ToolResult {
                tool_use_id: "t1".into(),
                content: ToolResultContent::Text("fn main() { println!(\"hello\"); }".repeat(100)),
                is_error: Some(false),
            }]),
            Message::assistant("The main function prints hello."),
        ]
    }

    #[test]
    fn test_analyze_context() {
        let messages = sample_messages();
        let analysis = analyze_context(
            Some("You are a helpful assistant."),
            Some(r#"[{"name":"Read","description":"Read files"}]"#),
            &messages,
        );

        assert!(analysis.system_prompt_tokens > 0);
        assert!(analysis.tool_definitions_tokens > 0);
        assert!(analysis.conversation_history_tokens > 0);
        assert!(analysis.tool_results_tokens > 0);
        assert!(analysis.total_tokens > 0);
        assert!(analysis.compressibility > 0.0);
        assert!(analysis.compressibility <= 1.0);
    }

    #[test]
    fn test_category_pct() {
        let analysis = ContextAnalysis {
            system_prompt_tokens: 100,
            tool_definitions_tokens: 200,
            conversation_history_tokens: 300,
            tool_results_tokens: 400,
            attachments_tokens: 0,
            total_tokens: 1000,
            compressibility: 0.5,
        };

        assert!((analysis.category_pct(ContextCategory::SystemPrompt) - 0.1).abs() < 0.01);
        assert!((analysis.category_pct(ContextCategory::ToolResults) - 0.4).abs() < 0.01);
    }

    #[test]
    fn test_suggest_none_under_75() {
        let analysis = ContextAnalysis {
            total_tokens: 50_000,
            ..Default::default()
        };
        assert!(matches!(
            suggest_compaction(&analysis, 200_000),
            CompactionStrategy::None
        ));
    }

    #[test]
    fn test_suggest_full_over_90() {
        let analysis = ContextAnalysis {
            total_tokens: 185_000,
            conversation_history_tokens: 100_000,
            tool_results_tokens: 80_000,
            compressibility: 0.7,
            ..Default::default()
        };
        assert!(matches!(
            suggest_compaction(&analysis, 200_000),
            CompactionStrategy::FullCompact { .. }
        ));
    }

    #[test]
    fn test_suggest_collapse_reads() {
        let analysis = ContextAnalysis {
            total_tokens: 170_000, // 85%
            tool_results_tokens: 90_000, // >40% of total
            conversation_history_tokens: 70_000,
            compressibility: 0.6,
            ..Default::default()
        };
        assert!(matches!(
            suggest_compaction(&analysis, 200_000),
            CompactionStrategy::CollapseReads { .. }
        ));
    }

    #[test]
    fn test_format_ctx_viz() {
        let analysis = ContextAnalysis {
            system_prompt_tokens: 5000,
            tool_definitions_tokens: 3000,
            conversation_history_tokens: 20000,
            tool_results_tokens: 10000,
            attachments_tokens: 0,
            total_tokens: 38000,
            compressibility: 0.5,
        };
        let viz = format_ctx_viz(&analysis, 200_000);
        assert!(viz.contains("Context:"));
        assert!(viz.contains("System Prompt"));
        assert!(viz.contains("Compressibility"));
    }

    #[test]
    fn test_empty_analysis() {
        let analysis = analyze_context(None, None, &[]);
        assert_eq!(analysis.total_tokens, 0);
        assert_eq!(analysis.compressibility, 0.0);
    }
}
