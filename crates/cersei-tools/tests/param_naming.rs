//! F-A5: the `file_path` / `path` naming split is a convention, not drift.
//!
//! `file_path` names a specific file (Read/Write/Edit/MultiEdit/NotebookEdit);
//! `path` names a search or worktree *scope* that may be a directory
//! (Glob/Grep/CodeSearch/EnterWorktree/ExitWorktree). This mirrors the tool
//! surface models are most heavily trained on, so unifying the names was
//! rejected — as was re-adding alias coercion, which F-10 removed after alias
//! leniency on `Edit` caused the silent search-widening bug (see the
//! unknown-parameter policy note in `lib.rs`). This test makes the convention
//! load-bearing: a future tool that mixes the two names, or renames one of
//! these parameters, fails here.

use serde_json::Value;

fn properties(schema: &Value) -> Vec<String> {
    schema["properties"]
        .as_object()
        .map(|m| m.keys().cloned().collect())
        .unwrap_or_default()
}

#[test]
fn file_tools_declare_file_path_and_scope_tools_declare_path() {
    const FILE_TOOLS: &[&str] = &["Read", "Write", "Edit", "MultiEdit", "NotebookEdit"];
    const SCOPE_TOOLS: &[&str] = &[
        "Glob",
        "Grep",
        "CodeSearch",
        "EnterWorktree",
        "ExitWorktree",
    ];

    let tools = cersei_tools::all();
    let mut seen_file = 0;
    let mut seen_scope = 0;

    for tool in &tools {
        let props = properties(&tool.input_schema());
        let name = tool.name();

        assert!(
            !(props.iter().any(|p| p == "file_path") && props.iter().any(|p| p == "path")),
            "{name} declares BOTH file_path and path — the convention assigns exactly one"
        );

        if FILE_TOOLS.contains(&name) {
            seen_file += 1;
            assert!(
                props.iter().any(|p| p == "file_path"),
                "{name} must take `file_path` (a specific file): {props:?}"
            );
        }
        if SCOPE_TOOLS.contains(&name) {
            seen_scope += 1;
            assert!(
                props.iter().any(|p| p == "path"),
                "{name} must take `path` (a file-or-directory scope): {props:?}"
            );
            assert!(
                !props.iter().any(|p| p == "file_path"),
                "{name} is a scope tool; `file_path` would misname a directory: {props:?}"
            );
        }
    }

    assert_eq!(seen_file, FILE_TOOLS.len(), "a file tool left the registry");
    assert_eq!(
        seen_scope,
        SCOPE_TOOLS.len(),
        "a scope tool left the registry"
    );
}
