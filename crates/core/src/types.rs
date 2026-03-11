use std::fmt;

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Account types
// ---------------------------------------------------------------------------

/// Domain model for an account — used throughout the application.
///
/// Implements a custom `Debug` that masks `api_key`, `access_token`, and
/// `refresh_token` to prevent credential leakage in logs.
#[derive(Clone, Serialize, Deserialize)]
pub struct Account {
    pub id: String,
    pub name: String,
    pub provider: String,
    #[serde(skip_serializing)]
    pub api_key: Option<String>,
    #[serde(skip_serializing)]
    pub refresh_token: String,
    #[serde(skip_serializing)]
    pub access_token: Option<String>,
    pub expires_at: Option<i64>,
    pub request_count: i64,
    pub total_requests: i64,
    pub last_used: Option<i64>,
    pub created_at: i64,
    pub rate_limited_until: Option<i64>,
    pub session_start: Option<i64>,
    pub session_request_count: i64,
    pub paused: bool,
    pub rate_limit_reset: Option<i64>,
    pub rate_limit_status: Option<String>,
    pub rate_limit_remaining: Option<i64>,
    pub priority: i64,
    pub auto_fallback_enabled: bool,
    pub auto_refresh_enabled: bool,
    pub custom_endpoint: Option<String>,
    pub model_mappings: Option<String>,
    pub reserve_5h: i64,
    pub reserve_weekly: i64,
    pub reserve_hard: bool,
    /// Human-readable subscription tier for OAuth accounts (e.g. "Max 20x", "Pro").
    pub subscription_tier: Option<String>,
    /// Email address of the authenticated OAuth user.
    pub email: Option<String>,
    /// Timestamp (ms) when the refresh token was last issued/rotated.
    /// Updated on every re-authentication. Used to track refresh token age
    /// independently of account creation date.
    pub refresh_token_updated_at: Option<i64>,
    /// Whether this account is shared with external users outside better-ccflare.
    /// When true, the utilization API reports usage from all sources, not just proxy
    /// traffic — this biases X-factor estimates downward.
    pub is_shared: bool,
    /// When true (default), the load balancer skips this account when any usage
    /// window reaches 100%, preventing overage billing on team/org plans.
    pub overage_protection: bool,
}

impl fmt::Debug for Account {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Account")
            .field("id", &self.id)
            .field("name", &self.name)
            .field("provider", &self.provider)
            .field("api_key", &self.api_key.as_ref().map(|_| "[REDACTED]"))
            .field("refresh_token", &"[REDACTED]")
            .field("access_token", &self.access_token.as_ref().map(|_| "[REDACTED]"))
            .field("expires_at", &self.expires_at)
            .field("request_count", &self.request_count)
            .field("total_requests", &self.total_requests)
            .field("last_used", &self.last_used)
            .field("created_at", &self.created_at)
            .field("rate_limited_until", &self.rate_limited_until)
            .field("session_start", &self.session_start)
            .field("session_request_count", &self.session_request_count)
            .field("paused", &self.paused)
            .field("rate_limit_reset", &self.rate_limit_reset)
            .field("rate_limit_status", &self.rate_limit_status)
            .field("rate_limit_remaining", &self.rate_limit_remaining)
            .field("priority", &self.priority)
            .field("auto_fallback_enabled", &self.auto_fallback_enabled)
            .field("auto_refresh_enabled", &self.auto_refresh_enabled)
            .field("custom_endpoint", &self.custom_endpoint)
            .field("model_mappings", &self.model_mappings)
            .field("reserve_5h", &self.reserve_5h)
            .field("reserve_weekly", &self.reserve_weekly)
            .field("reserve_hard", &self.reserve_hard)
            .field("subscription_tier", &self.subscription_tier)
            .field("email", &self.email)
            .field("refresh_token_updated_at", &self.refresh_token_updated_at)
            .field("is_shared", &self.is_shared)
            .field("overage_protection", &self.overage_protection)
            .finish()
    }
}

/// API response for an account — what clients receive.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountResponse {
    pub id: String,
    pub name: String,
    pub provider: String,
    pub request_count: i64,
    pub total_requests: i64,
    pub last_used: Option<String>,
    pub created: String,
    pub paused: bool,
    pub token_status: TokenStatus,
    pub token_expires_at: Option<String>,
    pub rate_limit_status: String,
    pub rate_limit_reset: Option<String>,
    pub rate_limit_remaining: Option<i64>,
    pub session_info: String,
    pub priority: i64,
    pub auto_fallback_enabled: bool,
    pub auto_refresh_enabled: bool,
    pub custom_endpoint: Option<String>,
    pub model_mappings: Option<serde_json::Value>,
    pub usage_utilization: Option<f64>,
    pub usage_window: Option<String>,
    pub usage_data: Option<serde_json::Value>,
    pub has_refresh_token: bool,
    pub reserve_5h: i64,
    pub reserve_weekly: i64,
    pub reserve_hard: bool,
    /// Human-readable subscription tier for OAuth accounts (e.g. "Max 20x", "Pro").
    pub subscription_tier: Option<String>,
    /// Email address of the authenticated OAuth user.
    pub email: Option<String>,
    /// Whether this account is shared with external users outside better-ccflare.
    pub is_shared: bool,
    /// Whether overage protection is enabled (skip account at 100% usage).
    pub overage_protection: bool,
}

/// Token validity status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TokenStatus {
    Valid,
    Expired,
    /// API-key provider — no OAuth token management needed.
    #[serde(rename = "api_key")]
    ApiKey,
}

/// Normalized usage info for load-balancer routing decisions.
///
/// Abstracts across provider-specific usage formats (Anthropic, Zai, NanoGPT)
/// into a single struct the load balancer can use.
#[derive(Debug, Clone)]
pub struct RoutingUsageInfo {
    /// Utilization percentage (0-100) — max across all windows (backwards compat).
    pub utilization_pct: f64,
    /// Epoch-ms timestamp when the most restrictive window resets.
    pub resets_at_ms: Option<i64>,
    /// Per-window breakdown for fine-grained reserve checks.
    pub windows: Vec<WindowUsage>,
}

/// Per-window usage data for reserve capacity checks.
#[derive(Debug, Clone)]
pub struct WindowUsage {
    pub kind: WindowKind,
    pub utilization_pct: f64,
    pub resets_at_ms: Option<i64>,
}

/// The kind of usage window, used to match against per-window reserve thresholds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowKind {
    FiveHour,
    Weekly,
    /// NanoGPT daily/monthly, Zai tokens, etc.
    Other,
}

/// Account creation options.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AddAccountOptions {
    pub name: String,
    #[serde(default)]
    pub mode: Option<String>,
    #[serde(default)]
    pub priority: Option<i64>,
    #[serde(default)]
    pub custom_endpoint: Option<String>,
}

// ---------------------------------------------------------------------------
// API key types
// ---------------------------------------------------------------------------

/// Scope of an API key — determines which endpoints it can access.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum KeyScope {
    /// Full access — management API (`/api/*`) plus proxy endpoints (`/v1/*`).
    #[default]
    Admin,
    /// Restricted access — proxy endpoints only (`/v1/*`).
    /// Cannot access `/api/*` management endpoints.
    Proxy,
}

impl KeyScope {
    /// Parse from DB string (unknown values default to `Admin`).
    pub fn from_db(s: &str) -> Self {
        match s {
            "proxy" => Self::Proxy,
            _ => Self::Admin,
        }
    }

    /// Serialize to DB string.
    pub fn as_db(&self) -> &'static str {
        match self {
            Self::Admin => "admin",
            Self::Proxy => "proxy",
        }
    }
}

/// Domain model for API keys.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiKey {
    pub id: String,
    pub name: String,
    pub hashed_key: String,
    pub prefix_last_8: String,
    pub created_at: i64,
    pub last_used: Option<i64>,
    pub usage_count: i64,
    pub is_active: bool,
    pub scope: KeyScope,
}

/// API response for API keys (sensitive data excluded).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiKeyResponse {
    pub id: String,
    pub name: String,
    pub prefix_last_8: String,
    pub created_at: String,
    pub last_used: Option<String>,
    pub usage_count: i64,
    pub is_active: bool,
    pub scope: KeyScope,
}

// ---------------------------------------------------------------------------
// Request types
// ---------------------------------------------------------------------------

/// Domain model for a proxied request log entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProxyRequest {
    pub id: String,
    pub timestamp: i64,
    pub method: String,
    pub path: String,
    pub account_used: Option<String>,
    pub status_code: Option<i64>,
    pub success: bool,
    pub error_message: Option<String>,
    pub response_time_ms: Option<i64>,
    pub failover_attempts: i64,
    pub model: Option<String>,
    pub prompt_tokens: Option<i64>,
    pub completion_tokens: Option<i64>,
    pub total_tokens: Option<i64>,
    pub cost_usd: Option<f64>,
    pub input_tokens: Option<i64>,
    pub cache_read_input_tokens: Option<i64>,
    pub cache_creation_input_tokens: Option<i64>,
    pub output_tokens: Option<i64>,
    pub agent_used: Option<String>,
    pub tokens_per_second: Option<f64>,
    pub project: Option<String>,
    pub api_key_id: Option<String>,
    pub api_key_name: Option<String>,
}

/// Request metadata for incoming proxied requests.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequestMeta {
    pub id: String,
    pub method: String,
    pub path: String,
    pub timestamp: i64,
    pub agent_used: Option<String>,
    pub project: Option<String>,
}

// ---------------------------------------------------------------------------
// Stats types
// ---------------------------------------------------------------------------

/// Aggregate statistics.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Stats {
    pub total_requests: i64,
    pub success_rate: f64,
    pub active_accounts: i64,
    pub avg_response_time: f64,
    pub total_tokens: i64,
    pub total_cost_usd: f64,
    pub top_models: Vec<ModelCount>,
    pub avg_tokens_per_second: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelCount {
    pub model: String,
    pub count: i64,
}

/// Health check response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthResponse {
    pub status: String,
    pub accounts: i64,
    pub timestamp: String,
    pub strategy: String,
}

// ---------------------------------------------------------------------------
// Strategy
// ---------------------------------------------------------------------------

/// Load-balancing strategy names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StrategyName {
    Session,
}

impl Default for StrategyName {
    fn default() -> Self {
        Self::Session
    }
}

impl StrategyName {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Session => "session",
        }
    }
}

pub const DEFAULT_STRATEGY: StrategyName = StrategyName::Session;

pub fn is_valid_strategy(s: &str) -> bool {
    matches!(s, "session")
}

/// Special account ID for requests without an account.
pub const NO_ACCOUNT_ID: &str = "no_account";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strategy_default() {
        assert_eq!(StrategyName::default(), StrategyName::Session);
    }

    #[test]
    fn is_valid_strategy_works() {
        assert!(is_valid_strategy("session"));
        assert!(!is_valid_strategy("round-robin"));
    }

    #[test]
    fn account_debug_redacts_tokens() {
        let account = Account {
            id: "test-id".to_string(),
            name: "test-account".to_string(),
            provider: "anthropic".to_string(),
            api_key: Some("sk-ant-secret-api-key".to_string()),
            refresh_token: "rt-secret-refresh-token".to_string(),
            access_token: Some("at-secret-access-token".to_string()),
            expires_at: Some(9999999999999),
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
            reserve_5h: 0,
            reserve_weekly: 0,
            reserve_hard: false,
            subscription_tier: None,
            email: None,
            refresh_token_updated_at: None,
            is_shared: false,
            overage_protection: true,
        };
        let debug_output = format!("{:?}", account);
        assert!(!debug_output.contains("sk-ant-secret-api-key"), "api_key leaked in Debug output");
        assert!(!debug_output.contains("rt-secret-refresh-token"), "refresh_token leaked in Debug output");
        assert!(!debug_output.contains("at-secret-access-token"), "access_token leaked in Debug output");
        assert!(debug_output.contains("[REDACTED]"), "Debug output should contain [REDACTED]");
        // Non-sensitive fields should still appear
        assert!(debug_output.contains("test-account"));
        assert!(debug_output.contains("anthropic"));
    }

    #[test]
    fn token_status_serde() {
        let json = serde_json::to_string(&TokenStatus::Valid).unwrap();
        assert_eq!(json, r#""valid""#);
        let back: TokenStatus = serde_json::from_str(r#""expired""#).unwrap();
        assert_eq!(back, TokenStatus::Expired);
    }
}
