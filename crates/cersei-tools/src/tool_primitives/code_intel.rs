//! Code intelligence via tree-sitter: extract imports, symbols, and build dependency graphs.
//!
//! Supports: Rust, TypeScript/JavaScript, Python, Go.
//! Used to intelligently select which files to read for codebase analysis.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use tree_sitter::{Parser, Query, QueryCursor};

/// A file's extracted metadata.
#[derive(Debug, Clone, Default)]
pub struct FileIntel {
    pub path: PathBuf,
    pub language: Language,
    pub imports: Vec<String>,
    pub symbols: Vec<Symbol>,
}

/// A symbol extracted from source code.
#[derive(Debug, Clone)]
pub struct Symbol {
    pub name: String,
    pub kind: SymbolKind,
    pub line: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SymbolKind {
    Function,
    Struct,
    Class,
    Interface,
    Enum,
    Module,
    Type,
    Constant,
}

impl SymbolKind {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Function => "fn",
            Self::Struct => "struct",
            Self::Class => "class",
            Self::Interface => "interface",
            Self::Enum => "enum",
            Self::Module => "mod",
            Self::Type => "type",
            Self::Constant => "const",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Language {
    Rust,
    TypeScript,
    JavaScript,
    Python,
    Go,
    #[default]
    Unknown,
}

impl Language {
    pub fn from_extension(ext: &str) -> Self {
        match ext {
            "rs" => Self::Rust,
            "ts" | "tsx" => Self::TypeScript,
            "js" | "jsx" | "mjs" | "cjs" => Self::JavaScript,
            "py" | "pyi" => Self::Python,
            "go" => Self::Go,
            _ => Self::Unknown,
        }
    }
}

/// Extract imports and symbols from a source file.
pub fn analyze_file(path: &Path, source: &str) -> Option<FileIntel> {
    let ext = path.extension()?.to_str()?;
    let lang = Language::from_extension(ext);
    if lang == Language::Unknown {
        return None;
    }

    let mut parser = Parser::new();
    let ts_lang = match lang {
        Language::Rust => tree_sitter_rust::LANGUAGE.into(),
        Language::TypeScript | Language::JavaScript => {
            tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into()
        }
        Language::Python => tree_sitter_python::LANGUAGE.into(),
        Language::Go => tree_sitter_go::LANGUAGE.into(),
        Language::Unknown => return None,
    };
    parser.set_language(&ts_lang).ok()?;
    let tree = parser.parse(source, None)?;
    let root = tree.root_node();
    let bytes = source.as_bytes();

    let mut imports = Vec::new();
    let mut symbols = Vec::new();

    // Walk AST and extract imports + symbols
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        let kind = node.kind();

        match lang {
            Language::Rust => match kind {
                "use_declaration" => {
                    if let Ok(text) = node.utf8_text(bytes) {
                        imports.push(text.trim().to_string());
                    }
                }
                "function_item" => {
                    if let Some(name) = node.child_by_field_name("name") {
                        if let Ok(n) = name.utf8_text(bytes) {
                            symbols.push(Symbol {
                                name: n.to_string(),
                                kind: SymbolKind::Function,
                                line: node.start_position().row + 1,
                            });
                        }
                    }
                }
                "struct_item" => {
                    if let Some(name) = node.child_by_field_name("name") {
                        if let Ok(n) = name.utf8_text(bytes) {
                            symbols.push(Symbol {
                                name: n.to_string(),
                                kind: SymbolKind::Struct,
                                line: node.start_position().row + 1,
                            });
                        }
                    }
                }
                "enum_item" => {
                    if let Some(name) = node.child_by_field_name("name") {
                        if let Ok(n) = name.utf8_text(bytes) {
                            symbols.push(Symbol {
                                name: n.to_string(),
                                kind: SymbolKind::Enum,
                                line: node.start_position().row + 1,
                            });
                        }
                    }
                }
                "mod_item" => {
                    if let Some(name) = node.child_by_field_name("name") {
                        if let Ok(n) = name.utf8_text(bytes) {
                            symbols.push(Symbol {
                                name: n.to_string(),
                                kind: SymbolKind::Module,
                                line: node.start_position().row + 1,
                            });
                        }
                    }
                }
                "trait_item" => {
                    if let Some(name) = node.child_by_field_name("name") {
                        if let Ok(n) = name.utf8_text(bytes) {
                            symbols.push(Symbol {
                                name: n.to_string(),
                                kind: SymbolKind::Interface,
                                line: node.start_position().row + 1,
                            });
                        }
                    }
                }
                "type_item" => {
                    if let Some(name) = node.child_by_field_name("name") {
                        if let Ok(n) = name.utf8_text(bytes) {
                            symbols.push(Symbol {
                                name: n.to_string(),
                                kind: SymbolKind::Type,
                                line: node.start_position().row + 1,
                            });
                        }
                    }
                }
                _ => {}
            },
            Language::TypeScript | Language::JavaScript => match kind {
                "import_statement" => {
                    if let Some(source_node) = node.child_by_field_name("source") {
                        if let Ok(text) = source_node.utf8_text(bytes) {
                            imports.push(text.trim_matches(|c| c == '"' || c == '\'').to_string());
                        }
                    }
                }
                "function_declaration" => {
                    if let Some(name) = node.child_by_field_name("name") {
                        if let Ok(n) = name.utf8_text(bytes) {
                            symbols.push(Symbol {
                                name: n.to_string(),
                                kind: SymbolKind::Function,
                                line: node.start_position().row + 1,
                            });
                        }
                    }
                }
                "class_declaration" => {
                    if let Some(name) = node.child_by_field_name("name") {
                        if let Ok(n) = name.utf8_text(bytes) {
                            symbols.push(Symbol {
                                name: n.to_string(),
                                kind: SymbolKind::Class,
                                line: node.start_position().row + 1,
                            });
                        }
                    }
                }
                "interface_declaration" => {
                    if let Some(name) = node.child_by_field_name("name") {
                        if let Ok(n) = name.utf8_text(bytes) {
                            symbols.push(Symbol {
                                name: n.to_string(),
                                kind: SymbolKind::Interface,
                                line: node.start_position().row + 1,
                            });
                        }
                    }
                }
                "type_alias_declaration" => {
                    if let Some(name) = node.child_by_field_name("name") {
                        if let Ok(n) = name.utf8_text(bytes) {
                            symbols.push(Symbol {
                                name: n.to_string(),
                                kind: SymbolKind::Type,
                                line: node.start_position().row + 1,
                            });
                        }
                    }
                }
                "enum_declaration" => {
                    if let Some(name) = node.child_by_field_name("name") {
                        if let Ok(n) = name.utf8_text(bytes) {
                            symbols.push(Symbol {
                                name: n.to_string(),
                                kind: SymbolKind::Enum,
                                line: node.start_position().row + 1,
                            });
                        }
                    }
                }
                "export_statement" => {
                    // Also extract exported declarations
                    if let Some(decl) = node.child_by_field_name("declaration") {
                        stack.push(decl);
                    }
                }
                _ => {}
            },
            Language::Python => match kind {
                "import_statement" | "import_from_statement" => {
                    if let Ok(text) = node.utf8_text(bytes) {
                        imports.push(text.trim().to_string());
                    }
                }
                "function_definition" => {
                    if let Some(name) = node.child_by_field_name("name") {
                        if let Ok(n) = name.utf8_text(bytes) {
                            symbols.push(Symbol {
                                name: n.to_string(),
                                kind: SymbolKind::Function,
                                line: node.start_position().row + 1,
                            });
                        }
                    }
                }
                "class_definition" => {
                    if let Some(name) = node.child_by_field_name("name") {
                        if let Ok(n) = name.utf8_text(bytes) {
                            symbols.push(Symbol {
                                name: n.to_string(),
                                kind: SymbolKind::Class,
                                line: node.start_position().row + 1,
                            });
                        }
                    }
                }
                _ => {}
            },
            Language::Go => match kind {
                "import_declaration" => {
                    if let Ok(text) = node.utf8_text(bytes) {
                        imports.push(text.trim().to_string());
                    }
                }
                "function_declaration" => {
                    if let Some(name) = node.child_by_field_name("name") {
                        if let Ok(n) = name.utf8_text(bytes) {
                            symbols.push(Symbol {
                                name: n.to_string(),
                                kind: SymbolKind::Function,
                                line: node.start_position().row + 1,
                            });
                        }
                    }
                }
                "method_declaration" => {
                    if let Some(name) = node.child_by_field_name("name") {
                        if let Ok(n) = name.utf8_text(bytes) {
                            symbols.push(Symbol {
                                name: n.to_string(),
                                kind: SymbolKind::Function,
                                line: node.start_position().row + 1,
                            });
                        }
                    }
                }
                "type_declaration" => {
                    // Type declarations contain type_spec children
                }
                "type_spec" => {
                    if let Some(name) = node.child_by_field_name("name") {
                        if let Ok(n) = name.utf8_text(bytes) {
                            let sk = if node
                                .child_by_field_name("type")
                                .map(|t| t.kind() == "struct_type")
                                .unwrap_or(false)
                            {
                                SymbolKind::Struct
                            } else if node
                                .child_by_field_name("type")
                                .map(|t| t.kind() == "interface_type")
                                .unwrap_or(false)
                            {
                                SymbolKind::Interface
                            } else {
                                SymbolKind::Type
                            };
                            symbols.push(Symbol {
                                name: n.to_string(),
                                kind: sk,
                                line: node.start_position().row + 1,
                            });
                        }
                    }
                }
                _ => {}
            },
            Language::Unknown => {}
        }

        // Push children for traversal (only top-level for performance)
        if node.child_count() > 0 && is_container_node(kind) {
            for i in 0..node.child_count() {
                if let Some(child) = node.child(i) {
                    stack.push(child);
                }
            }
        }
    }

    Some(FileIntel {
        path: path.to_path_buf(),
        language: lang,
        imports,
        symbols,
    })
}

/// Only descend into container nodes (not function bodies, etc.)
fn is_container_node(kind: &str) -> bool {
    matches!(
        kind,
        "source_file"
            | "program"
            | "module"
            | "declaration_list"
            | "block"
            | "statement_block"
            | "export_statement"
            | "type_declaration"
            | "impl_item" // Rust impl blocks contain methods
    )
}

/// Scan a project directory and build a dependency-ordered list of important files.
/// Returns files sorted by importance: entry points first, then most-imported files.
pub fn scan_project(root: &Path, max_files: usize) -> Vec<FileIntel> {
    let files = discover_source_files(root, 200);
    if files.is_empty() {
        return vec![];
    }

    let mut intels: Vec<FileIntel> = Vec::new();
    let mut import_counts: HashMap<String, usize> = HashMap::new();

    for file_path in &files {
        if let Ok(source) = std::fs::read_to_string(file_path) {
            // Limit parsing to first 500 lines for performance
            let truncated: String = source.lines().take(500).collect::<Vec<_>>().join("\n");
            if let Some(intel) = analyze_file(file_path, &truncated) {
                // Count how often each file is imported
                for imp in &intel.imports {
                    *import_counts.entry(imp.clone()).or_insert(0) += 1;
                }
                intels.push(intel);
            }
        }
    }

    // Score files by importance
    let mut scored: Vec<(usize, &FileIntel)> = intels
        .iter()
        .map(|intel| {
            let mut score = 0usize;
            let path_str = intel.path.display().to_string();

            // Entry points get highest score
            let filename = intel
                .path
                .file_name()
                .and_then(|f| f.to_str())
                .unwrap_or("");
            if matches!(
                filename,
                "main.rs"
                    | "lib.rs"
                    | "mod.rs"
                    | "index.ts"
                    | "index.tsx"
                    | "App.tsx"
                    | "App.ts"
                    | "main.ts"
                    | "main.tsx"
                    | "main.py"
                    | "__init__.py"
                    | "main.go"
                    | "app.go"
            ) {
                score += 100;
            }

            // Config files
            if matches!(
                filename,
                "package.json"
                    | "Cargo.toml"
                    | "tsconfig.json"
                    | "pyproject.toml"
                    | "go.mod"
                    | "vite.config.ts"
            ) {
                score += 80;
            }

            // Store/state files (key architectural files)
            if path_str.contains("store")
                || path_str.contains("state")
                || path_str.contains("context")
                || path_str.contains("reducer")
            {
                score += 60;
            }

            // Type definition files
            if path_str.contains("types")
                || path_str.contains("interfaces")
                || filename.ends_with(".d.ts")
            {
                score += 40;
            }

            // Files that are imported by many others
            for imp in &intel.imports {
                if let Some(count) = import_counts.get(imp) {
                    score += count * 5;
                }
            }

            // Files with many symbols are more important
            score += intel.symbols.len() * 3;

            score
        })
        .enumerate()
        .map(|(i, score)| (score, &intels[i]))
        .collect();

    scored.sort_by(|a, b| b.0.cmp(&a.0));

    scored
        .into_iter()
        .take(max_files)
        .map(|(_, intel)| intel.clone())
        .collect()
}

/// Discover source files in a project (respects .gitignore via git ls-files).
fn discover_source_files(root: &Path, max: usize) -> Vec<PathBuf> {
    use std::process::Command;

    // Try git ls-files first
    let output = Command::new("git")
        .args(["ls-files", "--cached", "--others", "--exclude-standard"])
        .current_dir(root)
        .output()
        .ok();

    let files: Vec<PathBuf> = if let Some(out) = output {
        if out.status.success() {
            String::from_utf8_lossy(&out.stdout)
                .lines()
                .filter(|l| {
                    let ext = l.rsplit('.').next().unwrap_or("");
                    matches!(
                        ext,
                        "rs" | "ts" | "tsx" | "js" | "jsx" | "py" | "go" | "mjs" | "cjs" | "mts"
                    )
                })
                .take(max)
                .map(|l| root.join(l))
                .collect()
        } else {
            vec![]
        }
    } else {
        vec![]
    };

    if files.is_empty() {
        // Fallback: walkdir
        walkdir_source_files(root, max)
    } else {
        files
    }
}

fn walkdir_source_files(root: &Path, max: usize) -> Vec<PathBuf> {
    let excluded = [
        "node_modules",
        "target",
        ".git",
        "__pycache__",
        "venv",
        ".venv",
        "dist",
        "build",
    ];
    let mut files = Vec::new();

    fn walk(dir: &Path, excluded: &[&str], files: &mut Vec<PathBuf>, max: usize) {
        if files.len() >= max {
            return;
        }
        let entries = match std::fs::read_dir(dir) {
            Ok(e) => e,
            Err(_) => return,
        };
        for entry in entries.flatten() {
            if files.len() >= max {
                return;
            }
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with('.') || excluded.contains(&name.as_str()) {
                continue;
            }
            let path = entry.path();
            if path.is_file() {
                let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
                if matches!(ext, "rs" | "ts" | "tsx" | "js" | "jsx" | "py" | "go") {
                    files.push(path);
                }
            } else if path.is_dir() {
                walk(&path, excluded, files, max);
            }
        }
    }

    walk(root, &excluded, &mut files, max);
    files
}

/// Format a project scan as a concise summary for injection into the system prompt.
pub fn format_project_intel(intels: &[FileIntel]) -> String {
    let mut out = String::new();

    for intel in intels {
        let rel_path = intel
            .path
            .file_name()
            .and_then(|f| f.to_str())
            .unwrap_or("?");

        // Format: path (lang) — symbols: fn foo, struct Bar; imports: ...
        let symbols_str: Vec<String> = intel
            .symbols
            .iter()
            .take(8)
            .map(|s| format!("{} {}", s.kind.label(), s.name))
            .collect();

        let imports_str: Vec<String> = intel.imports.iter().take(5).cloned().collect();

        out.push_str(&format!("• {} — ", intel.path.display()));
        if !symbols_str.is_empty() {
            out.push_str(&symbols_str.join(", "));
        }
        if !imports_str.is_empty() {
            if !symbols_str.is_empty() {
                out.push_str(" | imports: ");
            }
            out.push_str(&imports_str.join(", "));
        }
        out.push('\n');
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_analyze_rust_file() {
        let source = r#"
use std::collections::HashMap;
use serde::Serialize;

pub struct Config {
    pub name: String,
}

pub fn load_config() -> Config {
    Config { name: "test".into() }
}

enum Mode { Fast, Slow }
"#;
        let intel = analyze_file(Path::new("test.rs"), source).unwrap();
        assert_eq!(intel.language, Language::Rust);
        assert!(intel.imports.len() >= 2);
        assert!(intel
            .symbols
            .iter()
            .any(|s| s.name == "Config" && s.kind == SymbolKind::Struct));
        assert!(intel
            .symbols
            .iter()
            .any(|s| s.name == "load_config" && s.kind == SymbolKind::Function));
    }

    #[test]
    fn test_analyze_typescript_file() {
        let source = r#"
import { useState } from "react";
import { create } from "zustand";

interface AppState {
    count: number;
}

function increment() {}

class App {}

export type Config = { name: string };
"#;
        let intel = analyze_file(Path::new("test.ts"), source).unwrap();
        assert_eq!(intel.language, Language::TypeScript);
        assert!(intel.imports.iter().any(|i| i.contains("react")));
        assert!(intel
            .symbols
            .iter()
            .any(|s| s.name == "AppState" && s.kind == SymbolKind::Interface));
        assert!(intel
            .symbols
            .iter()
            .any(|s| s.name == "increment" && s.kind == SymbolKind::Function));
    }

    #[test]
    fn test_analyze_python_file() {
        let source = r#"
import os
from pathlib import Path

class MyModel:
    pass

def train():
    pass
"#;
        let intel = analyze_file(Path::new("test.py"), source).unwrap();
        assert_eq!(intel.language, Language::Python);
        assert!(intel.imports.len() >= 2);
        assert!(intel
            .symbols
            .iter()
            .any(|s| s.name == "MyModel" && s.kind == SymbolKind::Class));
        assert!(intel
            .symbols
            .iter()
            .any(|s| s.name == "train" && s.kind == SymbolKind::Function));
    }

    #[test]
    fn test_language_detection() {
        assert_eq!(Language::from_extension("rs"), Language::Rust);
        assert_eq!(Language::from_extension("tsx"), Language::TypeScript);
        assert_eq!(Language::from_extension("py"), Language::Python);
        assert_eq!(Language::from_extension("go"), Language::Go);
        assert_eq!(Language::from_extension("md"), Language::Unknown);
    }
}
