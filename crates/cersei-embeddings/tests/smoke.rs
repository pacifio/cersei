//! Network-hitting smoke tests. Run with `cargo test -p cersei-embeddings -- --ignored`.

use cersei_embeddings::{EmbeddingStore, Metric, OpenAiEmbeddings};

#[tokio::test]
#[ignore]
async fn openai_store_roundtrip() {
    let provider = OpenAiEmbeddings::from_env().expect("OPENAI_API_KEY not set");
    let store = EmbeddingStore::new(provider, Metric::Cosine).expect("store");
    store
        .add_batch(&[
            (1, "Rust is a systems programming language".into()),
            (2, "Pasta is best served al dente".into()),
        ])
        .await
        .expect("add_batch");

    let hits = store.search("compiled languages", 2).await.expect("search");
    assert!(!hits.is_empty());
    assert_eq!(
        hits[0].key, 1,
        "expected programming result first, got {:?}",
        hits
    );
}
