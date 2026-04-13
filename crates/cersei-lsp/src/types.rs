//! LSP protocol types (subset used by the agent).

use serde::{Deserialize, Serialize};

/// Position in a text document (0-based line and character).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Position {
    pub line: u32,
    pub character: u32,
}

/// A range in a text document.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Range {
    pub start: Position,
    pub end: Position,
}

/// A location in a text document (URI + range).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Location {
    pub uri: String,
    pub range: Range,
}

/// Diagnostic severity levels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum DiagnosticSeverity {
    Error = 1,
    Warning = 2,
    Information = 3,
    Hint = 4,
}

impl DiagnosticSeverity {
    pub fn from_lsp(value: u64) -> Self {
        match value {
            1 => Self::Error,
            2 => Self::Warning,
            3 => Self::Information,
            _ => Self::Hint,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Error => "error",
            Self::Warning => "warning",
            Self::Information => "info",
            Self::Hint => "hint",
        }
    }

    pub fn badge(&self) -> &'static str {
        match self {
            Self::Error => "[E]",
            Self::Warning => "[W]",
            Self::Information => "[I]",
            Self::Hint => "[H]",
        }
    }
}

/// A parsed diagnostic from the LSP server.
#[derive(Debug, Clone)]
pub struct LspDiagnostic {
    pub file: String,
    pub line: u32,
    pub col: u32,
    pub severity: DiagnosticSeverity,
    pub message: String,
    pub source: Option<String>,
    pub code: Option<String>,
}

impl std::fmt::Display for LspDiagnostic {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} {}:{}:{}: {}",
            self.severity.badge(),
            self.file,
            self.line + 1,
            self.col + 1,
            self.message
        )
    }
}

/// A symbol in a document.
#[derive(Debug, Clone)]
pub struct SymbolInfo {
    pub name: String,
    pub kind: String,
    pub range: Range,
    pub children: Vec<SymbolInfo>,
}

impl SymbolInfo {
    /// Format as indented text for display.
    pub fn format(&self, indent: usize) -> String {
        let mut out = format!("{}{} ({})\n", "  ".repeat(indent), self.name, self.kind);
        for child in &self.children {
            out.push_str(&child.format(indent + 1));
        }
        out
    }
}

/// Map LSP symbol kind integer to human-readable name.
pub fn symbol_kind_name(kind: u64) -> &'static str {
    match kind {
        1 => "file",
        2 => "module",
        3 => "namespace",
        4 => "package",
        5 => "class",
        6 => "method",
        7 => "property",
        8 => "field",
        9 => "constructor",
        10 => "enum",
        11 => "interface",
        12 => "function",
        13 => "variable",
        14 => "constant",
        15 => "string",
        16 => "number",
        17 => "boolean",
        18 => "array",
        19 => "object",
        20 => "key",
        21 => "null",
        22 => "enum_member",
        23 => "struct",
        24 => "event",
        25 => "operator",
        26 => "type_parameter",
        _ => "unknown",
    }
}
