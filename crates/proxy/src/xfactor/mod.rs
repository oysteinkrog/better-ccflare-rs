//! X-factor capacity estimation service.
//!
//! Provides per-account Bayesian capacity estimation and pool-level analytics.
//! Runs a background task that:
//!   1. On startup: restores posteriors from DB and rebuilds rolling windows.
//!   2. Every 90s: reads from UsageCache to update posteriors.
//!   3. On request completion (via `on_request`): updates rolling window + EMA.
//!   4. Periodically: persists state back to DB.

pub mod state;

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use dashmap::DashMap;
use tracing::{debug, info, warn};

use bccf_database::pool::DbPool;
use bccf_database::repositories::{account as account_repo, xfactor as xfactor_repo};
use bccf_providers::usage_polling::{AnyUsageData, UsageCache};

use crate::xfactor::state::{model_weight, AccountCapacityState};
use bccf_database::repositories::xfactor::XFactorDbState;

// ---------------------------------------------------------------------------
// Public cache type
// ---------------------------------------------------------------------------

/// Thread-safe shared cache of per-account X-factor states.
///
/// `Arc<DashMap<account_id, AccountCapacityState>>` — lock-free concurrent
/// reads from HTTP handlers and writes from the background task.
#[derive(Clone)]
pub struct XFactorCache {
    inner: Arc<DashMap<String, AccountCapacityState>>,
}

impl Default for XFactorCache {
    fn default() -> Self {
        Self::new()
    }
}

impl XFactorCache {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(DashMap::new()),
        }
    }

    /// Record a completed request for an account (updates rolling window + EMA).
    ///
    /// `model` — model name string (used for weight computation).
    /// `total_tokens` — raw (unweighted) token count for this request.
    pub fn on_request(
        &self,
        account_id: &str,
        model: Option<&str>,
        total_tokens: i64,
        now_ms: i64,
    ) {
        if total_tokens <= 0 {
            return;
        }
        let weight = model_weight(model);
        let weighted = total_tokens as f64 * weight;
        if let Some(mut state) = self.inner.get_mut(account_id) {
            state.on_request(now_ms, weighted);
        }
        // If account not yet in cache, we'll pick it up on next background cycle.
    }

    /// Number of accounts tracked.
    pub fn len(&self) -> usize {
        self.inner.len()
    }

    /// Whether the cache is empty.
    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    /// Snapshot all states for use in HTTP handlers.
    ///
    /// Returns a HashMap so callers can read without holding DashMap locks.
    pub fn snapshot(&self) -> HashMap<String, AccountCapacitySnapshot> {
        self.inner
            .iter()
            .map(|e| {
                let s = e.value();
                let now_ms = chrono::Utc::now().timestamp_millis();
                let snap = build_snapshot(s, now_ms);
                (e.key().clone(), snap)
            })
            .collect()
    }

    /// Get a single account snapshot.
    pub fn get_snapshot(&self, account_id: &str) -> Option<AccountCapacitySnapshot> {
        let s = self.inner.get(account_id)?;
        let now_ms = chrono::Utc::now().timestamp_millis();
        Some(build_snapshot(s.value(), now_ms))
    }
}

/// Build an `AccountCapacitySnapshot` from the mutable state.
fn build_snapshot(s: &state::AccountCapacityState, now_ms: i64) -> AccountCapacitySnapshot {
    let (r_p, r_e, r_o) = s.remaining_tokens_estimate();
    let (x_lo, x_mid, x_hi) = s.x_factor();
    AccountCapacitySnapshot {
        account_id: s.account_id.clone(),
        account_name: s.account_name.clone(),
        subscription_tier: s.subscription_tier.clone(),
        is_shared: s.is_shared,
        proxy_tokens_5h_weighted: s.proxy_tokens_5h_weighted,
        c_estimate: s.c_estimate(),
        remaining_pessimistic: r_p,
        remaining_expected: r_e,
        remaining_optimistic: r_o,
        tte_minutes: s.tte_minutes(),
        tte_minutes_with_recovery: s.tte_minutes_with_recovery(now_ms),
        window_recovery_15min: s.window_recovery_tokens(now_ms, 15 * 60 * 1000),
        utilization_pct: s.utilization_pct(),
        x_factor_lo: x_lo,
        x_factor_mid: x_mid,
        x_factor_hi: x_hi,
        n_eff: s.n_eff,
        confidence: s.confidence(),
        should_stop_assign: s.should_stop_assign(),
        last_poll_age_seconds: s.poll_age_seconds(now_ms),
        ema_proxy_rate: s.ema_proxy_rate,
        kf_e: s.kf_e,
        kf_e_dot: s.kf_e_dot,
        shared_score: s.shared_score(),
        suspected_shared: s.suspected_shared(),
    }
}

// ---------------------------------------------------------------------------
// Snapshot type (read-only view for handlers)
// ---------------------------------------------------------------------------

/// Immutable snapshot of one account's capacity state for HTTP responses.
#[derive(Debug, Clone)]
pub struct AccountCapacitySnapshot {
    pub account_id: String,
    pub account_name: String,
    pub subscription_tier: Option<String>,
    pub is_shared: bool,
    pub proxy_tokens_5h_weighted: f64,
    pub c_estimate: f64,
    pub remaining_pessimistic: f64,
    pub remaining_expected: f64,
    pub remaining_optimistic: f64,
    /// Time-to-exhaustion in minutes at current EMA rate (pessimistic capacity).
    pub tte_minutes: f64,
    /// Improved TTE accounting for rolling-window recovery (tokens expiring in 15min).
    pub tte_minutes_with_recovery: f64,
    /// Tokens falling off the 5h window within the next 15 minutes (free capacity recovery).
    pub window_recovery_15min: f64,
    /// Utilization percentage based on median capacity estimate.
    pub utilization_pct: f64,
    /// X-factor = C_i / C_base_pro estimates (lo, mid, hi).
    pub x_factor_lo: f64,
    pub x_factor_mid: f64,
    pub x_factor_hi: f64,
    /// Effective independent-window sample count.
    pub n_eff: f64,
    /// Confidence label: cold | low | medium | high.
    pub confidence: &'static str,
    /// Whether new sessions should stop being routed to this account.
    pub should_stop_assign: bool,
    /// Seconds since last usage poll (None if never polled).
    pub last_poll_age_seconds: Option<f64>,
    /// EMA proxy token rate (weighted tokens/sec).
    pub ema_proxy_rate: f64,
    /// KF-estimated external tokens in current 5h window.
    pub kf_e: f64,
    /// KF-estimated external token rate (tokens/sec).
    pub kf_e_dot: f64,
    /// Ratio of external to proxy tokens (0.0 = not shared, >0.15 = suspected shared).
    pub shared_score: f64,
    /// True if external usage pattern detected and account is not marked as shared.
    pub suspected_shared: bool,
}

// ---------------------------------------------------------------------------
// Background task
// ---------------------------------------------------------------------------

const PERSIST_INTERVAL: Duration = Duration::from_secs(5 * 60); // persist every 5 min
const UPDATE_INTERVAL: Duration = Duration::from_secs(95); // slightly after 90s poll

/// Start the X-factor background service.
///
/// Spawns a tokio task that:
/// 1. Loads accounts + restores DB state + rebuilds 5h window from requests.
/// 2. Every 95s: syncs posteriors from UsageCache + periodic DB persistence.
pub fn start(cache: XFactorCache, usage_cache: UsageCache, pool: DbPool) {
    let cache_bg = cache.clone();
    tokio::spawn(async move {
        run_background_task(cache_bg, usage_cache, pool).await;
    });
}

/// Main loop for the background service.
pub async fn run_background_task(cache: XFactorCache, usage_cache: UsageCache, pool: DbPool) {
    info!("XFactor service: initializing");

    // Startup: load accounts, restore posteriors, rebuild rolling windows
    if let Err(e) = initialize_cache(&cache, &pool).await {
        warn!("XFactor service: initialization error: {e}");
    }
    info!(accounts = cache.len(), "XFactor service: initialized");

    let mut update_interval = tokio::time::interval(UPDATE_INTERVAL);
    let mut persist_interval = tokio::time::interval(PERSIST_INTERVAL);
    // Drain the immediate first tick
    update_interval.tick().await;
    persist_interval.tick().await;

    loop {
        tokio::select! {
            _ = update_interval.tick() => {
                update_from_usage_cache(&cache, &usage_cache, &pool).await;
            }
            _ = persist_interval.tick() => {
                persist_to_db(&cache, &pool).await;
            }
        }
    }
}

/// Initialize cache on startup:
/// 1. Load all accounts from DB.
/// 2. Create states with tier priors.
/// 3. Restore posteriors from account_xfactor_state table.
/// 4. Rebuild 5h rolling window from recent requests.
async fn initialize_cache(cache: &XFactorCache, pool: &DbPool) -> Result<(), String> {
    let conn = pool.get().map_err(|e| e.to_string())?;

    // Load all accounts
    let accounts = account_repo::find_all(&conn).map_err(|e| e.to_string())?;

    // Load all persisted posteriors
    let db_states = xfactor_repo::load_all_states(&conn).map_err(|e| e.to_string())?;
    let db_map: HashMap<String, XFactorDbState> = db_states
        .into_iter()
        .map(|s| (s.account_id.clone(), s))
        .collect();

    let now_ms = chrono::Utc::now().timestamp_millis();
    let since_5h = now_ms - state::WINDOW_5H_MS;

    for account in &accounts {
        let mut state = AccountCapacityState::new(
            account.id.clone(),
            account.name.clone(),
            account.subscription_tier.clone(),
            account.is_shared,
        );

        // Restore persisted posterior if available
        if let Some(db) = db_map.get(&account.id) {
            state.restore_from_db(db);
        }

        // Rebuild rolling window from last 5h of requests
        match xfactor_repo::recent_requests_for_account(&conn, &account.id, since_5h) {
            Ok(rows) => {
                for (ts_ms, raw_tokens, model) in rows {
                    let weight = model_weight(model.as_deref());
                    let wt = raw_tokens * weight;
                    if wt > 0.0 {
                        state.window_5h.push_back((ts_ms, wt));
                        state.proxy_tokens_5h_weighted += wt;
                    }
                }
                debug!(
                    account = %account.name,
                    entries = state.window_5h.len(),
                    total_weighted = state.proxy_tokens_5h_weighted,
                    "XFactor: rebuilt 5h window"
                );
            }
            Err(e) => {
                warn!(account = %account.name, error = %e, "XFactor: failed to rebuild window");
            }
        }

        cache.inner.insert(account.id.clone(), state);
    }

    Ok(())
}

/// Sync posteriors from UsageCache (reads utilization, computes LB, updates posterior).
async fn update_from_usage_cache(cache: &XFactorCache, usage_cache: &UsageCache, pool: &DbPool) {
    let now_ms = chrono::Utc::now().timestamp_millis();

    // Check for new accounts not yet in cache
    if let Ok(conn) = pool.get() {
        if let Ok(accounts) = account_repo::find_all(&conn) {
            for account in accounts {
                if !cache.inner.contains_key(&account.id) {
                    let state = AccountCapacityState::new(
                        account.id.clone(),
                        account.name.clone(),
                        account.subscription_tier.clone(),
                        account.is_shared,
                    );
                    cache.inner.insert(account.id.clone(), state);
                    debug!(account = %account.name, "XFactor: added new account to cache");
                }
            }
        }
    }

    // Update posteriors from usage data
    for mut entry in cache.inner.iter_mut() {
        let account_id = entry.key().clone();
        let Some(usage) = usage_cache.get(&account_id) else {
            continue;
        };

        // Only update for providers that have meaningful 5h utilization
        let util_pct = match &usage {
            AnyUsageData::Anthropic(data) => extract_5h_utilization(data),
            _ => continue, // NanoGPT/Zai have different window semantics
        };

        let Some(util_pct) = util_pct else { continue };

        entry.value_mut().on_usage_poll(now_ms, util_pct);
        debug!(
            account = %entry.account_name,
            utilization_pct = util_pct,
            n_eff = entry.n_eff,
            "XFactor: updated posterior from usage poll"
        );
    }
}

/// Extract the `five_hour` utilization percentage from Anthropic usage data.
fn extract_5h_utilization(data: &bccf_providers::usage_polling::AnthropicUsageData) -> Option<f64> {
    let window = data.get("five_hour")?;
    let util = window.get("utilization")?.as_f64()?;
    // Normalise: Anthropic may return 0-1 or 0-100
    let pct = if util < 2.0 { util * 100.0 } else { util };
    Some(pct)
}

/// Persist current posterior state for all accounts to the DB.
async fn persist_to_db(cache: &XFactorCache, pool: &DbPool) {
    let states: Vec<XFactorDbState> = cache
        .inner
        .iter()
        .map(|e| e.value().to_db_state())
        .collect();

    if states.is_empty() {
        return;
    }

    let pool_clone = pool.clone();
    let result = tokio::task::spawn_blocking(move || {
        let conn = pool_clone.get()?;
        xfactor_repo::save_all_states(&conn, &states)
    })
    .await;

    match result {
        Ok(Ok(())) => debug!(count = cache.len(), "XFactor: persisted states to DB"),
        Ok(Err(e)) => warn!("XFactor: DB persist error: {e}"),
        Err(e) => warn!("XFactor: spawn_blocking error: {e}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_on_request_updates_state() {
        let cache = XFactorCache::new();
        let now = 1_700_000_000_000_i64;

        // Manually insert a state
        cache.inner.insert(
            "acc1".to_string(),
            AccountCapacityState::new("acc1".to_string(), "test".to_string(), None, false),
        );

        cache.on_request("acc1", Some("claude-sonnet-4"), 1000, now);

        let s = cache.inner.get("acc1").unwrap();
        assert!((s.proxy_tokens_5h_weighted - 1000.0).abs() < 1.0);
    }

    #[test]
    fn cache_on_request_opus_weighted() {
        let cache = XFactorCache::new();
        let now = 1_700_000_000_000_i64;
        cache.inner.insert(
            "acc1".to_string(),
            AccountCapacityState::new("acc1".to_string(), "test".to_string(), None, false),
        );

        cache.on_request("acc1", Some("claude-opus-4"), 1000, now);

        let s = cache.inner.get("acc1").unwrap();
        // opus weight = 4.0 → 1000 * 4 = 4000
        assert!((s.proxy_tokens_5h_weighted - 4000.0).abs() < 1.0);
    }

    #[test]
    fn cache_snapshot_returns_all_accounts() {
        let cache = XFactorCache::new();
        for i in 0..3 {
            cache.inner.insert(
                format!("acc{i}"),
                AccountCapacityState::new(
                    format!("acc{i}"),
                    format!("account-{i}"),
                    Some("Pro".to_string()),
                    false,
                ),
            );
        }
        let snap = cache.snapshot();
        assert_eq!(snap.len(), 3);
    }

    #[test]
    fn cache_get_snapshot_missing_returns_none() {
        let cache = XFactorCache::new();
        assert!(cache.get_snapshot("nonexistent").is_none());
    }

    #[test]
    fn extract_5h_utilization_fractional() {
        let mut data = serde_json::Map::new();
        data.insert(
            "five_hour".to_string(),
            serde_json::json!({"utilization": 0.42}),
        );
        let pct = extract_5h_utilization(&data);
        assert!((pct.unwrap() - 42.0).abs() < 0.01);
    }

    #[test]
    fn extract_5h_utilization_percentage() {
        let mut data = serde_json::Map::new();
        data.insert(
            "five_hour".to_string(),
            serde_json::json!({"utilization": 67.5}),
        );
        let pct = extract_5h_utilization(&data);
        assert!((pct.unwrap() - 67.5).abs() < 0.01);
    }

    #[test]
    fn extract_5h_utilization_missing_key() {
        let data = serde_json::Map::new();
        assert!(extract_5h_utilization(&data).is_none());
    }
}
