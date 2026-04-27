//! Bench-local cersei tools that wrap dbt + DuckDB so the agent can answer
//! "what's in this project?" and "what's in this DB?" in one tool call
//! instead of 3-5 turns of bash + grep + duckdb.
//!
//! Both tools live in the bench crate (not `cersei-tools`) because:
//!   1. They're dbt-specific — not generally useful outside this benchmark.
//!   2. Building them here demonstrates the cersei-from-scratch story:
//!      domain-specific tools compose with the generic agent loop without
//!      modifying the SDK.

pub mod dbt_project_map;
pub mod duckdb_query;

pub use dbt_project_map::DbtProjectMapTool;
pub use duckdb_query::DuckDbQueryTool;
