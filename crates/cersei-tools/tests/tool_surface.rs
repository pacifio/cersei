//! P2 tool-surface guarantees: `ToolSearch` is registered and every
//! registered tool carries a substantive description.
//!
//! The description is the only per-tool guidance channel that reaches the
//! model for most tools (the system prompt names just a handful), so the
//! floor test keeps a new tool from shipping with a bare label. Whether any
//! given description wording helps a model call the tool correctly is not
//! measurable offline and is not claimed here.

use cersei_tools::permissions::AllowAll;
use cersei_tools::{CostTracker, Extensions, ToolContext};
use std::sync::Arc;

fn ctx() -> ToolContext {
    ToolContext {
        working_dir: std::env::temp_dir(),
        session_id: "tool-surface-test".into(),
        permissions: Arc::new(AllowAll),
        cost_tracker: Arc::new(CostTracker::new()),
        mcp_manager: None,
        extensions: Extensions::default(),
    }
}

#[tokio::test]
async fn tool_search_is_registered_and_indexes_the_registry() {
    let tools = cersei_tools::all();
    let search = tools
        .iter()
        .find(|t| t.name() == "ToolSearch")
        .expect("ToolSearch must be registered in cersei_tools::all()");

    let result = search
        .execute(serde_json::json!({ "query": "notebook" }), &ctx())
        .await;
    assert!(!result.is_error);
    assert!(
        result.content.contains("NotebookEdit"),
        "ToolSearch must index the real registry: {}",
        result.content
    );
}

#[test]
fn every_tool_has_a_substantive_description() {
    for tool in cersei_tools::all() {
        let desc = tool.description();
        assert!(
            desc.trim().len() >= 30,
            "{} ships a bare description ({} chars): {desc:?}",
            tool.name(),
            desc.len()
        );
    }
}
