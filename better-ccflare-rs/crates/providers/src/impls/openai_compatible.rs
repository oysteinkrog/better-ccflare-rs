//! OpenAI-compatible provider — translates Anthropic format to OpenAI format.
//!
//! Used for: OpenAI API, Azure OpenAI, Groq, and any endpoint that speaks
//! the OpenAI Chat Completions protocol.
//!
//! Key differences from Anthropic-compatible:
//! - Request body: Anthropic messages → OpenAI chat format
//! - Response body: OpenAI format → Anthropic format (non-streaming)
//! - SSE stream: OpenAI delta format → Anthropic SSE format (streaming)
//! - Auth: Bearer token in Authorization header
//! - Path: `/v1/messages` → `/v1/chat/completions`

use async_trait::async_trait;
use http::{HeaderMap, HeaderName};

use bccf_core::types::Account;

use crate::error::ProviderError;
use crate::model_mapping;
use crate::traits::Provider;
use crate::types::{AuthType, RateLimitInfo, TokenRefreshResult, UsageInfo};

use super::openai_format;

/// Default endpoint for the OpenAI API.
pub const DEFAULT_OPENAI_ENDPOINT: &str = "https://api.openai.com";


/// Configuration for an OpenAI-compatible provider instance.
#[derive(Debug, Clone)]
pub struct OpenAiCompatibleConfig {
    /// Provider name.
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

impl Default for OpenAiCompatibleConfig {
    fn default() -> Self {
        Self {
            name: "openai-compatible".to_string(),
            endpoint: DEFAULT_OPENAI_ENDPOINT.to_string(),
            auth_header: "authorization".to_string(),
            auth_type: AuthType::Bearer,
            supports_streaming: true,
        }
    }
}

/// An OpenAI-compatible provider.
pub struct OpenAiCompatibleProvider {
    config: OpenAiCompatibleConfig,
}

impl OpenAiCompatibleProvider {
    pub fn new(config: OpenAiCompatibleConfig) -> Self {
        Self { config }
    }

    /// Create a default OpenAI provider.
    pub fn openai() -> Self {
        Self::new(OpenAiCompatibleConfig::default())
    }

    /// Create a provider with a custom endpoint.
    pub fn with_endpoint(name: &str, endpoint: &str) -> Self {
        Self::new(OpenAiCompatibleConfig {
            name: name.to_string(),
            endpoint: endpoint.trim_end_matches('/').to_string(),
            ..Default::default()
        })
    }

    /// Get the effective endpoint for a specific account.
    fn effective_endpoint<'a>(&'a self, account: Option<&'a Account>) -> &'a str {
        if let Some(acc) = account {
            if let Some(ref custom) = acc.custom_endpoint {
                if !custom.is_empty() {
                    return custom.as_str();
                }
            }
        }
        &self.config.endpoint
    }

    /// Convert Anthropic API path to OpenAI path.
    fn translate_path(path: &str) -> &str {
        if path.contains("/messages") {
            "/v1/chat/completions"
        } else {
            path
        }
    }

}

#[async_trait]
impl Provider for OpenAiCompatibleProvider {
    fn name(&self) -> &str {
        &self.config.name
    }

    fn build_url(&self, path: &str, query: &str, account: Option<&Account>) -> String {
        let endpoint = self.effective_endpoint(account);
        let translated_path = Self::translate_path(path);
        if query.is_empty() {
            format!("{endpoint}{translated_path}")
        } else {
            format!("{endpoint}{translated_path}?{query}")
        }
    }

    fn prepare_headers(
        &self,
        headers: &mut HeaderMap,
        access_token: Option<&str>,
        api_key: Option<&str>,
    ) {
        // Remove existing auth headers
        headers.remove("authorization");
        headers.remove("x-api-key");

        // Set auth header
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

        // Remove hop-by-hop headers
        headers.remove("host");
        headers.remove("accept-encoding");
        headers.remove("content-encoding");
    }

    fn parse_rate_limit(&self, headers: &HeaderMap, status_code: u16) -> RateLimitInfo {
        // OpenAI rate limit headers
        let remaining = headers
            .get("x-ratelimit-remaining-requests")
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.parse::<i64>().ok());

        let reset_time = headers
            .get("x-ratelimit-reset-requests")
            .and_then(|v| v.to_str().ok())
            .and_then(|s| {
                // Try parsing as seconds, then as ISO 8601
                s.parse::<i64>().ok().or_else(|| {
                    chrono::DateTime::parse_from_rfc3339(s)
                        .ok()
                        .map(|dt| dt.timestamp_millis())
                })
            });

        RateLimitInfo {
            is_rate_limited: status_code == 429,
            reset_time,
            status_header: None,
            remaining,
        }
    }

    async fn refresh_token(
        &self,
        account: &Account,
        _client_id: &str,
    ) -> Result<TokenRefreshResult, ProviderError> {
        // API key providers don't refresh — return the key directly.
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
            expires_at: chrono::Utc::now().timestamp_millis() + 30 * 24 * 60 * 60 * 1000,
            refresh_token: String::new(),
            subscription_tier: None,
        })
    }

    fn supports_streaming(&self) -> bool {
        self.config.supports_streaming
    }

    /// Transform request body: Anthropic → OpenAI format, then apply model mapping.
    async fn transform_request_body(
        &self,
        body: &[u8],
        account: Option<&Account>,
    ) -> Result<Option<Vec<u8>>, ProviderError> {
        // First translate Anthropic format → OpenAI format
        let translated = openai_format::anthropic_to_openai_request(body).ok_or_else(|| {
            ProviderError::RequestTransform("Failed to translate request body".to_string())
        })?;

        // Then apply model mapping
        let result = model_mapping::transform_body_model(&translated, account);
        Ok(Some(result.unwrap_or(translated)))
    }

    /// Extract usage from an OpenAI-format response body.
    fn extract_usage_info(&self, body: &[u8]) -> Option<UsageInfo> {
        let json: serde_json::Value = serde_json::from_slice(body).ok()?;
        let usage = json.get("usage")?;

        let prompt_tokens = usage.get("prompt_tokens").and_then(|v| v.as_i64());
        let completion_tokens = usage.get("completion_tokens").and_then(|v| v.as_i64());
        let total_tokens = usage
            .get("total_tokens")
            .and_then(|v| v.as_i64())
            .or_else(|| Some(prompt_tokens.unwrap_or(0) + completion_tokens.unwrap_or(0)));

        let model = json
            .get("model")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");

        Some(UsageInfo {
            model: Some(model.to_string()),
            prompt_tokens,
            completion_tokens,
            total_tokens,
            cost_usd: None, // Calculated by proxy pricing engine (LiteLLM + bundled)
            input_tokens: prompt_tokens,
            cache_read_input_tokens: None,
            cache_creation_input_tokens: None,
            output_tokens: completion_tokens,
        })
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_basics() {
        let p = OpenAiCompatibleProvider::openai();
        assert_eq!(p.name(), "openai-compatible");
        assert!(p.supports_streaming());
    }

    #[test]
    fn url_translates_messages_to_completions() {
        let p = OpenAiCompatibleProvider::openai();
        assert_eq!(
            p.build_url("/v1/messages", "", None),
            "https://api.openai.com/v1/chat/completions"
        );
    }

    #[test]
    fn url_with_query() {
        let p = OpenAiCompatibleProvider::openai();
        assert_eq!(
            p.build_url("/v1/messages", "stream=true", None),
            "https://api.openai.com/v1/chat/completions?stream=true"
        );
    }

    #[test]
    fn url_preserves_non_messages_path() {
        let p = OpenAiCompatibleProvider::openai();
        assert_eq!(
            p.build_url("/v1/models", "", None),
            "https://api.openai.com/v1/models"
        );
    }

    #[test]
    fn custom_endpoint_from_account() {
        let p = OpenAiCompatibleProvider::openai();
        let mut account = crate::test_util::test_account_with_key("sk-test");
        account.custom_endpoint = Some("https://custom.openai.com".to_string());

        let url = p.build_url("/v1/messages", "", Some(&account));
        assert_eq!(url, "https://custom.openai.com/v1/chat/completions");
    }

    #[test]
    fn prepare_headers_bearer() {
        let p = OpenAiCompatibleProvider::openai();
        let mut headers = HeaderMap::new();
        p.prepare_headers(&mut headers, None, Some("sk-test-key"));

        assert_eq!(headers.get("authorization").unwrap(), "Bearer sk-test-key");
    }

    #[test]
    fn parse_rate_limit_429() {
        let p = OpenAiCompatibleProvider::openai();
        let mut headers = HeaderMap::new();
        headers.insert("x-ratelimit-remaining-requests", "0".parse().unwrap());

        let info = p.parse_rate_limit(&headers, 429);
        assert!(info.is_rate_limited);
        assert_eq!(info.remaining, Some(0));
    }

    #[test]
    fn parse_rate_limit_ok() {
        let p = OpenAiCompatibleProvider::openai();
        let headers = HeaderMap::new();
        let info = p.parse_rate_limit(&headers, 200);
        assert!(!info.is_rate_limited);
    }

    #[tokio::test]
    async fn transform_body_translates_format() {
        let p = OpenAiCompatibleProvider::openai();
        let body =
            br#"{"model":"gpt-4","max_tokens":100,"messages":[{"role":"user","content":"Hi"}]}"#;

        let result = p.transform_request_body(body, None).await.unwrap().unwrap();
        let json: serde_json::Value = serde_json::from_slice(&result).unwrap();

        assert_eq!(json["model"], "gpt-4");
        let msgs = json["messages"].as_array().unwrap();
        assert_eq!(msgs[0]["role"], "user");
        assert_eq!(msgs[0]["content"], "Hi");
    }

    #[tokio::test]
    async fn transform_body_with_model_mapping() {
        let p = OpenAiCompatibleProvider::openai();
        let account = crate::test_util::test_account_with_mappings(r#"{"gpt-4":"gpt-4-turbo"}"#);
        let body =
            br#"{"model":"gpt-4","max_tokens":100,"messages":[{"role":"user","content":"Hi"}]}"#;

        let result = p
            .transform_request_body(body, Some(&account))
            .await
            .unwrap()
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&result).unwrap();
        assert_eq!(json["model"], "gpt-4-turbo");
    }

    #[test]
    fn extract_usage() {
        let p = OpenAiCompatibleProvider::openai();
        let body = br#"{
            "id": "chatcmpl-123",
            "model": "gpt-4",
            "choices": [{"message": {"content": "Hi"}, "finish_reason": "stop"}],
            "usage": {"prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 15}
        }"#;

        let usage = p.extract_usage_info(body).unwrap();
        assert_eq!(usage.model.as_deref(), Some("gpt-4"));
        assert_eq!(usage.prompt_tokens, Some(10));
        assert_eq!(usage.completion_tokens, Some(5));
        assert_eq!(usage.total_tokens, Some(15));
        assert!(usage.cost_usd.is_none()); // Cost calculated by proxy pricing engine
    }

    #[tokio::test]
    async fn refresh_token_returns_api_key() {
        let p = OpenAiCompatibleProvider::openai();
        let account = crate::test_util::test_account_with_key("sk-openai-123");
        let result = p.refresh_token(&account, "client-id").await.unwrap();
        assert_eq!(result.access_token, "sk-openai-123");
    }
}
