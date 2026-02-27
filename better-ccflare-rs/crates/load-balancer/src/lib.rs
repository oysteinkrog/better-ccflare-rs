//! Load Balancer crate — Request distribution across account providers.
//!
//! Implements priority-based routing with session affinity for OAuth accounts,
//! round-robin for pay-as-you-go accounts within the same priority tier,
//! and auto-fallback when higher-priority accounts recover from rate limits.

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};

use bccf_core::constants::time;
use bccf_core::providers::Provider;
use bccf_core::types::{Account, RoutingUsageInfo};

/// Debounce for rate-limit-reset detection: a window must have reset at least
/// this many ms ago before we act on the reset timestamp.
const RESET_DEBOUNCE_MS: i64 = 1000;

/// Metadata about an incoming request, used by the strategy to make routing decisions.
#[derive(Debug, Clone, Default)]
pub struct SelectionMeta {
    /// If set, force routing to this specific account id.
    pub force_account_id: Option<String>,
    /// If true, skip session tracking (used for auto-refresh messages).
    pub bypass_session: bool,
}

/// Result of a session reset, returned so the caller can persist changes.
#[derive(Debug, Clone)]
pub struct SessionReset {
    pub account_id: String,
    pub new_session_start: i64,
}

/// The session-based load balancing strategy.
///
/// Routes requests to accounts by priority with session affinity for OAuth
/// (Anthropic) accounts and round-robin within the same priority tier for
/// pay-as-you-go accounts.
pub struct SessionStrategy {
    session_duration_ms: i64,
    round_robin_counter: AtomicUsize,
}

impl SessionStrategy {
    pub fn new(session_duration_ms: i64) -> Self {
        Self {
            session_duration_ms,
            round_robin_counter: AtomicUsize::new(0),
        }
    }

    /// Select accounts for a request, returning them in priority order.
    ///
    /// The first account in the returned vec is the preferred account.
    /// Remaining accounts are fallbacks sorted by priority.
    /// Any `SessionReset` values should be persisted by the caller.
    ///
    /// `usage` provides per-account utilization data from the usage polling
    /// cache. Accounts with `reserve_5h/reserve_weekly > 0` that have exceeded their
    /// reserve threshold are deprioritised (soft) or excluded (hard).
    /// Within a priority tier, accounts with soonest reset are preferred so
    /// their remaining capacity isn't wasted.
    pub fn select(
        &self,
        accounts: &[Account],
        usage: &HashMap<String, RoutingUsageInfo>,
        meta: &SelectionMeta,
        now: i64,
    ) -> (Vec<Account>, Vec<SessionReset>) {
        let mut resets = Vec::new();

        // Force-account: if header specifies an account, use only that one
        if let Some(ref forced_id) = meta.force_account_id {
            if let Some(account) = accounts.iter().find(|a| &a.id == forced_id) {
                return (vec![account.clone()], resets);
            }
            // Account not found — fall through to normal selection
        }

        // Check for auto-fallback candidates first
        let fallback_candidates = self.check_auto_fallback_accounts(accounts, usage, now);
        if !fallback_candidates.is_empty() {
            let mut chosen = fallback_candidates[0].clone();
            if !meta.bypass_session {
                if let Some(reset) = self.maybe_reset_session(&mut chosen, now) {
                    resets.push(reset);
                }
            }

            let mut others: Vec<Account> = accounts
                .iter()
                .filter(|a| a.id != chosen.id && is_account_available(a, usage, now))
                .cloned()
                .collect();
            sort_accounts_usage_aware(&mut others, usage);

            let mut result = vec![chosen];
            result.extend(others);
            return (result, resets);
        }

        // Find account with the most recent active session (Anthropic only).
        // If the active-session account is at its hard reserve, skip it.
        let active_account = accounts
            .iter()
            .filter(|a| self.has_active_session(a, now))
            .max_by_key(|a| a.session_start.unwrap_or(0));

        if let Some(active) = active_account {
            if is_account_available(active, usage, now) {
                let mut chosen = active.clone();
                if !meta.bypass_session {
                    if let Some(reset) = self.maybe_reset_session(&mut chosen, now) {
                        resets.push(reset);
                    }
                }

                let mut others: Vec<Account> = accounts
                    .iter()
                    .filter(|a| a.id != chosen.id && is_account_available(a, usage, now))
                    .cloned()
                    .collect();
                sort_accounts_usage_aware(&mut others, usage);

                let mut result = vec![chosen];
                result.extend(others);
                return (result, resets);
            }
        }

        // No active session — select from available accounts by priority,
        // with usage-aware ordering within each tier.
        let mut available: Vec<Account> = accounts
            .iter()
            .filter(|a| is_account_available(a, usage, now))
            .cloned()
            .collect();
        sort_accounts_usage_aware(&mut available, usage);

        if available.is_empty() {
            return (vec![], resets);
        }

        // For pay-as-you-go accounts at the same priority tier, use round-robin
        let top_priority = available[0].priority;
        let same_tier: Vec<&Account> = available
            .iter()
            .filter(|a| a.priority == top_priority)
            .collect();

        let chosen_idx = if same_tier.len() > 1 {
            // Only apply round-robin for non-session-tracking (pay-as-you-go) accounts
            let all_payg = same_tier
                .iter()
                .all(|a| !requires_session_tracking(&a.provider));
            if all_payg {
                let counter = self.round_robin_counter.fetch_add(1, Ordering::Relaxed);
                counter % same_tier.len()
            } else {
                0
            }
        } else {
            0
        };

        let mut chosen = same_tier[chosen_idx].clone();
        if !meta.bypass_session {
            if let Some(reset) = self.maybe_reset_session(&mut chosen, now) {
                resets.push(reset);
            }
        }

        let others: Vec<Account> = available
            .iter()
            .filter(|a| a.id != chosen.id)
            .cloned()
            .collect();

        let mut result = vec![chosen];
        result.extend(others);
        (result, resets)
    }

    /// Check if a session has expired and needs resetting.
    /// Returns a `SessionReset` if the session was reset, so the caller can persist it.
    fn maybe_reset_session(&self, account: &mut Account, now: i64) -> Option<SessionReset> {
        let provider_requires_session = requires_session_tracking(&account.provider);

        // Check if fixed duration expired (only for session-tracking providers)
        let fixed_duration_expired = provider_requires_session
            && match account.session_start {
                Some(start) => now - start >= self.session_duration_ms,
                None => true, // No session started yet
            };

        // Check if the rate limit window has reset (Anthropic usage windows)
        let rate_limit_window_reset = account.provider == Provider::Anthropic.to_string()
            && account
                .rate_limit_reset
                .is_some_and(|reset| reset < now - RESET_DEBOUNCE_MS);

        if fixed_duration_expired || rate_limit_window_reset {
            account.session_start = Some(now);
            account.session_request_count = 0;
            // Clear rate_limit_reset so it doesn't fire again on the next request.
            // Without this, any past timestamp causes `rate_limit_window_reset` to be
            // permanently true, triggering a session reset on every request.
            if rate_limit_window_reset {
                account.rate_limit_reset = None;
            }
            return Some(SessionReset {
                account_id: account.id.clone(),
                new_session_start: now,
            });
        }

        None
    }

    /// Whether an account has an active session within the duration window.
    /// Only true for providers that require session tracking (Anthropic).
    fn has_active_session(&self, account: &Account, now: i64) -> bool {
        if !requires_session_tracking(&account.provider) {
            return false;
        }
        match account.session_start {
            // Guard start <= now to handle clock skew / future timestamps:
            // a future session_start would make now-start negative (i64), which
            // is always < session_duration_ms, falsely claiming an active session.
            Some(start) => start <= now && now - start < self.session_duration_ms,
            None => false,
        }
    }

    /// Find higher-priority accounts eligible for auto-fallback.
    /// These are accounts whose rate limit window has reset and are no longer rate-limited.
    fn check_auto_fallback_accounts(
        &self,
        accounts: &[Account],
        usage: &HashMap<String, RoutingUsageInfo>,
        now: i64,
    ) -> Vec<Account> {
        let mut candidates: Vec<Account> = accounts
            .iter()
            .filter(|a| {
                if !a.auto_fallback_enabled {
                    return false;
                }

                // Check if the usage window has reset (works for all providers)
                let window_reset = a.rate_limit_reset.is_some_and(|reset| reset < now - RESET_DEBOUNCE_MS);

                // Must pass full availability check (paused, rate-limited, hard reserve)
                window_reset && is_account_available(a, usage, now)
            })
            .cloned()
            .collect();

        candidates.sort_by_key(|a| a.priority);
        candidates
    }
}

impl Default for SessionStrategy {
    fn default() -> Self {
        Self::new(time::ANTHROPIC_SESSION_DURATION_DEFAULT)
    }
}

/// Check if an account is available for routing.
///
/// An account is unavailable if it's paused, rate-limited, or has hit its
/// hard reserve threshold (when `reserve_hard` is true and any usage window
/// exceeds its per-window reserve threshold).
pub fn is_account_available(
    account: &Account,
    usage: &HashMap<String, RoutingUsageInfo>,
    now: i64,
) -> bool {
    if account.paused {
        return false;
    }
    if account.rate_limited_until.is_some_and(|until| until >= now) {
        return false;
    }
    // Hard reserve: exclude account when any window hits its reserve threshold
    if account.reserve_hard {
        if is_at_reserve(account, usage) {
            return false;
        }
    }
    true
}

/// Sort accounts for routing: primary key is priority, secondary key is
/// usage-aware ordering within each priority tier.
///
/// Within a tier, accounts under their reserve threshold come before those
/// at/over reserve. Within each partition, accounts whose usage window resets
/// soonest come first (use them before capacity expires).
fn sort_accounts_usage_aware(accounts: &mut [Account], usage: &HashMap<String, RoutingUsageInfo>) {
    accounts.sort_by(|a, b| {
        // 1. Primary: priority (lower = higher priority)
        let pri = a.priority.cmp(&b.priority);
        if pri != std::cmp::Ordering::Equal {
            return pri;
        }

        // 2. Within same priority: under-reserve before at/over-reserve
        let a_at_reserve = is_at_reserve(a, usage);
        let b_at_reserve = is_at_reserve(b, usage);
        match (a_at_reserve, b_at_reserve) {
            (false, true) => return std::cmp::Ordering::Less,
            (true, false) => return std::cmp::Ordering::Greater,
            _ => {}
        }

        // 3. Soonest reset first (prefer accounts whose window expires soon)
        let a_reset = usage.get(&a.id).and_then(|u| u.resets_at_ms);
        let b_reset = usage.get(&b.id).and_then(|u| u.resets_at_ms);
        match (a_reset, b_reset) {
            (Some(ar), Some(br)) => ar.cmp(&br),
            (Some(_), None) => std::cmp::Ordering::Less, // known reset before unknown
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => std::cmp::Ordering::Equal,
        }
    });
}

/// Whether an account is at or over its reserve threshold.
///
/// Checks per-window thresholds: `reserve_5h` for `FiveHour` windows,
/// `reserve_weekly` for `Weekly` windows, and `reserve_5h` as fallback for `Other`.
/// Returns `true` if any window exceeds its threshold.
/// Returns `false` if no reserve is configured or no usage data is available.
fn is_at_reserve(account: &Account, usage: &HashMap<String, RoutingUsageInfo>) -> bool {
    use bccf_core::types::WindowKind;

    if account.reserve_5h == 0 && account.reserve_weekly == 0 {
        return false;
    }
    let Some(info) = usage.get(&account.id) else {
        return false; // no data = assume under reserve
    };

    // Check per-window thresholds
    if !info.windows.is_empty() {
        for w in &info.windows {
            let threshold = match w.kind {
                WindowKind::FiveHour => account.reserve_5h,
                WindowKind::Weekly => account.reserve_weekly,
                WindowKind::Other => account.reserve_5h, // fallback to 5h for non-Anthropic
            };
            // Clamp to [0, 100]: reserve values >100 would produce negative target
            // utilization, incorrectly marking every account as at-reserve.
            let trigger_at = (100_i64.saturating_sub(threshold)).max(0) as f64;
            if threshold > 0 && w.utilization_pct >= trigger_at {
                return true;
            }
        }
        return false;
    }

    // Fallback for accounts with no per-window data: use aggregate
    let threshold = account.reserve_5h.max(account.reserve_weekly);
    let trigger_at = (100_i64.saturating_sub(threshold)).max(0) as f64;
    if threshold > 0 && info.utilization_pct >= trigger_at {
        return true;
    }
    false
}

/// Check if a provider string requires session duration tracking.
fn requires_session_tracking(provider: &str) -> bool {
    Provider::from_str_loose(provider).is_some_and(|p| p.requires_session_tracking())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_account(id: &str, provider: &str, priority: i64) -> Account {
        Account {
            id: id.to_string(),
            name: id.to_string(),
            provider: provider.to_string(),
            api_key: None,
            refresh_token: String::new(),
            access_token: None,
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
            priority,
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
            is_shared: false,
        }
    }

    fn default_meta() -> SelectionMeta {
        SelectionMeta::default()
    }

    fn no_usage() -> HashMap<String, RoutingUsageInfo> {
        HashMap::new()
    }

    const NOW: i64 = 1_700_000_000_000; // Fixed timestamp for tests

    #[test]
    fn basic_priority_ordering() {
        let strategy = SessionStrategy::default();
        let accounts = vec![
            make_account("low", "zai", 10),
            make_account("high", "zai", 1),
            make_account("mid", "zai", 5),
        ];

        let (result, _) = strategy.select(&accounts, &no_usage(), &default_meta(), NOW);
        assert_eq!(result.len(), 3);
        assert_eq!(result[0].id, "high");
        assert_eq!(result[1].id, "mid");
        assert_eq!(result[2].id, "low");
    }

    #[test]
    fn skip_paused_accounts() {
        let strategy = SessionStrategy::default();
        let mut paused = make_account("paused", "zai", 1);
        paused.paused = true;
        let available = make_account("available", "zai", 5);

        let (result, _) = strategy.select(&[paused, available], &no_usage(), &default_meta(), NOW);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].id, "available");
    }

    #[test]
    fn skip_rate_limited_accounts() {
        let strategy = SessionStrategy::default();
        let mut limited = make_account("limited", "zai", 1);
        limited.rate_limited_until = Some(NOW + 60_000); // Rate limited for 60 more seconds
        let available = make_account("available", "zai", 5);

        let (result, _) = strategy.select(&[limited, available], &no_usage(), &default_meta(), NOW);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].id, "available");
    }

    #[test]
    fn expired_rate_limit_is_available() {
        let strategy = SessionStrategy::default();
        let mut was_limited = make_account("was-limited", "zai", 1);
        was_limited.rate_limited_until = Some(NOW - 1000); // Rate limit expired
        let other = make_account("other", "zai", 5);

        let (result, _) = strategy.select(&[was_limited, other], &no_usage(), &default_meta(), NOW);
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].id, "was-limited"); // Higher priority
    }

    #[test]
    fn session_affinity_for_anthropic() {
        let strategy = SessionStrategy::default();
        let mut anthropic = make_account("oauth", "anthropic", 5);
        anthropic.session_start = Some(NOW - time::HOUR); // 1 hour into session (within 5hr window)
        anthropic.session_request_count = 10;

        let higher_prio = make_account("higher", "zai", 1); // Higher priority but no session

        let (result, _) = strategy.select(&[anthropic, higher_prio], &no_usage(), &default_meta(), NOW);
        // Anthropic with active session should come first, even though lower priority
        assert_eq!(result[0].id, "oauth");
        assert_eq!(result[1].id, "higher");
    }

    #[test]
    fn session_expired_anthropic_falls_back() {
        let strategy = SessionStrategy::default();
        let mut anthropic = make_account("oauth", "anthropic", 5);
        // Session started 6 hours ago — expired (>5hr window)
        anthropic.session_start = Some(NOW - 6 * time::HOUR);

        let higher_prio = make_account("higher", "zai", 1);

        let (result, _) = strategy.select(&[anthropic, higher_prio], &no_usage(), &default_meta(), NOW);
        // Expired session means no active session, falls back to priority ordering
        assert_eq!(result[0].id, "higher");
        assert_eq!(result[1].id, "oauth");
    }

    #[test]
    fn session_expired_anthropic_resets_when_chosen() {
        let strategy = SessionStrategy::default();
        let mut anthropic = make_account("oauth", "anthropic", 1);
        // Session started 6 hours ago — expired (>5hr window), but highest priority
        anthropic.session_start = Some(NOW - 6 * time::HOUR);

        let (result, resets) = strategy.select(&[anthropic], &no_usage(), &default_meta(), NOW);
        assert_eq!(result[0].id, "oauth");
        // Session should be reset since it's expired
        assert_eq!(resets.len(), 1);
        assert_eq!(resets[0].account_id, "oauth");
        assert_eq!(resets[0].new_session_start, NOW);
    }

    #[test]
    fn no_session_affinity_for_payg() {
        let strategy = SessionStrategy::default();
        let mut zai = make_account("zai1", "zai", 5);
        zai.session_start = Some(NOW - time::HOUR); // Has a "session" but shouldn't stick

        let higher = make_account("zai2", "zai", 1);

        let (result, _) = strategy.select(&[zai, higher], &no_usage(), &default_meta(), NOW);
        // Pay-as-you-go should NOT have session affinity — priority wins
        assert_eq!(result[0].id, "zai2");
    }

    #[test]
    fn auto_fallback_when_rate_limit_resets() {
        let strategy = SessionStrategy::default();
        let mut high_prio = make_account("high", "anthropic", 1);
        high_prio.auto_fallback_enabled = true;
        high_prio.rate_limit_reset = Some(NOW - 2000); // Usage window reset 2 seconds ago

        let mut low_prio = make_account("low", "anthropic", 5);
        low_prio.session_start = Some(NOW - time::HOUR); // Active session

        let (result, _) = strategy.select(&[high_prio, low_prio], &no_usage(), &default_meta(), NOW);
        // Auto-fallback should choose the higher-priority account
        assert_eq!(result[0].id, "high");
    }

    #[test]
    fn auto_fallback_skips_rate_limited() {
        let strategy = SessionStrategy::default();
        let mut high_prio = make_account("high", "anthropic", 1);
        high_prio.auto_fallback_enabled = true;
        high_prio.rate_limit_reset = Some(NOW - 2000); // Window reset
        high_prio.rate_limited_until = Some(NOW + 60_000); // But still rate-limited by our system

        let low_prio = make_account("low", "zai", 5);

        let (result, _) = strategy.select(&[high_prio, low_prio], &no_usage(), &default_meta(), NOW);
        // Should not auto-fallback because account is still rate-limited
        assert_eq!(result[0].id, "low");
    }

    #[test]
    fn auto_fallback_only_for_anthropic() {
        let strategy = SessionStrategy::default();
        let mut zai = make_account("zai", "zai", 1);
        zai.auto_fallback_enabled = true;
        zai.rate_limit_reset = Some(NOW - 2000);

        let other = make_account("other", "zai", 5);

        let (result, _) = strategy.select(&[zai, other], &no_usage(), &default_meta(), NOW);
        // Non-Anthropic accounts don't get auto-fallback treatment
        // Falls through to normal priority ordering
        assert_eq!(result[0].id, "zai");
        assert_eq!(result[1].id, "other");
    }

    #[test]
    fn force_account_header() {
        let strategy = SessionStrategy::default();
        let a = make_account("a", "zai", 1);
        let b = make_account("b", "zai", 5);

        let meta = SelectionMeta {
            force_account_id: Some("b".to_string()),
            bypass_session: false,
        };

        let (result, _) = strategy.select(&[a, b], &no_usage(), &meta, NOW);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].id, "b");
    }

    #[test]
    fn force_account_not_found_falls_through() {
        let strategy = SessionStrategy::default();
        let a = make_account("a", "zai", 1);

        let meta = SelectionMeta {
            force_account_id: Some("nonexistent".to_string()),
            bypass_session: false,
        };

        let (result, _) = strategy.select(&[a], &no_usage(), &meta, NOW);
        // Should fall through to normal selection
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].id, "a");
    }

    #[test]
    fn round_robin_within_same_priority_tier() {
        let strategy = SessionStrategy::default();
        let accounts = vec![
            make_account("a", "zai", 1),
            make_account("b", "zai", 1),
            make_account("c", "zai", 1),
        ];

        // Call select multiple times and track which account is chosen first
        let mut first_picks = Vec::new();
        for _ in 0..6 {
            let (result, _) = strategy.select(&accounts, &no_usage(), &default_meta(), NOW);
            first_picks.push(result[0].id.clone());
        }

        // Should cycle through a, b, c (round-robin)
        assert_eq!(first_picks[0], "a");
        assert_eq!(first_picks[1], "b");
        assert_eq!(first_picks[2], "c");
        assert_eq!(first_picks[3], "a"); // Wraps around
    }

    #[test]
    fn empty_accounts_returns_empty() {
        let strategy = SessionStrategy::default();
        let (result, resets) = strategy.select(&[], &no_usage(), &default_meta(), NOW);
        assert!(result.is_empty());
        assert!(resets.is_empty());
    }

    #[test]
    fn all_paused_returns_empty() {
        let strategy = SessionStrategy::default();
        let mut a = make_account("a", "zai", 1);
        a.paused = true;
        let mut b = make_account("b", "zai", 2);
        b.paused = true;

        let (result, _) = strategy.select(&[a, b], &no_usage(), &default_meta(), NOW);
        assert!(result.is_empty());
    }

    #[test]
    fn session_reset_returns_reset_info() {
        let strategy = SessionStrategy::default();
        let mut anthropic = make_account("oauth", "anthropic", 1);
        // No session started yet — should trigger a reset
        anthropic.session_start = None;

        let (result, resets) = strategy.select(&[anthropic], &no_usage(), &default_meta(), NOW);
        assert_eq!(result.len(), 1);
        assert_eq!(resets.len(), 1);
        assert_eq!(resets[0].account_id, "oauth");
        assert_eq!(resets[0].new_session_start, NOW);
    }

    #[test]
    fn bypass_session_skips_reset() {
        let strategy = SessionStrategy::default();
        let mut anthropic = make_account("oauth", "anthropic", 1);
        anthropic.session_start = None; // Would normally trigger reset

        let meta = SelectionMeta {
            force_account_id: None,
            bypass_session: true,
        };

        let (result, resets) = strategy.select(&[anthropic], &no_usage(), &meta, NOW);
        assert_eq!(result.len(), 1);
        assert!(resets.is_empty()); // No resets when bypassing
    }

    #[test]
    fn mixed_tiers_respects_priority() {
        let strategy = SessionStrategy::default();
        let accounts = vec![
            make_account("p1a", "zai", 1),
            make_account("p1b", "zai", 1),
            make_account("p5", "minimax", 5),
            make_account("p10", "anthropic-compatible", 10),
        ];

        let (result, _) = strategy.select(&accounts, &no_usage(), &default_meta(), NOW);
        assert_eq!(result.len(), 4);
        // First should be from tier 1 (round-robin), rest by priority
        assert!(result[0].priority == 1);
        assert!(result[1].priority == 1);
        assert_eq!(result[2].id, "p5");
        assert_eq!(result[3].id, "p10");
    }

    #[test]
    fn rate_limit_window_reset_triggers_session_reset() {
        let strategy = SessionStrategy::default();
        let mut anthropic = make_account("oauth", "anthropic", 1);
        anthropic.session_start = Some(NOW - time::HOUR); // Active session, 1hr in
        anthropic.rate_limit_reset = Some(NOW - 2000); // Window reset 2s ago

        let (result, resets) = strategy.select(&[anthropic], &no_usage(), &default_meta(), NOW);
        assert_eq!(result.len(), 1);
        // Should reset session because rate limit window reset
        assert_eq!(resets.len(), 1);
        assert_eq!(resets[0].new_session_start, NOW);
    }

    #[test]
    fn is_account_available_checks() {
        let usage = no_usage();

        let available = make_account("a", "zai", 1);
        assert!(is_account_available(&available, &usage, NOW));

        let mut paused = make_account("b", "zai", 1);
        paused.paused = true;
        assert!(!is_account_available(&paused, &usage, NOW));

        let mut rate_limited = make_account("c", "zai", 1);
        rate_limited.rate_limited_until = Some(NOW + 1000);
        assert!(!is_account_available(&rate_limited, &usage, NOW));

        let mut expired_rl = make_account("d", "zai", 1);
        expired_rl.rate_limited_until = Some(NOW - 1000);
        assert!(is_account_available(&expired_rl, &usage, NOW));
    }

    // -----------------------------------------------------------------------
    // Usage-aware / reserve capacity tests
    // -----------------------------------------------------------------------

    fn make_usage(pct: f64, resets_at_ms: Option<i64>) -> RoutingUsageInfo {
        RoutingUsageInfo {
            utilization_pct: pct,
            resets_at_ms,
            windows: vec![],
        }
    }

    fn make_usage_with_windows(windows: Vec<bccf_core::types::WindowUsage>) -> RoutingUsageInfo {
        let pct = windows.iter().map(|w| w.utilization_pct).fold(0.0_f64, f64::max);
        let resets_at_ms = windows.iter().filter_map(|w| w.resets_at_ms).min();
        RoutingUsageInfo {
            utilization_pct: pct,
            resets_at_ms,
            windows,
        }
    }

    #[test]
    fn hard_reserve_excludes_account() {
        let strategy = SessionStrategy::default();
        let mut a = make_account("a", "zai", 1);
        a.reserve_5h = 20;
        a.reserve_hard = true;

        let b = make_account("b", "zai", 2);

        // a is at 85% util → threshold is 80% → excluded
        let mut usage = HashMap::new();
        usage.insert("a".to_string(), make_usage(85.0, None));

        let (result, _) = strategy.select(&[a, b], &usage, &default_meta(), NOW);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].id, "b");
    }

    #[test]
    fn hard_reserve_allows_under_threshold() {
        let strategy = SessionStrategy::default();
        let mut a = make_account("a", "zai", 1);
        a.reserve_5h = 20;
        a.reserve_hard = true;

        let b = make_account("b", "zai", 2);

        // a at 75% → threshold is 80% → still available
        let mut usage = HashMap::new();
        usage.insert("a".to_string(), make_usage(75.0, None));

        let (result, _) = strategy.select(&[a, b], &usage, &default_meta(), NOW);
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].id, "a");
    }

    #[test]
    fn soft_reserve_deprioritizes_within_tier() {
        let strategy = SessionStrategy::default();
        let mut a = make_account("a", "zai", 1);
        a.reserve_5h = 20;
        // reserve_hard = false (default) — soft reserve

        let b = make_account("b", "zai", 1); // same priority

        // a at 85% (over 80% threshold), b has no usage data (under reserve)
        let mut usage = HashMap::new();
        usage.insert("a".to_string(), make_usage(85.0, None));

        let (result, _) = strategy.select(&[a, b], &usage, &default_meta(), NOW);
        assert_eq!(result.len(), 2);
        // b should come first (under reserve), a second (at reserve, soft)
        assert_eq!(result[0].id, "b");
        assert_eq!(result[1].id, "a");
    }

    #[test]
    fn soonest_reset_preferred_within_tier() {
        let strategy = SessionStrategy::default();
        let a = make_account("a", "zai", 1);
        let b = make_account("b", "zai", 1);
        let c = make_account("c", "zai", 1);

        let mut usage = HashMap::new();
        usage.insert("a".to_string(), make_usage(50.0, Some(NOW + 3_600_000))); // resets in 1h
        usage.insert("b".to_string(), make_usage(50.0, Some(NOW + 600_000))); // resets in 10min
        usage.insert("c".to_string(), make_usage(50.0, Some(NOW + 7_200_000))); // resets in 2h

        let (result, _) = strategy.select(&[a, b, c], &usage, &default_meta(), NOW);
        assert_eq!(result.len(), 3);
        // b resets soonest → first; a next; c last
        assert_eq!(result[0].id, "b");
        assert_eq!(result[1].id, "a");
        assert_eq!(result[2].id, "c");
    }

    #[test]
    fn no_usage_data_falls_back_to_existing_behavior() {
        let strategy = SessionStrategy::default();
        let accounts = vec![
            make_account("low", "zai", 10),
            make_account("high", "zai", 1),
            make_account("mid", "zai", 5),
        ];

        // Empty usage map — should behave like before
        let (result, _) = strategy.select(&accounts, &no_usage(), &default_meta(), NOW);
        assert_eq!(result[0].id, "high");
        assert_eq!(result[1].id, "mid");
        assert_eq!(result[2].id, "low");
    }

    #[test]
    fn hard_reserve_skips_active_session() {
        let strategy = SessionStrategy::default();
        let mut anthropic = make_account("oauth", "anthropic", 1);
        anthropic.session_start = Some(NOW - time::HOUR); // active session
        anthropic.reserve_5h = 20;
        anthropic.reserve_hard = true;

        let fallback = make_account("fallback", "zai", 5);

        // oauth at 85% → hard reserve threshold 80% → excluded despite active session
        let mut usage = HashMap::new();
        usage.insert("oauth".to_string(), make_usage(85.0, None));

        let (result, _) = strategy.select(&[anthropic, fallback], &usage, &default_meta(), NOW);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].id, "fallback");
    }

    #[test]
    fn soft_reserve_keeps_active_session() {
        let strategy = SessionStrategy::default();
        let mut anthropic = make_account("oauth", "anthropic", 1);
        anthropic.session_start = Some(NOW - time::HOUR); // active session
        anthropic.reserve_5h = 20;
        // reserve_hard = false (default)

        let fallback = make_account("fallback", "zai", 5);

        // oauth at 85% → soft reserve → still used because session affinity wins
        let mut usage = HashMap::new();
        usage.insert("oauth".to_string(), make_usage(85.0, None));

        let (result, _) =
            strategy.select(&[anthropic, fallback], &usage, &default_meta(), NOW);
        assert_eq!(result[0].id, "oauth"); // session affinity preserved
        assert_eq!(result[1].id, "fallback");
    }

    #[test]
    fn is_account_available_hard_reserve() {
        let mut a = make_account("a", "zai", 1);
        a.reserve_5h = 25;
        a.reserve_hard = true;

        let mut usage = HashMap::new();
        usage.insert("a".to_string(), make_usage(80.0, None)); // at threshold (100-25=75) → excluded

        assert!(!is_account_available(&a, &usage, NOW));

        // Under threshold
        usage.insert("a".to_string(), make_usage(70.0, None));
        assert!(is_account_available(&a, &usage, NOW));

        // No usage data → available (assume under reserve)
        let empty = no_usage();
        assert!(is_account_available(&a, &empty, NOW));
    }

    // -----------------------------------------------------------------------
    // Per-window reserve capacity tests
    // -----------------------------------------------------------------------

    #[test]
    fn per_window_5h_reserve_triggered_weekly_not() {
        use bccf_core::types::{WindowKind, WindowUsage};

        let mut a = make_account("a", "anthropic", 1);
        a.reserve_5h = 20;
        a.reserve_weekly = 10;

        let mut usage = HashMap::new();
        usage.insert(
            "a".to_string(),
            make_usage_with_windows(vec![
                WindowUsage { kind: WindowKind::FiveHour, utilization_pct: 85.0, resets_at_ms: None },
                WindowUsage { kind: WindowKind::Weekly, utilization_pct: 50.0, resets_at_ms: None },
            ]),
        );

        // 5h at 85% ≥ (100-20)=80% → at reserve
        assert!(is_at_reserve(&a, &usage));
    }

    #[test]
    fn per_window_weekly_reserve_triggered_5h_not() {
        use bccf_core::types::{WindowKind, WindowUsage};

        let mut a = make_account("a", "anthropic", 1);
        a.reserve_5h = 20;
        a.reserve_weekly = 10;

        let mut usage = HashMap::new();
        usage.insert(
            "a".to_string(),
            make_usage_with_windows(vec![
                WindowUsage { kind: WindowKind::FiveHour, utilization_pct: 50.0, resets_at_ms: None },
                WindowUsage { kind: WindowKind::Weekly, utilization_pct: 95.0, resets_at_ms: None },
            ]),
        );

        // weekly at 95% ≥ (100-10)=90% → at reserve
        assert!(is_at_reserve(&a, &usage));
    }

    #[test]
    fn per_window_both_under_reserve() {
        use bccf_core::types::{WindowKind, WindowUsage};

        let mut a = make_account("a", "anthropic", 1);
        a.reserve_5h = 20;
        a.reserve_weekly = 10;

        let mut usage = HashMap::new();
        usage.insert(
            "a".to_string(),
            make_usage_with_windows(vec![
                WindowUsage { kind: WindowKind::FiveHour, utilization_pct: 50.0, resets_at_ms: None },
                WindowUsage { kind: WindowKind::Weekly, utilization_pct: 50.0, resets_at_ms: None },
            ]),
        );

        // Both under threshold → not at reserve
        assert!(!is_at_reserve(&a, &usage));
    }

    #[test]
    fn per_window_hard_reserve_excludes_on_either_window() {
        use bccf_core::types::{WindowKind, WindowUsage};

        let strategy = SessionStrategy::default();

        let mut a = make_account("a", "anthropic", 1);
        a.reserve_5h = 20;
        a.reserve_weekly = 10;
        a.reserve_hard = true;

        let b = make_account("b", "zai", 2);

        // Only weekly hits threshold
        let mut usage = HashMap::new();
        usage.insert(
            "a".to_string(),
            make_usage_with_windows(vec![
                WindowUsage { kind: WindowKind::FiveHour, utilization_pct: 50.0, resets_at_ms: None },
                WindowUsage { kind: WindowKind::Weekly, utilization_pct: 92.0, resets_at_ms: None },
            ]),
        );

        let (result, _) = strategy.select(&[a, b], &usage, &default_meta(), NOW);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].id, "b"); // a excluded by hard reserve on weekly
    }

    #[test]
    fn per_window_only_5h_reserve_set() {
        use bccf_core::types::{WindowKind, WindowUsage};

        let mut a = make_account("a", "anthropic", 1);
        a.reserve_5h = 20;
        a.reserve_weekly = 0; // no weekly reserve

        let mut usage = HashMap::new();
        usage.insert(
            "a".to_string(),
            make_usage_with_windows(vec![
                WindowUsage { kind: WindowKind::FiveHour, utilization_pct: 50.0, resets_at_ms: None },
                WindowUsage { kind: WindowKind::Weekly, utilization_pct: 99.0, resets_at_ms: None },
            ]),
        );

        // Weekly at 99% but reserve_weekly=0 → not checked, 5h at 50% < 80% → not at reserve
        assert!(!is_at_reserve(&a, &usage));
    }
}
