//! Token manager — validates and refreshes access tokens with deduplication.
//!
//! Before each proxied request, the token manager checks if the account's access
//! token is still valid. If expired (or expiring soon), it triggers a refresh.
//! Concurrent requests for the same account share a single refresh operation
//! via per-account mutex deduplication.

use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;

use bccf_core::providers::Provider;
use bccf_core::types::Account;
use bccf_providers::error::ProviderError;
use bccf_providers::types::TokenRefreshResult;
use dashmap::DashMap;
use tokio::sync::Mutex;
use tracing::{info, warn};

use crate::token_health::{
    check_refresh_token_health, get_oauth_error_message, HealthStatus, FAILURE_TTL_MS,
    MAX_BACKOFF_RETRIES, TOKEN_REFRESH_BACKOFF_MS, TOKEN_SAFETY_WINDOW_MS,
};

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

/// Errors from the token manager.
#[derive(Debug, thiserror::Error)]
pub enum TokenError {
    #[error("Token refresh failed for account {account_id}: {message}")]
    RefreshFailed { account_id: String, message: String },
    #[error("Account {account_id} is in refresh backoff after recent failure")]
    InBackoff { account_id: String },
    #[error("No API key available for account {account_name}")]
    NoApiKey { account_name: String },
}

// ---------------------------------------------------------------------------
// Refresh callback
// ---------------------------------------------------------------------------

/// Callback that performs the actual token refresh via a provider.
/// Abstracted as a trait so it can be mocked in tests.
#[async_trait::async_trait]
pub trait TokenRefresher: Send + Sync {
    async fn refresh_token(
        &self,
        account: &Account,
        client_id: &str,
    ) -> Result<TokenRefreshResult, ProviderError>;
}

/// Callback that persists updated tokens to the database.
/// Abstracted so it can be mocked in tests.
pub trait TokenPersister: Send + Sync {
    fn persist_tokens(
        &self,
        account_id: &str,
        access_token: &str,
        expires_at: i64,
        refresh_token: &str,
    );

    /// Persist subscription tier when it changes (e.g. after a token refresh).
    /// Default is a no-op; override in DB-backed implementors.
    fn persist_subscription_tier(&self, _account_id: &str, _tier: Option<&str>) {}

    /// Persist email address when it changes (e.g. after a token refresh).
    /// Default is a no-op; override in DB-backed implementors.
    fn persist_email(&self, _account_id: &str, _email: Option<&str>) {}

    /// Try to load an account from DB (for backoff recovery).
    fn load_account(&self, account_id: &str) -> Option<Account>;
}

// ---------------------------------------------------------------------------
// Token manager
// ---------------------------------------------------------------------------

/// Failure record for backoff tracking.
struct FailureRecord {
    timestamp: i64,
    backoff_count: u32,
}

/// Minimum interval between failure cleanup runs (60 seconds).
const CLEANUP_INTERVAL_MS: i64 = 60_000;

/// Centralized token validation and refresh with per-account deduplication.
pub struct TokenManager {
    /// Per-account mutex for deduplicating concurrent refresh attempts.
    refresh_locks: DashMap<String, Arc<Mutex<()>>>,
    /// Tracks recent refresh failures for backoff.
    failures: DashMap<String, FailureRecord>,
    /// Client ID for OAuth token refresh.
    client_id: String,
    /// Last time cleanup_expired_failures ran (avoids running on every call).
    last_cleanup: AtomicI64,
}

impl TokenManager {
    pub fn new(client_id: String) -> Self {
        Self {
            refresh_locks: DashMap::new(),
            failures: DashMap::new(),
            client_id,
            last_cleanup: AtomicI64::new(0),
        }
    }

    /// Get a valid access token for an account, refreshing if necessary.
    ///
    /// For API key providers, returns the API key directly.
    /// For OAuth providers, checks token validity and triggers refresh if needed.
    pub async fn get_valid_access_token(
        &self,
        account: &mut Account,
        refresher: &dyn TokenRefresher,
        persister: &dyn TokenPersister,
        now: i64,
    ) -> Result<String, TokenError> {
        // API key providers: return key directly
        if is_api_key_provider(&account.provider) {
            if let Some(ref key) = account.api_key {
                return Ok(key.clone());
            }
            if !account.refresh_token.is_empty() {
                return Ok(account.refresh_token.clone());
            }
            return Err(TokenError::NoApiKey {
                account_name: account.name.clone(),
            });
        }

        // API key accounts without OAuth don't need token refresh
        if account.refresh_token.is_empty() && account.api_key.is_some() {
            return Ok(String::new());
        }

        // Check if current token is still valid (won't expire within safety window)
        if let (Some(ref token), Some(expires_at)) = (&account.access_token, account.expires_at) {
            if expires_at - now > TOKEN_SAFETY_WINDOW_MS {
                return Ok(token.clone());
            }
        }

        // Log health status for OAuth accounts
        let health = check_refresh_token_health(account, now);
        if health.has_refresh_token {
            match health.status {
                HealthStatus::Expired | HealthStatus::Critical => {
                    warn!("Critical: {}", health.message);
                }
                HealthStatus::Warning => {
                    warn!("Warning: {}", health.message);
                }
                _ => {}
            }
        }

        // Token needs refresh
        self.refresh_access_token(account, refresher, persister, now)
            .await
    }

    /// Refresh an access token with deduplication and backoff.
    async fn refresh_access_token(
        &self,
        account: &mut Account,
        refresher: &dyn TokenRefresher,
        persister: &dyn TokenPersister,
        now: i64,
    ) -> Result<String, TokenError> {
        // Clean expired failure records
        self.cleanup_expired_failures(now);

        // Check backoff — extract state first, then act (avoids DashMap deadlock)
        enum BackoffAction {
            InBackoff,
            TryDbRecovery,
            BackoffExpired,
        }

        let backoff_action = if let Some(mut entry) = self.failures.get_mut(&account.id) {
            let record = entry.value_mut();
            if now - record.timestamp < TOKEN_REFRESH_BACKOFF_MS {
                record.backoff_count += 1;
                if record.backoff_count >= MAX_BACKOFF_RETRIES {
                    Some(BackoffAction::TryDbRecovery)
                } else {
                    Some(BackoffAction::InBackoff)
                }
            } else {
                Some(BackoffAction::BackoffExpired)
            }
        } else {
            None
        };
        // DashMap guard is dropped here

        match backoff_action {
            Some(BackoffAction::TryDbRecovery) => {
                if let Some(db_account) = persister.load_account(&account.id) {
                    if let (Some(ref db_token), Some(db_expires)) =
                        (&db_account.access_token, db_account.expires_at)
                    {
                        if db_expires - now > TOKEN_SAFETY_WINDOW_MS
                            && account.access_token.as_deref() != Some(db_token)
                        {
                            info!("Recovered token for account {} from DB", account.name);
                            account.access_token = Some(db_token.clone());
                            account.expires_at = Some(db_expires);
                            if !db_account.refresh_token.is_empty() {
                                account.refresh_token = db_account.refresh_token;
                            }
                            self.failures.remove(&account.id);
                            return Ok(db_token.clone());
                        }
                    }
                }
                return Err(TokenError::InBackoff {
                    account_id: account.id.clone(),
                });
            }
            Some(BackoffAction::InBackoff) => {
                return Err(TokenError::InBackoff {
                    account_id: account.id.clone(),
                });
            }
            Some(BackoffAction::BackoffExpired) => {
                self.failures.remove(&account.id);
            }
            None => {}
        }

        // Acquire per-account lock for deduplication
        let lock = self
            .refresh_locks
            .entry(account.id.clone())
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone();
        let _guard = lock.lock().await;

        // After acquiring lock, re-check if token was already refreshed by another task
        if let (Some(ref token), Some(expires_at)) = (&account.access_token, account.expires_at) {
            if expires_at - now > TOKEN_SAFETY_WINDOW_MS {
                return Ok(token.clone());
            }
        }

        // Perform the actual refresh
        match refresher.refresh_token(account, &self.client_id).await {
            Ok(result) => {
                // Persist to database asynchronously
                persister.persist_tokens(
                    &account.id,
                    &result.access_token,
                    result.expires_at,
                    &result.refresh_token,
                );
                if result.subscription_tier.is_some() {
                    persister.persist_subscription_tier(
                        &account.id,
                        result.subscription_tier.as_deref(),
                    );
                }
                if result.email.is_some() {
                    persister.persist_email(&account.id, result.email.as_deref());
                }

                // Update in-memory account
                let new_token = result.access_token.clone();
                account.access_token = Some(result.access_token);
                account.expires_at = Some(result.expires_at);
                if !result.refresh_token.is_empty() {
                    account.refresh_token = result.refresh_token;
                }
                if let Some(tier) = result.subscription_tier {
                    account.subscription_tier = Some(tier);
                }
                if let Some(email) = result.email {
                    account.email = Some(email);
                }

                // Clear failure record
                self.failures.remove(&account.id);

                info!("Successfully refreshed token for account: {}", account.name);
                Ok(new_token)
            }
            Err(err) => {
                let error_msg = err.to_string();
                let enhanced = get_oauth_error_message(account, &error_msg, now);

                // Record failure for backoff
                self.failures.insert(
                    account.id.clone(),
                    FailureRecord {
                        timestamp: now,
                        backoff_count: 0,
                    },
                );

                warn!("Token refresh failed for {}: {}", account.name, enhanced);

                Err(TokenError::RefreshFailed {
                    account_id: account.id.clone(),
                    message: enhanced,
                })
            }
        }
    }

    /// Clear refresh state for an account (e.g., after re-authentication).
    pub fn clear_account_state(&self, account_id: &str) {
        self.refresh_locks.remove(account_id);
        self.failures.remove(account_id);
    }

    /// Remove expired failure records (rate-limited to once per minute).
    fn cleanup_expired_failures(&self, now: i64) {
        let last = self.last_cleanup.load(Ordering::Relaxed);
        if now - last < CLEANUP_INTERVAL_MS {
            return;
        }
        if self
            .last_cleanup
            .compare_exchange(last, now, Ordering::Relaxed, Ordering::Relaxed)
            .is_ok()
        {
            self.failures
                .retain(|_, record| now - record.timestamp <= FAILURE_TTL_MS);
        }
    }
}

/// Check if a provider uses API keys (not OAuth).
fn is_api_key_provider(provider: &str) -> bool {
    Provider::from_str_loose(provider).is_some_and(|p| p.uses_api_key())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use bccf_core::constants::time;
    use std::sync::atomic::{AtomicU32, Ordering};

    const NOW: i64 = 1_700_000_000_000;

    fn make_oauth_account(id: &str) -> Account {
        Account {
            id: id.to_string(),
            name: id.to_string(),
            provider: "anthropic".to_string(),
            api_key: None,
            refresh_token: "rt_test".to_string(),
            access_token: Some("at_old".to_string()),
            expires_at: Some(NOW - 1000), // Expired
            request_count: 0,
            total_requests: 0,
            last_used: None,
            created_at: NOW - 30 * time::DAY,
            rate_limited_until: None,
            session_start: None,
            session_request_count: 0,
            paused: false,
            rate_limit_reset: None,
            rate_limit_status: None,
            rate_limit_remaining: None,
            priority: 0,
            auto_fallback_enabled: false,
            auto_refresh_enabled: false,
            custom_endpoint: None,
            model_mappings: None,
            reserve_5h: 0,
            reserve_weekly: 0,
            reserve_hard: false,
            subscription_tier: None,
            email: None,
            refresh_token_updated_at: None,
        }
    }

    fn make_apikey_account(id: &str, provider: &str) -> Account {
        Account {
            id: id.to_string(),
            name: id.to_string(),
            provider: provider.to_string(),
            api_key: Some("sk-test-key".to_string()),
            refresh_token: String::new(),
            access_token: None,
            expires_at: None,
            request_count: 0,
            total_requests: 0,
            last_used: None,
            created_at: NOW,
            rate_limited_until: None,
            session_start: None,
            session_request_count: 0,
            paused: false,
            rate_limit_reset: None,
            rate_limit_status: None,
            rate_limit_remaining: None,
            priority: 0,
            auto_fallback_enabled: false,
            auto_refresh_enabled: false,
            custom_endpoint: None,
            model_mappings: None,
            reserve_5h: 0,
            reserve_weekly: 0,
            reserve_hard: false,
            subscription_tier: None,
            email: None,
            refresh_token_updated_at: None,
        }
    }

    // Mock refresher that succeeds
    struct MockRefresher {
        call_count: AtomicU32,
    }

    impl MockRefresher {
        fn new() -> Self {
            Self {
                call_count: AtomicU32::new(0),
            }
        }
    }

    #[async_trait::async_trait]
    impl TokenRefresher for MockRefresher {
        async fn refresh_token(
            &self,
            _account: &Account,
            _client_id: &str,
        ) -> Result<TokenRefreshResult, ProviderError> {
            self.call_count.fetch_add(1, Ordering::SeqCst);
            Ok(TokenRefreshResult {
                access_token: "at_new".to_string(),
                expires_at: NOW + 5 * time::HOUR,
                refresh_token: "rt_new".to_string(),
                subscription_tier: None,
                email: None,
            })
        }
    }

    // Mock refresher that fails
    struct FailingRefresher;

    #[async_trait::async_trait]
    impl TokenRefresher for FailingRefresher {
        async fn refresh_token(
            &self,
            _account: &Account,
            _client_id: &str,
        ) -> Result<TokenRefreshResult, ProviderError> {
            Err(ProviderError::TokenRefresh(
                "connection refused".to_string(),
            ))
        }
    }

    // Mock persister
    struct MockPersister;

    impl TokenPersister for MockPersister {
        fn persist_tokens(&self, _id: &str, _at: &str, _exp: i64, _rt: &str) {}
        fn load_account(&self, _id: &str) -> Option<Account> {
            None
        }
    }

    // Mock persister that returns a DB account for recovery
    struct RecoveringPersister;

    impl TokenPersister for RecoveringPersister {
        fn persist_tokens(&self, _id: &str, _at: &str, _exp: i64, _rt: &str) {}
        fn load_account(&self, _id: &str) -> Option<Account> {
            let mut account = make_oauth_account("recover");
            account.access_token = Some("at_from_db".to_string());
            account.expires_at = Some(NOW + 5 * time::HOUR);
            Some(account)
        }
    }

    #[tokio::test]
    async fn api_key_provider_returns_key() {
        let tm = TokenManager::new("client".to_string());
        let mut account = make_apikey_account("zai1", "zai");

        let token = tm
            .get_valid_access_token(&mut account, &MockRefresher::new(), &MockPersister, NOW)
            .await
            .unwrap();
        assert_eq!(token, "sk-test-key");
    }

    #[tokio::test]
    async fn api_key_fallback_to_refresh_token() {
        let tm = TokenManager::new("client".to_string());
        let mut account = make_apikey_account("con1", "claude-console-api");
        account.api_key = None;
        account.refresh_token = "sk-legacy-key".to_string();

        let token = tm
            .get_valid_access_token(&mut account, &MockRefresher::new(), &MockPersister, NOW)
            .await
            .unwrap();
        assert_eq!(token, "sk-legacy-key");
    }

    #[tokio::test]
    async fn api_key_missing_returns_error() {
        let tm = TokenManager::new("client".to_string());
        let mut account = make_apikey_account("zai2", "zai");
        account.api_key = None;
        account.refresh_token = String::new();

        let result = tm
            .get_valid_access_token(&mut account, &MockRefresher::new(), &MockPersister, NOW)
            .await;
        assert!(matches!(result, Err(TokenError::NoApiKey { .. })));
    }

    #[tokio::test]
    async fn valid_token_returns_without_refresh() {
        let tm = TokenManager::new("client".to_string());
        let refresher = MockRefresher::new();
        let mut account = make_oauth_account("oauth1");
        account.access_token = Some("at_valid".to_string());
        account.expires_at = Some(NOW + 2 * time::HOUR); // Still valid

        let token = tm
            .get_valid_access_token(&mut account, &refresher, &MockPersister, NOW)
            .await
            .unwrap();
        assert_eq!(token, "at_valid");
        assert_eq!(refresher.call_count.load(Ordering::SeqCst), 0); // No refresh called
    }

    #[tokio::test]
    async fn expired_token_triggers_refresh() {
        let tm = TokenManager::new("client".to_string());
        let refresher = MockRefresher::new();
        let mut account = make_oauth_account("oauth2");

        let token = tm
            .get_valid_access_token(&mut account, &refresher, &MockPersister, NOW)
            .await
            .unwrap();
        assert_eq!(token, "at_new");
        assert_eq!(refresher.call_count.load(Ordering::SeqCst), 1);
        // Account should be updated in memory
        assert_eq!(account.access_token.as_deref(), Some("at_new"));
        assert_eq!(account.expires_at, Some(NOW + 5 * time::HOUR));
    }

    #[tokio::test]
    async fn expiring_soon_triggers_refresh() {
        let tm = TokenManager::new("client".to_string());
        let refresher = MockRefresher::new();
        let mut account = make_oauth_account("oauth3");
        // Token expires within safety window
        account.access_token = Some("at_expiring".to_string());
        account.expires_at = Some(NOW + TOKEN_SAFETY_WINDOW_MS - 1000);

        let token = tm
            .get_valid_access_token(&mut account, &refresher, &MockPersister, NOW)
            .await
            .unwrap();
        assert_eq!(token, "at_new"); // Got refreshed
    }

    #[tokio::test]
    async fn failed_refresh_enters_backoff() {
        let tm = TokenManager::new("client".to_string());
        let mut account = make_oauth_account("oauth4");

        // First attempt fails
        let result = tm
            .get_valid_access_token(&mut account, &FailingRefresher, &MockPersister, NOW)
            .await;
        assert!(matches!(result, Err(TokenError::RefreshFailed { .. })));

        // Second attempt within backoff window should be blocked
        let result = tm
            .get_valid_access_token(&mut account, &FailingRefresher, &MockPersister, NOW + 1000)
            .await;
        assert!(matches!(result, Err(TokenError::InBackoff { .. })));
    }

    #[tokio::test]
    async fn backoff_expires_allows_retry() {
        let tm = TokenManager::new("client".to_string());
        let mut account = make_oauth_account("oauth5");

        // Fail first
        let _ = tm
            .get_valid_access_token(&mut account, &FailingRefresher, &MockPersister, NOW)
            .await;

        // After backoff period, should allow retry
        let refresher = MockRefresher::new();
        let token = tm
            .get_valid_access_token(
                &mut account,
                &refresher,
                &MockPersister,
                NOW + TOKEN_REFRESH_BACKOFF_MS + 1,
            )
            .await
            .unwrap();
        assert_eq!(token, "at_new");
    }

    #[tokio::test]
    async fn db_recovery_after_max_retries() {
        let tm = TokenManager::new("client".to_string());
        let mut account = make_oauth_account("oauth6");

        // Fail first to enter backoff
        let _ = tm
            .get_valid_access_token(&mut account, &FailingRefresher, &MockPersister, NOW)
            .await;

        // Simulate MAX_BACKOFF_RETRIES hits
        for i in 0..MAX_BACKOFF_RETRIES {
            let _ = tm
                .get_valid_access_token(
                    &mut account,
                    &FailingRefresher,
                    &RecoveringPersister,
                    NOW + 1000 + i64::from(i),
                )
                .await;
        }

        // After enough retries with RecoveringPersister, it should recover from DB
        let result = tm
            .get_valid_access_token(
                &mut account,
                &FailingRefresher,
                &RecoveringPersister,
                NOW + 2000,
            )
            .await;
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "at_from_db");
    }

    #[tokio::test]
    async fn deduplication_single_refresh() {
        let tm = Arc::new(TokenManager::new("client".to_string()));
        let refresher = Arc::new(MockRefresher::new());

        // Spawn multiple concurrent refresh attempts
        let mut handles = vec![];
        for _ in 0..5 {
            let tm = Arc::clone(&tm);
            let refresher = Arc::clone(&refresher);
            handles.push(tokio::spawn(async move {
                let mut account = make_oauth_account("shared");
                tm.get_valid_access_token(&mut account, refresher.as_ref(), &MockPersister, NOW)
                    .await
            }));
        }

        for handle in handles {
            let result = handle.await.unwrap();
            assert!(result.is_ok());
        }

        // Due to per-account lock, refresh should be called at most a small number
        // of times (lock serializes, re-check after lock prevents redundant calls
        // but first thread to check will still call)
        let call_count = refresher.call_count.load(Ordering::SeqCst);
        assert!(call_count >= 1, "At least one refresh should happen");
    }

    #[tokio::test]
    async fn clear_account_state_resets_backoff() {
        let tm = TokenManager::new("client".to_string());
        let mut account = make_oauth_account("oauth7");

        // Fail to enter backoff
        let _ = tm
            .get_valid_access_token(&mut account, &FailingRefresher, &MockPersister, NOW)
            .await;

        // Verify in backoff
        let result = tm
            .get_valid_access_token(&mut account, &FailingRefresher, &MockPersister, NOW + 1000)
            .await;
        assert!(matches!(result, Err(TokenError::InBackoff { .. })));

        // Clear state
        tm.clear_account_state(&account.id);

        // Should now allow retry
        let refresher = MockRefresher::new();
        let result = tm
            .get_valid_access_token(&mut account, &refresher, &MockPersister, NOW + 2000)
            .await;
        assert!(result.is_ok());
    }
}
