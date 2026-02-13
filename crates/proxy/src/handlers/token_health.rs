//! Token health API handlers.
//!
//! - `GET /api/token-health` — per-account token health status

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::response::{IntoResponse, Response};
use axum::Json;
use http::StatusCode;
use serde_json::json;
use tracing::error;

use bccf_core::AppState;
use bccf_database::repositories::account as account_repo;
use bccf_database::DbPool;

use crate::token_health;

// ---------------------------------------------------------------------------
// GET /api/token-health
// ---------------------------------------------------------------------------

/// Token health report for all accounts.
pub async fn get_token_health(State(state): State<Arc<AppState>>) -> Response {
    let Some(pool) = state.db_pool::<DbPool>() else {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": "Database not available"})),
        )
            .into_response();
    };
    let Ok(conn) = pool.get() else {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": "Database connection failed"})),
        )
            .into_response();
    };

    let accounts = match account_repo::find_all(&conn) {
        Ok(a) => a,
        Err(e) => {
            error!("Failed to fetch accounts: {e}");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": "Database error"})),
            )
                .into_response();
        }
    };

    let now = chrono::Utc::now().timestamp_millis();
    let report = token_health::check_all_accounts_health(&accounts, now);

    Json(json!(report)).into_response()
}

// ---------------------------------------------------------------------------
// GET /api/token-health/reauth
// ---------------------------------------------------------------------------

/// Accounts needing re-authentication.
pub async fn get_reauth_needed(State(state): State<Arc<AppState>>) -> Response {
    let Some(pool) = state.db_pool::<DbPool>() else {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": "Database not available"})),
        )
            .into_response();
    };
    let Ok(conn) = pool.get() else {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": "Database connection failed"})),
        )
            .into_response();
    };

    let accounts = match account_repo::find_all(&conn) {
        Ok(a) => a,
        Err(e) => {
            error!("Failed to fetch accounts: {e}");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": "Database error"})),
            )
                .into_response();
        }
    };

    let now = chrono::Utc::now().timestamp_millis();
    let needing_reauth: Vec<_> = accounts
        .iter()
        .map(|a| token_health::check_refresh_token_health(a, now))
        .filter(|s| s.requires_reauth)
        .collect();

    let count = needing_reauth.len();

    Json(json!({
        "accounts": needing_reauth,
        "count": count,
        "needsReauth": count > 0,
    }))
    .into_response()
}

// ---------------------------------------------------------------------------
// GET /api/token-health/:account_name
// ---------------------------------------------------------------------------

/// Token health for a specific account by name.
pub async fn get_account_token_health(
    State(state): State<Arc<AppState>>,
    Path(account_name): Path<String>,
) -> Response {
    let Some(pool) = state.db_pool::<DbPool>() else {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": "Database not available"})),
        )
            .into_response();
    };
    let Ok(conn) = pool.get() else {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": "Database connection failed"})),
        )
            .into_response();
    };

    let account = match account_repo::find_by_name(&conn, &account_name) {
        Ok(Some(a)) => a,
        Ok(None) => {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({"error": format!("Account '{}' not found", account_name)})),
            )
                .into_response();
        }
        Err(e) => {
            error!("Failed to fetch account: {e}");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": "Database error"})),
            )
                .into_response();
        }
    };

    let now = chrono::Utc::now().timestamp_millis();
    let health = token_health::check_refresh_token_health(&account, now);

    Json(json!(health)).into_response()
}
