//! Portkey Models pricing and the local Abstract pricing cache.
//!
//! Portkey publishes provider catalogs without authentication at
//! `https://configs.portkey.ai/pricing/{provider}.json`. Catalog prices are
//! cents per token, so the exact conversion to USD per million tokens is
//! `price * 10_000`.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

pub const CATALOG_TTL: Duration = Duration::from_secs(24 * 60 * 60);
const PORTKEY_CATALOG_URL: &str = "https://configs.portkey.ai/pricing/{provider}.json";
const CACHE_FILE_NAME: &str = "pricing_cache.json";
const CACHE_PATH_ENV: &str = "CERSEI_PRICING_CACHE";

/// Rates normalized to USD per million tokens.
///
/// Cache rates remain optional so an absent Portkey field cannot be confused
/// with an explicit zero price. A missing rate only makes a calculation
/// unavailable when the corresponding usage counter is non-zero.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ModelRate {
    pub input_per_m: f64,
    pub output_per_m: f64,
    pub cache_read_per_m: Option<f64>,
    pub cache_write_per_m: Option<f64>,
}

impl ModelRate {
    pub fn cost(&self, usage: &cersei_types::Usage) -> Option<f64> {
        let cache_read = optional_cost(usage.cache_read_input_tokens, self.cache_read_per_m)?;
        let cache_write = optional_cost(usage.cache_creation_input_tokens, self.cache_write_per_m)?;

        Some(
            tokens_cost(usage.input_tokens, self.input_per_m)
                + tokens_cost(usage.output_tokens, self.output_per_m)
                + cache_read
                + cache_write,
        )
    }
}

fn tokens_cost(tokens: u64, usd_per_m: f64) -> f64 {
    (tokens as f64 / 1_000_000.0) * usd_per_m
}

fn optional_cost(tokens: u64, rate: Option<f64>) -> Option<f64> {
    if tokens == 0 {
        Some(0.0)
    } else {
        rate.map(|rate| tokens_cost(tokens, rate))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderCatalog {
    pub fetched_at: u64,
    pub models: HashMap<String, ModelRate>,
}

impl ProviderCatalog {
    pub fn is_fresh(&self, now_secs: u64) -> bool {
        now_secs.saturating_sub(self.fetched_at) < CATALOG_TTL.as_secs()
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PricingCache {
    pub providers: HashMap<String, ProviderCatalog>,
}

fn price_to_usd_per_m(price: f64) -> f64 {
    price * 10_000.0
}

fn parse_price(value: Option<&serde_json::Value>) -> Option<f64> {
    value
        .and_then(|value| value.get("price"))
        .and_then(serde_json::Value::as_f64)
        .filter(|price| price.is_finite() && *price >= 0.0)
        .map(price_to_usd_per_m)
}

/// Parse one real Portkey provider catalog.
///
/// Input and output prices are required for a usable text-model entry. Cache
/// prices may be absent or null; that absence is preserved as `None` rather
/// than silently turned into a free cache operation.
pub fn parse_catalog(json: &str) -> Result<ProviderCatalog, serde_json::Error> {
    let root: serde_json::Value = serde_json::from_str(json)?;
    let Some(entries) = root.as_object() else {
        return Err(<serde_json::Error as serde::de::Error>::custom(
            "Portkey catalog root must be an object",
        ));
    };

    let mut models = HashMap::new();
    for (model, entry) in entries {
        if model == "default" {
            continue;
        }

        let Some(payg) = entry
            .get("pricing_config")
            .and_then(|config| config.get("pay_as_you_go"))
        else {
            continue;
        };

        let Some(input_per_m) = parse_price(payg.get("request_token")) else {
            continue;
        };
        let Some(output_per_m) = parse_price(payg.get("response_token")) else {
            continue;
        };

        models.insert(
            model.clone(),
            ModelRate {
                input_per_m,
                output_per_m,
                cache_read_per_m: parse_price(payg.get("cache_read_input_token")),
                cache_write_per_m: parse_price(payg.get("cache_write_input_token")),
            },
        );
    }

    if models.is_empty() {
        return Err(<serde_json::Error as serde::de::Error>::custom(
            "Portkey catalog contains no complete token pricing entries",
        ));
    }

    Ok(ProviderCatalog {
        fetched_at: 0,
        models,
    })
}

pub fn cache_path() -> PathBuf {
    if let Ok(path) = std::env::var(CACHE_PATH_ENV) {
        return PathBuf::from(path);
    }
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".abstract")
        .join(CACHE_FILE_NAME)
}

pub fn load_cache(path: &Path) -> PricingCache {
    match std::fs::read_to_string(path) {
        Ok(text) => serde_json::from_str(&text).unwrap_or_else(|error| {
            tracing::warn!(path = %path.display(), %error, "corrupted pricing cache; ignoring");
            PricingCache::default()
        }),
        Err(_) => PricingCache::default(),
    }
}

/// Best-effort atomic persistence. The existing cache is not touched unless a
/// complete, flushed JSON document is ready in the same directory.
pub fn save_cache(path: &Path, cache: &PricingCache) -> bool {
    let Ok(json) = serde_json::to_vec_pretty(cache) else {
        return false;
    };
    let Some(parent) = path.parent() else {
        return false;
    };
    if let Err(error) = std::fs::create_dir_all(parent) {
        tracing::warn!(path = %parent.display(), %error, "cannot create pricing cache directory");
        return false;
    }

    let mut temporary = match tempfile::Builder::new()
        .prefix(".pricing_cache.")
        .tempfile_in(parent)
    {
        Ok(file) => file,
        Err(error) => {
            tracing::warn!(path = %parent.display(), %error, "cannot create pricing cache temp file");
            return false;
        }
    };

    if let Err(error) = temporary
        .write_all(&json)
        .and_then(|_| temporary.as_file().sync_all())
    {
        tracing::warn!(path = %temporary.path().display(), %error, "cannot write pricing cache");
        return false;
    }

    match temporary.persist(path) {
        Ok(_) => true,
        Err(error) => {
            tracing::warn!(path = %path.display(), error = %error.error, "cannot replace pricing cache");
            false
        }
    }
}

pub fn split_identity(identity: &str) -> Option<(&str, &str)> {
    let (provider, model) = identity.split_once('/')?;
    if provider.is_empty() || model.is_empty() {
        None
    } else {
        Some((provider, model))
    }
}

pub fn resolve_from_cache(cache: &PricingCache, provider: &str, model: &str) -> Option<ModelRate> {
    cache.providers.get(provider)?.models.get(model).copied()
}

pub fn refresh_needed(cache: &PricingCache, provider: &str, model: &str, now_secs: u64) -> bool {
    match cache.providers.get(provider) {
        None => true,
        Some(catalog) => !catalog.is_fresh(now_secs) || !catalog.models.contains_key(model),
    }
}

static GLOBAL: once_cell::sync::Lazy<parking_lot::RwLock<Option<Arc<PricingCache>>>> =
    once_cell::sync::Lazy::new(|| parking_lot::RwLock::new(None));

#[cfg(test)]
static GLOBAL_TEST_LOCK: once_cell::sync::Lazy<parking_lot::Mutex<()>> =
    once_cell::sync::Lazy::new(|| parking_lot::Mutex::new(()));

fn global_cache() -> Arc<PricingCache> {
    if let Some(cache) = GLOBAL.read().as_ref() {
        return Arc::clone(cache);
    }
    let cache = Arc::new(load_cache(&cache_path()));
    *GLOBAL.write() = Some(Arc::clone(&cache));
    cache
}

pub fn resolve_rate(provider: &str, model: &str) -> Option<ModelRate> {
    resolve_from_cache(&global_cache(), provider, model)
}

pub fn resolve_identity(identity: &str) -> Option<ModelRate> {
    let (provider, model) = split_identity(identity)?;
    resolve_rate(provider, model)
}

#[doc(hidden)]
pub fn _set_global_cache_for_tests(cache: PricingCache) {
    *GLOBAL.write() = Some(Arc::new(cache));
}

#[cfg(test)]
pub fn _lock_global_cache_for_tests() -> parking_lot::MutexGuard<'static, ()> {
    GLOBAL_TEST_LOCK.lock()
}

fn merge_refresh_body(
    cache: &mut PricingCache,
    provider: &str,
    body: &str,
    fetched_at: u64,
) -> Result<(), serde_json::Error> {
    let mut catalog = parse_catalog(body)?;
    catalog.fetched_at = fetched_at;
    cache.providers.insert(provider.to_string(), catalog);
    Ok(())
}

async fn fetch_catalog(url: &str, timeout: Duration) -> Result<String, String> {
    let client = reqwest::Client::builder()
        .timeout(timeout)
        .build()
        .map_err(|error| error.to_string())?;
    let response = client
        .get(url)
        .send()
        .await
        .map_err(|error| error.to_string())?;
    if !response.status().is_success() {
        return Err(format!("HTTP {}", response.status()));
    }
    response.text().await.map_err(|error| error.to_string())
}

/// Refresh a provider catalog once. Every failure leaves both the in-memory
/// and on-disk last known-good caches unchanged.
pub async fn refresh_provider(provider: &str) -> bool {
    let url = PORTKEY_CATALOG_URL.replace("{provider}", provider);
    refresh_provider_from_url(provider, &url, Duration::from_secs(5)).await
}

async fn refresh_provider_from_url(provider: &str, url: &str, timeout: Duration) -> bool {
    let body = match fetch_catalog(url, timeout).await {
        Ok(body) => body,
        Err(error) => {
            tracing::warn!(provider, %error, "Portkey pricing refresh failed");
            return false;
        }
    };
    let fetched_at = unix_now();
    let mut updated = (*global_cache()).clone();
    if let Err(error) = merge_refresh_body(&mut updated, provider, &body, fetched_at) {
        tracing::warn!(provider, %error, "invalid Portkey pricing catalog");
        return false;
    }

    save_cache(&cache_path(), &updated);
    *GLOBAL.write() = Some(Arc::new(updated));
    true
}

/// Lookup, refresh on TTL expiry or a missing model, then lookup again.
/// Callers schedule this outside rendering and outside the LLM request path.
pub async fn refresh_for_identity(identity: &str) -> Option<ModelRate> {
    let (provider, model) = split_identity(identity)?;
    let now = unix_now();
    let needs_refresh = refresh_needed(&global_cache(), provider, model, now);
    if needs_refresh {
        refresh_provider(provider).await;
    }
    resolve_rate(provider, model)
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_close(actual: f64, expected: f64) {
        assert!(
            (actual - expected).abs() < 1e-12,
            "expected {expected}, got {actual}"
        );
    }

    fn fixture(name: &str) -> String {
        std::fs::read_to_string(
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures")
                .join(name),
        )
        .expect("fixture exists")
    }

    #[test]
    fn cents_per_token_conversion_is_exact() {
        assert_close(price_to_usd_per_m(0.000014), 0.14);
        assert_close(price_to_usd_per_m(0.000028), 0.28);
        assert_close(price_to_usd_per_m(0.0003), 3.0);
        assert_close(price_to_usd_per_m(0.0015), 15.0);
        assert_close(price_to_usd_per_m(0.0), 0.0);
    }

    #[test]
    fn parses_real_deepseek_catalog_and_all_four_rates() {
        let catalog = parse_catalog(&fixture("portkey_deepseek.json")).expect("valid catalog");
        let rate = catalog.models.get("deepseek-chat").expect("known model");
        assert_close(rate.input_per_m, 0.14);
        assert_close(rate.output_per_m, 0.28);
        assert_close(rate.cache_read_per_m.unwrap(), 0.0028);
        assert_eq!(rate.cache_write_per_m, Some(0.0));
    }

    #[test]
    fn parses_other_provider_fixtures() {
        let anthropic = parse_catalog(&fixture("portkey_anthropic.json")).unwrap();
        let sonnet = anthropic.models["claude-3-5-sonnet-20241022"];
        assert_close(sonnet.input_per_m, 3.0);
        assert_close(sonnet.output_per_m, 15.0);
        assert_close(sonnet.cache_read_per_m.unwrap(), 0.3);
        assert_close(sonnet.cache_write_per_m.unwrap(), 3.75);

        let openai = parse_catalog(&fixture("portkey_openai.json")).unwrap();
        assert_close(openai.models["gpt-4o"].cache_read_per_m.unwrap(), 1.25);
        assert!(parse_catalog(&fixture("portkey_google.json")).is_ok());
    }

    #[test]
    fn absent_and_null_cache_fields_remain_absent_but_zero_is_preserved() {
        let catalog = parse_catalog(
            r#"{
                "absent": {"pricing_config":{"pay_as_you_go":{
                    "request_token":{"price":1},"response_token":{"price":2}
                }}},
                "null": {"pricing_config":{"pay_as_you_go":{
                    "request_token":{"price":1},"response_token":{"price":2},
                    "cache_read_input_token":{"price":null},
                    "cache_write_input_token":null
                }}},
                "zero": {"pricing_config":{"pay_as_you_go":{
                    "request_token":{"price":0},"response_token":{"price":0},
                    "cache_read_input_token":{"price":0},
                    "cache_write_input_token":{"price":0}
                }}}
            }"#,
        )
        .unwrap();

        assert_eq!(catalog.models["absent"].cache_read_per_m, None);
        assert_eq!(catalog.models["null"].cache_write_per_m, None);
        assert_eq!(catalog.models["zero"].cache_read_per_m, Some(0.0));
        assert_eq!(catalog.models["zero"].input_per_m, 0.0);
    }

    #[test]
    fn incomplete_or_invalid_catalog_is_rejected() {
        assert!(parse_catalog("{not json").is_err());
        assert!(parse_catalog("[]").is_err());
        assert!(parse_catalog(
            r#"{"model":{"pricing_config":{"pay_as_you_go":{"request_token":{"price":1}}}}}"#
        )
        .is_err());
        assert!(parse_catalog(
            r#"{"model":{"pricing_config":{"pay_as_you_go":{"request_token":{"price":null},"response_token":{"price":1}}}}}"#
        )
        .is_err());
    }

    #[test]
    fn formula_prices_each_counter_once() {
        let rate = ModelRate {
            input_per_m: 3.0,
            output_per_m: 15.0,
            cache_read_per_m: Some(0.3),
            cache_write_per_m: Some(3.75),
        };
        let usage = cersei_types::Usage {
            input_tokens: 100_000,
            output_tokens: 20_000,
            cache_read_input_tokens: 200_000,
            cache_creation_input_tokens: 50_000,
            ..Default::default()
        };
        assert_close(rate.cost(&usage).unwrap(), 0.8475);
    }

    #[test]
    fn absent_cache_rate_only_blocks_usage_that_needs_it() {
        let rate = ModelRate {
            input_per_m: 1.0,
            output_per_m: 2.0,
            cache_read_per_m: None,
            cache_write_per_m: None,
        };
        assert_eq!(
            rate.cost(&cersei_types::Usage {
                input_tokens: 1_000_000,
                output_tokens: 1_000_000,
                ..Default::default()
            }),
            Some(3.0)
        );
        assert_eq!(
            rate.cost(&cersei_types::Usage {
                cache_read_input_tokens: 1,
                ..Default::default()
            }),
            None
        );
    }

    #[test]
    fn identities_are_strict_and_never_guessed_from_model_names() {
        assert_eq!(
            split_identity("deepseek/deepseek-chat"),
            Some(("deepseek", "deepseek-chat"))
        );
        assert_eq!(
            split_identity("openrouter/anthropic/claude-sonnet"),
            Some(("openrouter", "anthropic/claude-sonnet"))
        );
        assert_eq!(split_identity("deepseek-chat"), None);
        assert_eq!(split_identity("/deepseek-chat"), None);
    }

    #[test]
    fn provider_and_model_must_both_match() {
        let mut cache = PricingCache::default();
        let mut deepseek = parse_catalog(&fixture("portkey_deepseek.json")).unwrap();
        deepseek.fetched_at = 1;
        cache.providers.insert("deepseek".into(), deepseek);

        assert!(resolve_from_cache(&cache, "deepseek", "deepseek-chat").is_some());
        assert!(resolve_from_cache(&cache, "openai", "deepseek-chat").is_none());
        assert!(resolve_from_cache(&cache, "deepseek", "unknown").is_none());
    }

    #[test]
    fn ttl_and_missing_model_force_refresh() {
        let now = 1_000_000;
        let mut cache = PricingCache::default();
        let mut catalog = parse_catalog(&fixture("portkey_deepseek.json")).unwrap();
        catalog.fetched_at = now;
        cache.providers.insert("deepseek".into(), catalog);

        assert!(!refresh_needed(&cache, "deepseek", "deepseek-chat", now));
        assert!(refresh_needed(&cache, "deepseek", "not-yet-listed", now));
        assert!(refresh_needed(&cache, "unknown", "model", now));
        assert!(refresh_needed(
            &cache,
            "deepseek",
            "deepseek-chat",
            now + CATALOG_TTL.as_secs()
        ));
    }

    #[test]
    fn valid_refresh_replaces_catalog_and_invalid_refresh_preserves_it() {
        let mut cache = PricingCache::default();
        merge_refresh_body(&mut cache, "deepseek", &fixture("portkey_deepseek.json"), 1).unwrap();
        assert_eq!(cache.providers["deepseek"].fetched_at, 1);

        let before = cache.clone();
        assert!(merge_refresh_body(&mut cache, "deepseek", r#"{"error":"down"}"#, 2).is_err());
        assert_eq!(cache.providers["deepseek"].fetched_at, 1);
        assert_eq!(
            cache.providers["deepseek"].models,
            before.providers["deepseek"].models
        );
    }

    #[test]
    fn successful_refresh_without_requested_model_stays_unknown() {
        let mut cache = PricingCache::default();
        merge_refresh_body(&mut cache, "deepseek", &fixture("portkey_deepseek.json"), 1).unwrap();

        assert!(resolve_from_cache(&cache, "deepseek", "not-listed").is_none());
    }

    #[test]
    fn cache_roundtrip_replaces_existing_file_on_windows() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pricing_cache.json");
        let mut cache = PricingCache::default();
        merge_refresh_body(&mut cache, "deepseek", &fixture("portkey_deepseek.json"), 1).unwrap();
        assert!(save_cache(&path, &cache));

        cache.providers.get_mut("deepseek").unwrap().fetched_at = 2;
        assert!(save_cache(&path, &cache));
        assert_eq!(load_cache(&path).providers["deepseek"].fetched_at, 2);
    }

    #[test]
    fn missing_and_corrupted_cache_load_empty() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("missing.json");
        assert!(load_cache(&missing).providers.is_empty());

        let corrupt = dir.path().join("corrupt.json");
        std::fs::write(&corrupt, "{broken").unwrap();
        assert!(load_cache(&corrupt).providers.is_empty());
    }

    #[tokio::test]
    async fn network_failure_is_non_fatal_and_returns_an_error() {
        let result = fetch_catalog(
            "http://127.0.0.1:9/portkey-offline-test",
            Duration::from_millis(100),
        )
        .await;
        assert!(result.is_err());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn failed_network_refresh_keeps_last_valid_cache() {
        let _guard = _lock_global_cache_for_tests();
        let mut cache = PricingCache::default();
        merge_refresh_body(&mut cache, "deepseek", &fixture("portkey_deepseek.json"), 1).unwrap();
        _set_global_cache_for_tests(cache);

        assert!(
            !refresh_provider_from_url(
                "deepseek",
                "http://127.0.0.1:9/portkey-offline-test",
                Duration::from_millis(100),
            )
            .await
        );
        assert!(resolve_rate("deepseek", "deepseek-chat").is_some());
    }

    #[test]
    fn empty_cache_keeps_price_unknown() {
        let _guard = _lock_global_cache_for_tests();
        _set_global_cache_for_tests(PricingCache::default());
        assert_eq!(resolve_rate("deepseek", "deepseek-chat"), None);
    }

    #[tokio::test]
    #[ignore = "optional live-network Portkey integration test"]
    async fn live_portkey_refresh() {
        assert!(refresh_provider("deepseek").await);
        assert!(resolve_rate("deepseek", "deepseek-chat").is_some());
    }
}
