//! Stub provider — minimal implementation for testing and placeholders.
//!
//! Used by registry tests and as a placeholder until real provider
//! implementations land (US-006, US-007a/b, US-008).

use async_trait::async_trait;
use http::HeaderMap;

use bccf_core::types::Account;

use crate::error::ProviderError;
use crate::traits::Provider;
use crate::types::{RateLimitInfo, TokenRefreshResult, UsageInfo};

/// A stub provider that returns defaults for everything.
pub struct StubProvider {
    name: String,
}

impl StubProvider {
    pub fn new(name: &str) -> Self {
        Self {
            name: name.to_string(),
        }
    }
}

#[async_trait]
impl Provider for StubProvider {
    fn name(&self) -> &str {
        &self.name
    }

    fn build_url(&self, path: &str, query: &str, _account: Option<&Account>) -> String {
        if query.is_empty() {
            format!("https://stub.example.com{path}")
        } else {
            format!("https://stub.example.com{path}?{query}")
        }
    }

    fn prepare_headers(
        &self,
        _headers: &mut HeaderMap,
        _access_token: Option<&str>,
        _api_key: Option<&str>,
    ) -> Result<(), ProviderError> {
        Ok(())
    }

    fn parse_rate_limit(&self, _headers: &HeaderMap, status_code: u16) -> RateLimitInfo {
        RateLimitInfo {
            is_rate_limited: status_code == 429,
            ..Default::default()
        }
    }

    async fn refresh_token(
        &self,
        account: &Account,
        _client_id: &str,
    ) -> Result<TokenRefreshResult, ProviderError> {
        Ok(TokenRefreshResult {
            access_token: account
                .api_key
                .clone()
                .unwrap_or_else(|| "stub-token".into()),
            expires_at: chrono::Utc::now().timestamp_millis() + 86_400_000,
            refresh_token: String::new(),
            subscription_tier: None,
            email: None,
        })
    }

    fn extract_usage_info(&self, body: &[u8]) -> Option<UsageInfo> {
        // Try to parse Anthropic-style usage from JSON body
        let json: serde_json::Value = serde_json::from_slice(body).ok()?;
        let usage = json.get("usage")?;
        Some(UsageInfo {
            model: json.get("model").and_then(|v| v.as_str()).map(String::from),
            input_tokens: usage.get("input_tokens").and_then(|v| v.as_i64()),
            output_tokens: usage.get("output_tokens").and_then(|v| v.as_i64()),
            cache_read_input_tokens: usage
                .get("cache_read_input_tokens")
                .and_then(|v| v.as_i64()),
            cache_creation_input_tokens: usage
                .get("cache_creation_input_tokens")
                .and_then(|v| v.as_i64()),
            ..Default::default()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stub_provider_basics() {
        let p = StubProvider::new("test-stub");
        assert_eq!(p.name(), "test-stub");
        assert!(p.can_handle_path("/v1/messages"));
        assert!(p.supports_streaming());
        assert_eq!(
            p.build_url("/v1/messages", "", None),
            "https://stub.example.com/v1/messages"
        );
        assert_eq!(
            p.build_url("/v1/messages", "foo=bar", None),
            "https://stub.example.com/v1/messages?foo=bar"
        );
    }

    #[test]
    fn stub_parse_rate_limit_429() {
        let p = StubProvider::new("test");
        let headers = HeaderMap::new();
        let info = p.parse_rate_limit(&headers, 429);
        assert!(info.is_rate_limited);

        let info_200 = p.parse_rate_limit(&headers, 200);
        assert!(!info_200.is_rate_limited);
    }

    #[test]
    fn stub_extract_usage() {
        let p = StubProvider::new("test");
        let body = br#"{"model":"claude-3-opus","usage":{"input_tokens":100,"output_tokens":50}}"#;
        let usage = p.extract_usage_info(body).unwrap();
        assert_eq!(usage.model.as_deref(), Some("claude-3-opus"));
        assert_eq!(usage.input_tokens, Some(100));
        assert_eq!(usage.output_tokens, Some(50));
    }

    #[tokio::test]
    async fn stub_refresh_token() {
        let p = StubProvider::new("test");
        let account = crate::test_util::test_account_with_key("sk-test-123");
        let result = p.refresh_token(&account, "client-id").await.unwrap();
        assert_eq!(result.access_token, "sk-test-123");
    }
}
