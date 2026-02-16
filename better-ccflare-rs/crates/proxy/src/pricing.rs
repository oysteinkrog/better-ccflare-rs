//! Token pricing engine — three-tier pricing with bundled fallback.
//!
//! Pricing sources (in priority order):
//! 1. Remote LiteLLM pricing (fetched on startup, refreshed every 24h)
//! 2. Bundled fallback (hardcoded prices for known models)
//! 3. Returns 0 cost for unknown models (warns once)
//!
//! NanoGPT pricing is handled separately with in-memory 24h cache.

use std::collections::HashMap;
use std::sync::{Arc, OnceLock, RwLock};

use arc_swap::ArcSwap;
use tracing::{info, warn};

use crate::streaming::StreamUsage;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// LiteLLM pricing JSON URL.
const LITELLM_PRICING_URL: &str =
    "https://raw.githubusercontent.com/BerriAI/litellm/main/model_prices_and_context_window.json";

/// Remote pricing refresh interval (24 hours).
pub const REMOTE_CACHE_TTL_MS: i64 = 24 * 60 * 60 * 1000;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// Per-model cost rates (dollars per 1M tokens).
#[derive(Debug, Clone, Copy)]
pub struct ModelCost {
    pub input: f64,
    pub output: f64,
    pub cache_read: f64,
    pub cache_write: f64,
}

/// Token breakdown for cost calculation.
#[derive(Debug, Clone, Default)]
pub struct TokenBreakdown {
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cache_read_input_tokens: i64,
    pub cache_creation_input_tokens: i64,
}

impl From<&StreamUsage> for TokenBreakdown {
    fn from(usage: &StreamUsage) -> Self {
        Self {
            input_tokens: usage.input_tokens.unwrap_or(0),
            output_tokens: usage.output_tokens.unwrap_or(0),
            cache_read_input_tokens: usage.cache_read_input_tokens.unwrap_or(0),
            cache_creation_input_tokens: usage.cache_creation_input_tokens.unwrap_or(0),
        }
    }
}

// ---------------------------------------------------------------------------
// Remote pricing store (ArcSwap)
// ---------------------------------------------------------------------------

/// Global remote pricing table, populated from LiteLLM on startup.
fn remote_pricing() -> &'static ArcSwap<HashMap<String, ModelCost>> {
    static STORE: OnceLock<ArcSwap<HashMap<String, ModelCost>>> = OnceLock::new();
    STORE.get_or_init(|| ArcSwap::from_pointee(HashMap::new()))
}

// ---------------------------------------------------------------------------
// Bundled pricing
// ---------------------------------------------------------------------------

/// Build the bundled pricing table (known models with hardcoded prices).
fn bundled_pricing() -> &'static HashMap<&'static str, ModelCost> {
    static PRICING: OnceLock<HashMap<&'static str, ModelCost>> = OnceLock::new();
    PRICING.get_or_init(|| {
        let mut m = HashMap::new();

        // Anthropic Claude models (dollars per 1M tokens)
        // Haiku 3
        m.insert(
            "claude-3-haiku-20240307",
            ModelCost {
                input: 0.25,
                output: 1.25,
                cache_read: 0.03,
                cache_write: 0.30,
            },
        );
        // Haiku 3.5
        m.insert(
            "claude-3-5-haiku-20241022",
            ModelCost {
                input: 0.8,
                output: 4.0,
                cache_read: 0.08,
                cache_write: 1.0,
            },
        );
        m.insert(
            "claude-3-5-haiku-latest",
            ModelCost {
                input: 0.8,
                output: 4.0,
                cache_read: 0.08,
                cache_write: 1.0,
            },
        );
        // Sonnet 3.5
        m.insert(
            "claude-3-5-sonnet-20241022",
            ModelCost {
                input: 3.0,
                output: 15.0,
                cache_read: 0.3,
                cache_write: 3.75,
            },
        );
        m.insert(
            "claude-3-5-sonnet-latest",
            ModelCost {
                input: 3.0,
                output: 15.0,
                cache_read: 0.3,
                cache_write: 3.75,
            },
        );
        // Haiku 4.5
        m.insert(
            "claude-haiku-4-5-20251001",
            ModelCost {
                input: 1.0,
                output: 5.0,
                cache_read: 0.1,
                cache_write: 1.25,
            },
        );
        // Sonnet 3.7
        m.insert(
            "claude-3-7-sonnet-20250219",
            ModelCost {
                input: 3.0,
                output: 15.0,
                cache_read: 0.3,
                cache_write: 3.75,
            },
        );
        // Sonnet 4 / 4.5
        m.insert(
            "claude-sonnet-4-20250514",
            ModelCost {
                input: 3.0,
                output: 15.0,
                cache_read: 0.3,
                cache_write: 3.75,
            },
        );
        m.insert(
            "claude-sonnet-4-5-20250929",
            ModelCost {
                input: 3.0,
                output: 15.0,
                cache_read: 0.3,
                cache_write: 3.75,
            },
        );
        // Opus 4
        m.insert(
            "claude-opus-4-20250514",
            ModelCost {
                input: 15.0,
                output: 75.0,
                cache_read: 1.5,
                cache_write: 18.75,
            },
        );
        // Opus 4.5
        m.insert(
            "claude-opus-4-5-20250414",
            ModelCost {
                input: 5.0,
                output: 25.0,
                cache_read: 0.5,
                cache_write: 6.25,
            },
        );
        // Opus 4.6
        m.insert(
            "claude-opus-4-6",
            ModelCost {
                input: 5.0,
                output: 25.0,
                cache_read: 0.5,
                cache_write: 6.25,
            },
        );
        // Opus 3
        m.insert(
            "claude-3-opus-20240229",
            ModelCost {
                input: 15.0,
                output: 75.0,
                cache_read: 1.5,
                cache_write: 18.75,
            },
        );

        // Zai/GLM models
        m.insert(
            "glm-4.5",
            ModelCost {
                input: 0.6,
                output: 2.2,
                cache_read: 0.0,
                cache_write: 0.0,
            },
        );
        m.insert(
            "glm-4.5-air",
            ModelCost {
                input: 0.2,
                output: 1.1,
                cache_read: 0.0,
                cache_write: 0.0,
            },
        );
        m.insert(
            "glm-4.6",
            ModelCost {
                input: 0.6,
                output: 2.2,
                cache_read: 0.0,
                cache_write: 0.0,
            },
        );
        m.insert(
            "glm-4.6-air",
            ModelCost {
                input: 0.2,
                output: 1.1,
                cache_read: 0.0,
                cache_write: 0.0,
            },
        );

        // Minimax
        m.insert(
            "MiniMax-M2",
            ModelCost {
                input: 0.3,
                output: 1.2,
                cache_read: 0.0,
                cache_write: 0.0,
            },
        );

        m
    })
}

// ---------------------------------------------------------------------------
// Remote pricing fetch
// ---------------------------------------------------------------------------

/// Per-token cost to per-million-token cost.
fn per_token_to_per_mtok(per_token: f64) -> f64 {
    per_token * 1_000_000.0
}

/// Fetch pricing from LiteLLM and store in the global ArcSwap.
///
/// Only keeps bare model keys (no provider prefix like "anthropic." or "azure_ai/").
/// Models without `input_cost_per_token` are skipped.
pub async fn refresh_remote_pricing() {
    info!("Fetching remote pricing from LiteLLM...");

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build();

    let client = match client {
        Ok(c) => c,
        Err(e) => {
            warn!("Failed to build HTTP client for pricing fetch: {e}");
            return;
        }
    };

    let resp = match client.get(LITELLM_PRICING_URL).send().await {
        Ok(r) => r,
        Err(e) => {
            warn!("Failed to fetch LiteLLM pricing: {e}");
            return;
        }
    };

    if !resp.status().is_success() {
        warn!(
            status = %resp.status(),
            "LiteLLM pricing fetch returned non-200"
        );
        return;
    }

    let body = match resp.bytes().await {
        Ok(b) => b,
        Err(e) => {
            warn!("Failed to read LiteLLM pricing response body: {e}");
            return;
        }
    };

    let raw: HashMap<String, serde_json::Value> = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(e) => {
            warn!("Failed to parse LiteLLM pricing JSON: {e}");
            return;
        }
    };

    let mut pricing = HashMap::new();

    for (key, obj) in &raw {
        // Skip provider-prefixed keys (e.g. "anthropic.claude-*", "azure_ai/claude-*")
        if key.contains('/') || key.contains('.') {
            continue;
        }

        let input = match obj.get("input_cost_per_token").and_then(|v| v.as_f64()) {
            Some(v) => v,
            None => continue,
        };
        let output = obj
            .get("output_cost_per_token")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0);
        let cache_read = obj
            .get("cache_read_input_token_cost")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0);
        let cache_write = obj
            .get("cache_creation_input_token_cost")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0);

        pricing.insert(
            key.clone(),
            ModelCost {
                input: per_token_to_per_mtok(input),
                output: per_token_to_per_mtok(output),
                cache_read: per_token_to_per_mtok(cache_read),
                cache_write: per_token_to_per_mtok(cache_write),
            },
        );
    }

    let count = pricing.len();
    remote_pricing().store(Arc::new(pricing));
    // Clear fuzzy cache since remote pricing changed
    if let Ok(mut cache) = fuzzy_cache().write() {
        cache.clear();
    }
    info!("Loaded {count} model prices from LiteLLM");
}

// ---------------------------------------------------------------------------
// Pricing catalog
// ---------------------------------------------------------------------------

/// Cache for fuzzy pricing lookups (avoids repeated O(n) scans).
fn fuzzy_cache() -> &'static RwLock<HashMap<String, Option<ModelCost>>> {
    static CACHE: OnceLock<RwLock<HashMap<String, Option<ModelCost>>>> = OnceLock::new();
    CACHE.get_or_init(|| RwLock::new(HashMap::new()))
}

/// Get the pricing for a model, searching remote then bundled pricing with fuzzy matching.
///
/// Tries exact match first (remote, then bundled), then cached fuzzy, then linear fuzzy scan.
pub fn get_model_pricing(model: &str) -> Option<ModelCost> {
    // 1. Check remote pricing (exact match)
    let remote = remote_pricing().load();
    if let Some(cost) = remote.get(model) {
        return Some(*cost);
    }

    // 2. Check bundled pricing (exact match)
    let bundled = bundled_pricing();
    if let Some(cost) = bundled.get(model) {
        return Some(*cost);
    }

    // 3. Check fuzzy cache
    {
        let cache = fuzzy_cache().read().unwrap();
        if let Some(cached) = cache.get(model) {
            return *cached;
        }
    }

    // 4. Fuzzy: check remote pricing
    let model_lower = model.to_lowercase();
    for (key, cost) in remote.iter() {
        if model_lower.contains(key.as_str()) || key.contains(&*model_lower) {
            let mut cache = fuzzy_cache().write().unwrap();
            cache.insert(model.to_string(), Some(*cost));
            return Some(*cost);
        }
    }

    // 5. Fuzzy: check bundled pricing
    for (key, cost) in bundled.iter() {
        if model_lower.contains(key) || key.contains(&*model_lower) {
            let mut cache = fuzzy_cache().write().unwrap();
            cache.insert(model.to_string(), Some(*cost));
            return Some(*cost);
        }
    }

    // Cache the miss too (avoids repeated scans for unknown models)
    {
        let mut cache = fuzzy_cache().write().unwrap();
        cache.insert(model.to_string(), None);
    }

    None
}

/// Estimate the cost in USD for a given model and token breakdown.
///
/// Returns 0.0 for unknown models (with a warning).
pub fn estimate_cost_usd(model: &str, tokens: &TokenBreakdown) -> f64 {
    let Some(cost) = get_model_pricing(model) else {
        // Warn once pattern would be nice, but for now just trace
        warn!(model = model, "No pricing data for model, returning 0 cost");
        return 0.0;
    };

    let per_token = 1_000_000.0_f64;
    (tokens.input_tokens as f64 * cost.input / per_token)
        + (tokens.output_tokens as f64 * cost.output / per_token)
        + (tokens.cache_read_input_tokens as f64 * cost.cache_read / per_token)
        + (tokens.cache_creation_input_tokens as f64 * cost.cache_write / per_token)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_model_pricing() {
        let cost = get_model_pricing("claude-sonnet-4-5-20250929").unwrap();
        assert!((cost.input - 3.0).abs() < f64::EPSILON);
        assert!((cost.output - 15.0).abs() < f64::EPSILON);
    }

    #[test]
    fn fuzzy_model_pricing() {
        // Model ID that contains a known key as substring
        let cost = get_model_pricing("claude-3-5-haiku-20241022").unwrap();
        assert!((cost.input - 0.8).abs() < f64::EPSILON);
    }

    #[test]
    fn unknown_model_returns_none() {
        assert!(get_model_pricing("gpt-4o-unknown").is_none());
    }

    #[test]
    fn cost_calculation_basic() {
        let tokens = TokenBreakdown {
            input_tokens: 1_000_000,
            output_tokens: 500_000,
            cache_read_input_tokens: 0,
            cache_creation_input_tokens: 0,
        };
        let cost = estimate_cost_usd("claude-sonnet-4-5-20250929", &tokens);
        // 1M * 3/1M + 500K * 15/1M = 3.0 + 7.5 = 10.5
        assert!((cost - 10.5).abs() < 0.001);
    }

    #[test]
    fn cost_with_cache_tokens() {
        let tokens = TokenBreakdown {
            input_tokens: 100_000,
            output_tokens: 50_000,
            cache_read_input_tokens: 200_000,
            cache_creation_input_tokens: 10_000,
        };
        let cost = estimate_cost_usd("claude-sonnet-4-5-20250929", &tokens);
        // 100K * 3/1M + 50K * 15/1M + 200K * 0.3/1M + 10K * 3.75/1M
        // = 0.3 + 0.75 + 0.06 + 0.0375 = 1.1475
        assert!((cost - 1.1475).abs() < 0.001);
    }

    #[test]
    fn cost_unknown_model_returns_zero() {
        let tokens = TokenBreakdown {
            input_tokens: 1000,
            output_tokens: 500,
            ..Default::default()
        };
        let cost = estimate_cost_usd("unknown-model", &tokens);
        assert!((cost - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn glm_pricing() {
        let cost = get_model_pricing("glm-4.5-air").unwrap();
        assert!((cost.input - 0.2).abs() < f64::EPSILON);
        assert!((cost.output - 1.1).abs() < f64::EPSILON);
    }

    #[test]
    fn token_breakdown_from_stream_usage() {
        let usage = StreamUsage {
            model: Some("test".to_string()),
            input_tokens: Some(100),
            output_tokens: Some(50),
            cache_read_input_tokens: None,
            cache_creation_input_tokens: Some(10),
        };
        let breakdown: TokenBreakdown = (&usage).into();
        assert_eq!(breakdown.input_tokens, 100);
        assert_eq!(breakdown.output_tokens, 50);
        assert_eq!(breakdown.cache_read_input_tokens, 0);
        assert_eq!(breakdown.cache_creation_input_tokens, 10);
    }

    #[test]
    fn per_token_to_per_mtok_conversion() {
        // 3e-06 per token = 3.0 per MTok
        assert!((per_token_to_per_mtok(3e-06) - 3.0).abs() < 1e-10);
        // 1.5e-05 per token = 15.0 per MTok
        assert!((per_token_to_per_mtok(1.5e-05) - 15.0).abs() < 1e-10);
    }

    #[test]
    fn remote_pricing_overrides_bundled() {
        // Store a custom price in remote
        let mut custom = HashMap::new();
        custom.insert(
            "claude-sonnet-4-5-20250929".to_string(),
            ModelCost {
                input: 99.0,
                output: 99.0,
                cache_read: 99.0,
                cache_write: 99.0,
            },
        );
        remote_pricing().store(Arc::new(custom));

        let cost = get_model_pricing("claude-sonnet-4-5-20250929").unwrap();
        assert!((cost.input - 99.0).abs() < f64::EPSILON);

        // Restore empty remote so other tests use bundled
        remote_pricing().store(Arc::new(HashMap::new()));
    }
}
