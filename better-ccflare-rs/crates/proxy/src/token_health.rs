//! Token health monitoring — tracks refresh token age and expiration estimates.
//!
//! Provides health status for each account's refresh token based on age relative
//! to a 90-day maximum lifespan. Runs periodic checks to warn about expiring tokens.

use std::sync::Arc;

use bccf_core::types::Account;
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use tracing::{info, warn};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Conservative maximum refresh token lifespan (90 days in ms).
const REFRESH_TOKEN_MAX_AGE_MS: i64 = 90 * 24 * 60 * 60 * 1000;

/// Token safety window for proactive refresh (30 minutes in ms).
pub const TOKEN_SAFETY_WINDOW_MS: i64 = 30 * 60 * 1000;

/// Backoff period after a failed token refresh (60 seconds in ms).
pub const TOKEN_REFRESH_BACKOFF_MS: i64 = 60_000;

/// Health check interval (6 hours in ms).
pub const HEALTH_CHECK_INTERVAL_MS: u64 = 6 * 60 * 60 * 1000;

/// Failure record TTL (5 minutes).
pub const FAILURE_TTL_MS: i64 = 5 * 60 * 1000;

/// Max backoff retries before checking DB.
pub const MAX_BACKOFF_RETRIES: u32 = 10;

// ---------------------------------------------------------------------------
// Health status types
// ---------------------------------------------------------------------------

/// Health status for a single account's refresh token.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum HealthStatus {
    Healthy,
    Warning,
    Critical,
    Expired,
    NoRefreshToken,
}

/// Detailed health status for one account.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TokenHealthStatus {
    pub account_id: String,
    pub account_name: String,
    pub provider: String,
    pub has_refresh_token: bool,
    pub refresh_token_age_days: Option<i64>,
    pub status: HealthStatus,
    pub message: String,
    pub days_until_expiration: Option<i64>,
    pub requires_reauth: bool,
}

/// Aggregated health report across all accounts.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenHealthReport {
    pub accounts: Vec<TokenHealthStatus>,
    pub summary: HealthSummary,
    pub timestamp: i64,
}

/// Summary counts for the health report.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HealthSummary {
    pub total: usize,
    pub healthy: usize,
    pub warning: usize,
    pub critical: usize,
    pub expired: usize,
    pub no_refresh_token: usize,
    pub requires_reauth: usize,
}

// ---------------------------------------------------------------------------
// Per-account health check
// ---------------------------------------------------------------------------

/// Check the health of a single account's refresh token.
pub fn check_refresh_token_health(account: &Account, now: i64) -> TokenHealthStatus {
    let base = TokenHealthStatus {
        account_id: account.id.clone(),
        account_name: account.name.clone(),
        provider: account.provider.clone(),
        has_refresh_token: false,
        refresh_token_age_days: None,
        status: HealthStatus::NoRefreshToken,
        message: String::new(),
        days_until_expiration: None,
        requires_reauth: false,
    };

    // No refresh token
    if account.refresh_token.is_empty() {
        return TokenHealthStatus {
            message: if account.api_key.is_some() {
                "API key account (no refresh token needed)".to_string()
            } else {
                "OAuth account missing refresh token - requires re-authentication".to_string()
            },
            requires_reauth: account.api_key.is_none(),
            ..base
        };
    }

    let has_refresh_token = true;

    // No creation date — can't estimate age
    if account.created_at == 0 {
        return TokenHealthStatus {
            has_refresh_token,
            status: HealthStatus::Warning,
            message: "Refresh token has unknown creation date - recommend re-authentication"
                .to_string(),
            requires_reauth: true,
            ..base
        };
    }

    let age_ms = now - account.created_at;
    let age_days = age_ms / (24 * 60 * 60 * 1000);
    let estimated_expiration = account.created_at + REFRESH_TOKEN_MAX_AGE_MS;
    let days_until_expiration =
        ((estimated_expiration - now) as f64 / (24.0 * 60.0 * 60.0 * 1000.0)).ceil() as i64;

    let (status, message, requires_reauth) = if days_until_expiration <= 0 {
        (
            HealthStatus::Expired,
            format!(
                "Refresh token expired ~{} days ago - requires immediate re-authentication",
                days_until_expiration.unsigned_abs()
            ),
            true,
        )
    } else if days_until_expiration <= 3 {
        (
            HealthStatus::Critical,
            format!(
                "Refresh token expires in {} days - immediate re-authentication required",
                days_until_expiration
            ),
            true,
        )
    } else if days_until_expiration <= 7 {
        (
            HealthStatus::Warning,
            format!(
                "Refresh token expires in {} days - re-authentication recommended soon",
                days_until_expiration
            ),
            false,
        )
    } else if age_days > 60 {
        (
            HealthStatus::Warning,
            format!(
                "Refresh token is {} days old - monitor for expiration",
                age_days
            ),
            false,
        )
    } else {
        (
            HealthStatus::Healthy,
            format!(
                "Refresh token is healthy (expires in ~{} days)",
                days_until_expiration
            ),
            false,
        )
    };

    TokenHealthStatus {
        has_refresh_token,
        refresh_token_age_days: Some(age_days),
        status,
        message,
        days_until_expiration: Some(days_until_expiration),
        requires_reauth,
        ..base
    }
}

/// Check if a refresh token is likely expired based on age.
pub fn is_refresh_token_likely_expired(account: &Account, now: i64) -> bool {
    if account.refresh_token.is_empty() || account.created_at == 0 {
        return true;
    }
    now - account.created_at > REFRESH_TOKEN_MAX_AGE_MS
}

/// Get an enhanced error message for OAuth token failures.
pub fn get_oauth_error_message(account: &Account, original_error: &str, now: i64) -> String {
    let health = check_refresh_token_health(account, now);

    match health.status {
        HealthStatus::Expired | HealthStatus::Critical => {
            format!(
                "OAuth tokens have expired for account '{}'. Please re-authenticate.",
                account.name
            )
        }
        HealthStatus::NoRefreshToken if health.requires_reauth => {
            format!(
                "OAuth account '{}' missing refresh token. Please re-authenticate.",
                account.name
            )
        }
        HealthStatus::Warning => {
            format!(
                "OAuth tokens for account '{}' are nearing expiration. Consider re-authenticating soon. Original error: {}",
                account.name, original_error
            )
        }
        _ => {
            format!(
                "OAuth token refresh failed for account '{}': {}",
                account.name, original_error
            )
        }
    }
}

// ---------------------------------------------------------------------------
// Batch health check
// ---------------------------------------------------------------------------

/// Check health of all accounts and produce an aggregated report.
pub fn check_all_accounts_health(accounts: &[Account], now: i64) -> TokenHealthReport {
    let statuses: Vec<TokenHealthStatus> = accounts
        .iter()
        .map(|a| check_refresh_token_health(a, now))
        .collect();

    let summary = HealthSummary {
        total: statuses.len(),
        healthy: statuses
            .iter()
            .filter(|s| s.status == HealthStatus::Healthy)
            .count(),
        warning: statuses
            .iter()
            .filter(|s| s.status == HealthStatus::Warning)
            .count(),
        critical: statuses
            .iter()
            .filter(|s| s.status == HealthStatus::Critical)
            .count(),
        expired: statuses
            .iter()
            .filter(|s| s.status == HealthStatus::Expired)
            .count(),
        no_refresh_token: statuses
            .iter()
            .filter(|s| s.status == HealthStatus::NoRefreshToken)
            .count(),
        requires_reauth: statuses.iter().filter(|s| s.requires_reauth).count(),
    };

    // Log problematic accounts
    let critical_or_expired: Vec<&TokenHealthStatus> = statuses
        .iter()
        .filter(|s| s.status == HealthStatus::Critical || s.status == HealthStatus::Expired)
        .collect();
    if !critical_or_expired.is_empty() {
        warn!("Critical token health issues detected:");
        for s in &critical_or_expired {
            warn!("  - {}: {}", s.account_name, s.message);
        }
    }

    let warnings: Vec<&TokenHealthStatus> = statuses
        .iter()
        .filter(|s| s.status == HealthStatus::Warning)
        .collect();
    if !warnings.is_empty() {
        info!("Token health warnings:");
        for s in &warnings {
            info!("  - {}: {}", s.account_name, s.message);
        }
    }

    if summary.healthy > 0 {
        info!("{} accounts have healthy tokens", summary.healthy);
    }

    TokenHealthReport {
        accounts: statuses,
        summary,
        timestamp: now,
    }
}

// ---------------------------------------------------------------------------
// Health service (periodic background checker)
// ---------------------------------------------------------------------------

/// Background token health service that periodically checks all accounts.
pub struct TokenHealthService {
    last_report: Arc<Mutex<Option<TokenHealthReport>>>,
    shutdown: tokio::sync::watch::Sender<bool>,
}

impl TokenHealthService {
    /// Start the health service with a callback to fetch current accounts.
    pub fn start<F>(get_accounts: F, interval_ms: u64) -> Self
    where
        F: Fn() -> Vec<Account> + Send + Sync + 'static,
    {
        let last_report = Arc::new(Mutex::new(None));
        let report_handle = Arc::clone(&last_report);
        let (shutdown_tx, mut shutdown_rx) = tokio::sync::watch::channel(false);

        tokio::spawn(async move {
            // Perform initial check
            let accounts = get_accounts();
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as i64;
            let report = check_all_accounts_health(&accounts, now);
            *report_handle.lock().await = Some(report);

            let mut interval =
                tokio::time::interval(tokio::time::Duration::from_millis(interval_ms));
            interval.tick().await; // Skip immediate tick (already ran above)

            loop {
                tokio::select! {
                    _ = interval.tick() => {
                        let accounts = get_accounts();
                        let now = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_millis() as i64;
                        let report = check_all_accounts_health(&accounts, now);
                        *report_handle.lock().await = Some(report);
                    }
                    _ = shutdown_rx.changed() => {
                        info!("Token health service shutting down");
                        break;
                    }
                }
            }
        });

        Self {
            last_report,
            shutdown: shutdown_tx,
        }
    }

    /// Get the most recent health report.
    pub async fn last_report(&self) -> Option<TokenHealthReport> {
        self.last_report.lock().await.clone()
    }

    /// Force an immediate health check.
    pub async fn force_check(&self, accounts: &[Account], now: i64) -> TokenHealthReport {
        let report = check_all_accounts_health(accounts, now);
        *self.last_report.lock().await = Some(report.clone());
        report
    }

    /// Stop the background health service.
    pub fn stop(&self) {
        let _ = self.shutdown.send(true);
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use bccf_core::constants::time;

    fn make_account(id: &str, provider: &str) -> Account {
        Account {
            id: id.to_string(),
            name: id.to_string(),
            provider: provider.to_string(),
            api_key: None,
            refresh_token: "rt_test".to_string(),
            access_token: Some("at_test".to_string()),
            expires_at: None,
            request_count: 0,
            total_requests: 0,
            last_used: None,
            created_at: 0,
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
        }
    }

    const NOW: i64 = 1_700_000_000_000;

    #[test]
    fn healthy_token() {
        let mut account = make_account("a1", "anthropic");
        // Created 30 days ago
        account.created_at = NOW - 30 * time::DAY;

        let status = check_refresh_token_health(&account, NOW);
        assert_eq!(status.status, HealthStatus::Healthy);
        assert!(!status.requires_reauth);
        assert!(status.days_until_expiration.unwrap() > 7);
    }

    #[test]
    fn warning_old_token() {
        let mut account = make_account("a2", "anthropic");
        // Created 70 days ago (>60 days old)
        account.created_at = NOW - 70 * time::DAY;

        let status = check_refresh_token_health(&account, NOW);
        assert_eq!(status.status, HealthStatus::Warning);
        assert!(!status.requires_reauth);
    }

    #[test]
    fn warning_expiring_soon() {
        let mut account = make_account("a3", "anthropic");
        // Created 85 days ago (5 days until 90-day expiration)
        account.created_at = NOW - 85 * time::DAY;

        let status = check_refresh_token_health(&account, NOW);
        assert_eq!(status.status, HealthStatus::Warning);
        assert!(status.days_until_expiration.unwrap() <= 7);
    }

    #[test]
    fn critical_token() {
        let mut account = make_account("a4", "anthropic");
        // Created 88 days ago (2 days until expiration)
        account.created_at = NOW - 88 * time::DAY;

        let status = check_refresh_token_health(&account, NOW);
        assert_eq!(status.status, HealthStatus::Critical);
        assert!(status.requires_reauth);
        assert!(status.days_until_expiration.unwrap() <= 3);
    }

    #[test]
    fn expired_token() {
        let mut account = make_account("a5", "anthropic");
        // Created 100 days ago (expired 10 days ago)
        account.created_at = NOW - 100 * time::DAY;

        let status = check_refresh_token_health(&account, NOW);
        assert_eq!(status.status, HealthStatus::Expired);
        assert!(status.requires_reauth);
        assert!(status.days_until_expiration.unwrap() <= 0);
    }

    #[test]
    fn no_refresh_token_api_key() {
        let mut account = make_account("a6", "zai");
        account.refresh_token = String::new();
        account.api_key = Some("sk-test".to_string());

        let status = check_refresh_token_health(&account, NOW);
        assert_eq!(status.status, HealthStatus::NoRefreshToken);
        assert!(!status.requires_reauth);
        assert!(status.message.contains("API key account"));
    }

    #[test]
    fn no_refresh_token_oauth_missing() {
        let mut account = make_account("a7", "anthropic");
        account.refresh_token = String::new();

        let status = check_refresh_token_health(&account, NOW);
        assert_eq!(status.status, HealthStatus::NoRefreshToken);
        assert!(status.requires_reauth);
    }

    #[test]
    fn unknown_creation_date() {
        let mut account = make_account("a8", "anthropic");
        account.created_at = 0;

        let status = check_refresh_token_health(&account, NOW);
        assert_eq!(status.status, HealthStatus::Warning);
        assert!(status.requires_reauth);
    }

    #[test]
    fn is_likely_expired_checks() {
        let mut account = make_account("a9", "anthropic");

        // No refresh token
        account.refresh_token = String::new();
        assert!(is_refresh_token_likely_expired(&account, NOW));

        // With refresh token, old
        account.refresh_token = "rt_test".to_string();
        account.created_at = NOW - 100 * time::DAY;
        assert!(is_refresh_token_likely_expired(&account, NOW));

        // With refresh token, new
        account.created_at = NOW - 30 * time::DAY;
        assert!(!is_refresh_token_likely_expired(&account, NOW));
    }

    #[test]
    fn batch_health_report() {
        let mut healthy = make_account("healthy", "anthropic");
        healthy.created_at = NOW - 30 * time::DAY;

        let mut expired = make_account("expired", "anthropic");
        expired.created_at = NOW - 100 * time::DAY;

        let mut api_key = make_account("apikey", "zai");
        api_key.refresh_token = String::new();
        api_key.api_key = Some("sk-test".to_string());

        let report = check_all_accounts_health(&[healthy, expired, api_key], NOW);
        assert_eq!(report.summary.total, 3);
        assert_eq!(report.summary.healthy, 1);
        assert_eq!(report.summary.expired, 1);
        assert_eq!(report.summary.no_refresh_token, 1);
        assert_eq!(report.summary.requires_reauth, 1); // Only expired requires reauth
    }

    #[test]
    fn enhanced_error_messages() {
        let mut expired_account = make_account("expired", "anthropic");
        expired_account.created_at = NOW - 100 * time::DAY;

        let msg = get_oauth_error_message(&expired_account, "connection refused", NOW);
        assert!(msg.contains("expired"));

        let mut healthy_account = make_account("healthy", "anthropic");
        healthy_account.created_at = NOW - 30 * time::DAY;

        let msg = get_oauth_error_message(&healthy_account, "connection refused", NOW);
        assert!(msg.contains("connection refused"));
    }
}
