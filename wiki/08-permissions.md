# Permissions

Permission policies control what tools the agent can execute. They sit between the model's tool call request and actual execution.

## The PermissionPolicy Trait

```rust
#[async_trait]
pub trait PermissionPolicy: Send + Sync {
    async fn check(&self, request: &PermissionRequest) -> PermissionDecision;
}
```

### PermissionRequest

```rust
pub struct PermissionRequest {
    pub tool_name: String,                 // which tool
    pub tool_input: serde_json::Value,     // what arguments
    pub permission_level: PermissionLevel, // what level the tool requires
    pub description: String,               // human-readable description
    pub id: String,                        // unique ID for this request
}
```

### PermissionDecision

```rust
pub enum PermissionDecision {
    Allow,                 // proceed
    Deny(String),          // block with reason (sent back to model)
    AllowOnce,             // allow this one invocation
    AllowForSession,       // allow all future invocations of this tool
}
```

## Built-in Policies

### AllowAll

Allow everything. Use for CI, headless agents, trusted environments.

```rust
Agent::builder().permission_policy(AllowAll)
```

### AllowReadOnly

Only allow tools with `PermissionLevel::None` or `ReadOnly`. Blocks writes, shell execution, etc.

```rust
Agent::builder().permission_policy(AllowReadOnly)
```

### DenyAll

Block all tool invocations. The model can still generate text but cannot act on the system.

```rust
Agent::builder().permission_policy(DenyAll)
```

### RuleBased

Pattern-matching rules evaluated in order. First match wins.

```rust
use cersei::prelude::*;
use cersei_tools::permissions::{RuleBased, PermissionRule, PermissionAction};

Agent::builder().permission_policy(RuleBased {
    rules: vec![
        // Allow all reads
        PermissionRule {
            tool_name: Some("Read".into()),
            path_pattern: None,
            action: PermissionAction::Allow,
        },
        // Allow Glob and Grep
        PermissionRule {
            tool_name: Some("Glob".into()),
            path_pattern: None,
            action: PermissionAction::Allow,
        },
        PermissionRule {
            tool_name: Some("Grep".into()),
            path_pattern: None,
            action: PermissionAction::Allow,
        },
        // Block everything else
        PermissionRule {
            tool_name: None, // matches all
            path_pattern: None,
            action: PermissionAction::Deny,
        },
    ],
})
```

### InteractivePolicy

Delegates to a callback function for human-in-the-loop approval:

```rust
Agent::builder().permission_policy(InteractivePolicy::new(|req| {
    println!("\nTool '{}' wants to run:", req.tool_name);
    println!("  Level: {:?}", req.permission_level);
    println!("  Input: {}", req.tool_input);
    print!("Allow? [y/n] ");

    // read user input...
    PermissionDecision::Allow
}))
```

### StreamDeferredPolicy

Emits `AgentEvent::PermissionRequired` through the stream, allowing the stream consumer to make the decision:

```rust
Agent::builder()
    .permission_policy(InteractivePolicy::via_stream())
    .build()?;

let mut stream = agent.run_stream("Delete unused files");
while let Some(event) = stream.next().await {
    if let AgentEvent::PermissionRequired(req) = event {
        // UI prompts the user
        let decision = prompt_user(&req);
        stream.respond_permission(req.id, decision);
    }
}
```

## Permission Levels

Tools declare their required permission level:

| Level | Risk | Example Tools |
|-------|------|---------------|
| `None` | Safe | Pure computation, formatting |
| `ReadOnly` | Low | Read, Glob, Grep, WebFetch |
| `Write` | Medium | Write, Edit |
| `Execute` | High | Bash, PowerShell |
| `Dangerous` | Very High | Sandbox bypass |
| `Forbidden` | Never | Blocked regardless of policy |

## Custom Policy

```rust
struct TimeBasedPolicy;

#[async_trait]
impl PermissionPolicy for TimeBasedPolicy {
    async fn check(&self, req: &PermissionRequest) -> PermissionDecision {
        let hour = chrono::Local::now().hour();

        // Only allow writes during business hours
        if req.permission_level == PermissionLevel::Write && !(9..17).contains(&hour) {
            PermissionDecision::Deny("Write operations only allowed 9am-5pm".into())
        } else {
            PermissionDecision::Allow
        }
    }
}
```

## How Permissions Flow

```
Model requests tool_use("Bash", {"command": "rm -rf /tmp/old"})
    │
    ▼
Permission check: policy.check(PermissionRequest {
    tool_name: "Bash",
    permission_level: Execute,
    ...
})
    │
    ├── Allow → execute tool → return result to model
    │
    └── Deny("reason") → return ToolResult::error("Permission denied: reason")
                          model sees the denial and can adjust
```

When a tool is denied, the model receives the denial reason as a tool error. This lets it try alternative approaches (e.g., using `Read` instead of `Bash cat`).
