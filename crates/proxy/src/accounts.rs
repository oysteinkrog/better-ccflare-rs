//! Account management API handlers.
//!
//! REST endpoints for listing, pausing, resuming, reloading, renaming,
//! deleting accounts, and updating priority/settings.

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::response::{IntoResponse, Response};
use axum::Json;
use http::StatusCode;
use serde_json::json;
use tracing::{debug, warn};

use bccf_core::providers::Provider;
use bccf_core::types::{Account, AccountResponse, TokenStatus};
use bccf_core::AppState;
use bccf_database::repositories::account as account_repo;
use bccf_database::DbPool;
use bccf_providers::usage_polling::{AnyUsageData, UsageCache};

/// Check if a provider uses API keys (not OAuth).
fn is_api_key_provider(provider: &str) -> bool {
    Provider::from_str_loose(provider).is_some_and(|p| p.uses_api_key())
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Session duration for computing session_info (5 hours in ms).
const SESSION_DURATION_MS: i64 = 5 * 60 * 60 * 1000;

/// Convert an `Account` to the API response format.
fn account_to_response(
    account: &Account,
    now: i64,
    usage: Option<&AnyUsageData>,
) -> AccountResponse {
    let auth_failed = account
        .rate_limit_status
        .as_deref()
        .is_some_and(|s| s.starts_with("Auth failed"));
    let token_status = if is_api_key_provider(&account.provider) {
        TokenStatus::ApiKey
    } else {
        match account.expires_at {
            Some(exp) if exp > now && !auth_failed => TokenStatus::Valid,
            _ => TokenStatus::Expired,
        }
    };

    let token_expires_at = account.expires_at.map(millis_to_iso);

    let last_used = account.last_used.map(millis_to_iso);
    let created = millis_to_iso(account.created_at);

    // Rate limit status
    let rate_limit_status = if let Some(ref status) = account.rate_limit_status {
        if let Some(reset) = account.rate_limit_reset {
            if reset > now {
                let minutes_left = ((reset - now) as f64 / 60000.0).ceil() as i64;
                format!("{status} ({minutes_left}m)")
            } else {
                status.clone()
            }
        } else {
            status.clone()
        }
    } else if let Some(until) = account.rate_limited_until {
        if until > now {
            let minutes_left = ((until - now) as f64 / 60000.0).ceil() as i64;
            format!("Rate limited ({minutes_left}m)")
        } else {
            "OK".to_string()
        }
    } else {
        "OK".to_string()
    };

    let rate_limit_reset = account.rate_limit_reset.map(millis_to_iso);

    // Session info
    let session_info = match account.session_start {
        Some(start) if (now - start) < SESSION_DURATION_MS => {
            format!("Active: {} reqs", account.session_request_count)
        }
        _ => "-".to_string(),
    };

    // Parse model mappings from JSON string
    let model_mappings = account
        .model_mappings
        .as_ref()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok());

    AccountResponse {
        id: account.id.clone(),
        name: account.name.clone(),
        provider: account.provider.clone(),
        request_count: account.request_count,
        total_requests: account.total_requests,
        last_used,
        created,
        paused: account.paused,
        token_status,
        token_expires_at,
        rate_limit_status,
        rate_limit_reset,
        rate_limit_remaining: account.rate_limit_remaining,
        session_info,
        priority: account.priority,
        auto_fallback_enabled: account.auto_fallback_enabled,
        auto_refresh_enabled: account.auto_refresh_enabled,
        custom_endpoint: account.custom_endpoint.clone(),
        model_mappings,
        usage_utilization: usage.and_then(|u| u.utilization()),
        usage_window: usage.and_then(|u| u.representative_window()),
        usage_data: usage.and_then(|u| u.to_json()),
        has_refresh_token: !account.refresh_token.is_empty(),
        reserve_5h: account.reserve_5h,
        reserve_weekly: account.reserve_weekly,
        reserve_hard: account.reserve_hard,
        subscription_tier: account.subscription_tier.clone(),
        email: account.email.clone(),
        is_shared: account.is_shared,
        overage_protection: account.overage_protection,
    }
}

/// Convert epoch millis to ISO 8601 string.
fn millis_to_iso(ms: i64) -> String {
    use chrono::{DateTime, Utc};
    let secs = ms / 1000;
    let nsecs = ((ms % 1000) * 1_000_000) as u32;
    DateTime::from_timestamp(secs, nsecs)
        .unwrap_or_else(Utc::now)
        .to_rfc3339()
}

/// Get a database connection or return 500.
macro_rules! get_conn {
    ($state:expr) => {
        match $state.db_pool::<DbPool>() {
            Some(pool) => match pool.get() {
                Ok(conn) => conn,
                Err(e) => {
                    warn!("Failed to get DB connection: {e}");
                    return (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        Json(json!({"error": "Database unavailable"})),
                    )
                        .into_response();
                }
            },
            None => {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({"error": "Database not configured"})),
                )
                    .into_response();
            }
        }
    };
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

/// GET /api/accounts — list all accounts with status.
pub async fn list_accounts(State(state): State<Arc<AppState>>) -> Response {
    let conn = get_conn!(state);

    let accounts = match account_repo::find_all(&conn) {
        Ok(a) => a,
        Err(e) => {
            warn!("Failed to list accounts: {e}");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": "Failed to list accounts"})),
            )
                .into_response();
        }
    };

    let now = chrono::Utc::now().timestamp_millis();
    let usage_cache = state.usage_cache::<UsageCache>();
    let response: Vec<AccountResponse> = accounts
        .iter()
        .map(|a| {
            let usage = usage_cache.and_then(|cache| cache.get(&a.id));
            account_to_response(a, now, usage.as_ref())
        })
        .collect();

    Json(response).into_response()
}

/// POST /api/accounts/:id/pause — pause an account.
pub async fn pause_account(
    State(state): State<Arc<AppState>>,
    Path(account_id): Path<String>,
) -> Response {
    let conn = get_conn!(state);

    // Check account exists
    match account_repo::find_by_id(&conn, &account_id) {
        Ok(Some(_)) => {}
        Ok(None) => {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({"error": "Account not found"})),
            )
                .into_response();
        }
        Err(e) => {
            warn!("Failed to find account {account_id}: {e}");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": "Database error"})),
            )
                .into_response();
        }
    }

    if let Err(e) = account_repo::pause(&conn, &account_id) {
        warn!("Failed to pause account {account_id}: {e}");
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": "Failed to pause account"})),
        )
            .into_response();
    }

    debug!("Paused account {account_id}");
    Json(json!({"success": true})).into_response()
}

/// POST /api/accounts/:id/resume — resume a paused account.
pub async fn resume_account(
    State(state): State<Arc<AppState>>,
    Path(account_id): Path<String>,
) -> Response {
    let conn = get_conn!(state);

    match account_repo::find_by_id(&conn, &account_id) {
        Ok(Some(_)) => {}
        Ok(None) => {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({"error": "Account not found"})),
            )
                .into_response();
        }
        Err(e) => {
            warn!("Failed to find account {account_id}: {e}");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": "Database error"})),
            )
                .into_response();
        }
    }

    if let Err(e) = account_repo::resume(&conn, &account_id) {
        warn!("Failed to resume account {account_id}: {e}");
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": "Failed to resume account"})),
        )
            .into_response();
    }

    debug!("Resumed account {account_id}");
    Json(json!({"success": true})).into_response()
}

/// POST /api/accounts/:id/reload — re-read account from database.
pub async fn reload_account(
    State(state): State<Arc<AppState>>,
    Path(account_id): Path<String>,
) -> Response {
    let conn = get_conn!(state);

    match account_repo::find_by_id(&conn, &account_id) {
        Ok(Some(account)) => {
            let now = chrono::Utc::now().timestamp_millis();
            let usage_cache = state.usage_cache::<UsageCache>();
            let usage = usage_cache.and_then(|cache| cache.get(&account.id));
            debug!("Reloaded account {account_id}");
            Json(json!({
                "success": true,
                "account": account_to_response(&account, now, usage.as_ref())
            }))
            .into_response()
        }
        Ok(None) => (
            StatusCode::NOT_FOUND,
            Json(json!({"error": "Account not found"})),
        )
            .into_response(),
        Err(e) => {
            warn!("Failed to reload account {account_id}: {e}");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": "Failed to reload account"})),
            )
                .into_response()
        }
    }
}

/// POST /api/accounts/:id/priority — update account priority.
pub async fn update_priority(
    State(state): State<Arc<AppState>>,
    Path(account_id): Path<String>,
    Json(body): Json<serde_json::Value>,
) -> Response {
    let conn = get_conn!(state);

    // Validate priority
    let priority = match body.get("priority").and_then(|v| v.as_i64()) {
        Some(p) if (0..=100).contains(&p) => p,
        Some(_) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": "Priority must be between 0 and 100"})),
            )
                .into_response();
        }
        None => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": "Priority is required and must be an integer"})),
            )
                .into_response();
        }
    };

    // Check account exists
    match account_repo::find_by_id(&conn, &account_id) {
        Ok(Some(_)) => {}
        Ok(None) => {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({"error": "Account not found"})),
            )
                .into_response();
        }
        Err(e) => {
            warn!("Failed to find account {account_id}: {e}");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": "Database error"})),
            )
                .into_response();
        }
    }

    if let Err(e) = account_repo::update_priority(&conn, &account_id, priority) {
        warn!("Failed to update priority for {account_id}: {e}");
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": "Failed to update priority"})),
        )
            .into_response();
    }

    debug!("Updated priority for account {account_id} to {priority}");
    Json(json!({"success": true, "priority": priority})).into_response()
}

/// POST /api/accounts/:id/rename — rename an account.
pub async fn rename_account(
    State(state): State<Arc<AppState>>,
    Path(account_id): Path<String>,
    Json(body): Json<serde_json::Value>,
) -> Response {
    let conn = get_conn!(state);

    let name = match body.get("name").and_then(|v| v.as_str()) {
        Some(n) if !n.trim().is_empty() && n.len() <= 100 => n.trim().to_string(),
        Some(_) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": "Name must be 1-100 characters"})),
            )
                .into_response();
        }
        None => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": "Name is required"})),
            )
                .into_response();
        }
    };

    match account_repo::find_by_id(&conn, &account_id) {
        Ok(Some(_)) => {}
        Ok(None) => {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({"error": "Account not found"})),
            )
                .into_response();
        }
        Err(e) => {
            warn!("Failed to find account {account_id}: {e}");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": "Database error"})),
            )
                .into_response();
        }
    }

    if let Err(e) = account_repo::rename(&conn, &account_id, &name) {
        warn!("Failed to rename account {account_id}: {e}");
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": "Failed to rename account"})),
        )
            .into_response();
    }

    debug!("Renamed account {account_id} to {name}");
    Json(json!({"success": true, "name": name})).into_response()
}

/// DELETE /api/accounts/:id — delete an account.
pub async fn delete_account(
    State(state): State<Arc<AppState>>,
    Path(account_id): Path<String>,
) -> Response {
    let conn = get_conn!(state);

    match account_repo::delete(&conn, &account_id) {
        Ok(true) => {
            debug!("Deleted account {account_id}");
            Json(json!({"success": true})).into_response()
        }
        Ok(false) => (
            StatusCode::NOT_FOUND,
            Json(json!({"error": "Account not found"})),
        )
            .into_response(),
        Err(e) => {
            warn!("Failed to delete account {account_id}: {e}");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": "Failed to delete account"})),
            )
                .into_response()
        }
    }
}

/// POST /api/accounts/:id/auto-fallback — toggle auto-fallback.
pub async fn set_auto_fallback(
    State(state): State<Arc<AppState>>,
    Path(account_id): Path<String>,
    Json(body): Json<serde_json::Value>,
) -> Response {
    let conn = get_conn!(state);

    let enabled = match body.get("enabled").and_then(|v| v.as_bool()) {
        Some(e) => e,
        None => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": "enabled (boolean) is required"})),
            )
                .into_response();
        }
    };

    match account_repo::find_by_id(&conn, &account_id) {
        Ok(Some(_)) => {}
        Ok(None) => {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({"error": "Account not found"})),
            )
                .into_response();
        }
        Err(e) => {
            warn!("Failed to find account {account_id}: {e}");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": "Database error"})),
            )
                .into_response();
        }
    }

    if let Err(e) = account_repo::set_auto_fallback_enabled(&conn, &account_id, enabled) {
        warn!("Failed to set auto_fallback for {account_id}: {e}");
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": "Failed to update auto-fallback"})),
        )
            .into_response();
    }

    Json(json!({"success": true, "enabled": enabled})).into_response()
}

/// POST /api/accounts/:id/auto-refresh — toggle auto-refresh.
pub async fn set_auto_refresh(
    State(state): State<Arc<AppState>>,
    Path(account_id): Path<String>,
    Json(body): Json<serde_json::Value>,
) -> Response {
    let conn = get_conn!(state);

    let enabled = match body.get("enabled").and_then(|v| v.as_bool()) {
        Some(e) => e,
        None => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": "enabled (boolean) is required"})),
            )
                .into_response();
        }
    };

    match account_repo::find_by_id(&conn, &account_id) {
        Ok(Some(_)) => {}
        Ok(None) => {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({"error": "Account not found"})),
            )
                .into_response();
        }
        Err(e) => {
            warn!("Failed to find account {account_id}: {e}");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": "Database error"})),
            )
                .into_response();
        }
    }

    if let Err(e) = account_repo::set_auto_refresh_enabled(&conn, &account_id, enabled) {
        warn!("Failed to set auto_refresh for {account_id}: {e}");
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": "Failed to update auto-refresh"})),
        )
            .into_response();
    }

    Json(json!({"success": true, "enabled": enabled})).into_response()
}

/// POST /api/accounts/:id/custom-endpoint — update custom endpoint URL.
pub async fn set_custom_endpoint(
    State(state): State<Arc<AppState>>,
    Path(account_id): Path<String>,
    Json(body): Json<serde_json::Value>,
) -> Response {
    let conn = get_conn!(state);

    let endpoint = body
        .get("endpoint")
        .and_then(|v| v.as_str())
        .map(|s| s.trim());

    // Empty string clears the endpoint
    let endpoint = match endpoint {
        Some(e) if e.is_empty() => None,
        Some(e) => {
            // Basic URL validation
            if !e.starts_with("http://") && !e.starts_with("https://") {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(json!({"error": "Endpoint must start with http:// or https://"})),
                )
                    .into_response();
            }
            Some(e)
        }
        None => None,
    };

    match account_repo::find_by_id(&conn, &account_id) {
        Ok(Some(_)) => {}
        Ok(None) => {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({"error": "Account not found"})),
            )
                .into_response();
        }
        Err(e) => {
            warn!("Failed to find account {account_id}: {e}");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": "Database error"})),
            )
                .into_response();
        }
    }

    if let Err(e) = account_repo::set_custom_endpoint(&conn, &account_id, endpoint) {
        warn!("Failed to set custom_endpoint for {account_id}: {e}");
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": "Failed to update custom endpoint"})),
        )
            .into_response();
    }

    Json(json!({"success": true, "endpoint": endpoint})).into_response()
}

/// POST /api/accounts/:id/model-mappings — update model mappings.
pub async fn set_model_mappings(
    State(state): State<Arc<AppState>>,
    Path(account_id): Path<String>,
    Json(body): Json<serde_json::Value>,
) -> Response {
    let conn = get_conn!(state);

    let mappings = body.get("mappings");

    // Validate mappings is an object or null
    let mappings_str = match mappings {
        Some(serde_json::Value::Object(_)) => {
            Some(serde_json::to_string(mappings.unwrap()).unwrap())
        }
        Some(serde_json::Value::Null) | None => None,
        _ => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": "mappings must be an object or null"})),
            )
                .into_response();
        }
    };

    match account_repo::find_by_id(&conn, &account_id) {
        Ok(Some(_)) => {}
        Ok(None) => {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({"error": "Account not found"})),
            )
                .into_response();
        }
        Err(e) => {
            warn!("Failed to find account {account_id}: {e}");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": "Database error"})),
            )
                .into_response();
        }
    }

    if let Err(e) = account_repo::set_model_mappings(&conn, &account_id, mappings_str.as_deref()) {
        warn!("Failed to set model_mappings for {account_id}: {e}");
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": "Failed to update model mappings"})),
        )
            .into_response();
    }

    Json(json!({"success": true})).into_response()
}

/// POST /api/accounts/:id/reserve-5h — set 5-hour reserve capacity percentage.
pub async fn set_reserve_5h(
    State(state): State<Arc<AppState>>,
    Path(account_id): Path<String>,
    Json(body): Json<serde_json::Value>,
) -> Response {
    let conn = get_conn!(state);

    let percent = match body.get("percent").and_then(|v| v.as_i64()) {
        Some(p) if (0..=100).contains(&p) => p,
        Some(_) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": "percent must be an integer 0-100"})),
            )
                .into_response();
        }
        None => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": "percent (integer 0-100) is required"})),
            )
                .into_response();
        }
    };

    match account_repo::find_by_id(&conn, &account_id) {
        Ok(Some(_)) => {}
        Ok(None) => {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({"error": "Account not found"})),
            )
                .into_response();
        }
        Err(e) => {
            warn!("Failed to find account {account_id}: {e}");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": "Database error"})),
            )
                .into_response();
        }
    }

    if let Err(e) = account_repo::set_reserve_5h(&conn, &account_id, percent) {
        warn!("Failed to set reserve_5h for {account_id}: {e}");
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": "Failed to update reserve 5h"})),
        )
            .into_response();
    }

    Json(json!({"success": true, "percent": percent})).into_response()
}

/// POST /api/accounts/:id/reserve-weekly — set weekly reserve capacity percentage.
pub async fn set_reserve_weekly(
    State(state): State<Arc<AppState>>,
    Path(account_id): Path<String>,
    Json(body): Json<serde_json::Value>,
) -> Response {
    let conn = get_conn!(state);

    let percent = match body.get("percent").and_then(|v| v.as_i64()) {
        Some(p) if (0..=100).contains(&p) => p,
        Some(_) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": "percent must be an integer 0-100"})),
            )
                .into_response();
        }
        None => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": "percent (integer 0-100) is required"})),
            )
                .into_response();
        }
    };

    match account_repo::find_by_id(&conn, &account_id) {
        Ok(Some(_)) => {}
        Ok(None) => {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({"error": "Account not found"})),
            )
                .into_response();
        }
        Err(e) => {
            warn!("Failed to find account {account_id}: {e}");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": "Database error"})),
            )
                .into_response();
        }
    }

    if let Err(e) = account_repo::set_reserve_weekly(&conn, &account_id, percent) {
        warn!("Failed to set reserve_weekly for {account_id}: {e}");
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": "Failed to update reserve weekly"})),
        )
            .into_response();
    }

    Json(json!({"success": true, "percent": percent})).into_response()
}

/// POST /api/accounts/:id/reserve-hard — toggle hard reserve.
pub async fn set_reserve_hard(
    State(state): State<Arc<AppState>>,
    Path(account_id): Path<String>,
    Json(body): Json<serde_json::Value>,
) -> Response {
    let conn = get_conn!(state);

    let enabled = match body.get("enabled").and_then(|v| v.as_bool()) {
        Some(e) => e,
        None => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": "enabled (boolean) is required"})),
            )
                .into_response();
        }
    };

    match account_repo::find_by_id(&conn, &account_id) {
        Ok(Some(_)) => {}
        Ok(None) => {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({"error": "Account not found"})),
            )
                .into_response();
        }
        Err(e) => {
            warn!("Failed to find account {account_id}: {e}");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": "Database error"})),
            )
                .into_response();
        }
    }

    if let Err(e) = account_repo::set_reserve_hard(&conn, &account_id, enabled) {
        warn!("Failed to set reserve_hard for {account_id}: {e}");
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": "Failed to update reserve hard"})),
        )
            .into_response();
    }

    Json(json!({"success": true, "enabled": enabled})).into_response()
}

/// Valid provider modes for account creation.
const VALID_MODES: &[&str] = &[
    "claude-oauth",
    "console",
    "zai",
    "minimax",
    "nanogpt",
    "anthropic-compatible",
    "openai-compatible",
    "vertex-ai",
];

/// Map a mode string to a provider string stored in the database.
fn mode_to_provider(mode: &str) -> &str {
    match mode {
        "claude-oauth" => "claude-oauth",
        "console" => "claude-console-api",
        "zai" => "zai",
        "minimax" => "minimax",
        "nanogpt" => "nanogpt",
        "anthropic-compatible" => "anthropic-compatible",
        "openai-compatible" => "openai-compatible",
        "vertex-ai" => "vertex-ai",
        other => other,
    }
}

/// POST /api/accounts — create a new account.
pub async fn create_account(
    State(state): State<Arc<AppState>>,
    Json(body): Json<serde_json::Value>,
) -> Response {
    let conn = get_conn!(state);

    // Validate name
    let name = match body.get("name").and_then(|v| v.as_str()) {
        Some(n) if !n.trim().is_empty() && n.len() <= 100 => n.trim().to_string(),
        _ => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": "name is required (1-100 characters)"})),
            )
                .into_response();
        }
    };

    // Validate mode
    let mode = match body.get("mode").and_then(|v| v.as_str()) {
        Some(m) if VALID_MODES.contains(&m) => m.to_string(),
        Some(m) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": format!("Invalid mode: '{}'. Valid modes: {}", m, VALID_MODES.join(", "))})),
            )
                .into_response();
        }
        None => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": format!("mode is required. Valid modes: {}", VALID_MODES.join(", "))})),
            )
                .into_response();
        }
    };

    // Validate priority
    let priority = body.get("priority").and_then(|v| v.as_i64()).unwrap_or(0);
    if !(0..=100).contains(&priority) {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "Priority must be between 0 and 100"})),
        )
            .into_response();
    }

    // Check for duplicate name (case-insensitive)
    let name_lower = name.to_lowercase();
    match account_repo::find_all(&conn) {
        Ok(accounts) => {
            if accounts.iter().any(|a| a.name.to_lowercase() == name_lower) {
                return (
                    StatusCode::CONFLICT,
                    Json(json!({"error": format!("Account '{}' already exists", name)})),
                )
                    .into_response();
            }
        }
        Err(e) => {
            warn!("Failed to check existing accounts: {e}");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": "Database error"})),
            )
                .into_response();
        }
    }

    let provider = mode_to_provider(&mode);
    let now = chrono::Utc::now().timestamp_millis();
    let id = uuid::Uuid::new_v4().to_string();
    let api_key_input = body.get("api_key").and_then(|v| v.as_str());

    let is_oauth = mode == "claude-oauth";

    // For non-OAuth modes, API key is required
    if !is_oauth && api_key_input.map_or(true, |k| k.trim().is_empty()) {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": format!("API key is required for '{}' accounts", mode)})),
        )
            .into_response();
    }

    let (api_key, access_token, expires_at) = if is_oauth {
        (None, None, None)
    } else {
        let key = api_key_input.unwrap().trim().to_string();
        let token = key.clone();
        let exp = now + 30 * 24 * 60 * 60 * 1000; // 30 days
        (Some(key), Some(token), Some(exp))
    };

    let account = Account {
        id: id.clone(),
        name: name.clone(),
        provider: provider.to_string(),
        api_key,
        refresh_token: String::new(),
        access_token,
        expires_at,
        request_count: 0,
        total_requests: 0,
        last_used: None,
        created_at: now,
        rate_limited_until: None,
        session_start: None,
        session_request_count: 0,
        paused: false,
        rate_limit_reset: None,
        rate_limit_status: None,
        rate_limit_remaining: None,
        priority,
        auto_fallback_enabled: true,
        auto_refresh_enabled: true,
        custom_endpoint: None,
        model_mappings: None,
        reserve_5h: 0,
        reserve_weekly: 0,
        reserve_hard: false,
        subscription_tier: None,
        email: None,
        refresh_token_updated_at: None,
        is_shared: false,
        overage_protection: true,
    };

    if let Err(e) = account_repo::create(&conn, &account) {
        warn!("Failed to create account {name}: {e}");
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": "Failed to create account"})),
        )
            .into_response();
    }

    let mut msg = format!("Account '{}' created ({})", name, provider);
    if is_oauth {
        msg.push_str(". Run `better-ccflare --reauthenticate ");
        msg.push_str(&name);
        msg.push_str("` to complete OAuth setup.");
    }

    debug!("Created account {name} (id={id}, provider={provider})");
    (
        StatusCode::CREATED,
        Json(json!({"success": true, "id": id, "message": msg})),
    )
        .into_response()
}

/// POST /api/accounts/:id/shared — mark account as shared with external users.
pub async fn set_is_shared(
    State(state): State<Arc<AppState>>,
    Path(account_id): Path<String>,
    Json(body): Json<serde_json::Value>,
) -> Response {
    let conn = get_conn!(state);

    let shared = match body.get("shared").and_then(|v| v.as_bool()) {
        Some(s) => s,
        None => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": "shared (boolean) is required"})),
            )
                .into_response();
        }
    };

    match account_repo::find_by_id(&conn, &account_id) {
        Ok(Some(_)) => {}
        Ok(None) => {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({"error": "Account not found"})),
            )
                .into_response();
        }
        Err(e) => {
            warn!("Failed to find account {account_id}: {e}");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": "Database error"})),
            )
                .into_response();
        }
    }

    if let Err(e) = account_repo::set_is_shared(&conn, &account_id, shared) {
        warn!("Failed to set is_shared for {account_id}: {e}");
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": "Failed to update shared status"})),
        )
            .into_response();
    }

    Json(json!({"success": true, "shared": shared})).into_response()
}

/// POST /api/accounts/:id/overage-protection — toggle overage protection.
pub async fn set_overage_protection(
    State(state): State<Arc<AppState>>,
    Path(account_id): Path<String>,
    Json(body): Json<serde_json::Value>,
) -> Response {
    let conn = get_conn!(state);

    let enabled = match body.get("enabled").and_then(|v| v.as_bool()) {
        Some(e) => e,
        None => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": "enabled (boolean) is required"})),
            )
                .into_response();
        }
    };

    match account_repo::find_by_id(&conn, &account_id) {
        Ok(Some(_)) => {}
        Ok(None) => {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({"error": "Account not found"})),
            )
                .into_response();
        }
        Err(e) => {
            warn!("Failed to find account {account_id}: {e}");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": "Database error"})),
            )
                .into_response();
        }
    }

    if let Err(e) = account_repo::set_overage_protection(&conn, &account_id, enabled) {
        warn!("Failed to set overage_protection for {account_id}: {e}");
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": "Failed to update overage protection"})),
        )
            .into_response();
    }

    Json(json!({"success": true, "enabled": enabled})).into_response()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::routing::{delete, get, post};
    use axum::Router;
    use bccf_core::config::Config;
    use bccf_core::state::AppStateBuilder;
    use bccf_database::pool::{create_memory_pool, PoolConfig};
    use bccf_database::DbPool;
    use http::Request;
    use tower::ServiceExt;

    /// Create a test AppState with an in-memory database that has the schema applied.
    fn test_state_with_db() -> Arc<AppState> {
        let config = Config::load(Some(std::path::PathBuf::from(
            "/tmp/bccf-test-accounts-nonexistent/config.json",
        )))
        .unwrap();
        let pool = create_memory_pool(&PoolConfig::default()).unwrap();
        let state = AppStateBuilder::new(config).db_pool(pool).build();
        Arc::new(state)
    }

    /// Insert a test account directly into the database.
    fn insert_test_account(state: &AppState, id: &str, name: &str) {
        let pool = state.db_pool::<DbPool>().unwrap();
        let conn = pool.get().unwrap();
        let account = Account {
            id: id.to_string(),
            name: name.to_string(),
            provider: "anthropic".to_string(),
            api_key: None,
            refresh_token: "rt_test".to_string(),
            access_token: Some("at_test".to_string()),
            expires_at: Some(9999999999999),
            request_count: 5,
            total_requests: 42,
            last_used: Some(1700000000000),
            created_at: 1700000000000,
            rate_limited_until: None,
            session_start: None,
            session_request_count: 0,
            paused: false,
            rate_limit_reset: None,
            rate_limit_status: None,
            rate_limit_remaining: None,
            priority: 0,
            auto_fallback_enabled: true,
            auto_refresh_enabled: true,
            custom_endpoint: None,
            model_mappings: None,
            reserve_5h: 0,
            reserve_weekly: 0,
            reserve_hard: false,
            subscription_tier: None,
            email: None,
            refresh_token_updated_at: None,
            is_shared: false,
            overage_protection: true,
        };
        account_repo::create(&conn, &account).unwrap();
    }

    fn build_test_router(state: Arc<AppState>) -> Router {
        Router::new()
            .route("/api/accounts", get(list_accounts).post(create_account))
            .route("/api/accounts/{id}/pause", post(pause_account))
            .route("/api/accounts/{id}/resume", post(resume_account))
            .route("/api/accounts/{id}/reload", post(reload_account))
            .route("/api/accounts/{id}/priority", post(update_priority))
            .route("/api/accounts/{id}/rename", post(rename_account))
            .route("/api/accounts/{id}", delete(delete_account))
            .route("/api/accounts/{id}/auto-fallback", post(set_auto_fallback))
            .with_state(state)
    }

    #[tokio::test]
    async fn list_accounts_empty() {
        let state = test_state_with_db();
        let app = build_test_router(state);

        let req = Request::builder()
            .uri("/api/accounts")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), 200);

        let body = axum::body::to_bytes(resp.into_body(), 4096).await.unwrap();
        let json: Vec<serde_json::Value> = serde_json::from_slice(&body).unwrap();
        assert!(json.is_empty());
    }

    #[tokio::test]
    async fn list_accounts_with_data() {
        let state = test_state_with_db();
        insert_test_account(&state, "acc1", "Test Account");
        let app = build_test_router(state);

        let req = Request::builder()
            .uri("/api/accounts")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), 200);

        let body = axum::body::to_bytes(resp.into_body(), 8192).await.unwrap();
        let json: Vec<serde_json::Value> = serde_json::from_slice(&body).unwrap();
        assert_eq!(json.len(), 1);
        assert_eq!(json[0]["name"], "Test Account");
        assert_eq!(json[0]["provider"], "anthropic");
        assert_eq!(json[0]["tokenStatus"], "valid");
        assert_eq!(json[0]["hasRefreshToken"], true);
        assert_eq!(json[0]["requestCount"], 5);
        assert_eq!(json[0]["totalRequests"], 42);
    }

    #[tokio::test]
    async fn pause_and_resume_account() {
        let state = test_state_with_db();
        insert_test_account(&state, "acc1", "Test");
        let app = build_test_router(state.clone());

        // Pause
        let req = Request::builder()
            .method("POST")
            .uri("/api/accounts/acc1/pause")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), 200);

        // Verify paused
        let pool = state.db_pool::<DbPool>().unwrap();
        let conn = pool.get().unwrap();
        let acct = account_repo::find_by_id(&conn, "acc1").unwrap().unwrap();
        assert!(acct.paused);

        // Resume
        let app = build_test_router(state.clone());
        let req = Request::builder()
            .method("POST")
            .uri("/api/accounts/acc1/resume")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), 200);

        let acct = account_repo::find_by_id(&conn, "acc1").unwrap().unwrap();
        assert!(!acct.paused);
    }

    #[tokio::test]
    async fn pause_nonexistent_returns_404() {
        let state = test_state_with_db();
        let app = build_test_router(state);

        let req = Request::builder()
            .method("POST")
            .uri("/api/accounts/nonexistent/pause")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), 404);
    }

    #[tokio::test]
    async fn reload_account_returns_data() {
        let state = test_state_with_db();
        insert_test_account(&state, "acc1", "Test");
        let app = build_test_router(state);

        let req = Request::builder()
            .method("POST")
            .uri("/api/accounts/acc1/reload")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), 200);

        let body = axum::body::to_bytes(resp.into_body(), 8192).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["success"], true);
        assert_eq!(json["account"]["name"], "Test");
    }

    #[tokio::test]
    async fn reload_nonexistent_returns_404() {
        let state = test_state_with_db();
        let app = build_test_router(state);

        let req = Request::builder()
            .method("POST")
            .uri("/api/accounts/missing/reload")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), 404);
    }

    #[tokio::test]
    async fn update_priority_success() {
        let state = test_state_with_db();
        insert_test_account(&state, "acc1", "Test");
        let app = build_test_router(state.clone());

        let req = Request::builder()
            .method("POST")
            .uri("/api/accounts/acc1/priority")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"priority":10}"#))
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), 200);

        let pool = state.db_pool::<DbPool>().unwrap();
        let conn = pool.get().unwrap();
        let acct = account_repo::find_by_id(&conn, "acc1").unwrap().unwrap();
        assert_eq!(acct.priority, 10);
    }

    #[tokio::test]
    async fn update_priority_invalid() {
        let state = test_state_with_db();
        insert_test_account(&state, "acc1", "Test");
        let app = build_test_router(state);

        let req = Request::builder()
            .method("POST")
            .uri("/api/accounts/acc1/priority")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"priority":999}"#))
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), 400);
    }

    #[tokio::test]
    async fn rename_account_success() {
        let state = test_state_with_db();
        insert_test_account(&state, "acc1", "Old Name");
        let app = build_test_router(state.clone());

        let req = Request::builder()
            .method("POST")
            .uri("/api/accounts/acc1/rename")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"name":"New Name"}"#))
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), 200);

        let pool = state.db_pool::<DbPool>().unwrap();
        let conn = pool.get().unwrap();
        let acct = account_repo::find_by_id(&conn, "acc1").unwrap().unwrap();
        assert_eq!(acct.name, "New Name");
    }

    #[tokio::test]
    async fn delete_account_success() {
        let state = test_state_with_db();
        insert_test_account(&state, "acc1", "Test");
        let app = build_test_router(state.clone());

        let req = Request::builder()
            .method("DELETE")
            .uri("/api/accounts/acc1")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), 200);

        let pool = state.db_pool::<DbPool>().unwrap();
        let conn = pool.get().unwrap();
        assert!(account_repo::find_by_id(&conn, "acc1").unwrap().is_none());
    }

    #[tokio::test]
    async fn delete_nonexistent_returns_404() {
        let state = test_state_with_db();
        let app = build_test_router(state);

        let req = Request::builder()
            .method("DELETE")
            .uri("/api/accounts/missing")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), 404);
    }

    #[tokio::test]
    async fn set_auto_fallback_success() {
        let state = test_state_with_db();
        insert_test_account(&state, "acc1", "Test");
        let app = build_test_router(state.clone());

        let req = Request::builder()
            .method("POST")
            .uri("/api/accounts/acc1/auto-fallback")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"enabled":false}"#))
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), 200);

        let pool = state.db_pool::<DbPool>().unwrap();
        let conn = pool.get().unwrap();
        let acct = account_repo::find_by_id(&conn, "acc1").unwrap().unwrap();
        assert!(!acct.auto_fallback_enabled);
    }

    #[test]
    fn millis_to_iso_works() {
        let iso = millis_to_iso(1700000000000);
        assert!(iso.starts_with("2023-11-14"));
    }

    #[test]
    fn account_to_response_token_expired() {
        let now = 1700000000000_i64;
        let account = Account {
            id: "a1".to_string(),
            name: "Test".to_string(),
            provider: "anthropic".to_string(),
            api_key: None,
            refresh_token: "rt".to_string(),
            access_token: Some("at".to_string()),
            expires_at: Some(now - 1000), // expired
            request_count: 0,
            total_requests: 0,
            last_used: None,
            created_at: now,
            rate_limited_until: None,
            session_start: None,
            session_request_count: 0,
            paused: false,
            rate_limit_reset: None,
            rate_limit_status: None,
            rate_limit_remaining: None,
            priority: 0,
            auto_fallback_enabled: true,
            auto_refresh_enabled: true,
            custom_endpoint: None,
            model_mappings: None,
            reserve_5h: 0,
            reserve_weekly: 0,
            reserve_hard: false,
            subscription_tier: None,
            email: None,
            refresh_token_updated_at: None,
            is_shared: false,
            overage_protection: true,
        };

        let resp = account_to_response(&account, now, None);
        assert_eq!(resp.token_status, TokenStatus::Expired);
        assert!(resp.has_refresh_token);
    }

    #[test]
    fn account_to_response_session_active() {
        let now = 1700000000000_i64;
        let account = Account {
            id: "a1".to_string(),
            name: "Test".to_string(),
            provider: "anthropic".to_string(),
            api_key: None,
            refresh_token: String::new(),
            access_token: None,
            expires_at: None,
            request_count: 0,
            total_requests: 0,
            last_used: None,
            created_at: now,
            rate_limited_until: None,
            session_start: Some(now - 60_000), // started 1 min ago
            session_request_count: 5,
            paused: false,
            rate_limit_reset: None,
            rate_limit_status: None,
            rate_limit_remaining: None,
            priority: 0,
            auto_fallback_enabled: true,
            auto_refresh_enabled: true,
            custom_endpoint: None,
            model_mappings: None,
            reserve_5h: 0,
            reserve_weekly: 0,
            reserve_hard: false,
            subscription_tier: None,
            email: None,
            refresh_token_updated_at: None,
            is_shared: false,
            overage_protection: true,
        };

        let resp = account_to_response(&account, now, None);
        assert_eq!(resp.session_info, "Active: 5 reqs");
        assert!(!resp.has_refresh_token); // empty refresh_token
    }
}
