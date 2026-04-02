# Memory

Memory provides session persistence and retrieval, enabling resumable conversations and long-term knowledge storage.

## The Memory Trait

```rust
#[async_trait]
pub trait Memory: Send + Sync {
    async fn store(&self, session_id: &str, messages: &[Message]) -> Result<()>;
    async fn load(&self, session_id: &str) -> Result<Vec<Message>>;
    async fn search(&self, query: &str, limit: usize) -> Result<Vec<MemoryEntry>>;
    async fn sessions(&self) -> Result<Vec<SessionInfo>>;
    async fn delete(&self, session_id: &str) -> Result<()>;
}
```

## Built-in Backends

### JsonlMemory

File-based storage using JSONL (one message per line). Each session is a separate file.

```rust
use cersei::memory::JsonlMemory;

let memory = JsonlMemory::new("/path/to/sessions");

let agent = Agent::builder()
    .provider(Anthropic::from_env()?)
    .memory(memory)
    .session_id("my-project-refactor")
    .build()?;
```

File structure:
```
/path/to/sessions/
  my-project-refactor.jsonl    # one JSON message per line
  other-session.jsonl
```

### InMemory

HashMap-backed store for tests and ephemeral agents.

```rust
use cersei::memory::InMemory;

let memory = InMemory::new();

let agent = Agent::builder()
    .memory(memory)
    .session_id("test-1")
    .build()?;
```

## Resumable Sessions

The key pattern: same `session_id` + same `Memory` backend = resumed conversation.

```rust
// Session 1
let agent = Agent::builder()
    .provider(Anthropic::from_env()?)
    .memory(JsonlMemory::new("./sessions"))
    .session_id("feature-x")
    .build()?;
agent.run("Start building the auth module").await?;

// Session 2 (later, even different process)
let agent = Agent::builder()
    .provider(Anthropic::from_env()?)
    .memory(JsonlMemory::new("./sessions"))
    .session_id("feature-x")  // same ID → loads previous messages
    .build()?;
agent.run("Continue. Add the JWT validation.").await?;
```

## Lifecycle

1. **On `agent.run()`**: If `memory` + `session_id` are set, `memory.load(session_id)` is called. Previous messages are prepended to the conversation.
2. **During execution**: New messages (user prompts, assistant responses, tool results) accumulate in memory.
3. **On completion**: `memory.store(session_id, all_messages)` persists the full conversation.

Events emitted:
- `AgentEvent::SessionLoaded { session_id, message_count }` — after loading
- `AgentEvent::SessionSaved { session_id }` — after persisting

## Session Management

```rust
let memory = JsonlMemory::new("./sessions");

// List all sessions
let sessions = memory.sessions().await?;
for s in &sessions {
    println!("{}: {} messages ({})", s.id, s.message_count, s.created_at);
}

// Delete a session
memory.delete("old-session").await?;
```

## Building a Custom Backend

Example: PostgreSQL-backed memory.

```rust
struct PgMemory { pool: sqlx::PgPool }

#[async_trait]
impl Memory for PgMemory {
    async fn store(&self, session_id: &str, messages: &[Message]) -> Result<()> {
        let json = serde_json::to_string(messages)?;
        sqlx::query("INSERT INTO sessions (id, messages) VALUES ($1, $2)
                      ON CONFLICT (id) DO UPDATE SET messages = $2")
            .bind(session_id)
            .bind(&json)
            .execute(&self.pool)
            .await
            .map_err(|e| CerseiError::Other(e.into()))?;
        Ok(())
    }

    async fn load(&self, session_id: &str) -> Result<Vec<Message>> {
        let row = sqlx::query_scalar::<_, String>(
            "SELECT messages FROM sessions WHERE id = $1"
        )
        .bind(session_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| CerseiError::Other(e.into()))?;

        match row {
            Some(json) => Ok(serde_json::from_str(&json)?),
            None => Ok(Vec::new()),
        }
    }

    async fn search(&self, query: &str, limit: usize) -> Result<Vec<MemoryEntry>> {
        // Full-text search with pg_trgm or vector search with pgvector
        todo!()
    }

    async fn sessions(&self) -> Result<Vec<SessionInfo>> {
        // SELECT id, message_count, created_at FROM sessions
        todo!()
    }

    async fn delete(&self, session_id: &str) -> Result<()> {
        sqlx::query("DELETE FROM sessions WHERE id = $1")
            .bind(session_id)
            .execute(&self.pool)
            .await
            .map_err(|e| CerseiError::Other(e.into()))?;
        Ok(())
    }
}
```
