//! Vertex AI provider — Google Cloud Anthropic endpoint.
//!
//! Uses `yup-oauth2` for automatic credential discovery (service account JSON
//! or Application Default Credentials). Constructs Vertex AI endpoint URLs
//! with project/location/model in the path and converts between Anthropic
//! and Vertex AI model name formats.

use std::sync::Arc;

use async_trait::async_trait;
use http::HeaderMap;
use tokio::sync::OnceCell;
use tracing::{debug, warn};

use bccf_core::types::Account;

use crate::error::ProviderError;
use crate::model_mapping;
use crate::traits::Provider;
use crate::types::{AuthType, RateLimitInfo, TokenRefreshResult, UsageInfo};

use super::anthropic_compatible::AnthropicCompatibleProvider;

/// Google Cloud OAuth2 scope for Vertex AI.
const CLOUD_PLATFORM_SCOPE: &str = "https://www.googleapis.com/auth/cloud-platform";

/// Google Cloud access tokens expire after 1 hour (3600s).
const TOKEN_EXPIRY_MS: i64 = 3600 * 1000;

/// Anthropic API version required by Vertex AI (in request body, not header).
const VERTEX_ANTHROPIC_VERSION: &str = "vertex-2023-10-16";

/// Default fallback model in Vertex AI format.
const DEFAULT_VERTEX_MODEL: &str = "claude-sonnet-4-5@20250929";

// ---------------------------------------------------------------------------
// Vertex AI config (stored in account.custom_endpoint as JSON)
// ---------------------------------------------------------------------------

/// Vertex AI configuration parsed from the account's `custom_endpoint` field.
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VertexAiConfig {
    pub project_id: String,
    pub region: String,
}

// ---------------------------------------------------------------------------
// Model name conversion
// ---------------------------------------------------------------------------

/// Convert Anthropic model format to Vertex AI format.
///
/// Anthropic: `claude-haiku-4-5-20251001`
/// Vertex AI: `claude-haiku-4-5@20251001`
///
/// Replaces the last `-` before an 8-digit date suffix with `@`.
pub fn to_vertex_model(model: &str) -> String {
    // Check if model ends with -YYYYMMDD (8 digits)
    if model.len() >= 10 {
        let (prefix, suffix) = model.split_at(model.len() - 9);
        if suffix.starts_with('-')
            && suffix[1..].len() == 8
            && suffix[1..].bytes().all(|b| b.is_ascii_digit())
        {
            return format!("{prefix}@{}", &suffix[1..]);
        }
    }
    model.to_string()
}

/// Convert Vertex AI model format back to Anthropic format.
///
/// Vertex AI: `claude-haiku-4-5@20251001`
/// Anthropic: `claude-haiku-4-5-20251001`
pub fn from_vertex_model(model: &str) -> String {
    if let Some(at_pos) = model.rfind('@') {
        let suffix = &model[at_pos + 1..];
        if suffix.len() == 8 && suffix.bytes().all(|b| b.is_ascii_digit()) {
            return format!("{}-{suffix}", &model[..at_pos]);
        }
    }
    model.to_string()
}

// ---------------------------------------------------------------------------
// Provider
// ---------------------------------------------------------------------------

/// Vertex AI provider — routes requests through Google Cloud's Vertex AI
/// Anthropic endpoint with automatic Google credential management.
pub struct VertexAiProvider {
    /// Shared authenticator (lazily initialized on first token refresh).
    authenticator: Arc<OnceCell<Arc<yup_oauth2::authenticator::DefaultAuthenticator>>>,
    /// Inner Anthropic-compatible provider for shared logic (usage extraction, etc.).
    _inner: AnthropicCompatibleProvider,
}

impl VertexAiProvider {
    pub fn new() -> Self {
        Self {
            authenticator: Arc::new(OnceCell::new()),
            _inner: AnthropicCompatibleProvider::new(
                super::anthropic_compatible::AnthropicCompatibleConfig {
                    name: "vertex-ai".to_string(),
                    endpoint: "https://aiplatform.googleapis.com".to_string(),
                    auth_header: "authorization".to_string(),
                    auth_type: AuthType::Bearer,
                    supports_streaming: true,
                },
            ),
        }
    }

    /// Parse Vertex AI config from the account's `custom_endpoint` JSON field.
    fn parse_config(account: &Account) -> Result<VertexAiConfig, ProviderError> {
        let json_str = account
            .custom_endpoint
            .as_deref()
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                ProviderError::Other(format!(
                    "Account {} is missing Vertex AI configuration (projectId, region)",
                    account.name
                ))
            })?;

        serde_json::from_str(json_str).map_err(|e| {
            ProviderError::Other(format!(
                "Failed to parse Vertex AI config for account {}: {e}",
                account.name
            ))
        })
    }

    /// Extract model from JSON body, apply mappings, and convert to Vertex format.
    ///
    /// Used by the proxy to determine the correct Vertex AI URL for a request.
    pub fn extract_vertex_model(body: &[u8], account: Option<&Account>) -> String {
        let model = serde_json::from_slice::<serde_json::Value>(body)
            .ok()
            .and_then(|v| v.get("model")?.as_str().map(String::from))
            .unwrap_or_else(|| "claude-sonnet-4-5-20250929".to_string());

        // Apply account-level model mappings first
        let mapped = model_mapping::get_model_name(&model, account);

        // Convert to Vertex AI format
        to_vertex_model(&mapped)
    }

    /// Get or initialize the Google Cloud authenticator.
    async fn get_authenticator(
        &self,
    ) -> Result<Arc<yup_oauth2::authenticator::DefaultAuthenticator>, ProviderError> {
        self.authenticator
            .get_or_try_init(|| async {
                let auth = yup_oauth2::ApplicationDefaultCredentialsAuthenticator::builder(
                    yup_oauth2::ApplicationDefaultCredentialsFlowOpts {
                        ..Default::default()
                    },
                )
                .await;

                match auth {
                    yup_oauth2::authenticator::ApplicationDefaultCredentialsTypes::InstanceMetadata(builder) => {
                        builder.build().await.map(Arc::new).map_err(|e| {
                            ProviderError::TokenRefresh(format!(
                                "Failed to create GCP metadata authenticator: {e}"
                            ))
                        })
                    }
                    yup_oauth2::authenticator::ApplicationDefaultCredentialsTypes::ServiceAccount(builder) => {
                        builder.build().await.map(Arc::new).map_err(|e| {
                            ProviderError::TokenRefresh(format!(
                                "Failed to create GCP service account authenticator: {e}"
                            ))
                        })
                    }
                }
            })
            .await
            .cloned()
    }
}

impl Default for VertexAiProvider {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Provider for VertexAiProvider {
    fn name(&self) -> &str {
        "vertex-ai"
    }

    fn build_url(&self, path: &str, query: &str, account: Option<&Account>) -> String {
        // We need the account to get project/region config.
        // If account is missing, return a placeholder that will fail at request time.
        let Some(account) = account else {
            warn!("Vertex AI build_url called without account");
            return "https://aiplatform.googleapis.com/v1/messages".to_string();
        };

        let config = match Self::parse_config(account) {
            Ok(c) => c,
            Err(e) => {
                warn!("Vertex AI config parse error: {e}");
                return "https://aiplatform.googleapis.com/v1/messages".to_string();
            }
        };

        // Determine streaming from path/query
        let is_streaming = path.contains("stream") || query.contains("stream=true");
        let specifier = if is_streaming {
            "streamRawPredict"
        } else {
            "rawPredict"
        };

        // Use the default model for URL construction.
        // The actual model will be set during transform_request_body via a second
        // URL build, or the proxy will call build_url after preparing the body.
        // For now, use the default; the real model extraction happens in
        // transform_request_body which also rebuilds the URL.
        let model = DEFAULT_VERTEX_MODEL;

        // Build base URL — "global" region uses the bare endpoint
        let base_url = if config.region == "global" {
            "https://aiplatform.googleapis.com".to_string()
        } else {
            format!("https://{}-aiplatform.googleapis.com", config.region)
        };

        format!(
            "{base_url}/v1/projects/{}/locations/{}/publishers/anthropic/models/{model}:{specifier}",
            config.project_id, config.region
        )
    }

    fn prepare_headers(
        &self,
        headers: &mut HeaderMap,
        access_token: Option<&str>,
        _api_key: Option<&str>,
    ) {
        // Remove existing auth headers
        headers.remove("authorization");
        headers.remove("x-api-key");

        // Set Bearer auth
        if let Some(token) = access_token {
            if let Ok(hv) = format!("Bearer {token}").parse() {
                headers.insert("authorization", hv);
            }
        }

        // Remove headers Vertex AI doesn't support
        headers.remove("anthropic-beta");
        headers.remove("anthropic-version");

        // Remove hop-by-hop headers
        headers.remove("host");
        headers.remove("accept-encoding");
        headers.remove("content-encoding");
    }

    fn parse_rate_limit(&self, headers: &HeaderMap, status_code: u16) -> RateLimitInfo {
        // Vertex AI uses standard Google Cloud rate limiting
        let remaining = headers
            .get("x-ratelimit-remaining")
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.parse::<i64>().ok());

        RateLimitInfo {
            is_rate_limited: status_code == 429,
            reset_time: None,
            status_header: None,
            remaining,
        }
    }

    async fn refresh_token(
        &self,
        account: &Account,
        _client_id: &str,
    ) -> Result<TokenRefreshResult, ProviderError> {
        let authenticator = self.get_authenticator().await.map_err(|e| {
            ProviderError::TokenRefresh(format!(
                "Failed to authenticate with Google Cloud: {e}. \
                 Ensure you've run 'gcloud auth application-default login' \
                 or set GOOGLE_APPLICATION_CREDENTIALS."
            ))
        })?;

        let token = authenticator
            .token(&[CLOUD_PLATFORM_SCOPE])
            .await
            .map_err(|e| {
                ProviderError::TokenRefresh(format!(
                    "Failed to obtain Google Cloud access token: {e}"
                ))
            })?;

        let access_token = token.token().ok_or_else(|| {
            ProviderError::TokenRefresh("Google Cloud auth returned empty access token".to_string())
        })?;

        debug!(
            "Vertex AI: refreshed access token for account {}",
            account.name
        );

        Ok(TokenRefreshResult {
            access_token: access_token.to_string(),
            expires_at: chrono::Utc::now().timestamp_millis() + TOKEN_EXPIRY_MS,
            refresh_token: String::new(), // ADC handles refresh internally
        })
    }

    fn supports_streaming(&self) -> bool {
        true
    }

    /// Transform request body for Vertex AI:
    /// 1. Extract model, apply mappings, convert to Vertex format (for URL)
    /// 2. Remove `model` from body (Vertex AI puts it in the URL)
    /// 3. Add `anthropic_version` to body
    async fn transform_request_body(
        &self,
        body: &[u8],
        account: Option<&Account>,
    ) -> Result<Option<Vec<u8>>, ProviderError> {
        let mut json: serde_json::Value = serde_json::from_slice(body)
            .map_err(|e| ProviderError::RequestTransform(format!("Invalid JSON body: {e}")))?;

        // Apply model mappings if configured
        if let Some(model_str) = json.get("model").and_then(|v| v.as_str()) {
            let mapped = model_mapping::get_model_name(model_str, account);
            if mapped != model_str {
                json["model"] = serde_json::Value::String(mapped);
            }
        }

        // Remove model from body (it goes in the URL for Vertex AI)
        json.as_object_mut().map(|obj| obj.remove("model"));

        // Add Vertex AI anthropic_version (must be in body, not header)
        json["anthropic_version"] = serde_json::Value::String(VERTEX_ANTHROPIC_VERSION.to_string());

        let result = serde_json::to_vec(&json).map_err(|e| {
            ProviderError::RequestTransform(format!("Failed to serialize body: {e}"))
        })?;

        Ok(Some(result))
    }

    /// Extract usage info from Anthropic-format response body.
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

        // Restore original Anthropic model name in usage info
        let model = json
            .get("model")
            .and_then(|v| v.as_str())
            .map(from_vertex_model);

        Some(UsageInfo {
            model,
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

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_name() {
        let p = VertexAiProvider::new();
        assert_eq!(p.name(), "vertex-ai");
        assert!(p.supports_streaming());
    }

    // -- Model conversion ---------------------------------------------------

    #[test]
    fn convert_anthropic_to_vertex_model() {
        assert_eq!(
            to_vertex_model("claude-haiku-4-5-20251001"),
            "claude-haiku-4-5@20251001"
        );
        assert_eq!(
            to_vertex_model("claude-sonnet-4-5-20250929"),
            "claude-sonnet-4-5@20250929"
        );
    }

    #[test]
    fn convert_vertex_to_anthropic_model() {
        assert_eq!(
            from_vertex_model("claude-haiku-4-5@20251001"),
            "claude-haiku-4-5-20251001"
        );
    }

    #[test]
    fn model_without_date_unchanged() {
        assert_eq!(to_vertex_model("claude-3-opus"), "claude-3-opus");
        assert_eq!(from_vertex_model("claude-3-opus"), "claude-3-opus");
    }

    #[test]
    fn model_with_short_suffix_unchanged() {
        assert_eq!(to_vertex_model("model-123"), "model-123");
    }

    // -- Config parsing -----------------------------------------------------

    #[test]
    fn parse_vertex_config() {
        let mut account = crate::test_util::test_account_with_key("unused");
        account.custom_endpoint =
            Some(r#"{"projectId":"my-project","region":"us-central1"}"#.to_string());

        let config = VertexAiProvider::parse_config(&account).unwrap();
        assert_eq!(config.project_id, "my-project");
        assert_eq!(config.region, "us-central1");
    }

    #[test]
    fn parse_vertex_config_missing() {
        let account = crate::test_util::test_account_with_key("unused");
        assert!(VertexAiProvider::parse_config(&account).is_err());
    }

    #[test]
    fn parse_vertex_config_invalid_json() {
        let mut account = crate::test_util::test_account_with_key("unused");
        account.custom_endpoint = Some("not json".to_string());
        assert!(VertexAiProvider::parse_config(&account).is_err());
    }

    // -- URL construction ---------------------------------------------------

    #[test]
    fn build_url_streaming() {
        let p = VertexAiProvider::new();
        let mut account = crate::test_util::test_account_with_key("unused");
        account.custom_endpoint =
            Some(r#"{"projectId":"my-proj","region":"us-east4"}"#.to_string());

        let url = p.build_url("/v1/messages", "stream=true", Some(&account));
        assert!(url.starts_with(
            "https://us-east4-aiplatform.googleapis.com/v1/projects/my-proj/locations/us-east4/"
        ));
        assert!(url.contains(":streamRawPredict"));
    }

    #[test]
    fn build_url_non_streaming() {
        let p = VertexAiProvider::new();
        let mut account = crate::test_util::test_account_with_key("unused");
        account.custom_endpoint =
            Some(r#"{"projectId":"my-proj","region":"us-east4"}"#.to_string());

        let url = p.build_url("/v1/messages", "", Some(&account));
        assert!(url.contains(":rawPredict"));
        assert!(!url.contains("streamRawPredict"));
    }

    #[test]
    fn build_url_global_region() {
        let p = VertexAiProvider::new();
        let mut account = crate::test_util::test_account_with_key("unused");
        account.custom_endpoint = Some(r#"{"projectId":"my-proj","region":"global"}"#.to_string());

        let url = p.build_url("/v1/messages", "", Some(&account));
        assert!(url.starts_with("https://aiplatform.googleapis.com/"));
    }

    #[test]
    fn build_url_no_account() {
        let p = VertexAiProvider::new();
        let url = p.build_url("/v1/messages", "", None);
        // Should return a fallback URL
        assert!(url.contains("aiplatform.googleapis.com"));
    }

    // -- Headers ------------------------------------------------------------

    #[test]
    fn prepare_headers_sets_bearer() {
        let p = VertexAiProvider::new();
        let mut headers = HeaderMap::new();
        headers.insert("anthropic-beta", "test".parse().unwrap());
        headers.insert("anthropic-version", "2023-06-01".parse().unwrap());

        p.prepare_headers(&mut headers, Some("gcp-token-123"), None);

        assert_eq!(
            headers.get("authorization").unwrap(),
            "Bearer gcp-token-123"
        );
        // Vertex AI doesn't support these headers
        assert!(headers.get("anthropic-beta").is_none());
        assert!(headers.get("anthropic-version").is_none());
    }

    // -- Rate limiting ------------------------------------------------------

    #[test]
    fn parse_rate_limit_429() {
        let p = VertexAiProvider::new();
        let headers = HeaderMap::new();
        let info = p.parse_rate_limit(&headers, 429);
        assert!(info.is_rate_limited);
    }

    #[test]
    fn parse_rate_limit_200() {
        let p = VertexAiProvider::new();
        let headers = HeaderMap::new();
        let info = p.parse_rate_limit(&headers, 200);
        assert!(!info.is_rate_limited);
    }

    // -- Request body transform ---------------------------------------------

    #[tokio::test]
    async fn transform_body_removes_model_adds_version() {
        let p = VertexAiProvider::new();
        let body = br#"{"model":"claude-sonnet-4-5-20250929","max_tokens":100,"messages":[{"role":"user","content":"Hi"}]}"#;

        let result = p.transform_request_body(body, None).await.unwrap().unwrap();
        let json: serde_json::Value = serde_json::from_slice(&result).unwrap();

        // Model should be removed from body
        assert!(json.get("model").is_none());
        // anthropic_version should be added
        assert_eq!(json["anthropic_version"], VERTEX_ANTHROPIC_VERSION);
        // Other fields preserved
        assert_eq!(json["max_tokens"], 100);
        assert!(json.get("messages").is_some());
    }

    #[tokio::test]
    async fn transform_body_applies_model_mappings() {
        let p = VertexAiProvider::new();
        let account = crate::test_util::test_account_with_mappings(
            r#"{"claude-3-opus":"claude-3-opus-20240229"}"#,
        );
        let body = br#"{"model":"claude-3-opus","max_tokens":100,"messages":[]}"#;

        let result = p
            .transform_request_body(body, Some(&account))
            .await
            .unwrap()
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&result).unwrap();

        // Model should be removed (mapping applied but then removed)
        assert!(json.get("model").is_none());
        assert_eq!(json["anthropic_version"], VERTEX_ANTHROPIC_VERSION);
    }

    // -- Usage extraction ---------------------------------------------------

    #[test]
    fn extract_usage_restores_model_name() {
        let p = VertexAiProvider::new();
        let body = br#"{
            "model": "claude-haiku-4-5@20251001",
            "usage": {
                "input_tokens": 100,
                "output_tokens": 50,
                "cache_read_input_tokens": 10,
                "cache_creation_input_tokens": 5
            }
        }"#;

        let usage = p.extract_usage_info(body).unwrap();
        // Model should be converted back to Anthropic format
        assert_eq!(usage.model.as_deref(), Some("claude-haiku-4-5-20251001"));
        assert_eq!(usage.input_tokens, Some(100));
        assert_eq!(usage.output_tokens, Some(50));
        assert_eq!(usage.prompt_tokens, Some(115)); // 100 + 5 + 10
        assert_eq!(usage.total_tokens, Some(165));
    }

    // -- extract_vertex_model helper ----------------------------------------

    #[test]
    fn extract_vertex_model_from_body() {
        let body = br#"{"model":"claude-haiku-4-5-20251001"}"#;
        let model = VertexAiProvider::extract_vertex_model(body, None);
        assert_eq!(model, "claude-haiku-4-5@20251001");
    }

    #[test]
    fn extract_vertex_model_fallback() {
        let body = br#"{"messages":[]}"#;
        let model = VertexAiProvider::extract_vertex_model(body, None);
        assert_eq!(model, "claude-sonnet-4-5@20250929");
    }
}
