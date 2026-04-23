//! Built-in reporters for structured event consumption.

use async_trait::async_trait;
use cersei_types::*;
use std::collections::HashMap;
use std::io::Write;
use std::sync::Arc;
use std::time::Duration;

use crate::events::AgentEvent;
use crate::AgentOutput;

// ─── Reporter trait ──────────────────────────────────────────────────────────

#[async_trait]
pub trait Reporter: Send + Sync {
    async fn on_event(&self, event: &AgentEvent);

    async fn on_complete(&self, _output: &AgentOutput) {}

    async fn on_error(&self, _error: &CerseiError) {}
}

// ─── Console reporter ────────────────────────────────────────────────────────

/// Prints streaming text and tool activity to stdout/stderr.
pub struct ConsoleReporter {
    pub verbose: bool,
}

#[async_trait]
impl Reporter for ConsoleReporter {
    async fn on_event(&self, event: &AgentEvent) {
        match event {
            AgentEvent::TextDelta(text) => {
                print!("{}", text);
                let _ = std::io::stdout().flush();
            }
            AgentEvent::ToolStart { name, .. } => {
                if self.verbose {
                    eprintln!("[tool] Running {}...", name);
                }
            }
            AgentEvent::ToolEnd {
                name,
                is_error,
                duration,
                ..
            } => {
                if self.verbose {
                    let status = if *is_error { "FAILED" } else { "OK" };
                    eprintln!(
                        "[tool] {} {} ({:.1}s)",
                        name,
                        status,
                        duration.as_secs_f64()
                    );
                }
            }
            AgentEvent::TurnComplete { turn, usage, .. } => {
                if self.verbose {
                    eprintln!(
                        "[turn {}] {} in / {} out tokens",
                        turn, usage.input_tokens, usage.output_tokens
                    );
                }
            }
            AgentEvent::Error(e) => {
                eprintln!("[error] {}", e);
            }
            _ => {}
        }
    }

    async fn on_complete(&self, output: &AgentOutput) {
        if self.verbose {
            eprintln!(
                "\n[done] {} turns, ${:.4}",
                output.turns,
                output.usage.cost_usd.unwrap_or(0.0)
            );
        }
    }
}

// ─── JSON reporter ───────────────────────────────────────────────────────────

/// Writes structured JSON events to a writer (file, stdout, etc.).
pub struct JsonReporter<W: Write + Send + Sync> {
    writer: Arc<parking_lot::Mutex<W>>,
}

impl<W: Write + Send + Sync> JsonReporter<W> {
    pub fn new(writer: W) -> Self {
        Self {
            writer: Arc::new(parking_lot::Mutex::new(writer)),
        }
    }
}

#[async_trait]
impl<W: Write + Send + Sync + 'static> Reporter for JsonReporter<W> {
    async fn on_event(&self, event: &AgentEvent) {
        let json = serde_json::json!({
            "type": format!("{:?}", std::mem::discriminant(event)),
            "timestamp": chrono::Utc::now().to_rfc3339(),
            "event": format!("{:?}", event),
        });
        if let Ok(line) = serde_json::to_string(&json) {
            let mut writer = self.writer.lock();
            let _ = writeln!(writer, "{}", line);
        }
    }
}

// ─── Collector reporter ──────────────────────────────────────────────────────

/// Collects events into a Vec for post-hoc analysis.
pub struct CollectorReporter {
    events: Arc<parking_lot::Mutex<Vec<AgentEvent>>>,
}

impl CollectorReporter {
    pub fn new() -> Self {
        Self {
            events: Arc::new(parking_lot::Mutex::new(Vec::new())),
        }
    }

    pub fn events(&self) -> Vec<AgentEvent> {
        self.events.lock().clone()
    }
}

impl Default for CollectorReporter {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Reporter for CollectorReporter {
    async fn on_event(&self, event: &AgentEvent) {
        self.events.lock().push(event.clone());
    }
}

// ─── Metrics reporter ────────────────────────────────────────────────────────

/// Aggregates usage/cost into periodic summaries.
pub struct MetricsReporter {
    pub interval: Duration,
    pub on_metrics: Box<dyn Fn(AgentMetrics) + Send + Sync>,
    metrics: Arc<parking_lot::Mutex<AgentMetrics>>,
}

impl MetricsReporter {
    pub fn new(
        interval: Duration,
        on_metrics: impl Fn(AgentMetrics) + Send + Sync + 'static,
    ) -> Self {
        Self {
            interval,
            on_metrics: Box::new(on_metrics),
            metrics: Arc::new(parking_lot::Mutex::new(AgentMetrics::default())),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct AgentMetrics {
    pub total_turns: u32,
    pub total_tool_calls: u32,
    pub total_cost_usd: f64,
    pub total_input_tokens: u64,
    pub total_output_tokens: u64,
    pub avg_turn_duration: Duration,
    pub tool_call_histogram: HashMap<String, u32>,
}

#[async_trait]
impl Reporter for MetricsReporter {
    async fn on_event(&self, event: &AgentEvent) {
        let mut metrics = self.metrics.lock();
        match event {
            AgentEvent::TurnComplete { usage, .. } => {
                metrics.total_turns += 1;
                metrics.total_input_tokens += usage.input_tokens;
                metrics.total_output_tokens += usage.output_tokens;
                if let Some(cost) = usage.cost_usd {
                    metrics.total_cost_usd += cost;
                }
            }
            AgentEvent::ToolEnd { name, .. } => {
                metrics.total_tool_calls += 1;
                *metrics.tool_call_histogram.entry(name.clone()).or_default() += 1;
            }
            _ => {}
        }
    }

    async fn on_complete(&self, _output: &AgentOutput) {
        let metrics = self.metrics.lock().clone();
        (self.on_metrics)(metrics);
    }
}
