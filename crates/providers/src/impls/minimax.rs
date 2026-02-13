//! Minimax provider — forces all requests to the MiniMax-M2 model.
//!
//! Uses the Anthropic-compatible base with a fixed endpoint and model override.

use async_trait::async_trait;
use http::HeaderMap;

use bccf_core::types::Account;

use crate::error::ProviderError;
use crate::model_mapping;
use crate::traits::Provider;
use crate::types::{RateLimitInfo, TokenRefreshResult, UsageInfo};

use super::anthropic_compatible::AnthropicCompatibleProvider;

/// Minimax API endpoint.
const MINIMAX_ENDPOINT: &str = "https://api.minimax.io/anthropic";

/// Default model — all requests are forced to this model.
const MINIMAX_DEFAULT_MODEL: &str = "MiniMax-M2";

/// Minimax provider — wraps AnthropicCompatibleProvider with model forcing.
pub struct MinimaxProvider {
    inner: AnthropicCompatibleProvider,
}

impl MinimaxProvider {
    pub fn new() -> Self {
        Self {
            inner: AnthropicCompatibleProvider::with_endpoint("minimax", MINIMAX_ENDPOINT),
        }
    }
}

impl Default for MinimaxProvider {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Provider for MinimaxProvider {
    fn name(&self) -> &str {
        "minimax"
    }

    fn build_url(&self, path: &str, query: &str, account: Option<&Account>) -> String {
        self.inner.build_url(path, query, account)
    }

    fn prepare_headers(
        &self,
        headers: &mut HeaderMap,
        access_token: Option<&str>,
        api_key: Option<&str>,
    ) {
        self.inner.prepare_headers(headers, access_token, api_key);
    }

    fn parse_rate_limit(&self, headers: &HeaderMap, status_code: u16) -> RateLimitInfo {
        self.inner.parse_rate_limit(headers, status_code)
    }

    async fn refresh_token(
        &self,
        account: &Account,
        client_id: &str,
    ) -> Result<TokenRefreshResult, ProviderError> {
        self.inner.refresh_token(account, client_id).await
    }

    fn supports_streaming(&self) -> bool {
        self.inner.supports_streaming()
    }

    /// Force all requests to use MiniMax-M2 model.
    async fn transform_request_body(
        &self,
        body: &[u8],
        _account: Option<&Account>,
    ) -> Result<Option<Vec<u8>>, ProviderError> {
        Ok(model_mapping::transform_body_model_force(
            body,
            MINIMAX_DEFAULT_MODEL,
        ))
    }

    fn extract_usage_info(&self, body: &[u8]) -> Option<UsageInfo> {
        self.inner.extract_usage_info(body)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn minimax_provider_name() {
        let p = MinimaxProvider::new();
        assert_eq!(p.name(), "minimax");
    }

    #[test]
    fn minimax_endpoint() {
        let p = MinimaxProvider::new();
        assert_eq!(
            p.build_url("/v1/messages", "", None),
            "https://api.minimax.io/anthropic/v1/messages"
        );
    }

    #[tokio::test]
    async fn minimax_forces_model() {
        let p = MinimaxProvider::new();
        let body = br#"{"model":"claude-3-opus","messages":[]}"#;

        let result = p.transform_request_body(body, None).await.unwrap();
        assert!(result.is_some());
        let json: serde_json::Value = serde_json::from_slice(&result.unwrap()).unwrap();
        assert_eq!(json["model"], "MiniMax-M2");
    }

    #[tokio::test]
    async fn minimax_refresh_returns_api_key() {
        let p = MinimaxProvider::new();
        let account = crate::test_util::test_account_with_key("mm-key-123");
        let result = p.refresh_token(&account, "client").await.unwrap();
        assert_eq!(result.access_token, "mm-key-123");
    }
}
