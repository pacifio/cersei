# Tools

Tools are capabilities the agent can invoke: reading files, running shell commands, searching codebases, calling APIs.

## The Tool Trait

```rust
#[async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn input_schema(&self) -> serde_json::Value;
    fn permission_level(&self) -> PermissionLevel { PermissionLevel::None }
    fn category(&self) -> ToolCategory { ToolCategory::Custom }
    async fn execute(&self, input: serde_json::Value, ctx: &ToolContext) -> ToolResult;
}
```

## Built-in Tools

### Tool Sets

```rust
cersei::tools::all()          // everything
cersei::tools::coding()       // filesystem + shell (most common)
cersei::tools::filesystem()   // Read, Write, Edit, Glob, Grep
cersei::tools::shell()        // Bash
cersei::tools::none()         // empty (pure chat, no tools)
```

### Individual Tools

| Tool | Permission | Description |
|------|-----------|-------------|
| `Read` | ReadOnly | Read files with line numbers, offset/limit support |
| `Write` | Write | Create or overwrite files, auto-creates parent directories |
| `Edit` | Write | Exact string replacement, uniqueness check, `replace_all` option |
| `Glob` | ReadOnly | Find files by glob pattern (`**/*.rs`, `src/**/*.ts`) |
| `Grep` | ReadOnly | Search with regex via `rg` (falls back to `grep`) |
| `Bash` | Execute | Run shell commands with timeout, persistent cwd/env across calls |

### Bash — Shell State Persistence

The `Bash` tool maintains a `ShellState` per session so `cd` and `export` commands persist:

```
Turn 1: Bash("cd src && export FOO=bar")
Turn 2: Bash("pwd")       → outputs /project/src
Turn 3: Bash("echo $FOO") → outputs bar
```

This matches the behavior described in Claude Code's tool descriptions.

## Creating Custom Tools

### Option 1: Implement Tool Directly

```rust
struct WeatherTool { api_key: String }

#[async_trait]
impl Tool for WeatherTool {
    fn name(&self) -> &str { "get_weather" }
    fn description(&self) -> &str { "Get current weather for a city." }
    fn permission_level(&self) -> PermissionLevel { PermissionLevel::ReadOnly }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "city": { "type": "string", "description": "City name" }
            },
            "required": ["city"]
        })
    }

    async fn execute(&self, input: serde_json::Value, _ctx: &ToolContext) -> ToolResult {
        let city = input["city"].as_str().unwrap_or("unknown");
        // Call weather API...
        ToolResult::success(format!("72F, sunny in {}", city))
    }
}
```

### Option 2: Derive Macro (Typed)

```rust
use cersei::prelude::*;

#[derive(Tool)]
#[tool(name = "search_docs", description = "Search documentation", permission = "read_only")]
struct SearchTool;

#[derive(Deserialize, schemars::JsonSchema)]
struct SearchInput {
    /// The search query
    query: String,
    /// Max results to return
    #[serde(default = "default_limit")]
    limit: usize,
}

fn default_limit() -> usize { 10 }

#[async_trait]
impl ToolExecute for SearchTool {
    type Input = SearchInput;
    async fn run(&self, input: SearchInput, _ctx: &ToolContext) -> ToolResult {
        ToolResult::success(format!("Found results for: {}", input.query))
    }
}
```

The derive macro generates:
1. `name()` / `description()` / `permission_level()` / `category()` from `#[tool(...)]`
2. `input_schema()` from the `JsonSchema` derive on `Input`
3. `execute()` that deserializes JSON into `Input` and calls `run()`

### Option 3: Closure Tool (Quick & Dirty)

For simple tools you can implement the trait on a closure wrapper:

```rust
struct FnTool {
    name: String,
    description: String,
    schema: serde_json::Value,
    handler: Box<dyn Fn(serde_json::Value) -> String + Send + Sync>,
}

#[async_trait]
impl Tool for FnTool {
    fn name(&self) -> &str { &self.name }
    fn description(&self) -> &str { &self.description }
    fn input_schema(&self) -> serde_json::Value { self.schema.clone() }
    async fn execute(&self, input: serde_json::Value, _ctx: &ToolContext) -> ToolResult {
        ToolResult::success((self.handler)(input))
    }
}
```

## ToolContext

Every tool receives a `ToolContext` with shared resources:

```rust
pub struct ToolContext {
    pub working_dir: PathBuf,                    // agent's working directory
    pub session_id: String,                      // unique session identifier
    pub permissions: Arc<dyn PermissionPolicy>,  // permission checker
    pub cost_tracker: Arc<CostTracker>,          // cumulative token usage
    pub mcp_manager: Option<Arc<McpManager>>,    // MCP server connections
    pub extensions: Extensions,                  // user-injected data
}
```

### Extensions (Type Map)

Inject custom data accessible to all tools:

```rust
let agent = Agent::builder()
    .provider(Anthropic::from_env()?)
    .tool(MyDbTool)
    .build()?;

// Inside MyDbTool::execute:
async fn execute(&self, input: Value, ctx: &ToolContext) -> ToolResult {
    let pool = ctx.extensions.get::<sqlx::PgPool>().unwrap();
    // use pool...
}
```

## ToolResult

```rust
pub struct ToolResult {
    pub content: String,           // text sent back to the model
    pub is_error: bool,            // true = model sees it as an error
    pub metadata: Option<Value>,   // structured data (for TUI rendering, etc.)
}

// Constructors
ToolResult::success("output text")
ToolResult::error("something went wrong")
ToolResult::success("ok").with_metadata(json!({ "diff": "..." }))
```

## Permission Levels

| Level | Description | Example Tools |
|-------|------------|---------------|
| `None` | No permission needed | Pure computation |
| `ReadOnly` | Reads filesystem or network | Read, Glob, Grep |
| `Write` | Modifies filesystem | Write, Edit |
| `Execute` | Runs arbitrary commands | Bash |
| `Dangerous` | Bypass safety mechanisms | (custom) |
| `Forbidden` | Never allowed | (safety rail) |

## Tool Categories

Used for grouping in tool listings and UI:

```rust
pub enum ToolCategory {
    FileSystem,     // Read, Write, Edit, Glob, Grep
    Shell,          // Bash, PowerShell
    Web,            // WebFetch, WebSearch
    Memory,         // TodoWrite, session memory
    Orchestration,  // Agent, Tasks
    Mcp,            // MCP-provided tools
    Custom,         // user-defined (default)
}
```

## MCP Tools

MCP (Model Context Protocol) tools from external servers are surfaced as regular `Tool` objects:

```rust
let mcp = McpManager::connect(&[
    McpServerConfig::stdio("db", "npx", &["-y", "@my/db-mcp"]),
]).await?;

Agent::builder()
    .tools(mcp.tool_definitions())  // MCP tools alongside built-ins
    .tools(cersei::tools::shell())
    .build()?;
```
