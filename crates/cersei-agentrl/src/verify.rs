//! Success verification for AgentRL runs.
//!
//! "Did the agent actually solve it?" is decided by a [`Verifier`] — typically a
//! shell command run in the working dir (e.g. `cargo test`, `python solve.py`).
//! Both the GeneralAgent and each proposal are judged by the same verifier, so a
//! proposal can only win if it genuinely passes the real check.

use async_trait::async_trait;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

#[derive(Debug, Clone)]
pub struct VerifyResult {
    pub passed: bool,
    pub detail: String,
}

#[async_trait]
pub trait Verifier: Send + Sync {
    async fn verify(&self, workdir: &Path) -> VerifyResult;

    /// Whether this verifier can produce a meaningful signal for `workdir`.
    /// A [`ChainVerifier`] uses this to pick the first applicable verifier.
    /// Defaults to `true` (most verifiers always apply).
    async fn applicable(&self, _workdir: &Path) -> bool {
        true
    }
}

/// Runs a shell command in the working dir; passes iff it exits 0.
pub struct CommandVerifier {
    pub command: String,
    pub timeout: Duration,
}

impl CommandVerifier {
    pub fn new(command: impl Into<String>) -> Self {
        Self {
            command: command.into(),
            timeout: Duration::from_secs(120),
        }
    }
}

#[async_trait]
impl Verifier for CommandVerifier {
    async fn verify(&self, workdir: &Path) -> VerifyResult {
        let mut cmd = tokio::process::Command::new("/bin/sh");
        cmd.arg("-c")
            .arg(&self.command)
            .current_dir(workdir)
            .kill_on_drop(true);
        let fut = cmd.output();
        match tokio::time::timeout(self.timeout, fut).await {
            Ok(Ok(out)) => {
                let stdout = String::from_utf8_lossy(&out.stdout);
                let stderr = String::from_utf8_lossy(&out.stderr);
                VerifyResult {
                    passed: out.status.success(),
                    detail: format!(
                        "exit={} stdout={} stderr={}",
                        out.status.code().unwrap_or(-1),
                        truncate(&stdout, 600),
                        truncate(&stderr, 600)
                    ),
                }
            }
            Ok(Err(e)) => VerifyResult {
                passed: false,
                detail: format!("verifier failed to run: {e}"),
            },
            Err(_) => VerifyResult {
                passed: false,
                detail: format!("verifier timed out after {:?}", self.timeout),
            },
        }
    }
}

/// Runs the first existing test script (e.g. a task-provided `run-tests.sh`) and
/// passes iff it exits 0. This is the strongest signal when a task ships an
/// executable check; `applicable` is false when no candidate script exists.
pub struct TestScriptVerifier {
    /// Candidate script paths, tried in order. Relative paths resolve against
    /// the working dir; absolute paths (e.g. `/tests/run-tests.sh`) are used verbatim.
    pub candidates: Vec<String>,
    pub timeout: Duration,
}

impl TestScriptVerifier {
    /// Default candidates covering the common terminal-bench layouts.
    pub fn default_candidates() -> Self {
        Self {
            candidates: vec![
                "run-tests.sh".into(),
                "tests/run-tests.sh".into(),
                "/tests/run-tests.sh".into(),
                "/app/tests/run-tests.sh".into(),
            ],
            timeout: Duration::from_secs(180),
        }
    }

    pub fn new(candidates: Vec<String>) -> Self {
        Self {
            candidates,
            timeout: Duration::from_secs(180),
        }
    }

    fn resolve(&self, workdir: &Path) -> Option<std::path::PathBuf> {
        for c in &self.candidates {
            let p = if Path::new(c).is_absolute() {
                std::path::PathBuf::from(c)
            } else {
                workdir.join(c)
            };
            if p.is_file() {
                return Some(p);
            }
        }
        None
    }
}

#[async_trait]
impl Verifier for TestScriptVerifier {
    async fn applicable(&self, workdir: &Path) -> bool {
        self.resolve(workdir).is_some()
    }

    async fn verify(&self, workdir: &Path) -> VerifyResult {
        let Some(script) = self.resolve(workdir) else {
            return VerifyResult {
                passed: false,
                detail: "no test script found".into(),
            };
        };
        let cmd = CommandVerifier {
            command: format!("sh {}", shell_quote(&script.to_string_lossy())),
            timeout: self.timeout,
        };
        cmd.verify(workdir).await
    }
}

/// Always passes — an optimistic fallback for tasks with no executable check, so
/// a clean run is not marked failed merely because no grader is available.
pub struct AcceptVerifier;

#[async_trait]
impl Verifier for AcceptVerifier {
    async fn verify(&self, _workdir: &Path) -> VerifyResult {
        VerifyResult {
            passed: true,
            detail: "no executable verifier; accepting the agent's result".into(),
        }
    }
}

/// Runs the first *applicable* verifier in order. If none apply, returns
/// `default_passed`. Use it to prefer a real test script, then fall back.
pub struct ChainVerifier {
    pub verifiers: Vec<Arc<dyn Verifier>>,
    pub default_passed: bool,
}

impl ChainVerifier {
    pub fn new(verifiers: Vec<Arc<dyn Verifier>>) -> Self {
        Self {
            verifiers,
            default_passed: false,
        }
    }

    pub fn with_default(mut self, passed: bool) -> Self {
        self.default_passed = passed;
        self
    }
}

#[async_trait]
impl Verifier for ChainVerifier {
    async fn applicable(&self, workdir: &Path) -> bool {
        for v in &self.verifiers {
            if v.applicable(workdir).await {
                return true;
            }
        }
        self.default_passed
    }

    async fn verify(&self, workdir: &Path) -> VerifyResult {
        for v in &self.verifiers {
            if v.applicable(workdir).await {
                return v.verify(workdir).await;
            }
        }
        VerifyResult {
            passed: self.default_passed,
            detail: "no applicable verifier in chain".into(),
        }
    }
}

fn shell_quote(s: &str) -> String {
    // single-quote escape for safe interpolation into `sh -c`
    format!("'{}'", s.replace('\'', r"'\''"))
}

fn truncate(s: &str, n: usize) -> String {
    if s.len() <= n {
        s.to_string()
    } else {
        format!("{}…", &s[..s.floor_char_boundary(n)])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn command_verifier_pass_fail() {
        let dir = std::env::temp_dir();
        assert!(CommandVerifier::new("true").verify(&dir).await.passed);
        assert!(!CommandVerifier::new("false").verify(&dir).await.passed);
    }

    #[tokio::test]
    async fn test_script_verifier_applicable_and_runs() {
        let dir = tempfile::tempdir().unwrap();
        let v = TestScriptVerifier::new(vec!["run-tests.sh".into()]);
        // not applicable until the script exists
        assert!(!v.applicable(dir.path()).await);
        std::fs::write(dir.path().join("run-tests.sh"), "#!/bin/sh\nexit 0\n").unwrap();
        assert!(v.applicable(dir.path()).await);
        assert!(v.verify(dir.path()).await.passed);
        std::fs::write(dir.path().join("run-tests.sh"), "#!/bin/sh\nexit 1\n").unwrap();
        assert!(!v.verify(dir.path()).await.passed);
    }

    #[tokio::test]
    async fn chain_prefers_test_script_then_falls_back() {
        let dir = tempfile::tempdir().unwrap();
        let chain = ChainVerifier::new(vec![
            Arc::new(TestScriptVerifier::new(vec!["run-tests.sh".into()])),
            Arc::new(AcceptVerifier),
        ]);
        // no test script → AcceptVerifier applies → passes
        assert!(chain.verify(dir.path()).await.passed);
        // failing test script → chain reports failure (test script wins)
        std::fs::write(dir.path().join("run-tests.sh"), "#!/bin/sh\nexit 1\n").unwrap();
        assert!(!chain.verify(dir.path()).await.passed);
    }
}
