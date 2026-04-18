//! # cersei-embeddings
//!
//! Provider-agnostic text embeddings + a `usearch`-backed vector index,
//! packaged as a standalone crate so any application can build semantic
//! search, retrieval-augmented generation, or custom clustering on top.
//!
//! ## Quick start
//!
//! ```no_run
//! use cersei_embeddings::{OpenAiEmbeddings, EmbeddingStore, Metric};
//!
//! # async fn run() -> Result<(), cersei_embeddings::EmbeddingError> {
//! let provider = OpenAiEmbeddings::from_env()?;
//! let store = EmbeddingStore::new(provider, Metric::Cosine)?;
//!
//! store.add_batch(&[
//!     (1, "Rust is a systems programming language".to_string()),
//!     (2, "Pasta is best served al dente".to_string()),
//! ]).await?;
//!
//! let hits = store.search("compiled languages", 1).await?;
//! assert_eq!(hits[0].key, 1);
//! # Ok(())
//! # }
//! ```
//!
//! ## Pieces
//!
//! - [`EmbeddingProvider`] — trait every embedding backend implements.
//! - [`GeminiEmbeddings`], [`OpenAiEmbeddings`] — built-in providers.
//! - [`VectorIndex`] — thin `usearch` wrapper (cosine / L2 / inner-product).
//! - [`EmbeddingStore`] — provider + index bundled together for the common case.
//! - [`auto_from_model`] — construct a provider from an LLM model string
//!   (`"gpt-4o"` → OpenAI embeddings, `"gemini-2.0-flash"` → Gemini).

mod error;
mod factory;
mod gemini;
mod index;
mod openai;
mod provider;
mod store;

pub use error::EmbeddingError;
pub use factory::auto_from_model;
pub use gemini::GeminiEmbeddings;
pub use index::{Metric, SearchHit, VectorIndex};
pub use openai::OpenAiEmbeddings;
pub use provider::EmbeddingProvider;
pub use store::EmbeddingStore;
