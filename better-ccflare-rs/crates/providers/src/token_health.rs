//! Token health monitoring — estimates refresh token expiration and reports.
//!
//! Classifies each account's refresh token into one of five health states
//! based on age relative to the estimated 90-day maximum lifetime.

use serde::{Deserialize, Serialize};

use bccf_core::types::Account;

use crate::token_manager::is_api_key_provider;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Conservative upper bound for refresh token lifetime (90 days).
pub const REFRESH_TOKEN_MAX_AGE_MS: i64 = 90 * 24 * 60 * 60 * 1000;

/// Warning threshold: 7 days until estimated expiration.
const WARNING_THRESHOLD_DAYS: i64 = 7;

/// Critical threshold: 3 days until estimated expiration.
const CRITICAL_THRESHOLD_DAYS: i64 = 3;

/// Age (in days) at which a refresh token triggers a warning (60 days).
const AGE_WARNING_DAYS: i64 = 60;

const MS_PER_DAY: i64 = 24 * 60 * 60 * 1000;

// ---------------------------------------------------------------------------
// Health status
// ---------------------------------------------------------------------------

/// Health state of a refresh token.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum HealthStatus {
    /// Token age < 60 days AND > 7 days until estimated expiration.
    Healthy,
    /// Token age > 60 days OR 3-7 days until expiration.
    Warning,
    /// 0-3 days until estimated expiration.
    Critical,
    /// Estimated expiration date has passed.
    Expired,
    /// Account has no refresh token (API key accounts or missing token).
    NoRefreshToken,
}

// ---------------------------------------------------------------------------
// Per-account health status
// ---------------------------------------------------------------------------

/// Detailed health status for a single account's token.
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

// ---------------------------------------------------------------------------
// Health report
// ---------------------------------------------------------------------------

/// Aggregated health report across all accounts.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TokenHealthReport {
    pub accounts: Vec<TokenHealthStatus>,
    pub summary: HealthSummary,
    pub timestamp: i64,
}

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
// Check a single account
// ---------------------------------------------------------------------------

/// Assess the health of a single account's refresh token.
pub fn check_token_health(account: &Account, now: i64) -> TokenHealthStatus {
    let has_refresh_token = !account.refresh_token.is_empty();

    // API key accounts: no refresh token needed
    if !has_refresh_token {
        let (message, requires_reauth) = if account.api_key.is_some() {
            (
                "API key account (no refresh token needed)".to_string(),
                false,
            )
        } else {
            (
                format!(
                    "OAuth account {} missing refresh token — re-authentication required",
                    account.name
                ),
                true,
            )
        };

        return TokenHealthStatus {
            account_id: account.id.clone(),
            account_name: account.name.clone(),
            provider: account.provider.clone(),
            has_refresh_token: false,
            refresh_token_age_days: None,
            status: HealthStatus::NoRefreshToken,
            message,
            days_until_expiration: None,
            requires_reauth,
        };
    }

    // API key providers with a refresh token — still healthy
    if is_api_key_provider(&account.provider) {
        return TokenHealthStatus {
            account_id: account.id.clone(),
            account_name: account.name.clone(),
            provider: account.provider.clone(),
            has_refresh_token: true,
            refresh_token_age_days: None,
            status: HealthStatus::Healthy,
            message: "API key provider (refresh token not used)".to_string(),
            days_until_expiration: None,
            requires_reauth: false,
        };
    }

    // Unknown created_at → can't calculate age
    if account.created_at == 0 {
        return TokenHealthStatus {
            account_id: account.id.clone(),
            account_name: account.name.clone(),
            provider: account.provider.clone(),
            has_refresh_token: true,
            refresh_token_age_days: None,
            status: HealthStatus::Warning,
            message: "Unknown account creation date — re-authentication recommended".to_string(),
            days_until_expiration: None,
            requires_reauth: true,
        };
    }

    // Calculate age and estimated expiration
    let age_ms = now - account.created_at;
    let age_days = age_ms / MS_PER_DAY;
    let estimated_expiration = account.created_at + REFRESH_TOKEN_MAX_AGE_MS;
    let days_until = ceil_div(estimated_expiration - now, MS_PER_DAY);

    let (status, message, requires_reauth) = if days_until <= 0 {
        (
            HealthStatus::Expired,
            format!(
                "Refresh token expired ~{} days ago — re-authentication required",
                days_until.unsigned_abs()
            ),
            true,
        )
    } else if days_until <= CRITICAL_THRESHOLD_DAYS {
        (
            HealthStatus::Critical,
            format!(
                "Refresh token expires in {} days — immediate re-authentication required",
                days_until
            ),
            true,
        )
    } else if days_until <= WARNING_THRESHOLD_DAYS || age_days > AGE_WARNING_DAYS {
        let msg = if age_days > AGE_WARNING_DAYS {
            format!(
                "Refresh token is {} days old — re-authentication recommended",
                age_days
            )
        } else {
            format!(
                "Refresh token expires in {} days — re-authentication recommended",
                days_until
            )
        };
        (HealthStatus::Warning, msg, false)
    } else {
        (
            HealthStatus::Healthy,
            format!("Refresh token is healthy (expires in ~{} days)", days_until),
            false,
        )
    };

    TokenHealthStatus {
        account_id: account.id.clone(),
        account_name: account.name.clone(),
        provider: account.provider.clone(),
        has_refresh_token: true,
        refresh_token_age_days: Some(age_days),
        status,
        message,
        days_until_expiration: Some(days_until),
        requires_reauth,
    }
}

// ---------------------------------------------------------------------------
// Generate full report
// ---------------------------------------------------------------------------

/// Generate a health report for all accounts.
pub fn generate_report(accounts: &[Account], now: i64) -> TokenHealthReport {
    let statuses: Vec<TokenHealthStatus> = accounts
        .iter()
        .map(|a| check_token_health(a, now))
        .collect();

    let mut summary = HealthSummary {
        total: statuses.len(),
        ..Default::default()
    };

    for s in &statuses {
        match s.status {
            HealthStatus::Healthy => summary.healthy += 1,
            HealthStatus::Warning => summary.warning += 1,
            HealthStatus::Critical => summary.critical += 1,
            HealthStatus::Expired => summary.expired += 1,
            HealthStatus::NoRefreshToken => summary.no_refresh_token += 1,
        }
        if s.requires_reauth {
            summary.requires_reauth += 1;
        }
    }

    TokenHealthReport {
        accounts: statuses,
        summary,
        timestamp: now,
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Ceiling integer division (rounds toward positive infinity).
fn ceil_div(a: i64, b: i64) -> i64 {
    if b == 0 {
        return 0;
    }
    if a >= 0 {
        (a + b - 1) / b
    } else {
        a / b
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_700_000_000_000;

    fn make_oauth_account(name: &str, created_days_ago: i64) -> Account {
        let mut account = crate::test_util::test_account_with_key("sk-test");
        account.id = name.to_string();
        account.name = name.to_string();
        account.provider = "anthropic".to_string();
        account.refresh_token = "rt-test".to_string();
        account.created_at = NOW - (created_days_ago * MS_PER_DAY);
        account
    }

    fn make_api_key_account(name: &str) -> Account {
        let mut account = crate::test_util::test_account_with_key("sk-test");
        account.id = name.to_string();
        account.name = name.to_string();
        account.provider = "zai".to_string();
        account.refresh_token = String::new();
        account
    }

    #[test]
    fn healthy_token() {
        let account = make_oauth_account("healthy", 30); // 30 days old
        let status = check_token_health(&account, NOW);
        assert_eq!(status.status, HealthStatus::Healthy);
        assert!(!status.requires_reauth);
        assert!(status.days_until_expiration.unwrap() > 7);
    }

    #[test]
    fn warning_by_age() {
        let account = make_oauth_account("old", 65); // 65 days old (>60)
        let status = check_token_health(&account, NOW);
        assert_eq!(status.status, HealthStatus::Warning);
        assert!(!status.requires_reauth);
    }

    #[test]
    fn warning_by_remaining() {
        let account = make_oauth_account("aging", 85); // 85 days old → 5 days left
        let status = check_token_health(&account, NOW);
        // 5 days remaining is between critical (3) and warning (7)
        assert_eq!(status.status, HealthStatus::Warning);
    }

    #[test]
    fn critical_token() {
        let account = make_oauth_account("critical", 88); // 88 days old → 2 days left
        let status = check_token_health(&account, NOW);
        assert_eq!(status.status, HealthStatus::Critical);
        assert!(status.requires_reauth);
    }

    #[test]
    fn expired_token() {
        let account = make_oauth_account("expired", 95); // 95 days old → -5 days
        let status = check_token_health(&account, NOW);
        assert_eq!(status.status, HealthStatus::Expired);
        assert!(status.requires_reauth);
    }

    #[test]
    fn no_refresh_token_api_key() {
        let account = make_api_key_account("api-key");
        let status = check_token_health(&account, NOW);
        assert_eq!(status.status, HealthStatus::NoRefreshToken);
        assert!(!status.requires_reauth); // API key accounts don't need reauth
    }

    #[test]
    fn no_refresh_token_oauth() {
        let mut account = make_oauth_account("missing-rt", 30);
        account.refresh_token = String::new();
        account.api_key = None;
        let status = check_token_health(&account, NOW);
        assert_eq!(status.status, HealthStatus::NoRefreshToken);
        assert!(status.requires_reauth);
    }

    #[test]
    fn unknown_created_at() {
        let mut account = make_oauth_account("unknown", 0);
        account.created_at = 0;
        let status = check_token_health(&account, NOW);
        assert_eq!(status.status, HealthStatus::Warning);
        assert!(status.requires_reauth);
    }

    #[test]
    fn health_report_summary() {
        let accounts = vec![
            make_oauth_account("healthy1", 30),
            make_oauth_account("healthy2", 10),
            make_oauth_account("warning1", 65),
            make_oauth_account("critical1", 88),
            make_oauth_account("expired1", 95),
            make_api_key_account("api-key1"),
        ];

        let report = generate_report(&accounts, NOW);
        assert_eq!(report.summary.total, 6);
        assert_eq!(report.summary.healthy, 2);
        assert_eq!(report.summary.warning, 1);
        assert_eq!(report.summary.critical, 1);
        assert_eq!(report.summary.expired, 1);
        assert_eq!(report.summary.no_refresh_token, 1);
        assert_eq!(report.summary.requires_reauth, 2); // critical + expired
    }

    #[test]
    fn ceil_div_works() {
        assert_eq!(ceil_div(10, 3), 4); // 10/3 = 3.33 → 4
        assert_eq!(ceil_div(9, 3), 3); // exact
        assert_eq!(ceil_div(-5, 3), -1); // negative
        assert_eq!(ceil_div(0, 3), 0);
        assert_eq!(ceil_div(5, 0), 0); // division by zero guard
    }
}
