//! Provider-related types: rate limit info, token refresh, usage info.

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Rate limit info
// ---------------------------------------------------------------------------

/// Information extracted from provider rate-limit headers.
#[derive(Debug, Clone, Default)]
pub struct RateLimitInfo {
    /// Whether the account is currently rate-limited.
    pub is_rate_limited: bool,
    /// Epoch millis when the limit resets.
    pub reset_time: Option<i64>,
    /// Raw status header value (e.g. "allowed", "rate_limited").
    pub status_header: Option<String>,
    /// Remaining requests in the current window.
    pub remaining: Option<i64>,
}

// ---------------------------------------------------------------------------
// Token refresh
// ---------------------------------------------------------------------------

/// Result of refreshing an account's access token.
#[derive(Debug, Clone)]
pub struct TokenRefreshResult {
    pub access_token: String,
    /// Epoch millis when the token expires.
    pub expires_at: i64,
    /// Updated refresh token (may be same as before).
    pub refresh_token: String,
    /// Human-readable subscription tier, populated for claude-oauth only (e.g. "Max 20x", "Pro").
    pub subscription_tier: Option<String>,
    /// Email address of the authenticated user (claude-oauth only).
    pub email: Option<String>,
}

// ---------------------------------------------------------------------------
// Usage info (extracted from provider responses)
// ---------------------------------------------------------------------------

/// Token usage extracted from a provider response.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UsageInfo {
    pub model: Option<String>,
    pub prompt_tokens: Option<i64>,
    pub completion_tokens: Option<i64>,
    pub total_tokens: Option<i64>,
    pub cost_usd: Option<f64>,
    pub input_tokens: Option<i64>,
    pub cache_read_input_tokens: Option<i64>,
    pub cache_creation_input_tokens: Option<i64>,
    pub output_tokens: Option<i64>,
}

// ---------------------------------------------------------------------------
// Auth type
// ---------------------------------------------------------------------------

/// How the provider expects credentials in headers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AuthType {
    /// `Authorization: Bearer <token>`
    #[default]
    Bearer,
    /// Header value is the raw key (e.g. `x-api-key: sk-...`)
    Direct,
}

// ---------------------------------------------------------------------------
// Hard rate limit statuses
// ---------------------------------------------------------------------------

/// Status values that indicate a hard rate limit (not soft/allowed).
pub const HARD_LIMIT_STATUSES: &[&str] = &[
    "rate_limited",
    "blocked",
    "queueing_hard",
    "payment_required",
];

/// Returns `true` if the status string indicates a hard rate limit.
pub fn is_hard_rate_limited(status: &str) -> bool {
    HARD_LIMIT_STATUSES.contains(&status)
}
