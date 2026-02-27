//! Proxy core handler — the hot path for proxying API requests.
//!
//! Buffers the request body once, tries accounts in load-balancer order,
//! handles rate limits and auth failures with failover, and streams
//! responses back to clients.

use std::fmt::Write;
use std::sync::atomic::{AtomicU64, Ordering};

use axum::http::{HeaderMap, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use bytes::Bytes;
use tracing::{debug, error, info, warn};

use bccf_core::types::Account;
use bccf_providers::types::RateLimitInfo;

/// Global request counter for lightweight request IDs.
static REQUEST_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Generate a lightweight request ID (timestamp-counter format).
/// Much cheaper than UUID: no entropy source, no formatting of 128-bit random.
fn generate_request_id(now: i64) -> String {
    let counter = REQUEST_COUNTER.fetch_add(1, Ordering::Relaxed);
    let mut id = String::with_capacity(30);
    let _ = write!(id, "req-{now}-{counter}");
    id
}

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Maximum request body size stored for analytics (256 KB).
pub const ANALYTICS_BODY_CAP: usize = 256 * 1024;

/// Thinking block signature error messages from Claude.
const INVALID_THINKING_SIGNATURE: &str = "Invalid `signature` in `thinking` block";
const THINKING_BLOCK_REQUIRED: &str = "final `assistant` message must start with a thinking block";

// ---------------------------------------------------------------------------
// Request metadata
// ---------------------------------------------------------------------------

/// Metadata for a single proxied request, generated at the start of handling.
#[derive(Debug, Clone)]
pub struct RequestMeta {
    pub id: String,
    pub method: String,
    pub path: String,
    pub query: String,
    pub timestamp: i64,
    pub agent_used: Option<String>,
    pub project: Option<String>,
}

impl RequestMeta {
    pub fn new(method: &Method, path: &str, query: &str, now: i64) -> Self {
        Self {
            id: generate_request_id(now),
            method: method.to_string(),
            path: path.to_string(),
            query: query.to_string(),
            timestamp: now,
            agent_used: None,
            project: None,
        }
    }
}

// ---------------------------------------------------------------------------
// Proxy result
// ---------------------------------------------------------------------------

/// Outcome of attempting to proxy through one account.
#[derive(Debug)]
pub enum AccountResult {
    /// Upstream returned a successful response.
    Success(Response),
    /// Account was rate-limited (429) — try next account.
    RateLimited(RateLimitInfo),
    /// Auth failure (401/403) — try next account.
    AuthFailed(StatusCode),
    /// Other error — try next account.
    Error(String),
}

// ---------------------------------------------------------------------------
// Thinking block filter
// ---------------------------------------------------------------------------

/// Filter thinking blocks from an assistant message in a request body.
///
/// When proxying to a Claude provider after conversation context included
/// thinking blocks from a different provider, Claude may reject the request.
/// This function removes thinking blocks and disables thinking mode.
///
/// Returns `Some(modified_bytes)` if changes were made, `None` otherwise.
pub fn filter_thinking_blocks(body: &[u8]) -> Option<Bytes> {
    // Fast reject: if "thinking" doesn't appear in the body, no thinking blocks exist
    if !body.windows(8).any(|w| w == b"thinking") {
        return None;
    }

    let mut json: serde_json::Value = serde_json::from_slice(body).ok()?;

    let messages = json.get_mut("messages")?.as_array_mut()?;
    let mut has_changes = false;

    // Process each message — filter thinking blocks from assistant messages
    let mut indices_to_remove = Vec::new();

    for (i, msg) in messages.iter_mut().enumerate() {
        let role = match msg.get("role").and_then(|v| v.as_str()) {
            Some(r) => r,
            None => continue, // no role field — keep message, skip filtering for this one
        };
        if role != "assistant" {
            continue;
        }

        let content = match msg.get_mut("content") {
            Some(c) if c.is_array() => c.as_array_mut()?,
            _ => continue,
        };

        // Filter thinking blocks
        let original_len = content.len();
        content.retain(|block| block.get("type").and_then(|t| t.as_str()) != Some("thinking"));

        if content.len() != original_len {
            has_changes = true;
        }

        // Check if message is now empty
        let is_empty = content.is_empty()
            || (content.len() == 1
                && content[0].get("type").and_then(|t| t.as_str()) == Some("text")
                && content[0]
                    .get("text")
                    .and_then(|t| t.as_str())
                    .is_none_or(|t| t.is_empty()));

        if is_empty {
            indices_to_remove.push(i);
        }
    }

    // Remove empty messages (in reverse order to preserve indices)
    for &i in indices_to_remove.iter().rev() {
        messages.remove(i);
    }

    if !has_changes {
        return None;
    }

    // Disable thinking mode
    json.as_object_mut()?.remove("thinking");

    info!("Disabled thinking mode due to incompatible thinking blocks from previous provider");

    let bytes = serde_json::to_vec(&json).ok()?;
    Some(Bytes::from(bytes))
}

/// Check if a response body indicates an invalid thinking block error.
pub fn is_thinking_block_error(status: StatusCode, body: &[u8]) -> bool {
    if status != StatusCode::BAD_REQUEST {
        return false;
    }

    let json: serde_json::Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(_) => return false,
    };

    json.get("error")
        .and_then(|e| e.get("message"))
        .and_then(|m| m.as_str())
        .is_some_and(|msg| {
            msg.contains(INVALID_THINKING_SIGNATURE) || msg.contains(THINKING_BLOCK_REQUIRED)
        })
}

// ---------------------------------------------------------------------------
// Agent interception
// ---------------------------------------------------------------------------

/// Extract agent name from a User-Agent header value.
///
/// Looks for patterns like "claude-code/X.Y.Z" or known agent identifiers.
/// Returns the detected agent name, if any.
pub fn detect_agent_from_user_agent(user_agent: &str) -> Option<String> {
    // Common agent patterns
    if user_agent.contains("claude-code") || user_agent.contains("ClaudeCode") {
        return Some("claude-code".to_string());
    }
    if user_agent.contains("cursor") || user_agent.contains("Cursor") {
        return Some("cursor".to_string());
    }
    if user_agent.contains("windsurf") || user_agent.contains("Windsurf") {
        return Some("windsurf".to_string());
    }
    if user_agent.contains("cline") || user_agent.contains("Cline") {
        return Some("cline".to_string());
    }
    None
}

/// Extract the model from a request body JSON.
///
/// Uses a lightweight struct to avoid parsing the entire body into a Value tree.
/// For large conversation payloads (multi-MB), this avoids megabytes of throw-away allocations.
pub fn extract_model_from_body(body: &[u8]) -> Option<String> {
    #[derive(serde::Deserialize)]
    struct ModelOnly {
        model: Option<String>,
    }
    let parsed: ModelOnly = serde_json::from_slice(body).ok()?;
    parsed.model
}

/// Replace the model in a request body JSON, returning modified bytes.
///
/// Uses fast string replacement for the common case (avoiding full JSON
/// parse/serialize of potentially multi-MB conversation bodies).
/// Falls back to full JSON parse for unusual formatting.
pub fn replace_model_in_body(body: &[u8], old_model: &str, new_model: &str) -> Option<Bytes> {
    let body_str = std::str::from_utf8(body).ok()?;

    // Try compact format: "model":"old_model"
    // Build needle without format! to avoid allocation
    let mut needle = String::with_capacity(10 + old_model.len()); // "model":"" = 10 chars
    needle.push_str("\"model\":\"");
    needle.push_str(old_model);
    needle.push('"');

    if let Some(pos) = body_str.find(&needle) {
        let extra = new_model.len().saturating_sub(old_model.len());
        let mut result = Vec::with_capacity(body_str.len() + extra);
        result.extend_from_slice(body_str[..pos].as_bytes());
        result.extend_from_slice(b"\"model\":\"");
        result.extend_from_slice(new_model.as_bytes());
        result.push(b'"');
        result.extend_from_slice(body_str[pos + needle.len()..].as_bytes());
        return Some(Bytes::from(result));
    }

    // Try with space after colon: "model": "old_model"
    needle.clear();
    needle.push_str("\"model\": \"");
    needle.push_str(old_model);
    needle.push('"');

    if let Some(pos) = body_str.find(&needle) {
        let extra = new_model.len().saturating_sub(old_model.len());
        let mut result = Vec::with_capacity(body_str.len() + extra);
        result.extend_from_slice(body_str[..pos].as_bytes());
        result.extend_from_slice(b"\"model\": \"");
        result.extend_from_slice(new_model.as_bytes());
        result.push(b'"');
        result.extend_from_slice(body_str[pos + needle.len()..].as_bytes());
        return Some(Bytes::from(result));
    }

    // Fallback: full JSON parse (handles unusual formatting)
    let mut json: serde_json::Value = serde_json::from_slice(body).ok()?;
    json["model"] = serde_json::Value::String(new_model.to_string());
    let bytes = serde_json::to_vec(&json).ok()?;
    Some(Bytes::from(bytes))
}

// ---------------------------------------------------------------------------
// Header utilities
// ---------------------------------------------------------------------------

/// Extract the force-account ID from request headers.
pub fn get_force_account_id(headers: &HeaderMap) -> Option<String> {
    headers
        .get("x-better-ccflare-account-id")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string())
}

/// Check if session bypass is requested.
pub fn is_session_bypass(headers: &HeaderMap) -> bool {
    headers
        .get("x-better-ccflare-bypass-session")
        .and_then(|v| v.to_str().ok())
        == Some("true")
}

/// Cap body bytes for analytics storage.
pub fn cap_body_for_analytics(body: &[u8]) -> String {
    if body.len() <= ANALYTICS_BODY_CAP {
        // Fast path: valid UTF-8 (almost always true for JSON) avoids double allocation
        match std::str::from_utf8(body) {
            Ok(s) => s.to_string(),
            Err(_) => String::from_utf8_lossy(body).into_owned(),
        }
    } else {
        let truncated = &body[..ANALYTICS_BODY_CAP];
        format!(
            "{}... [truncated, {} bytes total]",
            String::from_utf8_lossy(truncated),
            body.len()
        )
    }
}

/// Build an error response with JSON body.
pub fn error_response(status: StatusCode, message: &str) -> Response {
    let body = serde_json::json!({
        "error": {
            "type": "proxy_error",
            "message": message,
        }
    });
    (status, axum::Json(body)).into_response()
}

// ---------------------------------------------------------------------------
// Proxy orchestrator
// ---------------------------------------------------------------------------

/// Result of the full proxy handler.
pub enum ProxyOutcome {
    /// Successfully proxied (response to forward).
    Response(Response),
    /// All accounts failed, should try unauthenticated.
    AllAccountsFailed {
        request_meta: RequestMeta,
        body: Bytes,
    },
    /// No accounts available at all.
    NoAccounts {
        request_meta: RequestMeta,
        body: Bytes,
    },
}

/// Attempt to proxy a request through the given accounts in order.
///
/// Returns the first successful response, or signals that all accounts failed.
/// The caller is responsible for the unauthenticated fallback.
///
/// # Arguments
/// * `accounts` - Ordered list of accounts to try (from load balancer)
/// * `meta` - Request metadata (ID, method, path, etc.)
/// * `body` - Buffered request body (reused across attempts, zero-copy clone)
/// * `try_account` - Async closure that tries one account and returns the outcome
pub async fn try_accounts_in_order<F, Fut>(
    accounts: &[Account],
    meta: &RequestMeta,
    body: &Bytes,
    try_account: F,
) -> Option<(Response, String)>
where
    F: Fn(Account, RequestMeta, Bytes, usize) -> Fut,
    Fut: std::future::Future<Output = AccountResult>,
{
    for (attempt, account) in accounts.iter().enumerate() {
        debug!(
            request_id = %meta.id,
            account = %account.name,
            provider = %account.provider,
            attempt = attempt,
            "Trying account"
        );

        match try_account(account.clone(), meta.clone(), body.clone(), attempt).await {
            AccountResult::Success(response) => {
                info!(
                    request_id = %meta.id,
                    account = %account.name,
                    attempt = attempt,
                    "Request succeeded"
                );
                return Some((response, account.id.clone()));
            }
            AccountResult::RateLimited(rate_info) => {
                warn!(
                    request_id = %meta.id,
                    account = %account.name,
                    reset_time = ?rate_info.reset_time,
                    "Account rate-limited, trying next"
                );
            }
            AccountResult::AuthFailed(status) => {
                warn!(
                    request_id = %meta.id,
                    account = %account.name,
                    status = %status,
                    "Auth failed, trying next account"
                );
            }
            AccountResult::Error(msg) => {
                error!(
                    request_id = %meta.id,
                    account = %account.name,
                    error = %msg,
                    "Request failed, trying next account"
                );
            }
        }
    }

    None // All accounts exhausted
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    #[test]
    fn filter_thinking_blocks_removes_thinking() {
        let body = serde_json::json!({
            "model": "claude-sonnet-4-5-20250929",
            "messages": [
                {
                    "role": "user",
                    "content": "Hello"
                },
                {
                    "role": "assistant",
                    "content": [
                        {"type": "thinking", "thinking": "Let me think..."},
                        {"type": "text", "text": "Hello!"}
                    ]
                },
                {
                    "role": "user",
                    "content": "Thanks"
                }
            ],
            "thinking": {"type": "enabled", "budget_tokens": 10000}
        });
        let body_bytes = serde_json::to_vec(&body).unwrap();

        let result = filter_thinking_blocks(&body_bytes);
        assert!(result.is_some());

        let filtered: serde_json::Value = serde_json::from_slice(&result.unwrap()).unwrap();
        // Thinking blocks removed
        let messages = filtered["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 3); // All messages kept
        let assistant_content = messages[1]["content"].as_array().unwrap();
        assert_eq!(assistant_content.len(), 1); // Only text block
        assert_eq!(assistant_content[0]["type"], "text");
        // Thinking mode disabled
        assert!(filtered.get("thinking").is_none());
    }

    #[test]
    fn filter_thinking_blocks_no_changes() {
        let body = serde_json::json!({
            "model": "claude-sonnet-4-5-20250929",
            "messages": [
                {"role": "user", "content": "Hello"},
                {"role": "assistant", "content": [{"type": "text", "text": "Hi!"}]}
            ]
        });
        let body_bytes = serde_json::to_vec(&body).unwrap();

        let result = filter_thinking_blocks(&body_bytes);
        assert!(result.is_none()); // No changes needed
    }

    #[test]
    fn filter_thinking_blocks_removes_empty_messages() {
        let body = serde_json::json!({
            "model": "claude-sonnet-4-5-20250929",
            "messages": [
                {"role": "user", "content": "Hello"},
                {
                    "role": "assistant",
                    "content": [
                        {"type": "thinking", "thinking": "..."}
                    ]
                },
                {"role": "user", "content": "More"}
            ]
        });
        let body_bytes = serde_json::to_vec(&body).unwrap();

        let result = filter_thinking_blocks(&body_bytes);
        assert!(result.is_some());

        let filtered: serde_json::Value = serde_json::from_slice(&result.unwrap()).unwrap();
        let messages = filtered["messages"].as_array().unwrap();
        // Empty assistant message should be removed
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0]["role"], "user");
        assert_eq!(messages[1]["role"], "user");
    }

    #[test]
    fn filter_thinking_blocks_invalid_json() {
        let result = filter_thinking_blocks(b"not json");
        assert!(result.is_none());
    }

    #[test]
    fn is_thinking_error_detects_signature() {
        let body = serde_json::json!({
            "error": {
                "type": "invalid_request_error",
                "message": "Invalid `signature` in `thinking` block at index 0"
            }
        });
        let bytes = serde_json::to_vec(&body).unwrap();
        assert!(is_thinking_block_error(StatusCode::BAD_REQUEST, &bytes));
    }

    #[test]
    fn is_thinking_error_detects_final_assistant() {
        let body = serde_json::json!({
            "error": {
                "type": "invalid_request_error",
                "message": "When using extended thinking, the final `assistant` message must start with a thinking block"
            }
        });
        let bytes = serde_json::to_vec(&body).unwrap();
        assert!(is_thinking_block_error(StatusCode::BAD_REQUEST, &bytes));
    }

    #[test]
    fn is_thinking_error_rejects_non_400() {
        let body = serde_json::json!({
            "error": {"message": "Invalid `signature` in `thinking` block"}
        });
        let bytes = serde_json::to_vec(&body).unwrap();
        assert!(!is_thinking_block_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            &bytes
        ));
    }

    #[test]
    fn is_thinking_error_rejects_other_errors() {
        let body = serde_json::json!({
            "error": {"message": "Some other error"}
        });
        let bytes = serde_json::to_vec(&body).unwrap();
        assert!(!is_thinking_block_error(StatusCode::BAD_REQUEST, &bytes));
    }

    #[test]
    fn detect_agent_claude_code() {
        assert_eq!(
            detect_agent_from_user_agent("claude-code/2.1.37"),
            Some("claude-code".to_string())
        );
    }

    #[test]
    fn detect_agent_cursor() {
        assert_eq!(
            detect_agent_from_user_agent("cursor/0.50.1"),
            Some("cursor".to_string())
        );
    }

    #[test]
    fn detect_agent_unknown() {
        assert_eq!(detect_agent_from_user_agent("Mozilla/5.0"), None);
    }

    #[test]
    fn extract_model_from_body_works() {
        let body = serde_json::json!({"model": "claude-sonnet-4-5-20250929", "messages": []});
        let bytes = serde_json::to_vec(&body).unwrap();
        assert_eq!(
            extract_model_from_body(&bytes),
            Some("claude-sonnet-4-5-20250929".to_string())
        );
    }

    #[test]
    fn replace_model_works() {
        let body = serde_json::json!({"model": "old-model", "messages": []});
        let bytes = serde_json::to_vec(&body).unwrap();
        let result = replace_model_in_body(&bytes, "old-model", "new-model").unwrap();
        let parsed: serde_json::Value = serde_json::from_slice(&result).unwrap();
        assert_eq!(parsed["model"], "new-model");
    }

    #[test]
    fn get_force_account_from_headers() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-better-ccflare-account-id",
            HeaderValue::from_static("acct-123"),
        );
        assert_eq!(get_force_account_id(&headers), Some("acct-123".to_string()));
    }

    #[test]
    fn get_force_account_missing() {
        let headers = HeaderMap::new();
        assert_eq!(get_force_account_id(&headers), None);
    }

    #[test]
    fn session_bypass_detection() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-better-ccflare-bypass-session",
            HeaderValue::from_static("true"),
        );
        assert!(is_session_bypass(&headers));

        let empty = HeaderMap::new();
        assert!(!is_session_bypass(&empty));
    }

    #[test]
    fn cap_body_small() {
        let body = b"small body";
        let result = cap_body_for_analytics(body);
        assert_eq!(result, "small body");
    }

    #[test]
    fn cap_body_large() {
        let body = vec![b'x'; ANALYTICS_BODY_CAP + 100];
        let result = cap_body_for_analytics(&body);
        assert!(result.contains("truncated"));
        assert!(result.contains(&(ANALYTICS_BODY_CAP + 100).to_string()));
    }

    #[test]
    fn request_meta_generates_uuid() {
        let meta = RequestMeta::new(&Method::POST, "/v1/messages", "", 1_700_000_000_000);
        assert!(!meta.id.is_empty());
        assert_eq!(meta.method, "POST");
        assert_eq!(meta.path, "/v1/messages");
    }

    #[tokio::test]
    async fn try_accounts_returns_first_success() {
        let accounts = vec![
            make_test_account("a1"),
            make_test_account("a2"),
            make_test_account("a3"),
        ];
        let meta = RequestMeta::new(&Method::POST, "/v1/messages", "", 0);
        let body = Bytes::from_static(b"test");

        // First account rate-limited, second succeeds
        let result =
            try_accounts_in_order(&accounts, &meta, &body, |account, _, _, _| async move {
                if account.id == "a1" {
                    AccountResult::RateLimited(RateLimitInfo::default())
                } else {
                    AccountResult::Success((StatusCode::OK, "ok").into_response())
                }
            })
            .await;

        let (_, account_id) = result.unwrap();
        assert_eq!(account_id, "a2"); // second account succeeded, not the first
    }

    #[tokio::test]
    async fn try_accounts_returns_none_when_all_fail() {
        let accounts = vec![make_test_account("a1"), make_test_account("a2")];
        let meta = RequestMeta::new(&Method::POST, "/v1/messages", "", 0);
        let body = Bytes::from_static(b"test");

        let result = try_accounts_in_order(&accounts, &meta, &body, |_, _, _, _| async move {
            AccountResult::Error("failed".to_string())
        })
        .await;

        assert!(result.is_none());
    }

    #[tokio::test]
    async fn try_accounts_empty_returns_none() {
        let meta = RequestMeta::new(&Method::POST, "/v1/messages", "", 0);
        let body = Bytes::from_static(b"test");

        let result = try_accounts_in_order(&[], &meta, &body, |_, _, _, _| async move {
            AccountResult::Success((StatusCode::OK, "ok").into_response())
        })
        .await;

        assert!(result.is_none());
    }

    fn make_test_account(id: &str) -> Account {
        Account {
            id: id.to_string(),
            name: id.to_string(),
            provider: "anthropic".to_string(),
            api_key: None,
            refresh_token: "rt".to_string(),
            access_token: Some("at".to_string()),
            expires_at: Some(i64::MAX),
            request_count: 0,
            total_requests: 0,
            last_used: None,
            created_at: 0,
            rate_limited_until: None,
            session_start: None,
            session_request_count: 0,
            paused: false,
            rate_limit_reset: None,
            rate_limit_status: None,
            rate_limit_remaining: None,
            priority: 0,
            auto_fallback_enabled: false,
            auto_refresh_enabled: false,
            custom_endpoint: None,
            model_mappings: None,
            reserve_5h: 0,
            reserve_weekly: 0,
            reserve_hard: false,
            subscription_tier: None,
            email: None,
            refresh_token_updated_at: None,
        is_shared: false,
        }
    }
}
