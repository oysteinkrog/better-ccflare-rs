//! Zai (Zhipu AI) provider — Anthropic-compatible endpoint at api.z.ai.
//!
//! Uses direct `x-api-key` auth. Includes rate limit parsing with timezone-
//! aware reset time extraction from error messages.

use async_trait::async_trait;
use http::HeaderMap;

use bccf_core::types::Account;

use crate::error::ProviderError;
use crate::model_mapping;
use crate::traits::Provider;
use crate::types::{AuthType, RateLimitInfo, TokenRefreshResult, UsageInfo};

use super::anthropic_compatible::{AnthropicCompatibleConfig, AnthropicCompatibleProvider};

/// Zai API endpoint.
const ZAI_ENDPOINT: &str = "https://api.z.ai/api/anthropic";

/// Prefix to find reset time in Zai error messages.
/// Format: "reset at 2025-10-03 08:23:14" (Singapore time, UTC+8).
#[allow(dead_code)]
const RESET_PREFIX: &str = "reset at ";

// Note: parse_reset_from_message is used by the proxy layer to extract
// rate limit reset times from Zai 429 response bodies.

/// Zai provider — wraps AnthropicCompatibleProvider with Zai-specific rate
/// limit parsing.
pub struct ZaiProvider {
    inner: AnthropicCompatibleProvider,
}

impl ZaiProvider {
    pub fn new() -> Self {
        Self {
            inner: AnthropicCompatibleProvider::new(AnthropicCompatibleConfig {
                name: "zai".to_string(),
                endpoint: ZAI_ENDPOINT.to_string(),
                auth_header: "x-api-key".to_string(),
                auth_type: AuthType::Direct,
                supports_streaming: true,
            }),
        }
    }

    /// Parse a Zai error message for the reset time.
    ///
    /// Zai error format: "Usage limit reached for 5 hour. Your limit will
    /// reset at 2025-10-03 08:23:14"
    ///
    /// The timestamp is in Singapore time (UTC+8).
    #[allow(dead_code)]
    pub fn parse_reset_from_message(message: &str) -> Option<i64> {
        let idx = message.find(RESET_PREFIX)?;
        let timestamp_str = &message[idx + RESET_PREFIX.len()..];
        // Expected: "2025-10-03 08:23:14" (exactly 19 chars)
        if timestamp_str.len() < 19 {
            return None;
        }
        let ts = &timestamp_str[..19];

        // Parse as naive datetime, then convert from UTC+8 to UTC
        let dt = chrono::NaiveDateTime::parse_from_str(ts, "%Y-%m-%d %H:%M:%S").ok()?;
        // Singapore = UTC+8, so subtract 8 hours to get UTC
        let utc_ms = dt.and_utc().timestamp_millis() - 8 * 60 * 60 * 1000;
        Some(utc_ms)
    }
}

impl Default for ZaiProvider {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Provider for ZaiProvider {
    fn name(&self) -> &str {
        "zai"
    }

    fn build_url(&self, path: &str, query: &str, account: Option<&Account>) -> String {
        self.inner.build_url(path, query, account)
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
        if status_code != 429 {
            return self.inner.parse_rate_limit(headers, status_code);
        }

        // For 429, try to extract reset time from Retry-After header
        let retry_after = headers
            .get("retry-after")
            .and_then(|v| v.to_str().ok())
            .and_then(|s| {
                // Try seconds first
                s.parse::<i64>()
                    .ok()
                    .map(|secs| chrono::Utc::now().timestamp_millis() + secs * 1000)
            });

        RateLimitInfo {
            is_rate_limited: true,
            reset_time: retry_after,
            status_header: Some("rate_limited".to_string()),
            remaining: Some(0),
        }
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
// Zai usage fetcher
// ---------------------------------------------------------------------------

/// Zai usage data (from /api/monitor/usage/quota/limit).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ZaiUsageData {
    pub time_limit: Option<ZaiUsageWindow>,
    pub tokens_limit: Option<ZaiUsageWindow>,
}

/// A single usage window.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ZaiUsageWindow {
    pub used: f64,
    pub remaining: f64,
    pub percentage: f64,
    pub reset_at: Option<i64>,
    #[serde(rename = "type")]
    pub limit_type: String,
}

/// Parse Zai usage API response into structured data.
pub fn parse_zai_usage_response(body: &[u8]) -> Option<ZaiUsageData> {
    let json: serde_json::Value = serde_json::from_slice(body).ok()?;

    if json.get("success")?.as_bool() != Some(true) {
        return None;
    }

    let limits = json.get("data")?.get("limits")?.as_array()?;

    let mut data = ZaiUsageData {
        time_limit: None,
        tokens_limit: None,
    };

    for limit in limits {
        let Some(limit_type) = limit.get("type").and_then(|v| v.as_str()) else {
            continue;
        };
        let current_value = limit
            .get("currentValue")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0);
        let remaining = limit
            .get("remaining")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0);
        let percentage = limit
            .get("percentage")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0);
        let reset_at = limit.get("nextResetTime").and_then(|v| v.as_i64());

        let window = ZaiUsageWindow {
            used: current_value,
            remaining,
            percentage,
            reset_at,
            limit_type: limit_type.to_string(),
        };

        match limit_type {
            "TIME_LIMIT" => data.time_limit = Some(window),
            "TOKENS_LIMIT" => data.tokens_limit = Some(window),
            _ => {}
        }
    }

    Some(data)
}

/// Get the representative utilization percentage for Zai (0-100).
/// Only considers the 5-hour token quota (not time limit).
pub fn zai_utilization(data: &ZaiUsageData) -> Option<f64> {
    data.tokens_limit.as_ref().map(|w| w.percentage)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zai_provider_name() {
        let p = ZaiProvider::new();
        assert_eq!(p.name(), "zai");
    }

    #[test]
    fn zai_endpoint() {
        let p = ZaiProvider::new();
        assert_eq!(
            p.build_url("/v1/messages", "", None),
            "https://api.z.ai/api/anthropic/v1/messages"
        );
    }

    #[test]
    fn parse_reset_time_from_message() {
        let msg = "Usage limit reached for 5 hour. Your limit will reset at 2025-10-03 08:23:14";
        let reset = ZaiProvider::parse_reset_from_message(msg).unwrap();
        // 2025-10-03 08:23:14 SGT = 2025-10-03 00:23:14 UTC = 1759450994000 ms
        let expected = 1_759_450_994_000_i64;
        assert_eq!(reset, expected);
    }

    #[test]
    fn parse_reset_time_missing() {
        let msg = "Some other error message";
        assert!(ZaiProvider::parse_reset_from_message(msg).is_none());
    }

    #[test]
    fn parse_rate_limit_429() {
        let p = ZaiProvider::new();
        let headers = HeaderMap::new();
        let info = p.parse_rate_limit(&headers, 429);
        assert!(info.is_rate_limited);
    }

    #[test]
    fn parse_rate_limit_200() {
        let p = ZaiProvider::new();
        let headers = HeaderMap::new();
        let info = p.parse_rate_limit(&headers, 200);
        assert!(!info.is_rate_limited);
    }

    #[tokio::test]
    async fn zai_refresh_token() {
        let p = ZaiProvider::new();
        let account = crate::test_util::test_account_with_key("zai-key-123");
        let result = p.refresh_token(&account, "client").await.unwrap();
        assert_eq!(result.access_token, "zai-key-123");
    }

    #[test]
    fn parse_zai_usage() {
        let body = br#"{
            "success": true,
            "data": {
                "limits": [
                    {
                        "type": "TIME_LIMIT",
                        "currentValue": 30,
                        "remaining": 270,
                        "percentage": 10.0,
                        "nextResetTime": 1700000000000
                    },
                    {
                        "type": "TOKENS_LIMIT",
                        "currentValue": 5000,
                        "remaining": 45000,
                        "percentage": 10.0,
                        "nextResetTime": 1700000000000
                    }
                ]
            }
        }"#;

        let data = parse_zai_usage_response(body).unwrap();
        assert!(data.time_limit.is_some());
        assert!(data.tokens_limit.is_some());
        assert_eq!(data.tokens_limit.as_ref().unwrap().percentage, 10.0);
    }

    #[test]
    fn zai_utilization_value() {
        let data = ZaiUsageData {
            time_limit: None,
            tokens_limit: Some(ZaiUsageWindow {
                used: 5000.0,
                remaining: 45000.0,
                percentage: 10.0,
                reset_at: None,
                limit_type: "TOKENS_LIMIT".to_string(),
            }),
        };
        assert_eq!(zai_utilization(&data), Some(10.0));
    }

    #[test]
    fn zai_utilization_none() {
        let data = ZaiUsageData {
            time_limit: None,
            tokens_limit: None,
        };
        assert_eq!(zai_utilization(&data), None);
    }
}
