//! # Custom Provider
//!
//! Shows how to create a custom provider (e.g., for a local LLM like Ollama)
//! and also how to use the OpenAI provider with a custom base URL.
//!
//! ```bash
//! # With Ollama running locally:
//! cargo run --example custom_provider
//!
//! # With OpenAI:
//! OPENAI_API_KEY=sk-... cargo run --example custom_provider
//! ```

use cersei::prelude::*;
use cersei::provider::{CompletionStream, ProviderCapabilities};
use tokio::sync::mpsc;

// ─── Echo Provider (for testing without an API key) ──────────────────────────

/// A mock provider that echoes back the prompt. Useful for testing tool wiring.
struct EchoProvider;

#[async_trait]
impl Provider for EchoProvider {
    fn name(&self) -> &str { "echo" }
    fn context_window(&self, _model: &str) -> u64 { 4096 }

    fn capabilities(&self, _model: &str) -> ProviderCapabilities {
        ProviderCapabilities {
            streaming: true,
            tool_use: false, // Echo provider doesn't call tools
            vision: false,
            thinking: false,
            system_prompt: true,
            caching: false,
        }
    }

    async fn complete(&self, request: CompletionRequest) -> cersei_types::Result<CompletionStream> {
        let last_msg = request
            .messages
            .last()
            .and_then(|m| m.get_text())
            .unwrap_or("(empty)")
            .to_string();

        let response_text = format!(
            "[Echo] Received: \"{}\"\nModel: {}\nSystem: {}\nTools available: {}",
            last_msg,
            request.model,
            request.system.as_deref().unwrap_or("(none)"),
            request.tools.len()
        );

        // Emit as a streaming response
        let (tx, rx) = mpsc::channel(16);
        tokio::spawn(async move {
            let _ = tx.send(StreamEvent::MessageStart {
                id: "echo-1".into(),
                model: "echo".into(),
            }).await;
            let _ = tx.send(StreamEvent::ContentBlockStart {
                index: 0,
                block_type: "text".into(),
                id: None,
                name: None,
            }).await;

            // Stream word by word for demonstration
            for word in response_text.split_inclusive(' ') {
                let _ = tx.send(StreamEvent::TextDelta {
                    index: 0,
                    text: word.to_string(),
                }).await;
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }

            let _ = tx.send(StreamEvent::ContentBlockStop { index: 0 }).await;
            let _ = tx.send(StreamEvent::MessageDelta {
                stop_reason: Some(StopReason::EndTurn),
                usage: Some(Usage {
                    input_tokens: last_msg.len() as u64 / 4,
                    output_tokens: response_text.len() as u64 / 4,
                    ..Default::default()
                }),
            }).await;
            let _ = tx.send(StreamEvent::MessageStop).await;
        });

        Ok(CompletionStream::new(rx))
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // ── Echo provider (no API key needed) ─────────────────────────────
    println!("\x1b[36m── Echo Provider (mock) ──\x1b[0m\n");

    let output = Agent::builder()
        .provider(EchoProvider)
        .tools(cersei::tools::filesystem())
        .system_prompt("You are a test assistant.")
        .max_turns(1)
        .permission_policy(AllowAll)
        .run_with("Hello from the custom provider example!")
        .await?;

    println!("{}", output.text());
    println!("\nTurns: {}, Tokens: {}in/{}out",
        output.turns, output.usage.input_tokens, output.usage.output_tokens);

    // ── OpenAI-compatible provider (Ollama, etc.) ─────────────────────
    println!("\n\x1b[36m── OpenAI-Compatible Provider ──\x1b[0m\n");

    // This would work with Ollama, LM Studio, vLLM, etc.
    // Uncomment and set the right base URL:
    //
    // let provider = cersei::OpenAi::builder()
    //     .base_url("http://localhost:11434/v1")  // Ollama
    //     .model("llama3.1:8b")
    //     .api_key("ollama")  // Ollama doesn't check keys
    //     .build()?;
    //
    // let output = Agent::builder()
    //     .provider(provider)
    //     .tools(cersei::tools::coding())
    //     .run_with("Hello from Ollama!")
    //     .await?;

    println!("(Uncomment the Ollama section in the source to test with a local LLM)");

    Ok(())
}
