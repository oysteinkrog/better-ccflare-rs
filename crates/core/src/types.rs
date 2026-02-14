use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Account types
// ---------------------------------------------------------------------------

/// Domain model for an account — used throughout the application.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Account {
    pub id: String,
    pub name: String,
    pub provider: String,
    pub api_key: Option<String>,
    pub refresh_token: String,
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
    fn token_status_serde() {
        let json = serde_json::to_string(&TokenStatus::Valid).unwrap();
        assert_eq!(json, r#""valid""#);
        let back: TokenStatus = serde_json::from_str(r#""expired""#).unwrap();
        assert_eq!(back, TokenStatus::Expired);
    }
}
