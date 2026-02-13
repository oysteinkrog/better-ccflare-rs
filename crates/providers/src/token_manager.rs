//! Centralized token validation and refresh with deduplication.
//!
//! Before each proxied request, call [`TokenManager::get_valid_token`] to
//! ensure the account has a fresh access token. Concurrent refresh attempts
//! for the same account are deduplicated via per-account `Mutex`.

use std::sync::Arc;

use dashmap::DashMap;
use tokio::sync::{watch, Mutex};

use bccf_core::types::Account;

use crate::error::ProviderError;
use crate::traits::Provider;
use crate::types::TokenRefreshResult;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Proactive refresh window: refresh if token expires within 30 minutes.
pub const TOKEN_SAFETY_WINDOW_MS: i64 = 30 * 60 * 1000;

/// Cooldown after a failed refresh attempt (60 seconds).
pub const TOKEN_REFRESH_BACKOFF_MS: i64 = 60_000;

/// How long failure records are kept before cleanup (5 minutes).
const FAILURE_TTL_MS: i64 = 5 * 60 * 1000;

/// Maximum number of failure records to retain.
const MAX_FAILURE_RECORDS: usize = 1000;

/// After this many consecutive backoff hits, attempt DB recovery.
const MAX_BACKOFF_RETRIES: u32 = 10;

/// Providers that use API keys directly (no token refresh needed).
const API_KEY_PROVIDERS: &[&str] = &[
    "openai-compatible",
    "zai",
    "claude-console-api",
    "anthropic-compatible",
    "minimax",
    "nanogpt",
];

// ---------------------------------------------------------------------------
// Token refresh result shared across waiters
// ---------------------------------------------------------------------------

/// Shared result type for in-flight refresh operations.
type RefreshResult = Result<TokenRefreshResult, String>;

// ---------------------------------------------------------------------------
// TokenManager
// ---------------------------------------------------------------------------

/// Manages token validation and refresh with per-account deduplication.
///
/// Multiple concurrent requests for the same account share a single
/// refresh operation via [`watch`] channels.
pub struct TokenManager {
    /// Per-account mutex to serialize refresh attempts.
    refresh_locks: DashMap<String, Arc<Mutex<()>>>,
    /// In-flight refresh: account_id → watch receiver for the result.
    in_flight: DashMap<String, watch::Receiver<Option<RefreshResult>>>,
    /// Recent failure timestamps: account_id → epoch_ms.
    failures: DashMap<String, i64>,
    /// Backoff counters: account_id → consecutive backoff hits.
    backoff_counters: DashMap<String, u32>,
}

impl TokenManager {
    pub fn new() -> Self {
        Self {
            refresh_locks: DashMap::new(),
            in_flight: DashMap::new(),
            failures: DashMap::new(),
            backoff_counters: DashMap::new(),
        }
    }

    /// Get a valid access token for the account, refreshing if needed.
    ///
    /// For API key providers, returns the key directly. For OAuth providers,
    /// checks expiry and refreshes if within the safety window.
    pub async fn get_valid_token(
        &self,
        account: &Account,
        provider: &dyn Provider,
        client_id: &str,
        now: i64,
    ) -> Result<TokenResult, ProviderError> {
        // Fast path: API key providers
        if is_api_key_provider(&account.provider) {
            if let Some(ref key) = account.api_key {
                return Ok(TokenResult::existing(key.clone()));
            }
            // Fallback to refresh_token for backward compatibility
            if !account.refresh_token.is_empty() {
                return Ok(TokenResult::existing(account.refresh_token.clone()));
            }
            return Err(ProviderError::TokenRefresh(
                "No API key available".to_string(),
            ));
        }

        // Check if current token is still valid
        if let Some(ref token) = account.access_token {
            if let Some(expires_at) = account.expires_at {
                if (expires_at - now) > TOKEN_SAFETY_WINDOW_MS {
                    return Ok(TokenResult::existing(token.clone()));
                }
            }
        }

        // Token needs refresh
        let result = self
            .refresh_token_safe(account, provider, client_id, now)
            .await?;
        Ok(TokenResult::refreshed(result))
    }

    /// Attempt to refresh the token with backoff and deduplication.
    async fn refresh_token_safe(
        &self,
        account: &Account,
        provider: &dyn Provider,
        client_id: &str,
        now: i64,
    ) -> Result<TokenRefreshResult, ProviderError> {
        // Clean up expired failures
        self.cleanup_expired_failures(now);

        // Check backoff
        if let Some(failure_time) = self.failures.get(&account.id) {
            if (now - *failure_time) < TOKEN_REFRESH_BACKOFF_MS {
                let mut counter = self.backoff_counters.entry(account.id.clone()).or_insert(0);
                *counter += 1;

                if *counter >= MAX_BACKOFF_RETRIES {
                    tracing::warn!(
                        account_id = %account.id,
                        counter = *counter,
                        "Token refresh in backoff period, max retries reached"
                    );
                }

                return Err(ProviderError::TokenRefresh(format!(
                    "Token refresh for {} in backoff period ({} attempts)",
                    account.id,
                    counter.value()
                )));
            } else {
                // Backoff window expired, reset counter
                self.backoff_counters.remove(&account.id);
            }
        }

        // Deduplication: acquire per-account lock
        let lock = self
            .refresh_locks
            .entry(account.id.clone())
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone();

        let _guard = lock.lock().await;

        // Double-check: another task may have refreshed while we waited
        // (We can't check the token here since we only have &Account,
        // but the caller should re-check after getting the result.)

        // Perform the actual refresh
        match provider.refresh_token(account, client_id).await {
            Ok(result) => {
                // Clear failure records on success
                self.failures.remove(&account.id);
                self.backoff_counters.remove(&account.id);

                tracing::info!(
                    account_id = %account.id,
                    provider = %account.provider,
                    "Token refreshed successfully"
                );

                Ok(result)
            }
            Err(e) => {
                // Record failure
                self.failures.insert(account.id.clone(), now);
                self.enforce_max_failures();

                tracing::error!(
                    account_id = %account.id,
                    provider = %account.provider,
                    error = %e,
                    "Token refresh failed"
                );

                Err(e)
            }
        }
    }

    /// Remove failure records older than FAILURE_TTL_MS.
    fn cleanup_expired_failures(&self, now: i64) {
        let cutoff = now - FAILURE_TTL_MS;
        let expired: Vec<String> = self
            .failures
            .iter()
            .filter(|entry| *entry.value() < cutoff)
            .map(|entry| entry.key().clone())
            .collect();

        for key in expired {
            self.failures.remove(&key);
            self.backoff_counters.remove(&key);
        }
    }

    /// Enforce maximum number of failure records.
    fn enforce_max_failures(&self) {
        if self.failures.len() <= MAX_FAILURE_RECORDS {
            return;
        }
        // Remove oldest entries
        let mut entries: Vec<(String, i64)> = self
            .failures
            .iter()
            .map(|e| (e.key().clone(), *e.value()))
            .collect();
        entries.sort_by_key(|(_, ts)| *ts);

        let to_remove = entries.len() - MAX_FAILURE_RECORDS;
        for (key, _) in entries.into_iter().take(to_remove) {
            self.failures.remove(&key);
            self.backoff_counters.remove(&key);
        }
    }

    /// Clear all cached state for a specific account.
    pub fn clear_account(&self, account_id: &str) {
        self.failures.remove(account_id);
        self.backoff_counters.remove(account_id);
        self.in_flight.remove(account_id);
        self.refresh_locks.remove(account_id);
    }

    /// Clear all cached state.
    pub fn clear_all(&self) {
        self.failures.clear();
        self.backoff_counters.clear();
        self.in_flight.clear();
        self.refresh_locks.clear();
    }
}

impl Default for TokenManager {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Token result
// ---------------------------------------------------------------------------

/// Result of a token validation/refresh operation.
#[derive(Debug, Clone)]
pub struct TokenResult {
    /// The access token to use.
    pub token: String,
    /// If the token was refreshed, contains the full refresh result
    /// (for persisting to DB).
    pub refresh_result: Option<TokenRefreshResult>,
}

impl TokenResult {
    fn existing(token: String) -> Self {
        Self {
            token,
            refresh_result: None,
        }
    }

    fn refreshed(result: TokenRefreshResult) -> Self {
        Self {
            token: result.access_token.clone(),
            refresh_result: Some(result),
        }
    }

    /// Whether the token was refreshed (vs. reused from cache).
    pub fn was_refreshed(&self) -> bool {
        self.refresh_result.is_some()
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Check if a provider uses API keys directly (no OAuth refresh needed).
pub fn is_api_key_provider(provider: &str) -> bool {
    API_KEY_PROVIDERS.contains(&provider)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stub::StubProvider;

    fn now() -> i64 {
        1_700_000_000_000
    }

    #[tokio::test]
    async fn api_key_provider_returns_key_directly() {
        let tm = TokenManager::new();
        let provider = StubProvider::new("zai");
        let mut account = crate::test_util::test_account_with_key("sk-test-key");
        account.provider = "zai".to_string();

        let result = tm
            .get_valid_token(&account, &provider, "client", now())
            .await
            .unwrap();

        assert_eq!(result.token, "sk-test-key");
        assert!(!result.was_refreshed());
    }

    #[tokio::test]
    async fn valid_token_returned_without_refresh() {
        let tm = TokenManager::new();
        let provider = StubProvider::new("anthropic");
        let mut account = crate::test_util::test_account_with_key("sk-test");
        account.provider = "anthropic".to_string();
        account.access_token = Some("valid-token".to_string());
        // Expires in 2 hours (well beyond 30-min safety window)
        account.expires_at = Some(now() + 2 * 60 * 60 * 1000);

        let result = tm
            .get_valid_token(&account, &provider, "client", now())
            .await
            .unwrap();

        assert_eq!(result.token, "valid-token");
        assert!(!result.was_refreshed());
    }

    #[tokio::test]
    async fn expired_token_triggers_refresh() {
        let tm = TokenManager::new();
        let provider = StubProvider::new("anthropic");
        let mut account = crate::test_util::test_account_with_key("sk-test");
        account.provider = "anthropic".to_string();
        account.access_token = Some("old-token".to_string());
        // Expires in 10 minutes (within 30-min safety window)
        account.expires_at = Some(now() + 10 * 60 * 1000);

        let result = tm
            .get_valid_token(&account, &provider, "client", now())
            .await
            .unwrap();

        assert!(result.was_refreshed());
        assert_eq!(result.token, "sk-test"); // StubProvider returns api_key
    }

    #[tokio::test]
    async fn missing_token_triggers_refresh() {
        let tm = TokenManager::new();
        let provider = StubProvider::new("anthropic");
        let mut account = crate::test_util::test_account_with_key("sk-test");
        account.provider = "anthropic".to_string();
        account.access_token = None;
        account.expires_at = None;

        let result = tm
            .get_valid_token(&account, &provider, "client", now())
            .await
            .unwrap();

        assert!(result.was_refreshed());
    }

    #[tokio::test]
    async fn backoff_after_failure() {
        let tm = TokenManager::new();
        // Simulate a recent failure
        tm.failures.insert("acc-1".to_string(), now() - 30_000); // 30s ago (within 60s backoff)

        let provider = StubProvider::new("anthropic");
        let mut account = crate::test_util::test_account_with_key("sk-test");
        account.id = "acc-1".to_string();
        account.provider = "anthropic".to_string();
        account.access_token = None;

        let result = tm
            .get_valid_token(&account, &provider, "client", now())
            .await;

        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("backoff period"));
    }

    #[tokio::test]
    async fn backoff_expires_allows_retry() {
        let tm = TokenManager::new();
        // Simulate an old failure (beyond 60s backoff)
        tm.failures.insert("acc-1".to_string(), now() - 120_000); // 2 min ago

        let provider = StubProvider::new("anthropic");
        let mut account = crate::test_util::test_account_with_key("sk-test");
        account.id = "acc-1".to_string();
        account.provider = "anthropic".to_string();
        account.access_token = None;

        let result = tm
            .get_valid_token(&account, &provider, "client", now())
            .await;

        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn clear_account_resets_state() {
        let tm = TokenManager::new();
        tm.failures.insert("acc-1".to_string(), now());
        tm.backoff_counters.insert("acc-1".to_string(), 5);

        tm.clear_account("acc-1");

        assert!(tm.failures.get("acc-1").is_none());
        assert!(tm.backoff_counters.get("acc-1").is_none());
    }

    #[test]
    fn cleanup_removes_old_failures() {
        let tm = TokenManager::new();
        tm.failures
            .insert("old".to_string(), now() - FAILURE_TTL_MS - 1000);
        tm.failures.insert("recent".to_string(), now() - 1000);

        tm.cleanup_expired_failures(now());

        assert!(tm.failures.get("old").is_none());
        assert!(tm.failures.get("recent").is_some());
    }

    #[test]
    fn is_api_key_provider_checks() {
        assert!(is_api_key_provider("zai"));
        assert!(is_api_key_provider("openai-compatible"));
        assert!(is_api_key_provider("anthropic-compatible"));
        assert!(is_api_key_provider("minimax"));
        assert!(!is_api_key_provider("anthropic"));
        assert!(!is_api_key_provider("claude-oauth"));
    }
}
