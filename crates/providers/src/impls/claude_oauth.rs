//! Claude OAuth provider — PKCE auth flow, token refresh, 5-hour session windows.
//!
//! Supports two modes:
//! - **claude-oauth**: Full OAuth flow with refresh tokens and session tracking
//! - **console**: API key-based authentication via `/api/oauth/claude_cli/create_api_key`

use async_trait::async_trait;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use http::HeaderMap;
use tracing::debug;

use bccf_core::types::Account;

use crate::error::ProviderError;
use crate::model_mapping;
use crate::pkce;
use crate::traits::{OAuthProvider, Provider};
use crate::types::{RateLimitInfo, TokenRefreshResult, UsageInfo};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// OAuth token endpoint (always console.anthropic.com).
const TOKEN_URL: &str = "https://console.anthropic.com/v1/oauth/token";

/// OAuth redirect URI (Anthropic's registered callback — may redirect to platform.claude.com).
const REDIRECT_URI: &str = "https://console.anthropic.com/oauth/code/callback";

/// Default API endpoint.
const DEFAULT_ENDPOINT: &str = "https://api.anthropic.com";

/// Scopes requested for Claude OAuth.
const SCOPES: &str = "org:create_api_key user:profile user:inference";

/// Required beta header for OAuth access tokens.
const OAUTH_BETA_HEADER: &str = "oauth-2025-04-20";

/// CSRF state replay protection window (10 minutes — allows time for manual login).
const STATE_MAX_AGE_MS: i64 = 600_000;

/// Rate limit header names (unified Anthropic format).
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

// ---------------------------------------------------------------------------
// CSRF State Token
// ---------------------------------------------------------------------------

/// CSRF state token for OAuth flow replay protection.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CsrfState {
    #[serde(rename = "csrfToken")]
    pub csrf_token: String,
    pub timestamp: i64,
}

impl CsrfState {
    /// Generate a new CSRF state token.
    pub fn generate() -> Self {
        use rand::Rng;
        let mut bytes = [0u8; 32];
        rand::rng().fill(&mut bytes);
        let csrf_token = hex::encode(&bytes);
        Self {
            csrf_token,
            timestamp: chrono::Utc::now().timestamp_millis(),
        }
    }

    /// Encode to base64url JSON string for use as OAuth state parameter.
    pub fn encode(&self) -> Result<String, ProviderError> {
        let json = serde_json::to_string(self).map_err(ProviderError::Json)?;
        Ok(URL_SAFE_NO_PAD.encode(json.as_bytes()))
    }

    /// Decode from base64url JSON string.
    pub fn decode(encoded: &str) -> Result<Self, ProviderError> {
        let bytes = URL_SAFE_NO_PAD
            .decode(encoded)
            .map_err(|e| ProviderError::Other(format!("Invalid base64url state: {e}")))?;
        let json = String::from_utf8(bytes)
            .map_err(|e| ProviderError::Other(format!("Invalid UTF-8 state: {e}")))?;
        serde_json::from_str(&json).map_err(ProviderError::Json)
    }

    /// Validate the state token (check timestamp is within window).
    pub fn validate(&self) -> Result<(), ProviderError> {
        let now = chrono::Utc::now().timestamp_millis();
        let age = now - self.timestamp;
        if age > STATE_MAX_AGE_MS {
            return Err(ProviderError::Other(format!(
                "CSRF state expired ({age}ms > {STATE_MAX_AGE_MS}ms)"
            )));
        }
        if age < 0 {
            return Err(ProviderError::Other(
                "CSRF state timestamp is in the future".into(),
            ));
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Provider
// ---------------------------------------------------------------------------

/// Claude OAuth provider mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClaudeOAuthMode {
    /// Full OAuth with refresh tokens (claude.ai login).
    ClaudeOAuth,
    /// Console mode (console.anthropic.com).
    Console,
}

/// The Claude OAuth provider.
pub struct ClaudeOAuthProvider {
    mode: ClaudeOAuthMode,
    http_client: reqwest::Client,
}

impl ClaudeOAuthProvider {
    pub fn new(mode: ClaudeOAuthMode) -> Self {
        Self {
            mode,
            http_client: reqwest::Client::new(),
        }
    }

    /// Create a claude-oauth mode provider.
    pub fn claude_oauth() -> Self {
        Self::new(ClaudeOAuthMode::ClaudeOAuth)
    }

    /// Create a console mode provider.
    pub fn console() -> Self {
        Self::new(ClaudeOAuthMode::Console)
    }

    /// Whether this account uses an API key (console mode) vs OAuth refresh.
    fn is_api_key_account(account: &Account) -> bool {
        account.api_key.is_some()
    }

    /// Get the effective endpoint for a specific account.
    fn effective_endpoint(account: Option<&Account>) -> String {
        if let Some(acc) = account {
            if let Some(ref custom) = acc.custom_endpoint {
                if !custom.is_empty() {
                    return custom.clone();
                }
            }
        }
        DEFAULT_ENDPOINT.to_string()
    }

    /// Build the authorization URL for the OAuth flow.
    fn build_authorize_url(&self, state: &str, client_id: &str, challenge: &str) -> String {
        let params = format!(
            "code=true&client_id={client_id}&response_type=code&\
             redirect_uri={REDIRECT_URI}&scope={SCOPES}&\
             code_challenge={challenge}&code_challenge_method=S256&state={state}"
        );

        match self.mode {
            ClaudeOAuthMode::ClaudeOAuth => {
                let return_to = format!("/oauth/authorize?{params}");
                let encoded = percent_encode(&return_to);
                format!(
                    "https://claude.ai/login?selectAccount=true&returnTo={encoded}"
                )
            }
            ClaudeOAuthMode::Console => {
                format!("https://console.anthropic.com/oauth/authorize?{params}")
            }
        }
    }

    /// Exchange an authorization code for tokens via the token endpoint.
    async fn token_exchange(
        &self,
        code: &str,
        state: &str,
        verifier: &str,
        client_id: &str,
    ) -> Result<TokenRefreshResult, ProviderError> {
        // Anthropic embeds state in the code as "code#state" — extract actual code
        let actual_code = code.split('#').next().unwrap_or(code);

        let body = serde_json::json!({
            "code": actual_code,
            "state": state,
            "grant_type": "authorization_code",
            "client_id": client_id,
            "redirect_uri": REDIRECT_URI,
            "code_verifier": verifier,
        });

        debug!(
            code_len = actual_code.len(),
            client_id = %client_id,
            redirect_uri = REDIRECT_URI,
            verifier_len = verifier.len(),
            "Token exchange request"
        );

        let resp = self
            .http_client
            .post(TOKEN_URL)
            .json(&body)
            .send()
            .await
            .map_err(ProviderError::Http)?;

        let status = resp.status();
        let text = resp.text().await.map_err(ProviderError::Http)?;

        if !status.is_success() {
            return Err(ProviderError::TokenRefresh(format!(
                "Token exchange failed ({}): {text}",
                status.as_u16()
            )));
        }

        Self::parse_token_response(&text)
    }

    /// Refresh access token using a refresh token.
    async fn do_refresh(
        &self,
        refresh_token: &str,
        client_id: &str,
    ) -> Result<TokenRefreshResult, ProviderError> {
        let body = serde_json::json!({
            "grant_type": "refresh_token",
            "refresh_token": refresh_token,
            "client_id": client_id,
        });

        let resp = self
            .http_client
            .post(TOKEN_URL)
            .json(&body)
            .send()
            .await
            .map_err(ProviderError::Http)?;

        let status = resp.status();
        let text = resp.text().await.map_err(ProviderError::Http)?;

        if !status.is_success() {
            // Check for known error types
            if let Ok(err_json) = serde_json::from_str::<serde_json::Value>(&text) {
                let err_type = err_json["error"].as_str().unwrap_or_default();
                if err_type == "invalid_grant" || err_type == "invalid_refresh_token" {
                    return Err(ProviderError::TokenRefresh(
                        "Refresh token revoked — account needs re-authentication".into(),
                    ));
                }
            }
            return Err(ProviderError::TokenRefresh(format!(
                "Token refresh failed ({}): {text}",
                status.as_u16()
            )));
        }

        Self::parse_token_response(&text)
    }

    /// Parse the token endpoint JSON response.
    fn parse_token_response(text: &str) -> Result<TokenRefreshResult, ProviderError> {
        let json: serde_json::Value = serde_json::from_str(text).map_err(ProviderError::Json)?;

        let access_token = json["access_token"]
            .as_str()
            .ok_or_else(|| ProviderError::TokenRefresh("Missing access_token".into()))?
            .to_string();

        let expires_in = json["expires_in"].as_i64().unwrap_or(3600);
        let expires_at = chrono::Utc::now().timestamp_millis() + expires_in * 1000;

        // Preserve existing refresh token if response doesn't include new one
        let refresh_token = json["refresh_token"]
            .as_str()
            .unwrap_or_default()
            .to_string();

        Ok(TokenRefreshResult {
            access_token,
            expires_at,
            refresh_token,
        })
    }
}

#[async_trait]
impl Provider for ClaudeOAuthProvider {
    fn name(&self) -> &str {
        match self.mode {
            ClaudeOAuthMode::ClaudeOAuth => "claude-oauth",
            ClaudeOAuthMode::Console => "console",
        }
    }

    fn build_url(&self, path: &str, query: &str, account: Option<&Account>) -> String {
        let endpoint = Self::effective_endpoint(account);
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
        // Remove client auth headers to prevent leakage
        headers.remove("authorization");
        headers.remove("x-api-key");

        if let Some(token) = access_token {
            // OAuth access token → Bearer auth + beta header
            if let Ok(val) = format!("Bearer {token}").parse() {
                headers.insert("authorization", val);
            }
            if let Ok(val) = OAUTH_BETA_HEADER.parse() {
                headers.insert("anthropic-beta", val);
            }
        } else if let Some(key) = api_key {
            // Console API key → x-api-key header
            if let Ok(val) = key.parse() {
                headers.insert("x-api-key", val);
            }
        }

        // Remove hop-by-hop headers
        headers.remove("host");
        headers.remove("accept-encoding");
        headers.remove("content-encoding");
    }

    fn parse_rate_limit(&self, headers: &HeaderMap, status_code: u16) -> RateLimitInfo {
        let status_header = headers
            .get(HEADER_UNIFIED_STATUS)
            .and_then(|v| v.to_str().ok())
            .map(String::from);

        let is_rate_limited = if let Some(ref status) = status_header {
            status != "allowed" && HARD_LIMIT_STATUSES.contains(&status.as_str())
        } else {
            status_code == 429
        };

        let reset_time = headers
            .get(HEADER_UNIFIED_RESET)
            .and_then(|v| v.to_str().ok())
            .and_then(|s| {
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
        client_id: &str,
    ) -> Result<TokenRefreshResult, ProviderError> {
        // Console (API key) mode — no refresh needed
        if Self::is_api_key_account(account) {
            let key = account.api_key.as_ref().unwrap();
            debug!("Console account — returning API key as access token");
            return Ok(TokenRefreshResult {
                access_token: key.clone(),
                // API keys don't expire; set 24h to avoid repeated checks
                expires_at: chrono::Utc::now().timestamp_millis() + 24 * 60 * 60 * 1000,
                refresh_token: String::new(),
            });
        }

        // OAuth mode — refresh using refresh_token
        if account.refresh_token.is_empty() {
            return Err(ProviderError::TokenRefresh(
                "No refresh token available".into(),
            ));
        }

        debug!(account_id = %account.id, "Refreshing OAuth access token");
        let mut result = self.do_refresh(&account.refresh_token, client_id).await?;

        // Preserve existing refresh token if the response didn't include a new one
        if result.refresh_token.is_empty() {
            result.refresh_token = account.refresh_token.clone();
        }

        Ok(result)
    }

    fn supports_streaming(&self) -> bool {
        true
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
            cost_usd: None,
            input_tokens,
            cache_read_input_tokens: cache_read,
            cache_creation_input_tokens: cache_creation,
            output_tokens,
        })
    }
}

#[async_trait]
impl OAuthProvider for ClaudeOAuthProvider {
    async fn generate_auth_url(
        &self,
        state: &str,
        client_id: &str,
    ) -> Result<(String, String), ProviderError> {
        let pkce = pkce::generate();
        let url = self.build_authorize_url(state, client_id, &pkce.challenge);
        Ok((url, pkce.verifier))
    }

    async fn exchange_code(
        &self,
        code: &str,
        state: &str,
        verifier: &str,
        client_id: &str,
    ) -> Result<TokenRefreshResult, ProviderError> {
        self.token_exchange(code, state, verifier, client_id).await
    }
}

// ---------------------------------------------------------------------------
// Percent-encoding helper (avoids adding `urlencoding` crate dependency)
// ---------------------------------------------------------------------------

/// Percent-encode a string for use in a URL query parameter value.
fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 3);
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            _ => {
                out.push('%');
                out.push(hex::HEX_CHARS[(b >> 4) as usize] as char);
                out.push(hex::HEX_CHARS[(b & 0xf) as usize] as char);
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Hex encoding helper (avoids adding `hex` crate dependency)
// ---------------------------------------------------------------------------

mod hex {
    pub(super) const HEX_CHARS: &[u8; 16] = b"0123456789abcdef";

    pub fn encode(bytes: &[u8]) -> String {
        let mut s = String::with_capacity(bytes.len() * 2);
        for &b in bytes {
            s.push(HEX_CHARS[(b >> 4) as usize] as char);
            s.push(HEX_CHARS[(b & 0xf) as usize] as char);
        }
        s
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_name_claude_oauth() {
        let p = ClaudeOAuthProvider::claude_oauth();
        assert_eq!(p.name(), "claude-oauth");
    }

    #[test]
    fn provider_name_console() {
        let p = ClaudeOAuthProvider::console();
        assert_eq!(p.name(), "console");
    }

    #[test]
    fn build_url_default_endpoint() {
        let p = ClaudeOAuthProvider::claude_oauth();
        let url = p.build_url("/v1/messages", "", None);
        assert_eq!(url, "https://api.anthropic.com/v1/messages");
    }

    #[test]
    fn build_url_with_query() {
        let p = ClaudeOAuthProvider::claude_oauth();
        let url = p.build_url("/v1/messages", "stream=true", None);
        assert_eq!(url, "https://api.anthropic.com/v1/messages?stream=true");
    }

    #[test]
    fn build_url_custom_endpoint() {
        let p = ClaudeOAuthProvider::claude_oauth();
        let mut account = crate::test_util::test_account_with_key("sk-test");
        account.custom_endpoint = Some("https://custom.api.com".into());

        let url = p.build_url("/v1/messages", "", Some(&account));
        assert_eq!(url, "https://custom.api.com/v1/messages");
    }

    #[test]
    fn prepare_headers_oauth_token() {
        let p = ClaudeOAuthProvider::claude_oauth();
        let mut headers = HeaderMap::new();
        headers.insert("authorization", "old-value".parse().unwrap());
        headers.insert("host", "example.com".parse().unwrap());

        p.prepare_headers(&mut headers, Some("access-token-123"), None);

        assert_eq!(
            headers.get("authorization").unwrap(),
            "Bearer access-token-123"
        );
        assert_eq!(headers.get("anthropic-beta").unwrap(), OAUTH_BETA_HEADER);
        assert!(headers.get("host").is_none());
    }

    #[test]
    fn prepare_headers_api_key() {
        let p = ClaudeOAuthProvider::console();
        let mut headers = HeaderMap::new();

        p.prepare_headers(&mut headers, None, Some("sk-key-123"));

        assert_eq!(headers.get("x-api-key").unwrap(), "sk-key-123");
        assert!(headers.get("authorization").is_none());
        assert!(headers.get("anthropic-beta").is_none());
    }

    #[test]
    fn parse_rate_limit_allowed() {
        let p = ClaudeOAuthProvider::claude_oauth();
        let mut headers = HeaderMap::new();
        headers.insert(HEADER_UNIFIED_STATUS, "allowed".parse().unwrap());
        headers.insert(HEADER_UNIFIED_REMAINING, "50".parse().unwrap());

        let info = p.parse_rate_limit(&headers, 200);
        assert!(!info.is_rate_limited);
        assert_eq!(info.remaining, Some(50));
    }

    #[test]
    fn parse_rate_limit_hard_limited() {
        let p = ClaudeOAuthProvider::claude_oauth();
        let mut headers = HeaderMap::new();
        headers.insert(HEADER_UNIFIED_STATUS, "rate_limited".parse().unwrap());
        headers.insert(HEADER_UNIFIED_RESET, "1700000060000".parse().unwrap());

        let info = p.parse_rate_limit(&headers, 429);
        assert!(info.is_rate_limited);
        assert_eq!(info.reset_time, Some(1700000060000));
    }

    #[test]
    fn parse_rate_limit_429_fallback() {
        let p = ClaudeOAuthProvider::claude_oauth();
        let headers = HeaderMap::new();
        let info = p.parse_rate_limit(&headers, 429);
        assert!(info.is_rate_limited);
    }

    #[test]
    fn parse_rate_limit_soft_warning_not_blocked() {
        let p = ClaudeOAuthProvider::claude_oauth();
        let mut headers = HeaderMap::new();
        headers.insert(HEADER_UNIFIED_STATUS, "allowed_warning".parse().unwrap());

        let info = p.parse_rate_limit(&headers, 200);
        assert!(!info.is_rate_limited);
    }

    #[test]
    fn extract_usage_from_response() {
        let p = ClaudeOAuthProvider::claude_oauth();
        let body = br#"{
            "model": "claude-sonnet-4-5-20250929",
            "usage": {
                "input_tokens": 100,
                "output_tokens": 50,
                "cache_read_input_tokens": 10,
                "cache_creation_input_tokens": 5
            }
        }"#;

        let usage = p.extract_usage_info(body).unwrap();
        assert_eq!(usage.model.as_deref(), Some("claude-sonnet-4-5-20250929"));
        assert_eq!(usage.input_tokens, Some(100));
        assert_eq!(usage.output_tokens, Some(50));
        assert_eq!(usage.prompt_tokens, Some(115)); // 100 + 5 + 10
        assert_eq!(usage.total_tokens, Some(165));
    }

    #[test]
    fn authorize_url_claude_oauth_mode() {
        let p = ClaudeOAuthProvider::claude_oauth();
        let url = p.build_authorize_url("test-state", "test-client", "test-challenge");
        assert!(url.starts_with("https://claude.ai/login?selectAccount=true&returnTo="));
        // returnTo is percent-encoded, so params appear encoded (lowercase hex)
        assert!(url.contains("client_id%3dtest-client"), "url: {url}");
        assert!(url.contains("code_challenge%3dtest-challenge"), "url: {url}");
        assert!(url.contains("state%3dtest-state"), "url: {url}");
        // Only two top-level query params: selectAccount and returnTo
        let query = url.split('?').nth(1).unwrap();
        let top_params: Vec<&str> = query.split('&').collect();
        assert_eq!(top_params.len(), 2, "Should only have selectAccount and returnTo as top-level params, got: {top_params:?}");
    }

    #[test]
    fn authorize_url_console_mode() {
        let p = ClaudeOAuthProvider::console();
        let url = p.build_authorize_url("test-state", "test-client", "test-challenge");
        assert!(url.starts_with("https://console.anthropic.com/oauth/authorize?"));
        assert!(url.contains("client_id=test-client"));
    }

    #[test]
    fn csrf_state_roundtrip() {
        let state = CsrfState::generate();
        assert_eq!(state.csrf_token.len(), 64); // 32 bytes → 64 hex chars

        let encoded = state.encode().unwrap();
        let decoded = CsrfState::decode(&encoded).unwrap();
        assert_eq!(decoded.csrf_token, state.csrf_token);
        assert_eq!(decoded.timestamp, state.timestamp);
    }

    #[test]
    fn csrf_state_validation() {
        let state = CsrfState::generate();
        assert!(state.validate().is_ok());

        // Expired state
        let expired = CsrfState {
            csrf_token: "test".into(),
            timestamp: chrono::Utc::now().timestamp_millis() - STATE_MAX_AGE_MS - 1000,
        };
        assert!(expired.validate().is_err());

        // Future state
        let future = CsrfState {
            csrf_token: "test".into(),
            timestamp: chrono::Utc::now().timestamp_millis() + 1000,
        };
        assert!(future.validate().is_err());
    }

    #[test]
    fn parse_token_response_success() {
        let json = r#"{"access_token":"at-123","expires_in":3600,"refresh_token":"rt-456"}"#;
        let result = ClaudeOAuthProvider::parse_token_response(json).unwrap();
        assert_eq!(result.access_token, "at-123");
        assert_eq!(result.refresh_token, "rt-456");
        assert!(result.expires_at > chrono::Utc::now().timestamp_millis());
    }

    #[test]
    fn parse_token_response_no_refresh() {
        let json = r#"{"access_token":"at-123","expires_in":3600}"#;
        let result = ClaudeOAuthProvider::parse_token_response(json).unwrap();
        assert_eq!(result.access_token, "at-123");
        assert!(result.refresh_token.is_empty());
    }

    #[test]
    fn parse_token_response_missing_access_token() {
        let json = r#"{"error":"invalid_grant"}"#;
        assert!(ClaudeOAuthProvider::parse_token_response(json).is_err());
    }

    #[test]
    fn code_hash_state_extraction() {
        // Anthropic embeds state in code as "code#state"
        let code_with_state = "abc123#state456";
        let actual_code = code_with_state.split('#').next().unwrap();
        assert_eq!(actual_code, "abc123");
    }

    #[test]
    fn code_without_hash_passes_through() {
        let code = "abc123";
        let actual_code = code.split('#').next().unwrap();
        assert_eq!(actual_code, "abc123");
    }

    #[tokio::test]
    async fn refresh_token_api_key_account() {
        let p = ClaudeOAuthProvider::console();
        let account = crate::test_util::test_account_with_key("sk-console-key");
        let result = p.refresh_token(&account, "client-id").await.unwrap();
        assert_eq!(result.access_token, "sk-console-key");
    }

    #[tokio::test]
    async fn refresh_token_no_refresh_token_fails() {
        let p = ClaudeOAuthProvider::claude_oauth();
        let mut account = crate::test_util::test_account_with_key("sk-test");
        account.api_key = None; // Not an API key account
        account.refresh_token = String::new(); // No refresh token

        let result = p.refresh_token(&account, "client-id").await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn generate_auth_url_returns_url_and_verifier() {
        let p = ClaudeOAuthProvider::claude_oauth();
        let (url, verifier) = p
            .generate_auth_url("test-state", "test-client")
            .await
            .unwrap();

        assert!(url.contains("test-client"));
        assert!(url.contains("test-state"));
        assert!(!verifier.is_empty());
        // Verifier should be base64url (43 chars for 32 bytes)
        assert_eq!(verifier.len(), 43);
    }

    #[test]
    fn hex_encode_works() {
        assert_eq!(hex::encode(&[0xff, 0x00, 0xab]), "ff00ab");
        assert_eq!(hex::encode(&[]), "");
    }

    #[test]
    fn is_api_key_account_detection() {
        let with_key = crate::test_util::test_account_with_key("sk-test");
        assert!(ClaudeOAuthProvider::is_api_key_account(&with_key));

        let mut without_key = crate::test_util::test_account_with_key("sk-test");
        without_key.api_key = None;
        assert!(!ClaudeOAuthProvider::is_api_key_account(&without_key));
    }
}
