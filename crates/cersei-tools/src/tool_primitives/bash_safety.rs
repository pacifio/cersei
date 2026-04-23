//! Tree-sitter based bash command safety analysis.
//!
//! Parses bash commands into ASTs and validates them for safety before execution.
//! Detects dangerous constructs like command substitution, process substitution,
//! variable expansion in dangerous contexts, and destructive operations.

use tree_sitter::{Parser, Tree};

/// Risk level of a bash command.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum BashRiskLevel {
    /// Safe: read-only commands, navigation, inspection.
    Safe,
    /// Moderate: writes files, runs builds, modifies state.
    Moderate,
    /// High: destructive operations, network access, privilege escalation.
    High,
    /// Forbidden: never auto-approve (rm -rf /, sudo, etc.).
    Forbidden,
}

/// Result of analyzing a bash command.
#[derive(Debug, Clone)]
pub struct BashAnalysis {
    pub risk: BashRiskLevel,
    pub reasons: Vec<String>,
    /// File paths that the command reads from.
    pub read_paths: Vec<String>,
    /// File paths that the command writes to.
    pub write_paths: Vec<String>,
    /// Commands detected in the input.
    pub commands: Vec<String>,
}

/// Parse a bash command string into a tree-sitter AST.
pub fn parse_bash(source: &str) -> Option<Tree> {
    let mut parser = Parser::new();
    let lang = tree_sitter_bash::LANGUAGE;
    parser.set_language(&lang.into()).ok()?;
    parser.parse(source, None)
}

/// Analyze a bash command for safety.
pub fn analyze_command(source: &str) -> BashAnalysis {
    let mut analysis = BashAnalysis {
        risk: BashRiskLevel::Safe,
        reasons: Vec::new(),
        read_paths: Vec::new(),
        write_paths: Vec::new(),
        commands: Vec::new(),
    };

    let tree = match parse_bash(source) {
        Some(t) => t,
        None => {
            analysis.risk = BashRiskLevel::High;
            analysis.reasons.push("Failed to parse command".into());
            return analysis;
        }
    };

    let root = tree.root_node();
    if root.has_error() {
        analysis.risk = BashRiskLevel::Moderate;
        analysis.reasons.push("Command has parse errors".into());
    }

    // Walk the AST
    let mut cursor = root.walk();
    let mut stack = vec![root];
    let bytes = source.as_bytes();

    while let Some(node) = stack.pop() {
        let kind = node.kind();

        // Check for dangerous constructs
        match kind {
            // Command substitution: $(cmd) or `cmd`
            "command_substitution" => {
                raise(
                    &mut analysis,
                    BashRiskLevel::Moderate,
                    "command substitution detected",
                );
            }
            // Process substitution: <(cmd) or >(cmd)
            "process_substitution" => {
                raise(
                    &mut analysis,
                    BashRiskLevel::Moderate,
                    "process substitution detected",
                );
            }
            // Redirections: could overwrite files
            "file_redirect" | "heredoc_redirect" => {
                raise(
                    &mut analysis,
                    BashRiskLevel::Moderate,
                    "file redirection detected",
                );
                // Try to extract target path
                if let Some(dest) = node.child_by_field_name("destination") {
                    if let Ok(path) = dest.utf8_text(bytes) {
                        analysis.write_paths.push(path.to_string());
                    }
                }
            }
            // Pipeline: moderate risk (data flows between processes)
            "pipeline" => {
                raise(&mut analysis, BashRiskLevel::Moderate, "pipeline detected");
            }
            // Extract command names
            "command" => {
                if let Some(name_node) = node.child_by_field_name("name") {
                    if let Ok(cmd_name) = name_node.utf8_text(bytes) {
                        analysis.commands.push(cmd_name.to_string());
                        classify_command(cmd_name, &mut analysis, &node, bytes);
                    }
                }
            }
            _ => {}
        }

        // Push children for traversal
        for i in 0..node.child_count() {
            if let Some(child) = node.child(i) {
                stack.push(child);
            }
        }
    }

    // If no commands detected, it's likely safe (empty or just comments)
    if analysis.commands.is_empty() && analysis.risk == BashRiskLevel::Safe {
        analysis.risk = BashRiskLevel::Safe;
    }

    analysis
}

/// Classify a specific command by name.
fn classify_command(
    name: &str,
    analysis: &mut BashAnalysis,
    node: &tree_sitter::Node,
    bytes: &[u8],
) {
    match name {
        // ── Forbidden ──
        "sudo" | "doas" | "su" => {
            raise(analysis, BashRiskLevel::Forbidden, "privilege escalation");
        }

        // ── High risk: destructive ──
        "rm" => {
            // Check for -rf or dangerous flags
            let args = extract_arguments(node, bytes);
            if args
                .iter()
                .any(|a| a.contains("rf") || a == "/" || a == "/*")
            {
                raise(
                    analysis,
                    BashRiskLevel::Forbidden,
                    "rm -rf or root deletion",
                );
            } else {
                raise(analysis, BashRiskLevel::High, "file deletion (rm)");
            }
            for arg in &args {
                if !arg.starts_with('-') {
                    analysis.write_paths.push(arg.clone());
                }
            }
        }
        "chmod" | "chown" | "chgrp" => {
            raise(
                analysis,
                BashRiskLevel::High,
                &format!("permission change ({name})"),
            );
        }
        "kill" | "killall" | "pkill" => {
            raise(analysis, BashRiskLevel::High, "process termination");
        }
        "dd" | "mkfs" | "fdisk" | "mount" | "umount" => {
            raise(
                analysis,
                BashRiskLevel::Forbidden,
                &format!("disk operation ({name})"),
            );
        }
        "curl" | "wget" => {
            raise(analysis, BashRiskLevel::High, "network download");
        }
        "ssh" | "scp" | "rsync" => {
            raise(analysis, BashRiskLevel::High, "remote access");
        }

        // ── Moderate risk: writes ──
        "cp" | "mv" | "install" => {
            raise(
                analysis,
                BashRiskLevel::Moderate,
                &format!("file operation ({name})"),
            );
            for arg in extract_arguments(node, bytes) {
                if !arg.starts_with('-') {
                    analysis.write_paths.push(arg);
                }
            }
        }
        "mkdir" | "rmdir" | "touch" => {
            raise(
                analysis,
                BashRiskLevel::Moderate,
                &format!("directory/file creation ({name})"),
            );
        }
        "git" => {
            let args = extract_arguments(node, bytes);
            let subcommand = args.first().map(|s| s.as_str()).unwrap_or("");
            match subcommand {
                "push" | "reset" | "checkout" | "clean" | "rebase" => {
                    raise(analysis, BashRiskLevel::High, &format!("git {subcommand}"));
                }
                "status" | "log" | "diff" | "branch" | "show" | "blame" | "stash" => {
                    // Read-only git commands are safe
                }
                _ => {
                    raise(
                        analysis,
                        BashRiskLevel::Moderate,
                        &format!("git {subcommand}"),
                    );
                }
            }
        }
        "npm" | "yarn" | "pnpm" | "pip" | "cargo" => {
            let args = extract_arguments(node, bytes);
            let subcommand = args.first().map(|s| s.as_str()).unwrap_or("");
            match subcommand {
                "install" | "add" | "remove" | "uninstall" | "publish" => {
                    raise(
                        analysis,
                        BashRiskLevel::Moderate,
                        &format!("{name} {subcommand}"),
                    );
                }
                "run" | "exec" | "test" | "build" | "check" | "clippy" | "fmt" => {
                    raise(
                        analysis,
                        BashRiskLevel::Moderate,
                        &format!("{name} {subcommand}"),
                    );
                }
                _ => {}
            }
        }

        // ── Safe: read-only ──
        "ls" | "cat" | "head" | "tail" | "less" | "more" | "wc" | "file" | "stat" | "find"
        | "grep" | "rg" | "ag" | "fd" | "tree" | "du" | "df" | "echo" | "printf" | "date"
        | "whoami" | "hostname" | "uname" | "env" | "printenv" | "which" | "type" | "command"
        | "pwd" | "cd" | "pushd" | "popd" | "true" | "false" | "test" | "expr" | "seq" | "sort"
        | "uniq" | "tr" | "cut" | "awk" | "sed" | "jq" | "yq" | "xargs" | "tee" => {
            // These are generally safe (read-only or formatting)
        }

        // ── Unknown: moderate by default ──
        _ => {
            raise(
                analysis,
                BashRiskLevel::Moderate,
                &format!("unknown command: {name}"),
            );
        }
    }
}

/// Extract command arguments from AST.
fn extract_arguments(node: &tree_sitter::Node, bytes: &[u8]) -> Vec<String> {
    let mut args = Vec::new();
    let mut cursor = node.walk();

    for child in node.children(&mut cursor) {
        match child.kind() {
            "word" | "string" | "raw_string" | "number" | "concatenation" => {
                if let Ok(text) = child.utf8_text(bytes) {
                    // Skip the command name (first word)
                    if child.start_byte() > node.child(0).map(|c| c.end_byte()).unwrap_or(0) {
                        args.push(text.trim_matches(|c| c == '"' || c == '\'').to_string());
                    }
                }
            }
            _ => {}
        }
    }

    args
}

/// Raise the risk level if the new level is higher.
fn raise(analysis: &mut BashAnalysis, level: BashRiskLevel, reason: &str) {
    if level > analysis.risk {
        analysis.risk = level;
    }
    analysis.reasons.push(reason.to_string());
}

/// Quick check: is a command safe to auto-approve?
pub fn is_safe(source: &str) -> bool {
    analyze_command(source).risk <= BashRiskLevel::Safe
}

/// Quick check: should a command be blocked?
pub fn is_forbidden(source: &str) -> bool {
    analyze_command(source).risk >= BashRiskLevel::Forbidden
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_safe_commands() {
        assert!(is_safe("ls -la"));
        assert!(is_safe("cat README.md"));
        assert!(is_safe("grep -r 'TODO' src/"));
        assert!(is_safe("pwd"));
        assert!(is_safe("echo hello"));
    }

    #[test]
    fn test_moderate_commands() {
        let a = analyze_command("mkdir -p /tmp/test");
        assert_eq!(a.risk, BashRiskLevel::Moderate);

        let a = analyze_command("cargo build");
        assert_eq!(a.risk, BashRiskLevel::Moderate);

        let a = analyze_command("cp file1.txt file2.txt");
        assert_eq!(a.risk, BashRiskLevel::Moderate);
    }

    #[test]
    fn test_high_risk_commands() {
        let a = analyze_command("rm important_file.txt");
        assert_eq!(a.risk, BashRiskLevel::High);

        let a = analyze_command("chmod 777 /tmp/file");
        assert_eq!(a.risk, BashRiskLevel::High);

        let a = analyze_command("curl https://example.com/script.sh");
        assert_eq!(a.risk, BashRiskLevel::High);
    }

    #[test]
    fn test_forbidden_commands() {
        assert!(is_forbidden("sudo rm -rf /"));
        assert!(is_forbidden("rm -rf /"));
        assert!(is_forbidden("dd if=/dev/zero of=/dev/sda"));
    }

    #[test]
    fn test_git_classification() {
        let a = analyze_command("git status");
        assert_eq!(a.risk, BashRiskLevel::Safe);

        let a = analyze_command("git log --oneline");
        assert_eq!(a.risk, BashRiskLevel::Safe);

        let a = analyze_command("git push origin main");
        assert_eq!(a.risk, BashRiskLevel::High);

        let a = analyze_command("git add .");
        assert_eq!(a.risk, BashRiskLevel::Moderate);
    }

    #[test]
    fn test_pipeline_detection() {
        let a = analyze_command("cat file | grep pattern");
        assert!(a.risk >= BashRiskLevel::Moderate);
        assert!(a.reasons.iter().any(|r| r.contains("pipeline")));
    }

    #[test]
    fn test_command_extraction() {
        let a = analyze_command("ls -la && echo done && cat file.txt");
        assert!(a.commands.contains(&"ls".to_string()));
        assert!(a.commands.contains(&"echo".to_string()));
        assert!(a.commands.contains(&"cat".to_string()));
    }

    #[test]
    fn test_parse_bash() {
        let tree = parse_bash("echo hello world");
        assert!(tree.is_some());
        let tree = tree.unwrap();
        assert!(!tree.root_node().has_error());
    }
}
