//! The ExecutionGraph — a DAG of agent runs / turns / tool calls, with edges
//! encoding containment, failure causality, and sub-agent spawning.
//!
//! Populated passively by [`crate::graph_reporter::GraphReporter`] from the
//! agent's `AgentEvent` stream. A [`FailureTrace`] is extracted from it to give
//! directionality to recovery planners. Every string stored here is scrubbed.

use crate::scrub::redact_excerpt;
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub type NodeId = u64;

const EXCERPT_MAX: usize = 1200;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionGraph {
    pub nodes: Vec<RunNode>,
    pub edges: Vec<Edge>,
    pub root: NodeId,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum NodeKind {
    AgentRun,
    Turn,
    ToolCall,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum NodeStatus {
    Running,
    Ok,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunNode {
    pub id: NodeId,
    pub kind: NodeKind,
    pub label: String,
    pub status: NodeStatus,
    pub turn: u32,
    pub detail: NodeDetail,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum NodeDetail {
    Tool {
        input: String,
        result: String,
        is_error: bool,
    },
    Agent {
        summary: String,
    },
    Empty,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum EdgeRel {
    /// `to` happened as part of `from` (agent → turn, turn → tool call).
    Contains,
    /// `to` is a failure that followed `from` (ordering of failures).
    FailedAfter,
    /// `to` is a sub-agent spawned by `from`.
    Spawned,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct Edge {
    pub from: NodeId,
    pub to: NodeId,
    pub rel: EdgeRel,
}

impl ExecutionGraph {
    pub fn new() -> Self {
        let root = RunNode {
            id: 0,
            kind: NodeKind::AgentRun,
            label: "general-agent".to_string(),
            status: NodeStatus::Running,
            turn: 0,
            detail: NodeDetail::Empty,
        };
        Self {
            nodes: vec![root],
            edges: Vec::new(),
            root: 0,
        }
    }

    pub fn node(&self, id: NodeId) -> Option<&RunNode> {
        self.nodes.iter().find(|n| n.id == id)
    }

    fn node_mut(&mut self, id: NodeId) -> Option<&mut RunNode> {
        self.nodes.iter_mut().find(|n| n.id == id)
    }

    pub fn add_node(
        &mut self,
        kind: NodeKind,
        label: impl Into<String>,
        turn: u32,
        detail: NodeDetail,
    ) -> NodeId {
        let id = self.nodes.len() as NodeId;
        self.nodes.push(RunNode {
            id,
            kind,
            label: label.into(),
            status: NodeStatus::Running,
            turn,
            detail,
        });
        id
    }

    pub fn add_edge(&mut self, from: NodeId, to: NodeId, rel: EdgeRel) {
        self.edges.push(Edge { from, to, rel });
    }

    pub fn set_status(&mut self, id: NodeId, status: NodeStatus) {
        if let Some(n) = self.node_mut(id) {
            n.status = status;
        }
    }

    /// Record a tool call's outcome: set status and fill in the result detail.
    pub fn finish_tool(&mut self, id: NodeId, result: String, is_error: bool) {
        if let Some(n) = self.node_mut(id) {
            n.status = if is_error {
                NodeStatus::Failed
            } else {
                NodeStatus::Ok
            };
            if let NodeDetail::Tool {
                result: r,
                is_error: e,
                ..
            } = &mut n.detail
            {
                *r = result;
                *e = is_error;
            }
        }
    }

    /// Whether the overall run is considered failed (root failed or any failed node).
    pub fn has_failure(&self) -> bool {
        self.nodes.iter().any(|n| n.status == NodeStatus::Failed)
    }

    /// Extract an ordered, scrubbed failure trace giving recovery directionality.
    pub fn failure_trace(&self, problem_statement: &str) -> FailureTrace {
        let mut failing_nodes: Vec<FailurePoint> = self
            .nodes
            .iter()
            .filter(|n| n.status == NodeStatus::Failed && n.kind == NodeKind::ToolCall)
            .map(|n| {
                let (input, error) = match &n.detail {
                    NodeDetail::Tool { input, result, .. } => (input.clone(), result.clone()),
                    _ => (String::new(), String::new()),
                };
                FailurePoint {
                    tool: n.label.clone(),
                    input_excerpt: redact_excerpt(&input, EXCERPT_MAX),
                    error_excerpt: redact_excerpt(&error, EXCERPT_MAX),
                    turn: n.turn,
                }
            })
            .collect();
        failing_nodes.sort_by_key(|f| f.turn);

        let final_error = failing_nodes.last().map(|f| f.error_excerpt.clone());

        FailureTrace {
            problem_statement: problem_statement.to_string(),
            failing_nodes,
            final_error,
            hypotheses: Vec::new(),
        }
    }
}

impl Default for ExecutionGraph {
    fn default() -> Self {
        Self::new()
    }
}

/// The distilled, scrubbed account of why a run failed — fed to planners.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FailureTrace {
    pub problem_statement: String,
    pub failing_nodes: Vec<FailurePoint>,
    pub final_error: Option<String>,
    /// Optional planner-derived "what to fix" hints.
    pub hypotheses: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FailurePoint {
    pub tool: String,
    pub input_excerpt: String,
    pub error_excerpt: String,
    pub turn: u32,
}

impl FailureTrace {
    /// Render as directionality text to embed in a planner/proposal prompt.
    pub fn directionality(&self) -> String {
        let mut s = format!("Original task: {}\n\n", self.problem_statement);
        if self.failing_nodes.is_empty() {
            s.push_str("The previous attempt did not complete successfully.\n");
        } else {
            s.push_str("The previous attempt failed at these steps (in order):\n");
            for (i, f) in self.failing_nodes.iter().enumerate() {
                s.push_str(&format!(
                    "{}. tool `{}` (turn {}) failed: {}\n",
                    i + 1,
                    f.tool,
                    f.turn,
                    f.error_excerpt
                ));
            }
        }
        s.push_str("\nFix the specific failure(s) above; do not repeat the same approach.");
        s
    }
}

/// Helper to summarize a tool input Value into a compact string for the graph.
pub(crate) fn summarize_input(input: &Value) -> String {
    let s = input.to_string();
    if s.len() > EXCERPT_MAX {
        format!("{}…", &s[..floor_char_boundary(&s, EXCERPT_MAX)])
    } else {
        s
    }
}

fn floor_char_boundary(text: &str, max_bytes: usize) -> usize {
    let mut end = max_bytes.min(text.len());
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    end
}
