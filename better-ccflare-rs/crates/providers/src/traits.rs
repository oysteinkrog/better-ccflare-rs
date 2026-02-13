//! Provider trait — the core abstraction for AI service providers.
//!
//! Each provider (Claude OAuth, Console, Anthropic-compatible, OpenAI-compatible,
//! etc.) implements this trait. The proxy dispatches requests through the trait
//! so the hot path is provider-agnostic.

use async_trait::async_trait;
use http::HeaderMap;

use bccf_core::types::Account;

use crate::error::ProviderError;
use crate::types::{RateLimitInfo, TokenRefreshResult, UsageInfo};

/// Core provider abstraction. All providers implement this trait.
///
/// Methods with default implementations are overridden by providers that
/// need custom behavior (e.g. model mapping, request transformation).
#[async_trait]
pub trait Provider: Send + Sync {
    /// Unique provider name (e.g. "claude-oauth", "console", "anthropic-compatible").
    fn name(&self) -> &str;

    /// Whether this provider can handle the given request path.
    ///
    /// Default: always true (most providers handle all paths).
    fn can_handle_path(&self, _path: &str) -> bool {
        true
    }

    /// Build the upstream URL for the given path and query string.
    fn build_url(&self, path: &str, query: &str, account: Option<&Account>) -> String;

    /// Prepare request headers (add auth, remove dangerous headers).
    fn prepare_headers(
        &self,
        headers: &mut HeaderMap,
        access_token: Option<&str>,
        api_key: Option<&str>,
    );

    /// Extract rate-limit info from a provider response's headers.
    fn parse_rate_limit(&self, headers: &HeaderMap, status_code: u16) -> RateLimitInfo;

    /// Refresh the account's access token / API key.
    async fn refresh_token(
        &self,
        account: &Account,
        client_id: &str,
    ) -> Result<TokenRefreshResult, ProviderError>;

    /// Whether the provider supports streaming (SSE) responses.
    fn supports_streaming(&self) -> bool {
        true
    }

    /// Transform the request body before forwarding (e.g. model mapping).
    ///
    /// Default: return `None` (no transformation needed, use original body).
    async fn transform_request_body(
        &self,
        _body: &[u8],
        _account: Option<&Account>,
    ) -> Result<Option<Vec<u8>>, ProviderError> {
        Ok(None)
    }

    /// Extract usage info (tokens, cost) from a non-streaming response body.
    fn extract_usage_info(&self, _body: &[u8]) -> Option<UsageInfo> {
        None
    }

    /// Check whether a response is streaming (e.g. Content-Type: text/event-stream).
    fn is_streaming_response(&self, headers: &HeaderMap) -> bool {
        headers
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .map_or(false, |ct| ct.contains("text/event-stream"))
    }
}

/// Extended trait for OAuth-based providers (Claude OAuth, etc.).
#[async_trait]
pub trait OAuthProvider: Provider {
    /// Generate the authorization URL and PKCE verifier.
    async fn generate_auth_url(
        &self,
        state: &str,
        client_id: &str,
    ) -> Result<(String, String), ProviderError>;

    /// Exchange an authorization code for tokens.
    async fn exchange_code(
        &self,
        code: &str,
        verifier: &str,
        client_id: &str,
    ) -> Result<TokenRefreshResult, ProviderError>;
}

/// Trait for providers that support usage data fetching (background polling).
#[async_trait]
pub trait UsageFetcher: Send + Sync {
    /// Fetch usage data for the given access token.
    async fn fetch_usage(
        &self,
        access_token: &str,
        custom_endpoint: Option<&str>,
    ) -> Result<Option<serde_json::Value>, ProviderError>;
}
