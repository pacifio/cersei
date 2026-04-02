# Hooks

Hooks intercept events in the agent lifecycle. They can log, block, modify inputs, or inject messages. Multiple hooks stack — the first non-`Continue` action wins.

## The Hook Trait

```rust
#[async_trait]
pub trait Hook: Send + Sync {
    fn events(&self) -> &[HookEvent];
    async fn on_event(&self, ctx: &HookContext) -> HookAction;
    fn name(&self) -> &str { "unnamed-hook" }  // for logging
}
```

## Hook Events

| Event | When | Can Block? |
|-------|------|-----------|
| `PreToolUse` | Before a tool executes | Yes |
| `PostToolUse` | After a tool returns | No (informational) |
| `PreModelTurn` | Before sending to the LLM | Yes |
| `PostModelTurn` | After the LLM responds | Yes |
| `Stop` | Agent run finishing | No |
| `Error` | Error occurred | No |

## Hook Actions

| Action | Effect |
|--------|--------|
| `Continue` | Proceed normally |
| `Block(reason)` | Stop the operation (`PreToolUse`: skip tool; `PostModelTurn`: stop loop) |
| `ModifyInput(Value)` | Replace the tool's input JSON (`PreToolUse` only) |
| `InjectMessage(Message)` | Add a message to the conversation |

## HookContext

```rust
pub struct HookContext {
    pub event: HookEvent,
    pub tool_name: Option<String>,         // set for tool events
    pub tool_input: Option<Value>,         // set for PreToolUse
    pub tool_result: Option<String>,       // set for PostToolUse
    pub tool_is_error: Option<bool>,       // set for PostToolUse
    pub turn: u32,                         // current turn number
    pub cumulative_cost_usd: f64,          // running cost total
    pub message_count: usize,              // messages in conversation
}
```

## Examples

### Cost Guard

Stop the agent when spending exceeds a budget:

```rust
struct CostGuard { max_usd: f64 }

#[async_trait]
impl Hook for CostGuard {
    fn events(&self) -> &[HookEvent] { &[HookEvent::PostModelTurn] }
    fn name(&self) -> &str { "cost-guard" }

    async fn on_event(&self, ctx: &HookContext) -> HookAction {
        if ctx.cumulative_cost_usd() > self.max_usd {
            HookAction::Block(format!("Budget ${:.2} exceeded", self.max_usd))
        } else {
            HookAction::Continue
        }
    }
}
```

### Audit Logger

Record every tool invocation:

```rust
struct AuditLogger;

#[async_trait]
impl Hook for AuditLogger {
    fn events(&self) -> &[HookEvent] {
        &[HookEvent::PreToolUse, HookEvent::PostToolUse]
    }
    fn name(&self) -> &str { "audit" }

    async fn on_event(&self, ctx: &HookContext) -> HookAction {
        match ctx.event {
            HookEvent::PreToolUse => {
                println!("[audit] PRE  {} turn={}", ctx.tool_name.as_deref().unwrap_or("?"), ctx.turn);
            }
            HookEvent::PostToolUse => {
                let ok = !ctx.tool_is_error.unwrap_or(false);
                println!("[audit] POST {} status={}", ctx.tool_name.as_deref().unwrap_or("?"),
                    if ok { "OK" } else { "ERR" });
            }
            _ => {}
        }
        HookAction::Continue
    }
}
```

### Tool Blocker

Prevent specific tools from running:

```rust
struct ToolBlocker { blocked: Vec<String> }

#[async_trait]
impl Hook for ToolBlocker {
    fn events(&self) -> &[HookEvent] { &[HookEvent::PreToolUse] }
    fn name(&self) -> &str { "tool-blocker" }

    async fn on_event(&self, ctx: &HookContext) -> HookAction {
        if let Some(name) = &ctx.tool_name {
            if self.blocked.iter().any(|b| b == name) {
                return HookAction::Block(format!("Tool '{}' is blocked", name));
            }
        }
        HookAction::Continue
    }
}
```

### Input Sanitizer

Redact sensitive data from tool inputs:

```rust
struct InputSanitizer;

#[async_trait]
impl Hook for InputSanitizer {
    fn events(&self) -> &[HookEvent] { &[HookEvent::PreToolUse] }

    async fn on_event(&self, ctx: &HookContext) -> HookAction {
        if let Some(input) = &ctx.tool_input {
            if let Some(cmd) = input["command"].as_str() {
                if cmd.contains("password") || cmd.contains("secret") {
                    return HookAction::Block("Command contains sensitive keywords".into());
                }
            }
        }
        HookAction::Continue
    }
}
```

## Shell Hook

For compatibility with Claude Code's `settings.json` hook format:

```rust
use cersei::hooks::ShellHook;

Agent::builder()
    .hook(ShellHook::new(
        "./scripts/pre-tool.sh",           // shell command
        &[HookEvent::PreToolUse],          // events
        true,                               // blocking: non-zero exit = Block
    ))
```

The shell command receives a `CERSEI_HOOK_CONTEXT` environment variable with JSON context.

## Stacking Hooks

Hooks execute in registration order. The first non-`Continue` action wins:

```rust
Agent::builder()
    .hook(CostGuard { max_usd: 5.0 })   // checked first
    .hook(ToolBlocker { blocked: vec!["Write".into()] })
    .hook(AuditLogger)                    // always runs (returns Continue)
    .build()?;
```

## Hook Execution

Internally, `cersei_hooks::run_hooks()` iterates all hooks matching the event:

```rust
pub async fn run_hooks(hooks: &[Arc<dyn Hook>], ctx: &HookContext) -> HookAction {
    for hook in hooks {
        if hook.events().contains(&ctx.event) {
            let action = hook.on_event(ctx).await;
            match &action {
                HookAction::Continue => continue,
                _ => return action,  // first non-Continue wins
            }
        }
    }
    HookAction::Continue
}
```
