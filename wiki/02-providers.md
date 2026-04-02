# Providers

A `Provider` abstracts over an LLM backend. It handles authentication, request formatting, SSE streaming, and capability discovery.

## The Provider Trait

```rust
#[async_trait]
pub trait Provider: Send + Sync {
    fn name(&self) -> &str;
    fn context_window(&self, model: &str) -> u64;
    fn capabilities(&self, model: &str) -> ProviderCapabilities;
    async fn complete(&self, request: CompletionRequest) -> Result<CompletionStream>;
    async fn complete_blocking(&self, request: CompletionRequest) -> Result<CompletionResponse>;
    async fn count_tokens(&self, messages: &[Message], model: &str) -> Result<u64>;
}
```

## Built-in Providers

### Anthropic

Full streaming SSE support, thinking/reasoning, tool use, vision, caching.

```rust
// From environment variable
let provider = Anthropic::from_env()?;  // reads ANTHROPIC_API_KEY

// Explicit API key
let provider = Anthropic::new(Auth::ApiKey("sk-ant-...".into()));

// Builder with full configuration
let provider = Anthropic::builder()
    .api_key("sk-ant-...")
    .model("claude-opus-4-6")           // default model
    .base_url("https://api.anthropic.com") // custom endpoint
    .thinking(8192)                      // enable thinking with budget
    .max_retries(5)                      // retry on 429/529
    .build()?;

// OAuth (for Claude.ai authentication)
let provider = Anthropic::builder()
    .oauth(OAuthToken {
        access_token: "...".into(),
        refresh_token: Some("...".into()),
        expires_at_ms: Some(1234567890000),
        scopes: vec!["...".into()],
    })
    .build()?;
```

### OpenAI

Compatible with OpenAI, Azure OpenAI, Ollama, LM Studio, vLLM, and any OpenAI-compatible API.

```rust
// From environment variable
let provider = OpenAi::from_env()?;  // reads OPENAI_API_KEY

// Ollama (local)
let provider = OpenAi::builder()
    .base_url("http://localhost:11434/v1")
    .model("llama3.1:70b")
    .api_key("ollama")  // Ollama ignores the key
    .build()?;

// Azure OpenAI
let provider = OpenAi::builder()
    .base_url("https://my-resource.openai.azure.com/openai/deployments/gpt-4o")
    .api_key("azure-key")
    .build()?;
```

## Authentication

The `Auth` enum supports four modes:

```rust
pub enum Auth {
    ApiKey(String),                                    // x-api-key header
    Bearer(String),                                    // Authorization: Bearer
    OAuth { client_id: String, token: OAuthToken },    // OAuth2 flow
    Custom(Arc<dyn AuthProvider>),                      // anything else
}
```

### Custom Auth Provider

```rust
#[derive(Debug)]
struct MyAuth { /* ... */ }

#[async_trait]
impl AuthProvider for MyAuth {
    async fn get_credentials(&self) -> Result<(String, String)> {
        // Return (header_name, header_value)
        Ok(("x-custom-auth".into(), "my-token".into()))
    }

    async fn refresh(&self) -> Result<()> {
        // Refresh expired credentials
        Ok(())
    }
}
```

## Building a Custom Provider

Implement the `Provider` trait for any LLM backend:

```rust
struct MyLlm { base_url: String }

#[async_trait]
impl Provider for MyLlm {
    fn name(&self) -> &str { "my-llm" }

    fn context_window(&self, _model: &str) -> u64 { 8192 }

    fn capabilities(&self, _model: &str) -> ProviderCapabilities {
        ProviderCapabilities {
            streaming: true,
            tool_use: true,
            vision: false,
            thinking: false,
            system_prompt: true,
            caching: false,
        }
    }

    async fn complete(&self, request: CompletionRequest) -> Result<CompletionStream> {
        let (tx, rx) = tokio::sync::mpsc::channel(256);

        // Spawn async task to send StreamEvents
        tokio::spawn(async move {
            tx.send(StreamEvent::MessageStart { id: "1".into(), model: "my-llm".into() }).await.ok();
            tx.send(StreamEvent::ContentBlockStart { index: 0, block_type: "text".into() }).await.ok();
            tx.send(StreamEvent::TextDelta { index: 0, text: "Hello!".into() }).await.ok();
            tx.send(StreamEvent::ContentBlockStop { index: 0 }).await.ok();
            tx.send(StreamEvent::MessageDelta {
                stop_reason: Some(StopReason::EndTurn),
                usage: Some(Usage { input_tokens: 10, output_tokens: 5, ..Default::default() }),
            }).await.ok();
            tx.send(StreamEvent::MessageStop).await.ok();
        });

        Ok(CompletionStream::new(rx))
    }
}
```

## Provider Options

Provider-specific configuration is passed via `ProviderOptions`:

```rust
let mut options = ProviderOptions::default();
options.set("thinking_budget", 8192u32);
options.set("top_p", 0.9f32);

// Read back
let budget: Option<u32> = options.get("thinking_budget");
```

The `Agent` builder exposes common options directly:

```rust
Agent::builder()
    .thinking_budget(8192)  // sets options["thinking_budget"]
    .temperature(0.7)       // sets request.temperature
```

## CompletionRequest

The request sent to a provider:

```rust
pub struct CompletionRequest {
    pub model: String,
    pub messages: Vec<Message>,
    pub system: Option<String>,
    pub tools: Vec<ToolDefinition>,
    pub max_tokens: u32,
    pub temperature: Option<f32>,
    pub stop_sequences: Vec<String>,
    pub options: ProviderOptions,
}
```

## StreamEvent

Events emitted by the provider's streaming response:

| Event | Description |
|-------|-------------|
| `MessageStart { id, model }` | Response begins |
| `ContentBlockStart { index, block_type }` | New content block (text, tool_use, thinking) |
| `TextDelta { index, text }` | Incremental text |
| `InputJsonDelta { index, partial_json }` | Incremental tool input JSON |
| `ThinkingDelta { index, thinking }` | Incremental thinking text |
| `ContentBlockStop { index }` | Content block finished |
| `MessageDelta { stop_reason, usage }` | Response metadata |
| `MessageStop` | Response complete |
| `Error { message }` | Error occurred |
| `Ping` | Keep-alive |
