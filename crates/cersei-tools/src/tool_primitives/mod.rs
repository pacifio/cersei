//! Tool primitives — low-level building blocks for agent tools.
//!
//! These are the foundational async functions that the 34 built-in tools
//! are built on. Use them directly to build custom tools with fine-grained control.
//!
//! ```rust,ignore
//! use cersei_tools::tool_primitives::{fs, diff, process, http, search, git};
//!
//! // Read a file with line numbers
//! let content = fs::read_file(path, 0, 100).await?;
//!
//! // Produce a unified diff
//! let patch = diff::unified_diff(&old_text, &new_text, 3);
//!
//! // Execute a command
//! let output = process::exec("cargo test", Default::default()).await?;
//!
//! // Search files
//! let matches = search::grep("TODO", path, Default::default()).await?;
//!
//! // Fetch a URL
//! let html = http::fetch_html("https://example.com", 50_000, Default::default()).await?;
//!
//! // Check git status
//! let status = git::status(path).await?;
//! ```

pub mod bash_safety;
pub mod code_intel;
pub mod diff;
pub mod fs;
pub mod git;
pub mod http;
pub mod process;
pub mod search;
