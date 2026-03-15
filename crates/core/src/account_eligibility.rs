use std::collections::HashMap;

use crate::types::{Account, RoutingUsageInfo, WindowKind};

/// Parsed upstream/provider status.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpstreamStatus {
    Missing,
    Ok,
    AllowedWarning,
    AuthFailed,
    ReauthRequired,
    Unauthorized,
    Forbidden,
    RefreshTokenRevoked,
    Other(String),
}

impl UpstreamStatus {
    pub fn from_raw(status: Option<&str>) -> Self {
        let Some(raw) = status else {
            return Self::Missing;
        };
        let lowered = raw.trim().to_ascii_lowercase();
        if lowered.is_empty() {
            return Self::Missing;
        }
        if lowered == "ok" || lowered == "allowed" {
            return Self::Ok;
        }
        if lowered.contains("allowed_warning") {
            return Self::AllowedWarning;
        }
        if lowered.starts_with("auth failed") || lowered.starts_with("authentication failed") {
            return Self::AuthFailed;
        }
        if lowered.contains("re-authentication") || lowered.contains("reauth") {
            return Self::ReauthRequired;
        }
        if lowered.contains("unauthorized") {
            return Self::Unauthorized;
        }
        if lowered.contains("forbidden") {
            return Self::Forbidden;
        }
        if lowered.contains("refresh token revoked") {
            return Self::RefreshTokenRevoked;
        }
        Self::Other(raw.to_string())
    }

    pub fn is_auth_failure(&self) -> bool {
        matches!(
            self,
            Self::AuthFailed
                | Self::ReauthRequired
                | Self::Unauthorized
                | Self::Forbidden
                | Self::RefreshTokenRevoked
        )
    }
}

/// Normalize epoch timestamps to milliseconds.
pub fn normalize_epoch_millis(ts: i64) -> i64 {
    if ts.abs() < 1_000_000_000_000 {
        ts.saturating_mul(1000)
    } else {
        ts
    }
}

pub fn provider_supports_usage(provider: &str) -> bool {
    matches!(provider, "claude-oauth" | "anthropic" | "nanogpt" | "zai")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccountBlockReason {
    Paused,
    AuthFailed,
    ActiveRateLimit,
    OverageProtection,
    HardReserve,
}

/// Deterministic routing eligibility evaluation used by LB and dashboard.
#[derive(Debug, Clone)]
pub struct AccountEligibility {
    pub routable: bool,
    pub blocked_by: Vec<AccountBlockReason>,
    pub upstream_status: UpstreamStatus,
    pub overage_blocked: bool,
    pub hard_reserve_blocked: bool,
    pub soft_reserve_hit: bool,
    pub usage_missing: bool,
    pub usage_missing_overage_fail_open: bool,
    pub usage_missing_hard_reserve_fail_closed: bool,
    pub active_rate_limit: bool,
}

pub fn evaluate_account_eligibility(
    account: &Account,
    usage: &HashMap<String, RoutingUsageInfo>,
    now: i64,
) -> AccountEligibility {
    let usage_info = usage.get(&account.id);
    let usage_missing = provider_supports_usage(&account.provider) && usage_info.is_none();
    let has_reserves = account.reserve_5h > 0 || account.reserve_weekly > 0;
    let upstream_status = UpstreamStatus::from_raw(account.rate_limit_status.as_deref());
    let active_rate_limit = account
        .rate_limited_until
        .is_some_and(|until| normalize_epoch_millis(until) >= now);

    let overage_blocked = if account.overage_protection {
        usage_info
            .map(|info| is_at_overage(account, info))
            .unwrap_or(false)
    } else {
        false
    };

    let usage_missing_overage_fail_open = account.overage_protection && usage_info.is_none();
    let usage_missing_hard_reserve_fail_closed =
        account.reserve_hard && has_reserves && usage_info.is_none();
    let hard_reserve_blocked = usage_missing_hard_reserve_fail_closed
        || (account.reserve_hard && is_at_reserve(account, usage));
    let soft_reserve_hit = !account.reserve_hard && is_at_reserve(account, usage);

    let mut blocked_by = Vec::new();
    if account.paused {
        blocked_by.push(AccountBlockReason::Paused);
    }
    if upstream_status.is_auth_failure() {
        blocked_by.push(AccountBlockReason::AuthFailed);
    }
    if active_rate_limit {
        blocked_by.push(AccountBlockReason::ActiveRateLimit);
    }
    if overage_blocked {
        blocked_by.push(AccountBlockReason::OverageProtection);
    }
    if hard_reserve_blocked {
        blocked_by.push(AccountBlockReason::HardReserve);
    }

    AccountEligibility {
        routable: blocked_by.is_empty(),
        blocked_by,
        upstream_status,
        overage_blocked,
        hard_reserve_blocked,
        soft_reserve_hit,
        usage_missing,
        usage_missing_overage_fail_open,
        usage_missing_hard_reserve_fail_closed,
        active_rate_limit,
    }
}

/// Whether an account should be considered overage-exhausted.
///
/// General rule: any window at 100% blocks routing when overage protection is on.
///
/// Anthropic nuance: `extra_usage` is represented as `WindowKind::Other`.
/// That monthly budget should only block routing when the 5-hour window is
/// also exhausted.
pub fn is_at_overage(account: &Account, info: &RoutingUsageInfo) -> bool {
    let is_anthropic_family = matches!(account.provider.as_str(), "anthropic" | "claude-oauth");

    // Billing-safety mode for Anthropic-family accounts:
    // if per-window telemetry is absent, treat as exhausted to avoid extra charges.
    if info.windows.is_empty() {
        if is_anthropic_family {
            return true;
        }
        return !info.utilization_pct.is_finite() || info.utilization_pct >= 100.0;
    }

    let five_hour_windows: Vec<_> = info
        .windows
        .iter()
        .filter(|w| matches!(w.kind, WindowKind::FiveHour))
        .collect();
    let five_hour_known = !five_hour_windows.is_empty();
    let five_hour_exhausted = five_hour_windows
        .iter()
        .any(|w| !w.utilization_pct.is_finite() || w.utilization_pct >= 100.0);

    info.windows.iter().any(|w| {
        let saturated = !w.utilization_pct.is_finite() || w.utilization_pct >= 100.0;
        if !saturated {
            return false;
        }
        if is_anthropic_family && matches!(w.kind, WindowKind::Other) {
            if !five_hour_known {
                return true;
            }
            return five_hour_exhausted;
        }
        true
    })
}

/// Whether an account is at or over its reserve threshold.
pub fn is_at_reserve(account: &Account, usage: &HashMap<String, RoutingUsageInfo>) -> bool {
    let is_anthropic_family = matches!(account.provider.as_str(), "anthropic" | "claude-oauth");

    if account.reserve_5h == 0 && account.reserve_weekly == 0 {
        return false;
    }
    let Some(info) = usage.get(&account.id) else {
        return false; // no data = assume under reserve
    };

    let five_hour_at_100 = info
        .windows
        .iter()
        .filter(|w| matches!(w.kind, WindowKind::FiveHour))
        .any(|w| !w.utilization_pct.is_finite() || w.utilization_pct >= 100.0);

    if !info.windows.is_empty() {
        for w in &info.windows {
            let threshold = match w.kind {
                WindowKind::FiveHour => account.reserve_5h,
                WindowKind::Weekly => account.reserve_weekly,
                WindowKind::Other => {
                    if is_anthropic_family && !five_hour_at_100 {
                        continue;
                    }
                    account.reserve_5h
                }
            };
            let trigger_at = (100_i64.saturating_sub(threshold)).max(0) as f64;
            if threshold > 0 && (!w.utilization_pct.is_finite() || w.utilization_pct >= trigger_at)
            {
                return true;
            }
        }
        return false;
    }

    let threshold = account.reserve_5h.max(account.reserve_weekly);
    threshold > 0
        && (!info.utilization_pct.is_finite() || info.utilization_pct >= (100 - threshold) as f64)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_account(id: &str, provider: &str) -> Account {
        Account {
            id: id.to_string(),
            name: id.to_string(),
            provider: provider.to_string(),
            api_key: None,
            refresh_token: String::new(),
            access_token: Some("tok".to_string()),
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
            rate_limit_status: Some("OK".to_string()),
            rate_limit_remaining: None,
            priority: 0,
            auto_fallback_enabled: true,
            auto_refresh_enabled: true,
            custom_endpoint: None,
            model_mappings: None,
            reserve_5h: 0,
            reserve_weekly: 0,
            reserve_hard: false,
            subscription_tier: None,
            email: None,
            refresh_token_updated_at: None,
            is_shared: false,
            overage_protection: true,
        }
    }

    #[test]
    fn upstream_status_parsing() {
        assert_eq!(
            UpstreamStatus::from_raw(Some("allowed")),
            UpstreamStatus::Ok
        );
        assert_eq!(
            UpstreamStatus::from_raw(Some("Auth failed (401)")),
            UpstreamStatus::AuthFailed
        );
        assert!(UpstreamStatus::from_raw(Some("refresh token revoked")).is_auth_failure());
    }

    #[test]
    fn normalize_seconds_to_millis() {
        assert_eq!(normalize_epoch_millis(1_773_417_600), 1_773_417_600_000);
        assert_eq!(normalize_epoch_millis(1_773_417_600_123), 1_773_417_600_123);
    }

    #[test]
    fn eligibility_flags_auth_failed() {
        let mut a = make_account("a", "claude-oauth");
        a.rate_limit_status = Some("Auth failed (401)".to_string());
        let usage = HashMap::new();
        let e = evaluate_account_eligibility(&a, &usage, 1_000);
        assert!(!e.routable);
        assert!(e.blocked_by.contains(&AccountBlockReason::AuthFailed));
    }
}
