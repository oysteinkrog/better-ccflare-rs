//! Usage polling service — background polling for account usage data.
//!
//! Runs a tokio task per provider type (Anthropic, NanoGPT, Zai) that polls
//! at 90-second intervals and caches usage data in memory. The cache is
//! queried by the dashboard and API handlers.

use std::sync::Arc;
use std::time::Duration;

use dashmap::DashMap;
use serde_json::Value as JsonValue;
use tokio::sync::Notify;
use tokio::task::JoinHandle;
use tracing::{debug, info, warn};

use crate::impls::nanogpt::{self, NanoGptUsageData};
use crate::impls::zai::{self, ZaiUsageData};

// ---------------------------------------------------------------------------
// Anthropic usage types (from OAuth usage endpoint)
// ---------------------------------------------------------------------------

/// Anthropic usage window (from /api/oauth/usage).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AnthropicUsageWindow {
    pub utilization: f64,
    pub resets_at: Option<String>,
}

/// Anthropic usage data — flexible to handle new fields from API updates.
pub type AnthropicUsageData = serde_json::Map<String, JsonValue>;

// Re-export from core so consumers can use `bccf_providers::usage_polling::RoutingUsageInfo`.
pub use bccf_core::types::RoutingUsageInfo;

/// Union of all provider usage data types.
#[derive(Debug, Clone)]
pub enum AnyUsageData {
    Anthropic(AnthropicUsageData),
    NanoGpt(NanoGptUsageData),
    Zai(ZaiUsageData),
}

impl AnyUsageData {
    /// Serialize to JSON for API responses.
    pub fn to_json(&self) -> Option<JsonValue> {
        match self {
            AnyUsageData::Anthropic(data) => Some(JsonValue::Object(data.clone())),
            AnyUsageData::NanoGpt(data) => serde_json::to_value(data).ok(),
            AnyUsageData::Zai(data) => serde_json::to_value(data).ok(),
        }
    }

    /// Get representative utilization percentage (0-100).
    pub fn utilization(&self) -> Option<f64> {
        match self {
            AnyUsageData::Anthropic(data) => anthropic_utilization(data),
            AnyUsageData::NanoGpt(data) => nanogpt::nanogpt_utilization(data),
            AnyUsageData::Zai(data) => zai::zai_utilization(data),
        }
    }

    /// Normalized routing info for the load balancer.
    ///
    /// Returns utilization percentage (0-100) and the reset time (epoch ms) of
    /// the most restrictive usage window, normalised across all provider types.
    pub fn routing_info(&self) -> Option<RoutingUsageInfo> {
        match self {
            AnyUsageData::Anthropic(data) => anthropic_routing_info(data),
            AnyUsageData::NanoGpt(data) => nanogpt_routing_info(data),
            AnyUsageData::Zai(data) => zai_routing_info(data),
        }
    }

    /// Get the name of the most restrictive usage window.
    pub fn representative_window(&self) -> Option<String> {
        match self {
            AnyUsageData::Anthropic(data) => anthropic_representative_window(data),
            AnyUsageData::NanoGpt(data) => {
                nanogpt::nanogpt_representative_window(data).map(String::from)
            }
            AnyUsageData::Zai(data) => {
                if data.tokens_limit.is_some() {
                    Some("five_hour".to_string())
                } else {
                    None
                }
            }
        }
    }
}

/// Get the highest utilization across all Anthropic usage windows.
fn anthropic_utilization(data: &AnthropicUsageData) -> Option<f64> {
    let mut max_util: Option<f64> = None;

    for (_key, value) in data {
        if let Some(util) = value.get("utilization").and_then(|v| v.as_f64()) {
            max_util = Some(max_util.map_or(util, |m: f64| m.max(util)));
        }
    }

    max_util.or(Some(0.0))
}

/// Get the name of the most restrictive Anthropic usage window.
fn anthropic_representative_window(data: &AnthropicUsageData) -> Option<String> {
    let mut max_name = None;
    let mut max_util = f64::NEG_INFINITY;

    for (key, value) in data {
        if let Some(util) = value.get("utilization").and_then(|v| v.as_f64()) {
            if util > max_util {
                max_util = util;
                max_name = Some(key.clone());
            }
        }
    }

    max_name
}

// ---------------------------------------------------------------------------
// Routing info extractors (per-provider)
// ---------------------------------------------------------------------------

/// Extract routing info from Anthropic usage data.
///
/// Picks the window with the highest utilization and parses its `resets_at`
/// ISO-8601 string into epoch-ms.
fn anthropic_routing_info(data: &AnthropicUsageData) -> Option<RoutingUsageInfo> {
    use bccf_core::types::{WindowKind, WindowUsage};

    let mut best_util: f64 = 0.0;
    let mut best_reset: Option<i64> = None;
    let mut windows = Vec::new();

    for (key, value) in data {
        if let Some(util) = value.get("utilization").and_then(|v| v.as_f64()) {
            let reset_ms = value
                .get("resets_at")
                .and_then(|v| v.as_str())
                .and_then(|s| {
                    chrono::DateTime::parse_from_rfc3339(s)
                        .ok()
                        .map(|dt| dt.timestamp_millis())
                });

            // Normalise to 0-100
            let pct = if util < 2.0 { util * 100.0 } else { util };

            // Map key to WindowKind
            let kind = match key.as_str() {
                "five_hour" => WindowKind::FiveHour,
                "seven_day" | "seven_day_opus" | "seven_day_sonnet" => WindowKind::Weekly,
                _ => WindowKind::Other,
            };

            windows.push(WindowUsage {
                kind,
                utilization_pct: pct,
                resets_at_ms: reset_ms,
            });

            if util > best_util {
                best_util = util;
                best_reset = reset_ms;
            }
        }
    }

    // Normalise aggregate to 0-100
    let pct = if best_util < 2.0 {
        best_util * 100.0
    } else {
        best_util
    };

    Some(RoutingUsageInfo {
        utilization_pct: pct,
        resets_at_ms: best_reset,
        windows,
    })
}

/// Extract routing info from NanoGPT usage data.
///
/// Picks the most restrictive window (daily vs monthly) and uses its reset_at
/// epoch-ms directly. `percent_used` is 0.0-1.0 → multiply by 100.
fn nanogpt_routing_info(data: &NanoGptUsageData) -> Option<RoutingUsageInfo> {
    use bccf_core::types::{WindowKind, WindowUsage};

    if !data.active {
        return None;
    }
    let daily_pct = data.daily.percent_used * 100.0;
    let monthly_pct = data.monthly.percent_used * 100.0;

    let (pct, reset) = if daily_pct >= monthly_pct {
        (daily_pct, data.daily.reset_at)
    } else {
        (monthly_pct, data.monthly.reset_at)
    };

    let windows = vec![
        WindowUsage {
            kind: WindowKind::Other,
            utilization_pct: daily_pct,
            resets_at_ms: if data.daily.reset_at > 0 { Some(data.daily.reset_at) } else { None },
        },
        WindowUsage {
            kind: WindowKind::Other,
            utilization_pct: monthly_pct,
            resets_at_ms: if data.monthly.reset_at > 0 { Some(data.monthly.reset_at) } else { None },
        },
    ];

    Some(RoutingUsageInfo {
        utilization_pct: pct,
        resets_at_ms: if reset > 0 { Some(reset) } else { None },
        windows,
    })
}

/// Extract routing info from Zai usage data.
///
/// Uses the tokens_limit window (the one that matters for routing).
/// `percentage` is already 0-100, `reset_at` is `Option<i64>` epoch-ms.
fn zai_routing_info(data: &ZaiUsageData) -> Option<RoutingUsageInfo> {
    use bccf_core::types::{WindowKind, WindowUsage};

    let w = data.tokens_limit.as_ref()?;
    let windows = vec![WindowUsage {
        kind: WindowKind::Other,
        utilization_pct: w.percentage,
        resets_at_ms: w.reset_at,
    }];
    Some(RoutingUsageInfo {
        utilization_pct: w.percentage,
        resets_at_ms: w.reset_at,
        windows,
    })
}

// ---------------------------------------------------------------------------
// Cache entry
// ---------------------------------------------------------------------------

/// Cached usage data with timestamp for staleness detection.
#[derive(Debug, Clone)]
struct CacheEntry {
    data: AnyUsageData,
    timestamp: i64,
}

/// Max age for cache entries (10 minutes).
const MAX_CACHE_AGE_MS: i64 = 10 * 60 * 1000;

// ---------------------------------------------------------------------------
// Usage cache (thread-safe)
// ---------------------------------------------------------------------------

/// Thread-safe in-memory cache for account usage data.
#[derive(Debug, Clone)]
pub struct UsageCache {
    entries: Arc<DashMap<String, CacheEntry>>,
}

impl Default for UsageCache {
    fn default() -> Self {
        Self::new()
    }
}

impl UsageCache {
    pub fn new() -> Self {
        Self {
            entries: Arc::new(DashMap::new()),
        }
    }

    /// Get cached usage data for an account (returns None if stale).
    pub fn get(&self, account_id: &str) -> Option<AnyUsageData> {
        let entry = self.entries.get(account_id)?;
        let age = chrono::Utc::now().timestamp_millis() - entry.timestamp;
        if age > MAX_CACHE_AGE_MS {
            drop(entry);
            self.entries.remove(account_id);
            return None;
        }
        Some(entry.data.clone())
    }

    /// Store usage data for an account.
    pub fn set(&self, account_id: &str, data: AnyUsageData) {
        self.entries.insert(
            account_id.to_string(),
            CacheEntry {
                data,
                timestamp: chrono::Utc::now().timestamp_millis(),
            },
        );
    }

    /// Remove cached data for an account.
    pub fn remove(&self, account_id: &str) {
        self.entries.remove(account_id);
    }

    /// Clear all cached data.
    pub fn clear(&self) {
        self.entries.clear();
    }

    /// Clean up stale entries.
    pub fn cleanup_stale(&self) {
        let now = chrono::Utc::now().timestamp_millis();
        self.entries
            .retain(|_, entry| now - entry.timestamp <= MAX_CACHE_AGE_MS);
    }

    /// Number of cached entries.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the cache is empty.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

// ---------------------------------------------------------------------------
// Account info for polling (minimal subset)
// ---------------------------------------------------------------------------

/// Minimal account info needed for usage polling.
#[derive(Debug, Clone)]
pub struct PollableAccount {
    pub id: String,
    pub provider: String,
    pub access_token: Option<String>,
    pub refresh_token: Option<String>,
    pub client_id: Option<String>,
    pub api_key: Option<String>,
    pub custom_endpoint: Option<String>,
    pub paused: bool,
}

/// Refreshed token data returned by a successful token refresh.
#[derive(Debug)]
pub struct RefreshedToken {
    pub access_token: String,
    pub refresh_token: String,
    pub expires_at: Option<i64>,
}

/// Trait for fetching accounts eligible for usage polling.
/// Implemented by the server layer to provide database access.
pub trait AccountSource: Send + Sync + 'static {
    /// Return accounts that support usage tracking.
    fn get_pollable_accounts(&self) -> Vec<PollableAccount>;
    /// Persist a freshly-refreshed token pair back to the database.
    fn persist_refreshed_token(&self, account_id: &str, token: &RefreshedToken);
}

// ---------------------------------------------------------------------------
// Fetcher functions
// ---------------------------------------------------------------------------

/// Result of fetching Anthropic usage data — distinguishes auth failures from other errors.
pub enum AnthropicUsageResult {
    Ok(AnthropicUsageData),
    Unauthorized,
    Error,
}

/// Fetch Anthropic usage data from the OAuth usage endpoint.
pub async fn fetch_anthropic_usage(
    client: &reqwest::Client,
    access_token: &str,
) -> AnthropicUsageResult {
    let resp = client
        .get("https://api.anthropic.com/api/oauth/usage")
        .header("Authorization", format!("Bearer {access_token}"))
        .header("anthropic-beta", "oauth-2025-04-20")
        .header("Accept", "application/json")
        .send()
        .await;

    match resp {
        Ok(r) if r.status().is_success() => {
            match r.json::<serde_json::Map<String, JsonValue>>().await {
                Ok(data) => AnthropicUsageResult::Ok(data),
                Err(_) => AnthropicUsageResult::Error,
            }
        }
        Ok(r) if r.status() == reqwest::StatusCode::UNAUTHORIZED => {
            let body = r.text().await.unwrap_or_default();
            warn!(
                body = body.chars().take(200).collect::<String>(),
                "Anthropic usage fetch: token expired (401)"
            );
            AnthropicUsageResult::Unauthorized
        }
        Ok(r) => {
            let status = r.status();
            let body = r.text().await.unwrap_or_default();
            warn!(
                %status,
                body = body.chars().take(200).collect::<String>(),
                "Failed to fetch Anthropic usage data"
            );
            AnthropicUsageResult::Error
        }
        Err(e) => {
            warn!(error = %e, "Error fetching Anthropic usage data");
            AnthropicUsageResult::Error
        }
    }
}

const TOKEN_URL: &str = "https://console.anthropic.com/v1/oauth/token";

/// Attempt to refresh an OAuth access token using the stored refresh token.
///
/// Returns `Some(RefreshedToken)` on success, `None` on any failure.
async fn refresh_oauth_token(
    client: &reqwest::Client,
    refresh_token: &str,
    client_id: &str,
) -> Option<RefreshedToken> {
    let body = serde_json::json!({
        "grant_type": "refresh_token",
        "refresh_token": refresh_token,
        "client_id": client_id,
    });

    let resp = client
        .post(TOKEN_URL)
        .header("Content-Type", "application/json")
        .json(&body)
        .send()
        .await;

    match resp {
        Ok(r) if r.status().is_success() => {
            let json: JsonValue = r.json().await.ok()?;
            let access_token = json["access_token"].as_str()?.to_string();
            let new_refresh = json["refresh_token"]
                .as_str()
                .unwrap_or(refresh_token)
                .to_string();
            let expires_at = json["expires_in"].as_i64().map(|secs| {
                chrono::Utc::now().timestamp() + secs
            });
            Some(RefreshedToken {
                access_token,
                refresh_token: new_refresh,
                expires_at,
            })
        }
        Ok(r) => {
            let status = r.status();
            let body = r.text().await.unwrap_or_default();
            warn!(
                %status,
                body = body.chars().take(200).collect::<String>(),
                "Failed to refresh OAuth token for usage polling"
            );
            None
        }
        Err(e) => {
            warn!(error = %e, "Error refreshing OAuth token for usage polling");
            None
        }
    }
}

/// Fetch NanoGPT usage data from subscription endpoint.
pub async fn fetch_nanogpt_usage(
    client: &reqwest::Client,
    api_key: &str,
    custom_endpoint: Option<&str>,
) -> Option<NanoGptUsageData> {
    let base_url = custom_endpoint.unwrap_or("https://nano-gpt.com/api");
    let url = format!("{}/subscription/v1/usage", base_url.trim_end_matches('/'));

    let resp = client
        .get(&url)
        .header("x-api-key", api_key)
        .header("Accept", "application/json")
        .send()
        .await;

    match resp {
        Ok(r) if r.status().is_success() => {
            let body = r.bytes().await.ok()?;
            nanogpt::parse_nanogpt_usage_response(&body)
        }
        Ok(r) => {
            warn!(
                status = %r.status(),
                "Failed to fetch NanoGPT usage data"
            );
            None
        }
        Err(e) => {
            warn!(error = %e, "Error fetching NanoGPT usage data");
            None
        }
    }
}

/// Fetch Zai usage data from monitoring endpoint.
pub async fn fetch_zai_usage(client: &reqwest::Client, api_key: &str) -> Option<ZaiUsageData> {
    let resp = client
        .get("https://api.z.ai/api/monitor/usage/quota/limit")
        .header("x-api-key", api_key)
        .header("Accept", "application/json")
        .send()
        .await;

    match resp {
        Ok(r) if r.status().is_success() => {
            let body = r.bytes().await.ok()?;
            zai::parse_zai_usage_response(&body)
        }
        Ok(r) => {
            warn!(
                status = %r.status(),
                "Failed to fetch Zai usage data"
            );
            None
        }
        Err(e) => {
            warn!(error = %e, "Error fetching Zai usage data");
            None
        }
    }
}

// ---------------------------------------------------------------------------
// Polling service
// ---------------------------------------------------------------------------

/// Default polling interval (90 seconds).
const DEFAULT_POLL_INTERVAL: Duration = Duration::from_secs(90);

/// Delay before the very first poll so startup token refresh can complete.
const STARTUP_DELAY: Duration = Duration::from_secs(10);

/// Max retries per poll cycle.
/// Usage data is non-critical display info — fail fast and retry next cycle.
const MAX_RETRIES: u32 = 2;

/// Usage polling service that runs background tasks to fetch account usage.
pub struct UsagePollingService {
    cache: UsageCache,
    shutdown: Arc<Notify>,
    handle: Option<JoinHandle<()>>,
}

impl UsagePollingService {
    /// Start the polling service.
    ///
    /// Spawns a single tokio task that polls all eligible accounts every 90s.
    /// The task is tracked via JoinHandle for clean shutdown.
    pub fn start(
        account_source: Arc<dyn AccountSource>,
        cache: UsageCache,
        client: reqwest::Client,
    ) -> Self {
        let shutdown = Arc::new(Notify::new());
        let shutdown_rx = shutdown.clone();
        let cache_inner = cache.clone();

        let handle = tokio::spawn(async move {
            info!("Usage polling service started");
            let mut interval = tokio::time::interval(DEFAULT_POLL_INTERVAL);
            // Delay missed ticks instead of bursting to catch up — prevents
            // back-to-back polls after a long system sleep or suspend.
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            // Consume the immediate first tick so the loop only fires at the
            // regular 90s cadence (not immediately upon entering it).
            interval.tick().await;

            // Brief delay so startup token refresh can write fresh tokens to DB
            // before the first poll reads them.
            tokio::time::sleep(STARTUP_DELAY).await;
            poll_all_accounts(&account_source, &cache_inner, &client).await;

            loop {
                tokio::select! {
                    _ = interval.tick() => {
                        poll_all_accounts(&account_source, &cache_inner, &client).await;
                        cache_inner.cleanup_stale();
                    }
                    _ = shutdown_rx.notified() => {
                        info!("Usage polling service shutting down");
                        break;
                    }
                }
            }
        });

        Self {
            cache,
            shutdown,
            handle: Some(handle),
        }
    }

    /// Get the shared usage cache.
    pub fn cache(&self) -> &UsageCache {
        &self.cache
    }

    /// Gracefully shut down the polling service.
    pub async fn shutdown(mut self) {
        self.shutdown.notify_one();
        if let Some(handle) = self.handle.take() {
            let _ = handle.await;
        }
        info!("Usage polling service stopped");
    }
}

/// Poll all accounts that support usage tracking.
async fn poll_all_accounts(
    source: &Arc<dyn AccountSource>,
    cache: &UsageCache,
    client: &reqwest::Client,
) {
    let accounts = source.get_pollable_accounts();
    if accounts.is_empty() {
        debug!("No accounts eligible for usage polling");
        return;
    }

    debug!(count = accounts.len(), "Polling usage for accounts");

    for account in accounts {
        poll_single_account(&account, source, cache, client).await;
    }
}

/// Poll usage data for a single account with retry logic.
async fn poll_single_account(
    account: &PollableAccount,
    source: &Arc<dyn AccountSource>,
    cache: &UsageCache,
    client: &reqwest::Client,
) {
    let token = account
        .access_token
        .as_deref()
        .or(account.api_key.as_deref());

    let Some(token) = token else {
        debug!(
            account_id = %account.id,
            "No token available for usage polling, skipping"
        );
        return;
    };

    if token.trim().is_empty() {
        debug!(
            account_id = %account.id,
            "Empty token for usage polling, skipping"
        );
        return;
    }

    // For OAuth providers, try fetching usage; on 401 refresh the token and retry once.
    if account.provider == "claude-oauth" || account.provider == "anthropic" {
        let result = fetch_anthropic_usage(client, token).await;
        match result {
            AnthropicUsageResult::Ok(data) => {
                debug!(
                    account_id = %account.id,
                    utilization = ?AnyUsageData::Anthropic(data.clone()).utilization(),
                    "Fetched Anthropic usage data"
                );
                cache.set(&account.id, AnyUsageData::Anthropic(data));
                return;
            }
            AnthropicUsageResult::Unauthorized => {
                // Token expired — try to refresh and retry once
                if let (Some(rt), Some(cid)) = (
                    account.refresh_token.as_deref(),
                    account.client_id.as_deref(),
                ) {
                    debug!(account_id = %account.id, "Usage poll got 401, refreshing token");
                    if let Some(refreshed) = refresh_oauth_token(client, rt, cid).await {
                        info!(account_id = %account.id, "Token refreshed for usage polling");
                        source.persist_refreshed_token(&account.id, &refreshed);
                        // Retry with the new token
                        if let AnthropicUsageResult::Ok(data) =
                            fetch_anthropic_usage(client, &refreshed.access_token).await
                        {
                            cache.set(&account.id, AnyUsageData::Anthropic(data));
                            return;
                        }
                    }
                } else {
                    debug!(
                        account_id = %account.id,
                        "No refresh_token/client_id available, cannot refresh"
                    );
                }
                warn!(
                    account_id = %account.id,
                    provider = %account.provider,
                    "Usage fetch failed (token expired, refresh failed or unavailable)"
                );
            }
            AnthropicUsageResult::Error => {
                warn!(
                    account_id = %account.id,
                    provider = %account.provider,
                    "Usage fetch failed, will retry next cycle"
                );
            }
        }
        return;
    }

    // Non-OAuth providers: simple retry loop
    for attempt in 1..=MAX_RETRIES {
        let result = fetch_usage_for_provider(
            &account.provider,
            client,
            token,
            account.custom_endpoint.as_deref(),
        )
        .await;

        match result {
            Some(data) => {
                debug!(
                    account_id = %account.id,
                    provider = %account.provider,
                    utilization = ?data.utilization(),
                    "Fetched usage data"
                );
                cache.set(&account.id, data);
                return;
            }
            None if attempt < MAX_RETRIES => {
                debug!(
                    account_id = %account.id,
                    attempt,
                    "Usage fetch failed, retrying in 2s"
                );
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
            None => {
                warn!(
                    account_id = %account.id,
                    provider = %account.provider,
                    "Usage fetch failed after {MAX_RETRIES} attempts, will retry next cycle"
                );
            }
        }
    }
}

/// Dispatch usage fetch to the correct provider-specific function.
async fn fetch_usage_for_provider(
    provider: &str,
    client: &reqwest::Client,
    token: &str,
    custom_endpoint: Option<&str>,
) -> Option<AnyUsageData> {
    match provider {
        "nanogpt" => fetch_nanogpt_usage(client, token, custom_endpoint)
            .await
            .map(AnyUsageData::NanoGpt),
        "zai" => fetch_zai_usage(client, token).await.map(AnyUsageData::Zai),
        _ => {
            debug!(provider, "Provider does not support usage polling via this path");
            None
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -- Cache tests --------------------------------------------------------

    #[test]
    fn cache_set_and_get() {
        let cache = UsageCache::new();
        let data = AnyUsageData::Zai(ZaiUsageData {
            time_limit: None,
            tokens_limit: Some(zai::ZaiUsageWindow {
                used: 100.0,
                remaining: 900.0,
                percentage: 10.0,
                reset_at: None,
                limit_type: "TOKENS_LIMIT".to_string(),
            }),
        });

        cache.set("acc1", data);
        assert!(cache.get("acc1").is_some());
        assert_eq!(cache.len(), 1);
    }

    #[test]
    fn cache_remove() {
        let cache = UsageCache::new();
        cache.set(
            "acc1",
            AnyUsageData::Zai(ZaiUsageData {
                time_limit: None,
                tokens_limit: None,
            }),
        );
        cache.remove("acc1");
        assert!(cache.get("acc1").is_none());
        assert!(cache.is_empty());
    }

    #[test]
    fn cache_clear() {
        let cache = UsageCache::new();
        cache.set(
            "acc1",
            AnyUsageData::Zai(ZaiUsageData {
                time_limit: None,
                tokens_limit: None,
            }),
        );
        cache.set(
            "acc2",
            AnyUsageData::Zai(ZaiUsageData {
                time_limit: None,
                tokens_limit: None,
            }),
        );
        cache.clear();
        assert!(cache.is_empty());
    }

    #[test]
    fn cache_missing_key_returns_none() {
        let cache = UsageCache::new();
        assert!(cache.get("nonexistent").is_none());
    }

    // -- Anthropic utilization -----------------------------------------------

    #[test]
    fn anthropic_utilization_from_windows() {
        let mut data = serde_json::Map::new();
        data.insert(
            "five_hour".to_string(),
            serde_json::json!({"utilization": 0.25, "resets_at": null}),
        );
        data.insert(
            "seven_day".to_string(),
            serde_json::json!({"utilization": 0.75, "resets_at": null}),
        );

        let util = anthropic_utilization(&data);
        assert_eq!(util, Some(0.75));
    }

    #[test]
    fn anthropic_utilization_empty() {
        let data = serde_json::Map::new();
        let util = anthropic_utilization(&data);
        assert_eq!(util, Some(0.0));
    }

    #[test]
    fn anthropic_representative_window_picks_highest() {
        let mut data = serde_json::Map::new();
        data.insert(
            "five_hour".to_string(),
            serde_json::json!({"utilization": 0.8}),
        );
        data.insert(
            "seven_day".to_string(),
            serde_json::json!({"utilization": 0.3}),
        );

        assert_eq!(
            anthropic_representative_window(&data),
            Some("five_hour".to_string())
        );
    }

    // -- AnyUsageData methods ------------------------------------------------

    #[test]
    fn any_usage_data_utilization_zai() {
        let data = AnyUsageData::Zai(ZaiUsageData {
            time_limit: None,
            tokens_limit: Some(zai::ZaiUsageWindow {
                used: 5000.0,
                remaining: 45000.0,
                percentage: 10.0,
                reset_at: None,
                limit_type: "TOKENS_LIMIT".to_string(),
            }),
        });
        assert_eq!(data.utilization(), Some(10.0));
    }

    #[test]
    fn any_usage_data_utilization_nanogpt() {
        let data = AnyUsageData::NanoGpt(NanoGptUsageData {
            active: true,
            limits: nanogpt::NanoGptLimits {
                daily: 1000.0,
                monthly: 30000.0,
            },
            enforce_daily_limit: true,
            daily: nanogpt::NanoGptUsageWindow {
                used: 500.0,
                remaining: 500.0,
                percent_used: 0.5,
                reset_at: 0,
            },
            monthly: nanogpt::NanoGptUsageWindow {
                used: 3000.0,
                remaining: 27000.0,
                percent_used: 0.1,
                reset_at: 0,
            },
            state: "active".to_string(),
            grace_until: None,
        });
        assert_eq!(data.utilization(), Some(50.0)); // daily 50% > monthly 10%
    }

    #[test]
    fn any_usage_data_to_json() {
        let data = AnyUsageData::Zai(ZaiUsageData {
            time_limit: None,
            tokens_limit: None,
        });
        let json = data.to_json().unwrap();
        assert!(json.is_object());
    }

    #[test]
    fn any_usage_data_representative_window_zai() {
        let data = AnyUsageData::Zai(ZaiUsageData {
            time_limit: None,
            tokens_limit: Some(zai::ZaiUsageWindow {
                used: 5000.0,
                remaining: 45000.0,
                percentage: 10.0,
                reset_at: None,
                limit_type: "TOKENS_LIMIT".to_string(),
            }),
        });
        assert_eq!(data.representative_window(), Some("five_hour".to_string()));
    }

    // -- Dispatch tests ------------------------------------------------------

    #[tokio::test]
    async fn dispatch_unknown_provider_returns_none() {
        let client = reqwest::Client::new();
        let result = fetch_usage_for_provider("openai-compatible", &client, "key", None).await;
        assert!(result.is_none());
    }

    // -- Polling service lifecycle -------------------------------------------

    struct MockAccountSource {
        accounts: Vec<PollableAccount>,
    }

    impl AccountSource for MockAccountSource {
        fn get_pollable_accounts(&self) -> Vec<PollableAccount> {
            self.accounts.clone()
        }
        fn persist_refreshed_token(&self, _account_id: &str, _token: &RefreshedToken) {}
    }

    #[tokio::test]
    async fn polling_service_starts_and_shuts_down() {
        let source = Arc::new(MockAccountSource { accounts: vec![] });
        let cache = UsageCache::new();
        let client = reqwest::Client::new();

        let service = UsagePollingService::start(source, cache, client);
        assert!(service.cache().is_empty());

        service.shutdown().await;
    }

    #[tokio::test]
    async fn poll_single_account_no_token_skips() {
        let source: Arc<dyn AccountSource> = Arc::new(MockAccountSource { accounts: vec![] });
        let cache = UsageCache::new();
        let client = reqwest::Client::new();
        let account = PollableAccount {
            id: "acc1".to_string(),
            provider: "zai".to_string(),
            access_token: None,
            refresh_token: None,
            client_id: None,
            api_key: None,
            custom_endpoint: None,
            paused: false,
        };

        poll_single_account(&account, &source, &cache, &client).await;
        assert!(cache.get("acc1").is_none());
    }

    #[tokio::test]
    async fn poll_single_account_empty_token_skips() {
        let source: Arc<dyn AccountSource> = Arc::new(MockAccountSource { accounts: vec![] });
        let cache = UsageCache::new();
        let client = reqwest::Client::new();
        let account = PollableAccount {
            id: "acc1".to_string(),
            provider: "zai".to_string(),
            access_token: Some("  ".to_string()),
            refresh_token: None,
            client_id: None,
            api_key: None,
            custom_endpoint: None,
            paused: false,
        };

        poll_single_account(&account, &source, &cache, &client).await;
        assert!(cache.get("acc1").is_none());
    }

    // -- RoutingUsageInfo tests -----------------------------------------------

    #[test]
    fn routing_info_zai() {
        let data = AnyUsageData::Zai(ZaiUsageData {
            time_limit: None,
            tokens_limit: Some(zai::ZaiUsageWindow {
                used: 5000.0,
                remaining: 45000.0,
                percentage: 10.0,
                reset_at: Some(1700000000000),
                limit_type: "TOKENS_LIMIT".to_string(),
            }),
        });
        let info = data.routing_info().unwrap();
        assert!((info.utilization_pct - 10.0).abs() < 0.01);
        assert_eq!(info.resets_at_ms, Some(1700000000000));
    }

    #[test]
    fn routing_info_zai_no_tokens_limit() {
        let data = AnyUsageData::Zai(ZaiUsageData {
            time_limit: None,
            tokens_limit: None,
        });
        assert!(data.routing_info().is_none());
    }

    #[test]
    fn routing_info_nanogpt_daily_dominant() {
        let data = AnyUsageData::NanoGpt(NanoGptUsageData {
            active: true,
            limits: nanogpt::NanoGptLimits {
                daily: 1000.0,
                monthly: 30000.0,
            },
            enforce_daily_limit: true,
            daily: nanogpt::NanoGptUsageWindow {
                used: 800.0,
                remaining: 200.0,
                percent_used: 0.8,
                reset_at: 1700001000000,
            },
            monthly: nanogpt::NanoGptUsageWindow {
                used: 3000.0,
                remaining: 27000.0,
                percent_used: 0.1,
                reset_at: 1700100000000,
            },
            state: "active".to_string(),
            grace_until: None,
        });
        let info = data.routing_info().unwrap();
        assert!((info.utilization_pct - 80.0).abs() < 0.01);
        assert_eq!(info.resets_at_ms, Some(1700001000000));
    }

    #[test]
    fn routing_info_nanogpt_inactive() {
        let data = AnyUsageData::NanoGpt(NanoGptUsageData {
            active: false,
            limits: nanogpt::NanoGptLimits {
                daily: 1000.0,
                monthly: 30000.0,
            },
            enforce_daily_limit: true,
            daily: nanogpt::NanoGptUsageWindow {
                used: 0.0,
                remaining: 1000.0,
                percent_used: 0.0,
                reset_at: 0,
            },
            monthly: nanogpt::NanoGptUsageWindow {
                used: 0.0,
                remaining: 30000.0,
                percent_used: 0.0,
                reset_at: 0,
            },
            state: "inactive".to_string(),
            grace_until: None,
        });
        assert!(data.routing_info().is_none());
    }

    #[test]
    fn routing_info_anthropic_fractional() {
        // Anthropic sometimes reports utilization as 0.0-1.0 (fraction)
        let mut data = serde_json::Map::new();
        data.insert(
            "five_hour".to_string(),
            serde_json::json!({"utilization": 0.84, "resets_at": "2025-10-03T12:00:00Z"}),
        );
        let info = AnyUsageData::Anthropic(data).routing_info().unwrap();
        assert!((info.utilization_pct - 84.0).abs() < 0.01);
        assert!(info.resets_at_ms.is_some());
    }

    #[test]
    fn routing_info_anthropic_percentage() {
        // Anthropic may also report as 0-100
        let mut data = serde_json::Map::new();
        data.insert(
            "seven_day".to_string(),
            serde_json::json!({"utilization": 42.5}),
        );
        let info = AnyUsageData::Anthropic(data).routing_info().unwrap();
        assert!((info.utilization_pct - 42.5).abs() < 0.01);
        assert!(info.resets_at_ms.is_none());
    }

    #[test]
    fn routing_info_anthropic_empty() {
        let data = serde_json::Map::new();
        let info = AnyUsageData::Anthropic(data).routing_info().unwrap();
        assert!((info.utilization_pct - 0.0).abs() < 0.01);
    }

    // -- Polling tests -------------------------------------------------------

    #[tokio::test]
    async fn poll_all_empty_accounts_no_panic() {
        let source: Arc<dyn AccountSource> = Arc::new(MockAccountSource { accounts: vec![] });
        let cache = UsageCache::new();
        let client = reqwest::Client::new();

        poll_all_accounts(&source, &cache, &client).await;
        assert!(cache.is_empty());
    }
}
