//! cersei-compression — Structural and command-aware compression for tool
//! outputs in the Cersei SDK.
//!
//! Credits: this crate is a port / adaptation of rtk (Rust Token Killer) by
//! Patrick Szymkowiak, <https://github.com/rtk-ai/rtk>, MIT licensed. See
//! LICENSE for full attribution and the per-module mapping table.

pub mod ansi;
pub mod code;
pub mod dispatch;
pub mod level;
pub mod toml_rules;
pub mod truncate;

pub use dispatch::compress_tool_output;
pub use level::CompressionLevel;
