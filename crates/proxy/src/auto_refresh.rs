//! Auto-refresh scheduler — sends dummy requests to refresh OAuth usage windows.
//!
//! Monitors Anthropic OAuth accounts with `auto_refresh_enabled=true` and sends
//! dummy messages through the proxy when their usage window resets. This ensures
//! accounts maintain fresh rate-limit data even when idle.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use rand::Rng;
use tokio::sync::{Mutex, Notify};
use tokio::task::JoinHandle;
use tracing::{debug, error, info, warn};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// How often to check for accounts needing refresh (60 seconds).
const CHECK_INTERVAL: Duration = Duration::from_secs(60);

/// Consecutive failures before flagging an account for re-auth.
const FAILURE_THRESHOLD: u32 = 5;

/// Accounts with rate_limit_reset older than this are considered stale.
const STALE_THRESHOLD_MS: i64 = 24 * 60 * 60 * 1000; // 24 hours

/// Models to try in order (cheapest first).
const MODEL_CASCADE: &[&str] = &[
    "claude-haiku-4-5-20251001",
    "claude-3-5-haiku-20241022",
    "claude-3-haiku-20240307",
    "claude-3-5-sonnet-20241022",
    "claude-3-sonnet-20240229",
];

/// Dummy messages for auto-refresh requests.
const DUMMY_MESSAGES: &[&str] = &[
    "Write a hello world program in Python",
    "What is 2+2?",
    "Tell me a programmer joke",
    "What is the capital of France?",
    "Explain recursion in one sentence",
];

// ---------------------------------------------------------------------------
// Account info for refresh checks
// ---------------------------------------------------------------------------

/// Minimal account info needed for auto-refresh decisions.
#[derive(Debug, Clone)]
pub struct RefreshableAccount {
    pub id: String,
    pub name: String,
    pub rate_limit_reset: Option<i64>,
}

/// Trait for querying accounts eligible for auto-refresh.
pub trait RefreshAccountSource: Send + Sync + 'static {
    /// Get accounts with auto_refresh_enabled=true and provider='anthropic'.
    fn get_refreshable_accounts(&self) -> Vec<RefreshableAccount>;

    /// Get the list of active auto-refresh account IDs (for cleanup).
    fn get_active_refresh_account_ids(&self) -> Vec<String>;

    /// Persist an auth-failure marker for UI/API status (default: no-op).
    fn mark_auth_failed(&self, _account_id: &str, _http_status: u16) {}
}

// ---------------------------------------------------------------------------
// Scheduler state
// ---------------------------------------------------------------------------

/// Internal state guarded by mutex.
struct SchedulerState {
    /// Last rate_limit_reset seen per account (used for new-window detection).
    last_refresh_reset: HashMap<String, i64>,
    /// Consecutive failure count per account.
    consecutive_failures: HashMap<String, u32>,
    /// Global backoff multiplier — increases on 429s, resets on success.
    backoff_multiplier: u32,
}

impl SchedulerState {
    fn new() -> Self {
        Self {
            last_refresh_reset: HashMap::new(),
            consecutive_failures: HashMap::new(),
            backoff_multiplier: 1,
        }
    }

    /// Determine if an account should be refreshed.
    fn should_refresh(&self, account: &RefreshableAccount, now: i64) -> bool {
        let last_reset = self.last_refresh_reset.get(&account.id);

        // First time seeing this account → refresh
        if last_reset.is_none() {
            debug!(account = %account.name, "First-time refresh");
            return true;
        }

        let Some(rate_limit_reset) = account.rate_limit_reset else {
            return false;
        };

        // Reset time has passed → new window
        if rate_limit_reset <= now {
            debug!(account = %account.name, "Window reset passed, refreshing");
            return true;
        }

        // Database has a newer reset time than our tracking
        if let Some(&last) = last_reset {
            if rate_limit_reset > last {
                debug!(account = %account.name, "Newer reset time detected");
                return true;
            }
        }

        // Stale reset time (>24h old)
        let stale_cutoff = now - STALE_THRESHOLD_MS;
        if rate_limit_reset < stale_cutoff {
            debug!(account = %account.name, "Stale reset time, forcing refresh");
            return true;
        }

        false
    }

    /// Record a successful refresh.
    fn record_success(&mut self, account_id: &str, reset_time: Option<i64>) {
        if let Some(rt) = reset_time {
            self.last_refresh_reset.insert(account_id.to_string(), rt);
        }
        self.consecutive_failures.remove(account_id);
    }

    /// Record a failed refresh. Returns the new failure count.
    fn record_failure(&mut self, account_id: &str) -> u32 {
        let count = self
            .consecutive_failures
            .entry(account_id.to_string())
            .or_insert(0);
        *count += 1;
        *count
    }

    /// Remove tracking for accounts that are no longer active.
    fn cleanup(&mut self, active_ids: &[String]) {
        let active_set: std::collections::HashSet<&str> =
            active_ids.iter().map(|s| s.as_str()).collect();

        self.last_refresh_reset
            .retain(|id, _| active_set.contains(id.as_str()));
        self.consecutive_failures
            .retain(|id, _| active_set.contains(id.as_str()));
    }
}

// ---------------------------------------------------------------------------
// Auto-refresh scheduler
// ---------------------------------------------------------------------------

/// Auto-refresh scheduler that monitors OAuth accounts and sends dummy
/// requests to refresh usage windows.
pub struct AutoRefreshScheduler {
    shutdown: Arc<Notify>,
    handle: Option<JoinHandle<()>>,
}

impl AutoRefreshScheduler {
    /// Start the auto-refresh scheduler.
    ///
    /// Spawns a tokio task that checks every 60s for accounts with new/stale
    /// usage windows and sends dummy requests through the proxy.
    pub fn start(
        account_source: Arc<dyn RefreshAccountSource>,
        proxy_port: u16,
        use_tls: bool,
        internal_api_key: Option<String>,
    ) -> Self {
        let shutdown = Arc::new(Notify::new());
        let shutdown_rx = shutdown.clone();
        let state = Arc::new(Mutex::new(SchedulerState::new()));
        let client = reqwest::Client::builder()
            .danger_accept_invalid_certs(true) // localhost self-signed
            .timeout(Duration::from_secs(30))
            .build()
            .expect("failed to build HTTP client");

        let handle = tokio::spawn(async move {
            info!(
                "Auto-refresh scheduler started (interval: {}s)",
                CHECK_INTERVAL.as_secs()
            );

            loop {
                // Dynamic sleep: base interval × backoff multiplier
                let multiplier = { state.try_lock().map(|g| g.backoff_multiplier).unwrap_or(1) };
                let sleep_duration = CHECK_INTERVAL * multiplier;

                tokio::select! {
                    _ = tokio::time::sleep(sleep_duration) => {
                        check_and_refresh(
                            &account_source,
                            &state,
                            &client,
                            proxy_port,
                            use_tls,
                            internal_api_key.as_deref(),
                        ).await;
                    }
                    _ = shutdown_rx.notified() => {
                        info!("Auto-refresh scheduler shutting down");
                        break;
                    }
                }
            }
        });

        Self {
            shutdown,
            handle: Some(handle),
        }
    }

    /// Gracefully shut down the scheduler.
    pub async fn shutdown(mut self) {
        self.shutdown.notify_one();
        if let Some(handle) = self.handle.take() {
            let _ = handle.await;
        }
        info!("Auto-refresh scheduler stopped");
    }
}

// ---------------------------------------------------------------------------
// Core logic
// ---------------------------------------------------------------------------

/// Check all eligible accounts and refresh those with new/stale windows.
async fn check_and_refresh(
    source: &Arc<dyn RefreshAccountSource>,
    state: &Arc<Mutex<SchedulerState>>,
    client: &reqwest::Client,
    proxy_port: u16,
    use_tls: bool,
    internal_api_key: Option<&str>,
) {
    // Try to acquire mutex — skip if previous check is still running
    let Ok(mut guard) = state.try_lock() else {
        debug!("Auto-refresh check skipped — previous check still in progress");
        return;
    };

    // Cleanup stale tracking entries
    let active_ids = source.get_active_refresh_account_ids();
    guard.cleanup(&active_ids);

    let now = chrono::Utc::now().timestamp_millis();
    let accounts = source.get_refreshable_accounts();

    if accounts.is_empty() {
        return;
    }

    let to_refresh: Vec<_> = accounts
        .iter()
        .filter(|a| guard.should_refresh(a, now))
        .collect();

    if to_refresh.is_empty() {
        return;
    }

    // Collect owned data before dropping the lock so HTTP calls don't hold it.
    let to_refresh: Vec<RefreshableAccount> = to_refresh.into_iter().cloned().collect();

    info!(
        count = to_refresh.len(),
        "Found accounts with new windows for auto-refresh"
    );

    // Drop the mutex guard before making any HTTP calls. Each call can take up
    // to 30 s (the client timeout) and holding the lock across them would
    // block the next scheduled check from running at all.
    drop(guard);

    let mut saw_429 = false;
    for (i, account) in to_refresh.iter().enumerate() {
        // Stagger requests to avoid bursting when multiple accounts need refresh
        if i > 0 {
            tokio::time::sleep(Duration::from_secs(3)).await;
        }

        let result = send_dummy_request(client, account, proxy_port, use_tls, internal_api_key).await;

        // Re-acquire the lock briefly just to record the result.
        if let Ok(mut guard) = state.try_lock() {
            match result {
                RefreshResult::Success(reset_time) => {
                    guard.record_success(&account.id, reset_time);
                    // Reset backoff on any success
                    guard.backoff_multiplier = 1;
                }
                RefreshResult::RateLimited => {
                    saw_429 = true;
                    warn!(account = %account.name, "Auto-refresh hit 429, aborting remaining accounts");
                    guard.backoff_multiplier = (guard.backoff_multiplier * 2).min(8);
                    break;
                }
                RefreshResult::Failed => {
                    let failures = guard.record_failure(&account.id);
                    if failures >= FAILURE_THRESHOLD {
                        error!(
                            account = %account.name,
                            failures,
                            "Account has exceeded failure threshold — may need re-authentication"
                        );
                    }
                }
                RefreshResult::AuthFailed(status) => {
                    source.mark_auth_failed(&account.id, status);
                    let failures = guard.record_failure(&account.id);
                    if failures >= FAILURE_THRESHOLD {
                        error!(
                            account = %account.name,
                            status = status,
                            failures,
                            "Account has exceeded failure threshold — re-authentication required"
                        );
                    }
                }
            }
        }
    }

    if saw_429 {
        warn!("Auto-refresh cycle interrupted by rate limiting — next check will use backoff");
    }
}

/// Outcome of a single auto-refresh attempt.
enum RefreshResult {
    /// Success — contains the new rate_limit_reset timestamp (if available).
    Success(Option<i64>),
    /// Got a 429 from the proxy (upstream rate-limited).
    RateLimited,
    /// Non-rate-limit failure (auth error, network error, all models failed).
    Failed,
    /// Upstream auth failed (401/403) and should be surfaced immediately in UI/API.
    AuthFailed(u16),
}

/// Send a dummy request through the proxy for a specific account.
///
/// Tries models from MODEL_CASCADE in order. Returns the new rate_limit_reset
/// on success, or None on failure.
async fn send_dummy_request(
    client: &reqwest::Client,
    account: &RefreshableAccount,
    proxy_port: u16,
    use_tls: bool,
    internal_api_key: Option<&str>,
) -> RefreshResult {
    let protocol = if use_tls { "https" } else { "http" };
    let endpoint = format!("{protocol}://localhost:{proxy_port}/v1/messages");

    let message = {
        let mut rng = rand::rng();
        DUMMY_MESSAGES[rng.random_range(0..DUMMY_MESSAGES.len())]
    };

    for model in MODEL_CASCADE {
        debug!(
            account = %account.name,
            model,
            "Attempting auto-refresh"
        );

        let body = serde_json::json!({
            "model": model,
            "max_tokens": 10,
            "messages": [{"role": "user", "content": message}]
        });

        let mut req = client
            .post(&endpoint)
            .header("content-type", "application/json")
            .header("accept", "application/json")
            .header("anthropic-version", "2023-06-01")
            .header("x-better-ccflare-account-id", &account.id)
            .header("x-better-ccflare-force-account-strict", "true")
            .header("x-better-ccflare-bypass-session", "true");
        if let Some(key) = internal_api_key {
            req = req.header("x-api-key", key);
        }
        let result = req.json(&body).send().await;

        match result {
            Ok(resp) if resp.status() == 429 || resp.status() == 503 => {
                // Strict force-account mode returns 503 (not 429) when the
                // pinned account is exhausted. Treat both as rate limiting.
                warn!(
                    account = %account.name,
                    model,
                    status = %resp.status(),
                    "Auto-refresh got rate-limit response from proxy"
                );
                return RefreshResult::RateLimited;
            }
            Ok(resp) if resp.status().is_success() => {
                info!(
                    account = %account.name,
                    model,
                    status = %resp.status(),
                    "Auto-refresh successful"
                );

                // Extract new rate_limit_reset from response headers
                let reset_time = resp
                    .headers()
                    .get("anthropic-ratelimit-unified-reset")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|s| {
                        s.parse::<i64>().ok().or_else(|| {
                            chrono::DateTime::parse_from_rfc3339(s)
                                .ok()
                                .map(|dt| dt.timestamp_millis())
                        })
                    });

                return RefreshResult::Success(
                    reset_time.or(Some(chrono::Utc::now().timestamp_millis())),
                );
            }
            Ok(resp) if resp.status() == 401 || resp.status() == 403 => {
                let status = resp.status();
                let has_upstream_headers = resp.headers().contains_key("anthropic-organization-id")
                    || resp.headers().contains_key("request-id");
                let body_text = resp.text().await.unwrap_or_default();
                if !has_upstream_headers {
                    error!(
                        account = %account.name,
                        status = %status,
                        "Auto-refresh request was unauthorized before reaching upstream; not marking account auth-failed"
                    );
                    return RefreshResult::Failed;
                }
                // 401/403 can come from proxy auth (missing/invalid API key), not upstream account auth.
                if body_text.contains("API key required") || body_text.contains("Invalid API key") {
                    error!(
                        account = %account.name,
                        status = %status,
                        "Auto-refresh request was rejected by proxy auth; check internal API key config"
                    );
                    return RefreshResult::Failed;
                }
                error!(
                    account = %account.name,
                    status = %status,
                    "Authentication failed — account needs re-authentication"
                );
                return RefreshResult::AuthFailed(status.as_u16());
            }
            Ok(resp) if resp.status() == 404 => {
                debug!(
                    account = %account.name,
                    model,
                    "Model not found, trying next"
                );
                continue;
            }
            Ok(resp) => {
                warn!(
                    account = %account.name,
                    model,
                    status = %resp.status(),
                    "Auto-refresh request failed"
                );
                return RefreshResult::Failed;
            }
            Err(e) => {
                warn!(
                    account = %account.name,
                    model,
                    error = %e,
                    "Network error during auto-refresh"
                );
                continue;
            }
        }
    }

    warn!(
        account = %account.name,
        "All models failed for auto-refresh"
    );
    RefreshResult::Failed
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn test_account(id: &str, name: &str, reset: Option<i64>) -> RefreshableAccount {
        RefreshableAccount {
            id: id.to_string(),
            name: name.to_string(),
            rate_limit_reset: reset,
        }
    }

    // -- SchedulerState tests -----------------------------------------------

    #[test]
    fn should_refresh_first_time() {
        let state = SchedulerState::new();
        let account = test_account("a1", "TestAccount", Some(1000));
        assert!(state.should_refresh(&account, 2000));
    }

    #[test]
    fn should_refresh_window_passed() {
        let mut state = SchedulerState::new();
        state.last_refresh_reset.insert("a1".to_string(), 1000);
        let account = test_account("a1", "TestAccount", Some(1500));
        // now=2000, reset=1500 → reset has passed
        assert!(state.should_refresh(&account, 2000));
    }

    #[test]
    fn should_not_refresh_window_not_passed() {
        let mut state = SchedulerState::new();
        state.last_refresh_reset.insert("a1".to_string(), 1000);
        let now = chrono::Utc::now().timestamp_millis();
        // rate_limit_reset is in the future and same as last refresh
        let account = test_account("a1", "TestAccount", Some(now + 60_000));
        state
            .last_refresh_reset
            .insert("a1".to_string(), now + 60_000);
        assert!(!state.should_refresh(&account, now));
    }

    #[test]
    fn should_refresh_newer_reset_detected() {
        let mut state = SchedulerState::new();
        state.last_refresh_reset.insert("a1".to_string(), 1000);
        let now = chrono::Utc::now().timestamp_millis();
        // Database has newer reset than our tracking, and it's in the future
        let account = test_account("a1", "TestAccount", Some(now + 120_000));
        assert!(state.should_refresh(&account, now));
    }

    #[test]
    fn should_refresh_stale_reset() {
        let mut state = SchedulerState::new();
        state.last_refresh_reset.insert("a1".to_string(), 1000);
        let now = chrono::Utc::now().timestamp_millis();
        // rate_limit_reset is more than 24h old
        let stale = now - STALE_THRESHOLD_MS - 1000;
        let account = test_account("a1", "TestAccount", Some(stale));
        assert!(state.should_refresh(&account, now));
    }

    #[test]
    fn should_not_refresh_no_reset() {
        let mut state = SchedulerState::new();
        state.last_refresh_reset.insert("a1".to_string(), 1000);
        let account = test_account("a1", "TestAccount", None);
        assert!(!state.should_refresh(&account, 2000));
    }

    // -- Success/failure tracking -------------------------------------------

    #[test]
    fn record_success_resets_failures() {
        let mut state = SchedulerState::new();
        state.record_failure("a1");
        state.record_failure("a1");
        assert_eq!(state.consecutive_failures.get("a1"), Some(&2));

        state.record_success("a1", Some(5000));
        assert!(state.consecutive_failures.get("a1").is_none());
        assert_eq!(state.last_refresh_reset.get("a1"), Some(&5000));
    }

    #[test]
    fn record_failure_increments() {
        let mut state = SchedulerState::new();
        assert_eq!(state.record_failure("a1"), 1);
        assert_eq!(state.record_failure("a1"), 2);
        assert_eq!(state.record_failure("a1"), 3);
    }

    #[test]
    fn failure_threshold_detection() {
        let mut state = SchedulerState::new();
        for _ in 0..FAILURE_THRESHOLD {
            state.record_failure("a1");
        }
        assert_eq!(
            state.consecutive_failures.get("a1"),
            Some(&FAILURE_THRESHOLD)
        );
    }

    // -- Cleanup tests ------------------------------------------------------

    #[test]
    fn cleanup_removes_inactive_accounts() {
        let mut state = SchedulerState::new();
        state.last_refresh_reset.insert("a1".to_string(), 1000);
        state.last_refresh_reset.insert("a2".to_string(), 2000);
        state.consecutive_failures.insert("a1".to_string(), 3);
        state.consecutive_failures.insert("a3".to_string(), 1);

        // Only a2 is active
        state.cleanup(&["a2".to_string()]);

        assert!(!state.last_refresh_reset.contains_key("a1"));
        assert!(state.last_refresh_reset.contains_key("a2"));
        assert!(!state.consecutive_failures.contains_key("a1"));
        assert!(!state.consecutive_failures.contains_key("a3"));
    }

    #[test]
    fn cleanup_empty_active_list_clears_all() {
        let mut state = SchedulerState::new();
        state.last_refresh_reset.insert("a1".to_string(), 1000);
        state.consecutive_failures.insert("a1".to_string(), 1);

        state.cleanup(&[]);

        assert!(state.last_refresh_reset.is_empty());
        assert!(state.consecutive_failures.is_empty());
    }

    // -- Model cascade and constants ----------------------------------------

    #[test]
    fn model_cascade_has_expected_models() {
        assert_eq!(MODEL_CASCADE.len(), 5);
        assert!(MODEL_CASCADE[0].contains("haiku"));
        assert!(MODEL_CASCADE.last().unwrap().contains("sonnet"));
    }

    #[test]
    fn dummy_messages_non_empty() {
        assert!(!DUMMY_MESSAGES.is_empty());
        for msg in DUMMY_MESSAGES {
            assert!(!msg.is_empty());
        }
    }

    // -- Service lifecycle --------------------------------------------------

    struct MockRefreshSource;

    impl RefreshAccountSource for MockRefreshSource {
        fn get_refreshable_accounts(&self) -> Vec<RefreshableAccount> {
            vec![]
        }
        fn get_active_refresh_account_ids(&self) -> Vec<String> {
            vec![]
        }
    }

    #[tokio::test]
    async fn scheduler_starts_and_stops() {
        let source = Arc::new(MockRefreshSource);
        let scheduler = AutoRefreshScheduler::start(source, 8080, false, None);
        scheduler.shutdown().await;
    }

    #[tokio::test]
    async fn check_and_refresh_no_accounts() {
        let source: Arc<dyn RefreshAccountSource> = Arc::new(MockRefreshSource);
        let state = Arc::new(Mutex::new(SchedulerState::new()));
        let client = reqwest::Client::new();

        check_and_refresh(&source, &state, &client, 8080, false, None).await;
        // Should not panic
    }

    struct MockWithAccounts {
        accounts: Vec<RefreshableAccount>,
    }

    impl RefreshAccountSource for MockWithAccounts {
        fn get_refreshable_accounts(&self) -> Vec<RefreshableAccount> {
            self.accounts.clone()
        }
        fn get_active_refresh_account_ids(&self) -> Vec<String> {
            self.accounts.iter().map(|a| a.id.clone()).collect()
        }
    }

    #[tokio::test]
    async fn check_and_refresh_filters_accounts() {
        let source: Arc<dyn RefreshAccountSource> = Arc::new(MockWithAccounts {
            accounts: vec![test_account("a1", "Test", Some(1000))],
        });
        let state = Arc::new(Mutex::new(SchedulerState::new()));
        let client = reqwest::Client::new();

        // First call should try to refresh (first-time detection)
        // Will fail because no actual proxy is running, but shouldn't panic
        check_and_refresh(&source, &state, &client, 19999, false, None).await;
    }
}
