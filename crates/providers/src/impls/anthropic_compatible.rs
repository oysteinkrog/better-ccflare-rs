//! Anthropic-compatible provider — works with any Anthropic API-compatible endpoint.
//!
//! Used for: Console (api.anthropic.com), OpenRouter, Together AI, and any
//! custom endpoint that speaks the Anthropic API protocol.

use async_trait::async_trait;
use http::{HeaderMap, HeaderName};

use bccf_core::types::Account;

use crate::error::ProviderError;
use crate::model_mapping;
use crate::traits::Provider;
use crate::types::{AuthType, RateLimitInfo, TokenRefreshResult, UsageInfo};

/// Default endpoint for the Anthropic API.
pub const DEFAULT_ANTHROPIC_ENDPOINT: &str = "https://api.anthropic.com";

/// Anthropic rate limit header names (unified).
const HEADER_UNIFIED_STATUS: &str = "anthropic-ratelimit-unified-status";
const HEADER_UNIFIED_RESET: &str = "anthropic-ratelimit-unified-reset";
const HEADER_UNIFIED_REMAINING: &str = "anthropic-ratelimit-unified-remaining";

/// Status values that indicate a hard rate limit.
const HARD_LIMIT_STATUSES: &[&str] = &[
    "rate_limited",
    "blocked",
    "queueing_hard",
    "payment_required",
];

/// Configuration for an Anthropic-compatible provider instance.
#[derive(Debug, Clone)]
pub struct AnthropicCompatibleConfig {
    /// Provider name (e.g. "anthropic-compatible", "claude-console-api").
    pub name: String,
    /// Base endpoint URL (without trailing slash).
    pub endpoint: String,
    /// HTTP header name for authentication.
    pub auth_header: String,
    /// How the auth value is formatted in the header.
    pub auth_type: AuthType,
    /// Whether streaming is supported.
    pub supports_streaming: bool,
}

impl Default for AnthropicCompatibleConfig {
    fn default() -> Self {
        Self {
            name: "anthropic-compatible".to_string(),
            endpoint: DEFAULT_ANTHROPIC_ENDPOINT.to_string(),
            auth_header: "x-api-key".to_string(),
            auth_type: AuthType::Direct,
            supports_streaming: true,
        }
    }
}

/// An Anthropic-compatible provider.
pub struct AnthropicCompatibleProvider {
    config: AnthropicCompatibleConfig,
}

impl AnthropicCompatibleProvider {
    pub fn new(config: AnthropicCompatibleConfig) -> Self {
        Self { config }
    }

    /// Create a console provider (api.anthropic.com with x-api-key).
    pub fn console() -> Self {
        Self::new(AnthropicCompatibleConfig {
            name: "claude-console-api".to_string(),
            ..Default::default()
        })
    }

    /// Create a provider with a custom endpoint from an account's config.
    pub fn with_endpoint(name: &str, endpoint: &str) -> Self {
        Self::new(AnthropicCompatibleConfig {
            name: name.to_string(),
            endpoint: endpoint.trim_end_matches('/').to_string(),
            ..Default::default()
        })
    }

    /// Get the effective endpoint for a specific account (custom or default).
    fn effective_endpoint(&self, account: Option<&Account>) -> String {
        if let Some(acc) = account {
            if let Some(ref custom) = acc.custom_endpoint {
                if !custom.is_empty() {
                    return custom.clone();
                }
            }
        }
        self.config.endpoint.clone()
    }
}

#[async_trait]
impl Provider for AnthropicCompatibleProvider {
    fn name(&self) -> &str {
        &self.config.name
    }

    fn build_url(&self, path: &str, query: &str, account: Option<&Account>) -> String {
        let endpoint = self.effective_endpoint(account);
        if query.is_empty() {
            format!("{endpoint}{path}")
        } else {
            format!("{endpoint}{path}?{query}")
        }
    }

    fn prepare_headers(
        &self,
        headers: &mut HeaderMap,
        access_token: Option<&str>,
        api_key: Option<&str>,
    ) {
        // Remove any existing auth headers
        headers.remove("authorization");
        headers.remove("x-api-key");

        // Set the auth header based on config
        let credential = access_token.or(api_key);
        if let Some(cred) = credential {
            let value = match self.config.auth_type {
                AuthType::Bearer => format!("Bearer {cred}"),
                AuthType::Direct => cred.to_string(),
            };
            if let (Ok(hn), Ok(hv)) = (
                HeaderName::from_bytes(self.config.auth_header.as_bytes()),
                value.parse(),
            ) {
                headers.insert(hn, hv);
            }
        }

        // Remove hop-by-hop headers that shouldn't be forwarded
        headers.remove("host");
        headers.remove("accept-encoding");
        headers.remove("content-encoding");
    }

    fn parse_rate_limit(&self, headers: &HeaderMap, status_code: u16) -> RateLimitInfo {
        // Check unified Anthropic rate limit headers
        let status_header = headers
            .get(HEADER_UNIFIED_STATUS)
            .and_then(|v| v.to_str().ok())
            .map(String::from);

        let is_rate_limited = if let Some(ref status) = status_header {
            // "allowed" = not rate limited; everything else = check hard limits
            status != "allowed" && HARD_LIMIT_STATUSES.contains(&status.as_str())
        } else {
            status_code == 429
        };

        let reset_time = headers
            .get(HEADER_UNIFIED_RESET)
            .and_then(|v| v.to_str().ok())
            .and_then(|s| {
                // Try parsing as epoch millis first, then as ISO 8601
                s.parse::<i64>().ok().or_else(|| {
                    chrono::DateTime::parse_from_rfc3339(s)
                        .ok()
                        .map(|dt| dt.timestamp_millis())
                })
            });

        let remaining = headers
            .get(HEADER_UNIFIED_REMAINING)
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.parse::<i64>().ok());

        RateLimitInfo {
            is_rate_limited,
            reset_time,
            status_header,
            remaining,
        }
    }

    async fn refresh_token(
        &self,
        account: &Account,
        _client_id: &str,
    ) -> Result<TokenRefreshResult, ProviderError> {
        // API key providers don't need token refresh.
        // Return the API key as the "access token".
        let token = account
            .api_key
            .clone()
            .or_else(|| {
                if !account.refresh_token.is_empty() {
                    Some(account.refresh_token.clone())
                } else {
                    None
                }
            })
            .ok_or_else(|| ProviderError::TokenRefresh("No API key available".to_string()))?;

        Ok(TokenRefreshResult {
            access_token: token,
            expires_at: chrono::Utc::now().timestamp_millis() + 30 * 24 * 60 * 60 * 1000, // 30 days
            refresh_token: String::new(),
            subscription_tier: None,
            email: None,
        })
    }

    fn supports_streaming(&self) -> bool {
        self.config.supports_streaming
    }

    async fn transform_request_body(
        &self,
        body: &[u8],
        account: Option<&Account>,
    ) -> Result<Option<Vec<u8>>, ProviderError> {
        Ok(model_mapping::transform_body_model(body, account))
    }

    fn extract_usage_info(&self, body: &[u8]) -> Option<UsageInfo> {
        let json: serde_json::Value = serde_json::from_slice(body).ok()?;
        let usage = json.get("usage")?;

        let input_tokens = usage.get("input_tokens").and_then(|v| v.as_i64());
        let output_tokens = usage.get("output_tokens").and_then(|v| v.as_i64());
        let cache_read = usage
            .get("cache_read_input_tokens")
            .and_then(|v| v.as_i64());
        let cache_creation = usage
            .get("cache_creation_input_tokens")
            .and_then(|v| v.as_i64());

        // prompt_tokens = input + cache_creation + cache_read
        let prompt_tokens = input_tokens
            .unwrap_or(0)
            .checked_add(cache_creation.unwrap_or(0))
            .and_then(|v| v.checked_add(cache_read.unwrap_or(0)));

        let total_tokens = prompt_tokens
            .unwrap_or(0)
            .checked_add(output_tokens.unwrap_or(0));

        Some(UsageInfo {
            model: json.get("model").and_then(|v| v.as_str()).map(String::from),
            prompt_tokens,
            completion_tokens: output_tokens,
            total_tokens,
            cost_usd: None, // Computed by the caller using model pricing
            input_tokens,
            cache_read_input_tokens: cache_read,
            cache_creation_input_tokens: cache_creation,
            output_tokens,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn console_provider_basics() {
        let p = AnthropicCompatibleProvider::console();
        assert_eq!(p.name(), "claude-console-api");
        assert!(p.supports_streaming());
        assert_eq!(
            p.build_url("/v1/messages", "", None),
            "https://api.anthropic.com/v1/messages"
        );
    }

    #[test]
    fn custom_endpoint_from_account() {
        let p = AnthropicCompatibleProvider::new(AnthropicCompatibleConfig::default());
        let mut account = crate::test_util::test_account_with_key("sk-test");
        account.custom_endpoint = Some("https://custom.api.com".to_string());

        let url = p.build_url("/v1/messages", "stream=true", Some(&account));
        assert_eq!(url, "https://custom.api.com/v1/messages?stream=true");
    }

    #[test]
    fn prepare_headers_sets_x_api_key() {
        let p = AnthropicCompatibleProvider::console();
        let mut headers = HeaderMap::new();
        headers.insert("authorization", "old-value".parse().unwrap());
        headers.insert("host", "example.com".parse().unwrap());

        p.prepare_headers(&mut headers, None, Some("sk-test-key"));

        assert_eq!(headers.get("x-api-key").unwrap(), "sk-test-key");
        assert!(headers.get("authorization").is_none()); // Removed
        assert!(headers.get("host").is_none()); // Removed
    }

    #[test]
    fn prepare_headers_bearer_auth() {
        let p = AnthropicCompatibleProvider::new(AnthropicCompatibleConfig {
            auth_header: "authorization".to_string(),
            auth_type: AuthType::Bearer,
            ..Default::default()
        });
        let mut headers = HeaderMap::new();
        p.prepare_headers(&mut headers, Some("token-123"), None);

        assert_eq!(headers.get("authorization").unwrap(), "Bearer token-123");
    }

    #[test]
    fn parse_rate_limit_allowed() {
        let p = AnthropicCompatibleProvider::console();
        let mut headers = HeaderMap::new();
        headers.insert(HEADER_UNIFIED_STATUS, "allowed".parse().unwrap());
        headers.insert(HEADER_UNIFIED_REMAINING, "100".parse().unwrap());

        let info = p.parse_rate_limit(&headers, 200);
        assert!(!info.is_rate_limited);
        assert_eq!(info.remaining, Some(100));
    }

    #[test]
    fn parse_rate_limit_hard_limited() {
        let p = AnthropicCompatibleProvider::console();
        let mut headers = HeaderMap::new();
        headers.insert(HEADER_UNIFIED_STATUS, "rate_limited".parse().unwrap());
        headers.insert(HEADER_UNIFIED_RESET, "1700000060000".parse().unwrap());

        let info = p.parse_rate_limit(&headers, 429);
        assert!(info.is_rate_limited);
        assert_eq!(info.reset_time, Some(1700000060000));
    }

    #[test]
    fn parse_rate_limit_429_fallback() {
        let p = AnthropicCompatibleProvider::console();
        let headers = HeaderMap::new(); // No unified headers
        let info = p.parse_rate_limit(&headers, 429);
        assert!(info.is_rate_limited);
    }

    #[test]
    fn extract_usage_from_response() {
        let p = AnthropicCompatibleProvider::console();
        let body = br#"{
            "model": "claude-3-opus",
            "usage": {
                "input_tokens": 100,
                "output_tokens": 50,
                "cache_read_input_tokens": 10,
                "cache_creation_input_tokens": 5
            }
        }"#;

        let usage = p.extract_usage_info(body).unwrap();
        assert_eq!(usage.model.as_deref(), Some("claude-3-opus"));
        assert_eq!(usage.input_tokens, Some(100));
        assert_eq!(usage.output_tokens, Some(50));
        assert_eq!(usage.cache_read_input_tokens, Some(10));
        assert_eq!(usage.cache_creation_input_tokens, Some(5));
        // prompt_tokens = input(100) + cache_creation(5) + cache_read(10) = 115
        assert_eq!(usage.prompt_tokens, Some(115));
        assert_eq!(usage.total_tokens, Some(165));
    }

    #[tokio::test]
    async fn refresh_token_returns_api_key() {
        let p = AnthropicCompatibleProvider::console();
        let account = crate::test_util::test_account_with_key("sk-test-123");
        let result = p.refresh_token(&account, "client-id").await.unwrap();
        assert_eq!(result.access_token, "sk-test-123");
    }

    #[tokio::test]
    async fn transform_body_with_mappings() {
        let p = AnthropicCompatibleProvider::console();
        let account =
            crate::test_util::test_account_with_mappings(r#"{"claude-3-opus":"custom-model"}"#);
        let body = br#"{"model":"claude-3-opus","messages":[]}"#;

        let result = p
            .transform_request_body(body, Some(&account))
            .await
            .unwrap();
        assert!(result.is_some());
        let json: serde_json::Value = serde_json::from_slice(&result.unwrap()).unwrap();
        assert_eq!(json["model"], "custom-model");
    }
}
