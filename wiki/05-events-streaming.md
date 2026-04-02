# Events & Streaming

Cersei provides three mechanisms for real-time observation of agent activity, from simple to sophisticated.

## 1. Callback (Simple)

A single closure attached at build time. Good for logging or simple TUI updates.

```rust
Agent::builder()
    .on_event(|event: &AgentEvent| {
        match event {
            AgentEvent::TextDelta(t) => print!("{}", t),
            AgentEvent::ToolStart { name, .. } => eprintln!("[tool] {}...", name),
            AgentEvent::TurnComplete { turn, usage, .. } => {
                eprintln!("[turn {}] {} tokens", turn, usage.total());
            }
            AgentEvent::Error(e) => eprintln!("[error] {}", e),
            _ => {}
        }
    })
    .build()?;
```

The callback fires synchronously in the agent loop — keep it fast (no blocking I/O).

## 2. Broadcast Channel (Multi-Consumer)

Multiple independent listeners, each receiving every event. Uses `tokio::sync::broadcast` internally.

```rust
let agent = Agent::builder()
    .provider(Anthropic::from_env()?)
    .enable_broadcast(256)  // buffer capacity
    .build()?;

// Listener 1: Dashboard
let mut rx1 = agent.subscribe().unwrap();
tokio::spawn(async move {
    while let Ok(event) = rx1.recv().await {
        // update dashboard...
    }
});

// Listener 2: Cost tracker
let mut rx2 = agent.subscribe().unwrap();
tokio::spawn(async move {
    while let Ok(event) = rx2.recv().await {
        if let AgentEvent::CostUpdate { cumulative_cost, .. } = event {
            println!("Cost: ${:.4}", cumulative_cost);
        }
    }
});

agent.run("...").await?;
```

Listeners that fall behind receive a `Lagged` error — the channel is lossy by design so the agent is never blocked.

## 3. Event Stream (Async Iterator)

`agent.run_stream()` returns an `AgentStream` — an async iterator with bidirectional control.

```rust
let mut stream = agent.run_stream("Deploy to staging");

while let Some(event) = stream.next().await {
    match event {
        AgentEvent::TextDelta(t) => print!("{}", t),

        AgentEvent::PermissionRequired(req) => {
            // Interactive: pause, prompt user, resume
            let ok = prompt_user(&req.description);
            stream.respond_permission(
                req.id,
                if ok { PermissionDecision::Allow }
                else { PermissionDecision::Deny("User rejected".into()) }
            );
        }

        AgentEvent::Complete(output) => {
            println!("\nDone: {} turns", output.turns);
            break;
        }

        AgentEvent::Error(e) => {
            eprintln!("Error: {}", e);
            break;
        }

        _ => {}
    }
}
```

### AgentStream Methods

```rust
impl AgentStream {
    async fn next(&mut self) -> Option<AgentEvent>;
    fn respond_permission(&self, request_id: String, decision: PermissionDecision);
    fn cancel(&self);
    fn inject_message(&self, message: String);
    async fn collect(self) -> Result<AgentOutput>;
    async fn collect_text(self) -> Result<String>;
}
```

## Reporters

Structured event consumers implementing the `Reporter` trait:

```rust
#[async_trait]
pub trait Reporter: Send + Sync {
    async fn on_event(&self, event: &AgentEvent);
    async fn on_complete(&self, output: &AgentOutput) {}
    async fn on_error(&self, error: &CerseiError) {}
}
```

### Built-in Reporters

| Reporter | Description |
|----------|-------------|
| `ConsoleReporter` | Prints text and tool activity to stdout/stderr |
| `JsonReporter<W>` | Writes JSON-lines to any `Write` impl |
| `CollectorReporter` | Collects events into a `Vec` for post-hoc analysis |
| `MetricsReporter` | Aggregates into `AgentMetrics` (turns, cost, histogram) |

```rust
use cersei::reporters::*;

Agent::builder()
    .reporter(ConsoleReporter { verbose: true })
    .reporter(JsonReporter::new(File::create("agent.jsonl")?))
    .reporter(MetricsReporter::new(Duration::from_secs(10), |m| {
        println!("Turns: {}, Cost: ${:.4}", m.total_turns, m.total_cost_usd);
    }))
    .build()?;
```

### Custom Reporter

```rust
struct SlackNotifier { webhook_url: String }

#[async_trait]
impl Reporter for SlackNotifier {
    async fn on_event(&self, event: &AgentEvent) {
        if let AgentEvent::Error(e) = event {
            // POST to Slack webhook
        }
    }

    async fn on_complete(&self, output: &AgentOutput) {
        // Notify Slack that the agent finished
    }
}
```

### AgentMetrics

```rust
pub struct AgentMetrics {
    pub total_turns: u32,
    pub total_tool_calls: u32,
    pub total_cost_usd: f64,
    pub total_input_tokens: u64,
    pub total_output_tokens: u64,
    pub avg_turn_duration: Duration,
    pub tool_call_histogram: HashMap<String, u32>,
}
```

## Event Filtering

Only deliver events matching a predicate:

```rust
Agent::builder()
    .event_filter(|e| {
        // Only forward tool and cost events
        matches!(e,
            AgentEvent::ToolStart { .. } |
            AgentEvent::ToolEnd { .. } |
            AgentEvent::CostUpdate { .. }
        )
    })
```

## Full AgentEvent Enum

See the [README](../README.md#agentevent--full-enum) for the complete 26-variant enum.
