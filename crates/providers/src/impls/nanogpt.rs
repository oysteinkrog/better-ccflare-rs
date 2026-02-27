//! NanoGPT provider — subscription-based Anthropic-compatible endpoint.
//!
//! Uses direct `x-api-key` auth. Supports custom endpoints and includes
//! subscription/balance usage data parsing.

use async_trait::async_trait;
use http::HeaderMap;

use bccf_core::types::Account;

use crate::error::ProviderError;
use crate::model_mapping;
use crate::traits::Provider;
use crate::types::{RateLimitInfo, TokenRefreshResult, UsageInfo};

use super::anthropic_compatible::{AnthropicCompatibleConfig, AnthropicCompatibleProvider};

/// Default NanoGPT API endpoint.
const NANOGPT_ENDPOINT: &str = "https://nano-gpt.com/api";

/// NanoGPT provider — wraps AnthropicCompatibleProvider with custom endpoint
/// support and usage data parsing.
pub struct NanoGptProvider {
    inner: AnthropicCompatibleProvider,
}

impl NanoGptProvider {
    pub fn new() -> Self {
        Self {
            inner: AnthropicCompatibleProvider::new(AnthropicCompatibleConfig {
                name: "nanogpt".to_string(),
                endpoint: NANOGPT_ENDPOINT.to_string(),
                auth_header: "x-api-key".to_string(),
                auth_type: crate::types::AuthType::Direct,
                supports_streaming: true,
            }),
        }
    }
}

impl Default for NanoGptProvider {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Provider for NanoGptProvider {
    fn name(&self) -> &str {
        "nanogpt"
    }

    fn build_url(&self, path: &str, query: &str, account: Option<&Account>) -> String {
        // NanoGPT supports custom endpoints per account
        let endpoint = if let Some(acc) = account {
            if let Some(ref custom) = acc.custom_endpoint {
                if !custom.is_empty() {
                    custom.trim_end_matches('/').to_string()
                } else {
                    NANOGPT_ENDPOINT.to_string()
                }
            } else {
                NANOGPT_ENDPOINT.to_string()
            }
        } else {
            NANOGPT_ENDPOINT.to_string()
        };

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
    ) -> Result<(), ProviderError> {
        self.inner.prepare_headers(headers, access_token, api_key)
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

    async fn transform_request_body(
        &self,
        body: &[u8],
        account: Option<&Account>,
    ) -> Result<Option<Vec<u8>>, ProviderError> {
        Ok(model_mapping::transform_body_model(body, account))
    }

    fn extract_usage_info(&self, body: &[u8]) -> Option<UsageInfo> {
        self.inner.extract_usage_info(body)
    }
}

// ---------------------------------------------------------------------------
// NanoGPT usage data
// ---------------------------------------------------------------------------

/// NanoGPT subscription usage data (from /subscription/v1/usage).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NanoGptUsageData {
    /// Whether the subscription is active (false = pay-as-you-go).
    pub active: bool,
    /// Usage limits.
    pub limits: NanoGptLimits,
    /// Whether daily limits are enforced.
    pub enforce_daily_limit: bool,
    /// Daily usage window.
    pub daily: NanoGptUsageWindow,
    /// Monthly usage window.
    pub monthly: NanoGptUsageWindow,
    /// Subscription state.
    pub state: String,
    /// Grace period end (ISO 8601), if applicable.
    pub grace_until: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct NanoGptLimits {
    pub daily: f64,
    pub monthly: f64,
}

/// A usage window (daily or monthly).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NanoGptUsageWindow {
    pub used: f64,
    pub remaining: f64,
    /// Utilization as a decimal (0.0 - 1.0).
    pub percent_used: f64,
    /// Reset timestamp (epoch ms).
    pub reset_at: i64,
}

/// Parse NanoGPT usage API response.
pub fn parse_nanogpt_usage_response(body: &[u8]) -> Option<NanoGptUsageData> {
    serde_json::from_slice(body).ok()
}

/// Get the representative utilization percentage (0-100) for NanoGPT.
///
/// Returns the maximum of daily and monthly utilization.
pub fn nanogpt_utilization(data: &NanoGptUsageData) -> Option<f64> {
    if !data.active {
        return None;
    }
    let daily_pct = data.daily.percent_used * 100.0;
    let monthly_pct = data.monthly.percent_used * 100.0;
    Some(daily_pct.max(monthly_pct))
}

/// Get the representative window name ("daily" or "monthly").
pub fn nanogpt_representative_window(data: &NanoGptUsageData) -> Option<&'static str> {
    if !data.active {
        return None;
    }
    if data.daily.percent_used >= data.monthly.percent_used {
        Some("daily")
    } else {
        Some("monthly")
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nanogpt_provider_name() {
        let p = NanoGptProvider::new();
        assert_eq!(p.name(), "nanogpt");
    }

    #[test]
    fn nanogpt_endpoint() {
        let p = NanoGptProvider::new();
        assert_eq!(
            p.build_url("/v1/messages", "", None),
            "https://nano-gpt.com/api/v1/messages"
        );
    }

    #[test]
    fn nanogpt_custom_endpoint() {
        let p = NanoGptProvider::new();
        let mut account = crate::test_util::test_account_with_key("ng-key");
        account.custom_endpoint = Some("https://custom.nanogpt.com/api".to_string());

        let url = p.build_url("/v1/messages", "", Some(&account));
        assert_eq!(url, "https://custom.nanogpt.com/api/v1/messages");
    }

    #[tokio::test]
    async fn nanogpt_refresh_token() {
        let p = NanoGptProvider::new();
        let account = crate::test_util::test_account_with_key("ng-key-123");
        let result = p.refresh_token(&account, "client").await.unwrap();
        assert_eq!(result.access_token, "ng-key-123");
    }

    #[test]
    fn parse_nanogpt_usage() {
        let body = br#"{
            "active": true,
            "limits": {"daily": 1000, "monthly": 30000},
            "enforceDailyLimit": true,
            "daily": {"used": 100, "remaining": 900, "percentUsed": 0.1, "resetAt": 1700000000000},
            "monthly": {"used": 3000, "remaining": 27000, "percentUsed": 0.1, "resetAt": 1702000000000},
            "state": "active",
            "graceUntil": null
        }"#;

        let data = parse_nanogpt_usage_response(body).unwrap();
        assert!(data.active);
        assert_eq!(data.daily.used, 100.0);
        assert_eq!(data.monthly.remaining, 27000.0);
    }

    #[test]
    fn nanogpt_utilization_active() {
        let data = NanoGptUsageData {
            active: true,
            limits: NanoGptLimits {
                daily: 1000.0,
                monthly: 30000.0,
            },
            enforce_daily_limit: true,
            daily: NanoGptUsageWindow {
                used: 500.0,
                remaining: 500.0,
                percent_used: 0.5,
                reset_at: 0,
            },
            monthly: NanoGptUsageWindow {
                used: 3000.0,
                remaining: 27000.0,
                percent_used: 0.1,
                reset_at: 0,
            },
            state: "active".to_string(),
            grace_until: None,
        };

        assert_eq!(nanogpt_utilization(&data), Some(50.0)); // daily 50% > monthly 10%
        assert_eq!(nanogpt_representative_window(&data), Some("daily"));
    }

    #[test]
    fn nanogpt_utilization_inactive() {
        let data = NanoGptUsageData {
            active: false,
            limits: NanoGptLimits {
                daily: 0.0,
                monthly: 0.0,
            },
            enforce_daily_limit: false,
            daily: NanoGptUsageWindow {
                used: 0.0,
                remaining: 0.0,
                percent_used: 0.0,
                reset_at: 0,
            },
            monthly: NanoGptUsageWindow {
                used: 0.0,
                remaining: 0.0,
                percent_used: 0.0,
                reset_at: 0,
            },
            state: "inactive".to_string(),
            grace_until: None,
        };

        assert_eq!(nanogpt_utilization(&data), None);
        assert_eq!(nanogpt_representative_window(&data), None);
    }
}
